mod app_password;
mod instances;
mod setup;

use std::path::Path;

use crate::config::Config;
use crate::sync::run_sync;

use anyhow::{anyhow, Result};
use app_password::acquire_app_password;
use clap::{Parser, Subcommand};
use instances::{run_instances_cmd, Instances};
use setup::{create_default_config, create_systemd_service};
use tracing::{info, warn};

#[derive(Parser)]
#[command(name = "pihole-sync")]
#[command(about = "Syncs Pi-Hole v6 instances using REST API", long_about = None)]
pub struct Cli {
    /// Path to the configuration file
    #[arg(short, long)]
    pub config: Option<String>,

    /// Print version information
    #[arg(short = 'V', long = "version", action)]
    pub version: bool,

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

    /// Create helper files
    Setup {
        #[command(subcommand)]
        command: SetupCommands,
    },

    #[command(subcommand)]
    Instances(Instances),
}

#[derive(Subcommand)]
enum SetupCommands {
    /// Create a default config.yaml in the current directory
    DefaultConfig,
    /// Create a systemd service file for the current directory
    Systemd,
}

impl Cli {
    pub async fn parse_args() -> Result<()> {
        let cli = Cli::parse();

        if cli.version {
            println!("pihole-sync {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }

        if let Some(command) = &cli.command {
            match command {
                Commands::Setup {
                    command: SetupCommands::DefaultConfig,
                } => {
                    create_default_config()?;
                    return Ok(());
                }
                Commands::Setup {
                    command: SetupCommands::Systemd,
                } => {
                    create_systemd_service()?;
                    return Ok(());
                }
                _ => {}
            }
        }

        let config_path_str = Self::resolve_config_path(cli.config.as_deref())?;

        info!("Using config: {}", config_path_str);

        if let Some(command) = cli.command {
            let mut config = Config::load(&config_path_str)?;

            match command {
                Commands::Sync {
                    once,
                    no_initial_sync,
                } => {
                    let has_teleporter_secondaries = config.secondary.iter().any(|secondary| {
                        matches!(
                            secondary.sync_mode,
                            Some(crate::config::SyncMode::Teleporter) | None
                        )
                    });
                    if has_teleporter_secondaries
                        && matches!(
                            config.sync.trigger_mode,
                            crate::config::SyncTriggerMode::WatchConfigFile
                                | crate::config::SyncTriggerMode::WatchConfigApi
                        )
                    {
                        let trigger_mode = match config.sync.trigger_mode {
                            crate::config::SyncTriggerMode::WatchConfigFile => "watch_config_file",
                            crate::config::SyncTriggerMode::WatchConfigApi => "watch_config_api",
                            _ => "interval",
                        };
                        warn!(
                            "Teleporter imports temporarily make the target Pi-hole unresponsive. Using sync mode {} with teleporter is not recommended; frequent config changes can leave secondary Pi-holes unresponsive for significant periods.",
                            trigger_mode
                        );
                    }

                    run_sync(&config_path_str, once, no_initial_sync).await?;
                }

                Commands::AppPassword => {
                    acquire_app_password(&config_path_str).await?;
                }

                Commands::Instances(instances_cmd) => {
                    run_instances_cmd(instances_cmd, &mut config, &config_path_str)?;
                }

                Commands::Setup { .. } => unreachable!("Setup commands handled earlier"),
            }
            return Ok(()); // Exit after CLI command execution
        } else {
            warn!("Please specify a command to run. Use --help for more information.");
        }

        Ok(())
    }

    fn resolve_config_path(config_path_cli: Option<&str>) -> Result<String> {
        if let Some(config_path_cli) = config_path_cli {
            return Ok(config_path_cli.to_string());
        }

        let config_path_yaml = Path::new("/etc/pihole-sync/config.yaml");

        if config_path_yaml.exists() {
            if let Some(path_str) = config_path_yaml.to_str() {
                return Ok(path_str.to_string());
            }
        }

        Err(anyhow!(
            "No default config found and --config not specified. Please create a default YAML config file (/etc/pihole-sync/config.yaml) or use the --config flag."
        ))
    }
}
