use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
use tracing::warn;

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    Teleporter,
    ConfigApi,
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
    pub interval: u64,
    pub cache_location: String,
    #[serde(default = "default_trigger_mode")]
    pub trigger_mode: SyncTriggerMode,
    #[serde(default = "default_pihole_config_path")]
    pub config_path: String,
    #[serde(default)]
    pub api_poll_interval: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct InstanceConfig {
    pub host: String,
    pub schema: String,
    pub port: u16,
    pub api_key: String,
    pub update_gravity: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_mode: Option<SyncMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_api_sync_options: Option<ConfigSyncOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none", skip_serializing)]
    pub config_sync: Option<ConfigSyncOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teleporter_sync_options: Option<TeleporterImportOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none", skip_serializing)]
    pub teleporter_options: Option<TeleporterImportOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none", skip_serializing)]
    pub import_options: Option<TeleporterImportOptions>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConfigSyncOptions {
    #[serde(default)]
    pub mode: Option<ConfigApiSyncMode>,
    pub filter_keys: Vec<String>,
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
    "/etc/pihole/pihole.toml".to_string()
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

impl Config {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file: {:?}", path.as_ref()))?;

        let mut config: Config = serde_yaml::from_str(&content)
            .with_context(|| "Failed to parse config file as YAML")?;

        for secondary in &mut config.secondary {
            // Migrate deprecated config keys to new names (keep backwards compatibility).
            if secondary.config_api_sync_options.is_none() && secondary.config_sync.is_some() {
                warn!(
                    "[{}] DEPRECATION WARNING: config_sync has been renamed to config_api_sync_options; please update your config file.",
                    secondary.host
                );
                secondary.config_api_sync_options = secondary.config_sync.clone();
            } else if secondary.config_api_sync_options.is_some() && secondary.config_sync.is_some()
            {
                warn!(
                    "[{}] Found config_api_sync_options and deprecated config_sync. Ignoring config_sync.",
                    secondary.host
                );
            }

            if secondary.teleporter_sync_options.is_none() && secondary.teleporter_options.is_some()
            {
                warn!(
                    "[{}] DEPRECATION WARNING: teleporter_options has been renamed to teleporter_sync_options; please update your config file.",
                    secondary.host
                );
                secondary.teleporter_sync_options = secondary.teleporter_options.clone();
            } else if secondary.teleporter_sync_options.is_some()
                && secondary.teleporter_options.is_some()
            {
                warn!(
                    "[{}] Found teleporter_sync_options and deprecated teleporter_options. Ignoring teleporter_options.",
                    secondary.host
                );
            }

            if secondary.import_options.is_some() {
                warn!(
                    "[{}] DEPRECATION WARNING: import_options has been renamed to teleporter_sync_options; this field will be removed in 1.0.0. Please update your config file.",
                    secondary.host
                );
                if secondary.teleporter_sync_options.is_none() {
                    secondary.teleporter_sync_options = secondary.import_options.clone();
                } else {
                    warn!(
                        "[{}] Found import_options and teleporter_sync_options. Ignoring import_options.",
                        secondary.host
                    );
                }
            }

            // Determine effective sync mode (backwards compatible).
            let effective_mode = match secondary.sync_mode {
                Some(mode) => mode,
                None => {
                    if secondary.config_api_sync_options.is_some() {
                        SyncMode::ConfigApi
                    } else {
                        SyncMode::Teleporter
                    }
                }
            };
            secondary.sync_mode = Some(effective_mode);

            if let Some(options) = secondary.config_api_sync_options.as_mut() {
                if options.mode.is_none() {
                    warn!(
                        "[{}] config_api_sync_options.mode is not set; defaulting to \"include\"",
                        secondary.host
                    );
                    options.mode = Some(ConfigApiSyncMode::Include);
                }
            }

            match effective_mode {
                SyncMode::ConfigApi => {
                    if secondary.config_api_sync_options.is_none() {
                        return Err(anyhow::anyhow!(
                            "[{}] sync_mode is config_api but config_api_sync_options is missing",
                            secondary.host
                        ));
                    }

                    if secondary.teleporter_sync_options.is_some()
                        || secondary.teleporter_options.is_some()
                        || secondary.import_options.is_some()
                    {
                        warn!(
                            "[{}] sync_mode is config_api; teleporter options are ignored",
                            secondary.host
                        );
                    }
                }
                SyncMode::Teleporter => {
                    if secondary.config_api_sync_options.is_some()
                        || secondary.config_sync.is_some()
                    {
                        return Err(anyhow::anyhow!(
                            "[{}] sync_mode is teleporter but config_api_sync_options is present; remove it or set sync_mode to config_api",
                            secondary.host
                        ));
                    }

                    if secondary.teleporter_sync_options.is_none() {
                        secondary.teleporter_sync_options =
                            Some(TeleporterImportOptions::default());
                    }
                }
            }

            // Clear deprecated fields after migration to avoid ambiguity.
            secondary.config_sync = None;
            secondary.teleporter_options = None;
            secondary.import_options = None;
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
