use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncConfig {
    pub interval: u64,
    pub cache_location: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Instance {
    pub host: String,
    pub schema: String,
    pub port: u16,
    pub api_key: String,
    pub update_gravity: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub sync: SyncConfig,
    pub main: Instance,
    pub secondary: Vec<Instance>,
}

impl Config {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file: {:?}", path.as_ref()))?;
        let config: Config =
            toml::from_str(&content).with_context(|| "Failed to parse config file as TOML")?;
        Ok(config)
    }

    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let content = toml::to_string_pretty(self).context("Failed to serialize configuration")?;
        fs::write(path, content).context("Failed to write configuration file")?;
        Ok(())
    }
}
