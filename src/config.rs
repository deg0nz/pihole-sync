use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
use tracing::warn;

use crate::constants::{DEFAULT_PIHOLE_CONFIG_PATH, DEFAULT_SYNC_INTERVAL_MINUTES};

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    Teleporter,
    Api,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncTriggerMode {
    Interval,
    WatchConfigFile,
    WatchConfigApi,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncConfig {
    #[serde(default = "default_interval_minutes")]
    pub interval: u64,
    pub cache_location: String,
    #[serde(default = "default_trigger_mode")]
    pub trigger_mode: SyncTriggerMode,
    #[serde(default = "default_pihole_config_path")]
    pub config_path: String,
    #[serde(default)]
    pub api_poll_interval: Option<u64>,
    /// Timeout (in seconds) to wait for Pi-hole API readiness after a trigger before running sync
    #[serde(default = "default_trigger_api_readiness_timeout_secs")]
    pub trigger_api_readiness_timeout_secs: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct InstanceConfig {
    pub host: String,
    pub schema: String,
    pub port: u16,
    pub api_key: String,
    /// Accept invalid TLS certificates (e.g. self-signed Pi-hole certs). Disables verification!
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accept_invalid_certs: bool,
    pub update_gravity: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_mode: Option<SyncMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_sync_options: Option<ApiSyncOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teleporter_sync_options: Option<TeleporterImportOptions>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConfigSyncOptions {
    #[serde(default)]
    pub mode: Option<ConfigApiSyncMode>,
    pub filter_keys: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ApiSyncOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_config: Option<ConfigSyncOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_groups: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_lists: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigApiSyncMode {
    Include,
    Exclude,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TeleporterImportOptions {
    #[serde(default = "default_true")]
    pub config: bool,
    #[serde(default = "default_true")]
    pub dhcp_leases: bool,
    #[serde(default)]
    pub gravity: GravitySyncIncludes,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GravitySyncIncludes {
    #[serde(default = "default_true")]
    pub group: bool,
    #[serde(default = "default_true")]
    pub adlist: bool,
    #[serde(default = "default_true")]
    pub adlist_by_group: bool,
    #[serde(default = "default_true")]
    pub domainlist: bool,
    #[serde(default = "default_true")]
    pub domainlist_by_group: bool,
    #[serde(default = "default_true")]
    pub client: bool,
    #[serde(default = "default_true")]
    pub client_by_group: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub sync: SyncConfig,
    pub main: InstanceConfig,
    pub secondary: Vec<InstanceConfig>,
}

fn default_true() -> bool {
    true
}

fn default_trigger_mode() -> SyncTriggerMode {
    SyncTriggerMode::Interval
}

fn default_pihole_config_path() -> String {
    DEFAULT_PIHOLE_CONFIG_PATH.to_string()
}

fn default_interval_minutes() -> u64 {
    DEFAULT_SYNC_INTERVAL_MINUTES
}

fn default_trigger_api_readiness_timeout_secs() -> u64 {
    60
}

impl Default for TeleporterImportOptions {
    fn default() -> Self {
        Self {
            config: true,
            dhcp_leases: true,
            gravity: GravitySyncIncludes::default(),
        }
    }
}

impl Default for GravitySyncIncludes {
    fn default() -> Self {
        Self {
            group: true,
            adlist: true,
            adlist_by_group: true,
            domainlist: true,
            domainlist_by_group: true,
            client: true,
            client_by_group: true,
        }
    }
}

impl InstanceConfig {
    /// Effective sync mode: explicit `sync_mode`, else api if `api_sync_options` is set
    pub fn sync_mode(&self) -> SyncMode {
        self.sync_mode
            .unwrap_or(if self.api_sync_options.is_some() {
                SyncMode::Api
            } else {
                SyncMode::Teleporter
            })
    }
}

/// Apply defaults and validate a secondary instance
fn resolve_secondary(secondary: &mut InstanceConfig) -> Result<()> {
    match secondary.sync_mode() {
        SyncMode::Api => {
            let Some(options) = secondary.api_sync_options.as_mut() else {
                return Err(anyhow::anyhow!(
                    "[{}] sync_mode is api but api_sync_options is missing",
                    secondary.host
                ));
            };
            if let Some(config_opts) = options.sync_config.as_mut() {
                if config_opts.mode.is_none() {
                    warn!(
                        "[{}] api_sync_options.sync_config.mode is not set; defaulting to \"include\"",
                        secondary.host
                    );
                    config_opts.mode = Some(ConfigApiSyncMode::Include);
                }
            }
            if secondary.teleporter_sync_options.is_some() {
                warn!(
                    "[{}] sync_mode is api; teleporter options are ignored",
                    secondary.host
                );
            }
        }
        SyncMode::Teleporter => {
            if secondary.api_sync_options.is_some() {
                return Err(anyhow::anyhow!(
                    "[{}] sync_mode is teleporter but api_sync_options is present; remove it or set sync_mode to api",
                    secondary.host
                ));
            }
            secondary
                .teleporter_sync_options
                .get_or_insert_with(TeleporterImportOptions::default);
        }
    }

    Ok(())
}

impl Config {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file: {:?}", path.as_ref()))?;

        if path
            .as_ref()
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("toml"))
            .unwrap_or(false)
        {
            return Err(anyhow::anyhow!(
                "TOML configs are no longer supported. Please migrate to YAML (e.g. config.yaml)."
            ));
        }

        let mut config: Config = serde_yaml::from_str(&content)
            .with_context(|| "Failed to parse config file as YAML")?;

        for secondary in &mut config.secondary {
            resolve_secondary(secondary)?;
        }

        Ok(config)
    }

    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let content =
            serde_yaml::to_string(self).context("Failed to serialize configuration to YAML")?;

        fs::write(&path, content).context("Failed to write configuration file")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_current_format_with_api_and_teleporter_secondaries() {
        let yaml = r#"
sync:
  trigger_mode: interval
  interval: 60
  cache_location: /tmp/c
main:
  host: main.example.com
  schema: https
  port: 443
  api_key: k
secondary:
  - host: a.example.com
    schema: https
    port: 443
    api_key: k
    sync_mode: api
    api_sync_options:
      sync_config:
        mode: include
        filter_keys: ["dns.hosts"]
  - host: b.example.com
    schema: http
    port: 80
    api_key: k
    sync_mode: teleporter
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        fs::write(&path, yaml).unwrap();
        let cfg = Config::load(&path).unwrap();

        assert!(!cfg.main.accept_invalid_certs);
        assert_eq!(cfg.secondary[0].sync_mode(), SyncMode::Api);
        let sc = cfg.secondary[0]
            .api_sync_options
            .as_ref()
            .unwrap()
            .sync_config
            .as_ref()
            .unwrap();
        assert_eq!(sc.mode, Some(ConfigApiSyncMode::Include));
        assert_eq!(sc.filter_keys, vec!["dns.hosts"]);
        assert_eq!(cfg.secondary[1].sync_mode(), SyncMode::Teleporter);
        assert!(cfg.secondary.iter().all(|s| !s.accept_invalid_certs));
    }
}
