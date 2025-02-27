mod config;
mod pihole;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::{Config, Instance};
use dialoguer::{theme::ColorfulTheme, Password, Select};
use indicatif::ProgressBar;
use pihole::{AppPassword, PiHoleClient};
use std::{fs, path::Path, time::Duration};
use tokio::time::sleep;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "pihole-sync")]
#[command(about = "Syncs PiHole v6 instances using REST API", long_about = None)]
struct Cli {
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

    /// Aquire an app password for a PiHole instance
    AppPassword,

    #[command(subcommand)]
    Instances(Instances),
}

#[derive(Subcommand)]
/// Manage PiHole instances
enum Instances {
    /// List all configured Pi-hole instances
    List,

    /// Add a new secondary instance
    Add {
        host: String,
        schema: String,
        port: u16,
        api_key: String,
        #[arg(short, long)]
        update_gravity: bool,
    },

    /// Remove a secondary instance by hostname
    Remove { host: String },
}

fn setup_logging() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();
}

async fn run_sync(config_path: &str, run_once: bool) -> Result<()> {
    // Load config
    let config = Config::load(config_path)?;
    let sync_interval = Duration::from_secs(config.sync.interval * 60);
    let backup_path = Path::new(&config.sync.cache_location).join("pihole_backup.zip");

    // Check cache directory
    info!("Checking cache directory: {}", backup_path.display());
    let path = Path::new(&config.sync.cache_location);

    if !path.exists() {
        info!("Cache directory does not exist. Trying to create it.");

        match fs::create_dir_all(&config.sync.cache_location) {
            Ok(_) => info!("Directory created successfully"),
            Err(e) => {
                error!("Error creating directory: {}", e);
                panic!("Failed to create cache directory. Please ensure the process has the necessary permissions for {}", &config.sync.cache_location);
            }
        }
    }

    info!("Starting Pi-hole sync...");
    loop {
        let main_pihole = PiHoleClient::new(
            &config.main.schema,
            &config.main.host,
            config.main.port,
            &config.main.api_key,
        );

        info!("Downloading backup from main instance...");
        if let Err(e) = main_pihole.download_backup(&backup_path).await {
            error!("Failed to download backup: {:?}", e);
        } else {
            for secondary in &config.secondary {
                let secondary_pihole = PiHoleClient::new(
                    &secondary.schema,
                    &secondary.host,
                    secondary.port,
                    &secondary.api_key,
                );

                info!("Uploading backup to {}", secondary.host);
                if let Err(e) = secondary_pihole.upload_backup(&backup_path).await {
                    error!("Failed to upload backup to {}: {:?}", secondary.host, e);
                    continue;
                } else if secondary.update_gravity.unwrap_or(false) {
                    info!("Updating gravity on {}", secondary.host);
                    if let Err(e) = secondary_pihole.trigger_gravity_update().await {
                        error!("Failed to update gravity on {}: {:?}", secondary.host, e);
                    }
                }
            }
        }

        if run_once {
            info!("Sync complete. Exiting because --once was specified.");
            return Ok(());
        }

        info!(
            "Sync complete. Sleeping for {} minutes...",
            sync_interval.as_secs() / 60
        );
        sleep(sync_interval).await;
    }
}

async fn aquire_app_password(config_path: &str) -> Result<()> {
    let config = Config::load(config_path)?;
    let mut instances_list: Vec<Instance> = Vec::new();

    instances_list.push(config.main);

    for secondary in &config.secondary {
        instances_list.push(secondary.clone());
    }

    let selection_list: Vec<String> = instances_list
        .iter()
        .map(|instance| instance.host.clone())
        .collect();

    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Please select the instance to fetch an API app password from")
        .items(&selection_list)
        .interact()
        .unwrap();

    let password = Password::with_theme(&ColorfulTheme::default())
        .with_prompt(format!(
            "Please enter your PiHole webinterface password for {}",
            instances_list[selection].host
        ))
        .interact()
        .unwrap();

    let pihole_client = PiHoleClient::from_instance(instances_list[selection].clone());

    let bar = ProgressBar::new_spinner();
    bar.enable_steady_tick(Duration::from_millis(100));
    let app_pw: AppPassword = pihole_client.fetch_app_password(password).await?;
    bar.finish();

    println!(
        "🎉 Successfully fetched API app password for {}",
        instances_list[selection].host
    );
    println!("Password (add to pihole-sync config): {}", app_pw.password);
    println!("Hash (add to PiHole): {}", app_pw.hash);
    println!("");
    println!("-----");
    println!("Hint:");
    println!(
        "Add the password to the pihole-sync configuration for the instance {}.",
        instances_list[selection].host
    );
    println!(
        "You need to add the hash to webserver.api.app_password configuration in the PiHole web interface."
    );
    println!("Refer to PiHole API documentation for more information: https://pihole.com/api/docs/#get-/auth/app");

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    setup_logging();
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
                aquire_app_password(config_path).await?;
            }

            Commands::Instances(instances_cmd) => match instances_cmd {
                Instances::List => {
                    println!("Main Instance:");
                    println!("  Host: {}", config.main.host);
                    println!("  Schema: {}", config.main.schema);
                    println!("  Port: {}", config.main.port);
                    println!("  API Key: [hidden]");
                    println!("\nSecondary Instances:");
                    for instance in &config.secondary {
                        println!("  Host: {}", instance.host);
                        println!("  Schema: {}", instance.schema);
                        println!("  Port: {}", instance.port);
                        println!("  API Key: [hidden]");
                        println!(
                            "  Update Gravity: {}",
                            instance.update_gravity.unwrap_or(false)
                        );
                        println!();
                    }
                }

                Instances::Add {
                    host,
                    schema,
                    port,
                    api_key,
                    update_gravity,
                } => {
                    config.secondary.push(Instance {
                        host,
                        schema,
                        port,
                        api_key,
                        update_gravity: Some(update_gravity),
                    });
                    config.save(config_path)?;
                    info!("Instance added successfully!");
                }

                Instances::Remove { host } => {
                    let original_len = config.secondary.len();
                    config.secondary.retain(|instance| instance.host != host);
                    if config.secondary.len() < original_len {
                        config.save(config_path)?;
                        info!("Instance '{}' removed successfully!", host);
                    } else {
                        info!("No instance found with hostname '{}'.", host);
                    }
                }
            },
        }
        return Ok(()); // Exit after CLI command execution
    } else {
        warn!("Please specify a command to run. Use --help for more information.");
    }

    Ok(())
}
