mod app_password;
mod instances;
mod sync;

use std::path::Path;

use crate::config::Config;

use anyhow::Result;
use app_password::acquire_app_password;
use clap::{Parser, Subcommand};
use instances::{run_instances_cmd, Instances};
use sync::run_sync;
use tracing::{info, warn};

#[derive(Parser)]
#[command(name = "pihole-sync")]
#[command(about = "Syncs Pi-Hole v6 instances using REST API", long_about = None)]
pub struct Cli {
    /// Path to the configuration file
    #[arg(short, long)]
    pub config: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run sync
    Sync {
        /// Run once and exit
        #[arg(short, long, action)]
        once: bool,
    },

    /// Acquire an app password for a Pi-hole instance
    AppPassword,

    #[command(subcommand)]
    Instances(Instances),
}

impl Cli {
    pub async fn parse_args() -> Result<()> {
        let cli = Cli::parse();

        let mut config_path_str = "";

        let config_path_toml = Path::new("/etc/pihole-sync/config.toml");
        let config_path_yaml = Path::new("/etc/pihole-sync/config.yaml");

        if config_path_toml.exists() && config_path_yaml.exists() {
            panic!("TOML and YAML config files found. Please remove one of them.");
        }

        if config_path_toml.exists() {
            warn!(
                "DEPRECATED: TOML config files are deprecated. Please migrate to YAML config file."
            );
            config_path_str = config_path_toml.to_str().unwrap();
        } else if config_path_yaml.exists() {
            config_path_str = config_path_yaml.to_str().unwrap();
        }

        if let Some(config_path_cli) = &cli.config {
            config_path_str = config_path_cli;
        } else if config_path_str.is_empty() {
            panic!("No default config found and --config not specified. Please create a default config file (/etc/pihole-sync/config.yaml) or use the --config flag.")
        }

        info!("Using config: {}", config_path_str);

        if let Some(command) = cli.command {
            let mut config = Config::load(config_path_str)?;

            match command {
                Commands::Sync { once } => {
                    run_sync(config_path_str, once).await?;
                }

                Commands::AppPassword => {
                    acquire_app_password(config_path_str).await?;
                }

                Commands::Instances(instances_cmd) => {
                    run_instances_cmd(instances_cmd, &mut config, config_path_str)?;
                }
            }
            return Ok(()); // Exit after CLI command execution
        } else {
            warn!("Please specify a command to run. Use --help for more information.");
        }

        Ok(())
    }
}
