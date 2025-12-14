mod app_password;
mod instances;

use std::path::Path;

use crate::config::Config;
use crate::sync::run_sync;

use anyhow::Result;
use app_password::acquire_app_password;
use clap::{Parser, Subcommand};
use instances::{run_instances_cmd, Instances};
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
        /// Skip the initial sync run on startup (useful for watch modes)
        #[arg(long, action)]
        no_initial_sync: bool,
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

        let config_path_yaml = Path::new("/etc/pihole-sync/config.yaml");

        if config_path_yaml.exists() {
            config_path_str = config_path_yaml.to_str().unwrap();
        }

        if let Some(config_path_cli) = &cli.config {
            config_path_str = config_path_cli;
        } else if config_path_str.is_empty() {
            panic!("No default config found and --config not specified. Please create a default YAML config file (/etc/pihole-sync/config.yaml) or use the --config flag.")
        }

        info!("Using config: {}", config_path_str);

        if let Some(command) = cli.command {
            let mut config = Config::load(config_path_str)?;

            match command {
                Commands::Sync {
                    once,
                    no_initial_sync,
                } => {
                    run_sync(config_path_str, once, no_initial_sync).await?;
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
