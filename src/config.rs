use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
use tracing::{debug, warn};

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncConfig {
    pub interval: u64,
    pub cache_location: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct InstanceConfig {
    pub host: String,
    pub schema: String,
    pub port: u16,
    pub api_key: String,
    pub update_gravity: Option<bool>,
    pub config_excludes: Option<Vec<String>>,
    pub import_options: Option<TeleporterImportOptions>,
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

        // Get file extension
        let extension = path
            .as_ref()
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("");

        // Parse based on file extension
        let mut config: Config = match extension.to_lowercase().as_str() {
            "yaml" | "yml" => serde_yaml::from_str(&content)
                .with_context(|| "Failed to parse config file as YAML")?,
            "toml" => {
                warn!("DEPRECATION WARNING: TOML configs are deprecated and support for them will be removed soon. Please migrate to YAML config");
                toml::from_str(&content).with_context(|| "Failed to parse config file as TOML")?
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Unsupported config file format. Use .yaml, .yml, or .toml"
                ))
            }
        };

        // Disable teleporter sync of config, when excludes are found
        for secondary in &mut config.secondary {
            if secondary.config_excludes.is_some() {
                if let Some(import_options) = &mut secondary.import_options {
                    debug!("Found exclude_config_properies. Disabling Teleporter config sync.");
                    import_options.config = false;
                }
            }
        }

        Ok(config)
    }

    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        // Get file extension
        let extension = path
            .as_ref()
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("");

        // Serialize based on file extension
        let content =
            match extension.to_lowercase().as_str() {
                "yaml" | "yml" => serde_yaml::to_string(self)
                    .context("Failed to serialize configuration to YAML")?,
                "toml" => toml::to_string_pretty(self)
                    .context("Failed to serialize configuration to TOML")?,
                _ => {
                    return Err(anyhow::anyhow!(
                        "Unsupported config file format. Use .yaml, .yml, or .toml"
                    ))
                }
            };

        fs::write(&path, content).context("Failed to write configuration file")?;
        Ok(())
    }
}
