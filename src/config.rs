use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
use tracing::warn;

use crate::constants::{DEFAULT_PIHOLE_CONFIG_PATH, DEFAULT_SYNC_INTERVAL_MINUTES};

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    Teleporter,
    #[serde(alias = "config_api")]
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
    pub update_gravity: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_mode: Option<SyncMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_sync_options: Option<ApiSyncOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none", skip_serializing)]
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

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ApiSyncOptions {
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "config")]
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

/// Helper for migrating deprecated config fields
struct ConfigMigration<'a> {
    secondary: &'a mut InstanceConfig,
}

impl<'a> ConfigMigration<'a> {
    fn new(secondary: &'a mut InstanceConfig) -> Self {
        Self { secondary }
    }

    /// Migrate all deprecated fields
    fn migrate_all(&mut self) {
        self.migrate_config_sync();
        self.migrate_config_api_sync_options();
        self.migrate_teleporter_options();
        self.migrate_import_options();
    }

    /// Migrate config_sync -> api_sync_options.config
    fn migrate_config_sync(&mut self) {
        if self.secondary.config_api_sync_options.is_none() && self.secondary.config_sync.is_some()
        {
            warn!(
                "[{}] DEPRECATION WARNING: config_sync has been renamed to api_sync_options.config; please update your config file.",
                self.secondary.host
            );
            self.secondary.config_api_sync_options = self.secondary.config_sync.clone();
        } else if self.secondary.config_api_sync_options.is_some()
            && self.secondary.config_sync.is_some()
        {
            warn!(
                "[{}] Found api_sync_options.config (config_api_sync_options) and deprecated config_sync. Ignoring config_sync.",
                self.secondary.host
            );
        }
    }

    /// Migrate config_api_sync_options -> api_sync_options.sync_config
    fn migrate_config_api_sync_options(&mut self) {
        if self.secondary.api_sync_options.is_none()
            && self.secondary.config_api_sync_options.is_some()
        {
            warn!(
                "[{}] DEPRECATION WARNING: config_api_sync_options has been renamed to api_sync_options.sync_config; please update your config file.",
                self.secondary.host
            );
            self.secondary.api_sync_options = Some(ApiSyncOptions {
                sync_config: self.secondary.config_api_sync_options.clone(),
                ..ApiSyncOptions::default()
            });
        } else if self.secondary.api_sync_options.is_some()
            && self.secondary.config_api_sync_options.is_some()
        {
            warn!(
                "[{}] Found api_sync_options and deprecated config_api_sync_options. Ignoring config_api_sync_options.",
                self.secondary.host
            );
        }
    }

    /// Migrate teleporter_options -> teleporter_sync_options
    fn migrate_teleporter_options(&mut self) {
        if self.secondary.teleporter_sync_options.is_none()
            && self.secondary.teleporter_options.is_some()
        {
            warn!(
                "[{}] DEPRECATION WARNING: teleporter_options has been renamed to teleporter_sync_options; please update your config file.",
                self.secondary.host
            );
            self.secondary.teleporter_sync_options = self.secondary.teleporter_options.clone();
        } else if self.secondary.teleporter_sync_options.is_some()
            && self.secondary.teleporter_options.is_some()
        {
            warn!(
                "[{}] Found teleporter_sync_options and deprecated teleporter_options. Ignoring teleporter_options.",
                self.secondary.host
            );
        }
    }

    /// Migrate import_options -> teleporter_sync_options
    fn migrate_import_options(&mut self) {
        if self.secondary.import_options.is_some() {
            warn!(
                "[{}] DEPRECATION WARNING: import_options has been renamed to teleporter_sync_options; this field will be removed in 1.0.0. Please update your config file.",
                self.secondary.host
            );
            if self.secondary.teleporter_sync_options.is_none() {
                self.secondary.teleporter_sync_options = self.secondary.import_options.clone();
            } else {
                warn!(
                    "[{}] Found import_options and teleporter_sync_options. Ignoring import_options.",
                    self.secondary.host
                );
            }
        }
    }

    /// Determine and set the effective sync mode
    fn determine_sync_mode(&mut self) {
        let effective_mode = match self.secondary.sync_mode {
            Some(mode) => mode,
            None => {
                if self.secondary.api_sync_options.is_some() {
                    SyncMode::Api
                } else {
                    SyncMode::Teleporter
                }
            }
        };
        self.secondary.sync_mode = Some(effective_mode);
    }

    /// Set default config mode if not specified
    fn set_default_config_mode(&mut self) {
        if let Some(options) = self.secondary.api_sync_options.as_mut() {
            if let Some(config_opts) = options.sync_config.as_mut() {
                if config_opts.mode.is_none() {
                    warn!(
                        "[{}] api_sync_options.sync_config.mode is not set; defaulting to \"include\"",
                        self.secondary.host
                    );
                    config_opts.mode = Some(ConfigApiSyncMode::Include);
                }
            }
        }
    }

    /// Validate sync mode configuration
    fn validate(&mut self) -> Result<()> {
        let effective_mode = self.secondary.sync_mode.unwrap();

        match effective_mode {
            SyncMode::Api => {
                if self.secondary.api_sync_options.is_none() {
                    return Err(anyhow::anyhow!(
                        "[{}] sync_mode is api but api_sync_options is missing",
                        self.secondary.host
                    ));
                }

                if self.secondary.teleporter_sync_options.is_some()
                    || self.secondary.teleporter_options.is_some()
                    || self.secondary.import_options.is_some()
                {
                    warn!(
                        "[{}] sync_mode is api; teleporter options are ignored",
                        self.secondary.host
                    );
                }
            }
            SyncMode::Teleporter => {
                if self.secondary.api_sync_options.is_some() || self.secondary.config_sync.is_some()
                {
                    return Err(anyhow::anyhow!(
                        "[{}] sync_mode is teleporter but api_sync_options is present; remove it or set sync_mode to api",
                        self.secondary.host
                    ));
                }

                if self.secondary.teleporter_sync_options.is_none() {
                    self.secondary.teleporter_sync_options =
                        Some(TeleporterImportOptions::default());
                }
            }
        }

        Ok(())
    }

    /// Clear deprecated fields after migration
    fn clear_deprecated_fields(&mut self) {
        self.secondary.config_sync = None;
        self.secondary.config_api_sync_options = None;
        self.secondary.teleporter_options = None;
        self.secondary.import_options = None;
    }
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

        // Migrate deprecated config fields for each secondary instance
        for secondary in &mut config.secondary {
            let mut migration = ConfigMigration::new(secondary);
            migration.migrate_all();
            migration.determine_sync_mode();
            migration.set_default_config_mode();
            migration.validate()?;
            migration.clear_deprecated_fields();
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
