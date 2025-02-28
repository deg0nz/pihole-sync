mod app_password;
mod instances;
mod sync;

use crate::config::Config;

use anyhow::Result;
use app_password::acquire_app_password;
use clap::{Parser, Subcommand};
use instances::{run_instances_cmd, Instances};
use sync::run_sync;
use tracing::{info, warn};

#[derive(Parser)]
#[command(name = "pihole-sync")]
#[command(about = "Syncs Pi-hole v6 instances using REST API", long_about = None)]
pub struct Cli {
    /// Path to the configuration file
    #[arg(short, long, default_value = "/etc/pihole-sync/config.toml")]
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

        let mut config_path = "/etc/pihole-sync/config.toml";

        if let Some(config_path_cli) = &cli.config {
            config_path = config_path_cli;
        }

        info!("Using config: {}", config_path);

        if let Some(command) = cli.command {
            let mut config = Config::load(config_path)?;

            match command {
                Commands::Sync { once } => {
                    run_sync(config_path, once).await?;
                }

                Commands::AppPassword => {
                    acquire_app_password(config_path).await?;
                }

                Commands::Instances(instances_cmd) => {
                    run_instances_cmd(instances_cmd, &mut config, config_path)?;
                }
            }
            return Ok(()); // Exit after CLI command execution
        } else {
            warn!("Please specify a command to run. Use --help for more information.");
        }

        Ok(())
    }
}
