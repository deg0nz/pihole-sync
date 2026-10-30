use clap::{arg, Subcommand};
use tracing::info;

use crate::config::{Config, InstanceConfig};
use anyhow::Result;

#[derive(Subcommand)]
/// Manage Pi-hole instances
pub enum Instances {
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
        /// Accept invalid (e.g. self-signed) TLS certificates
        #[arg(long)]
        accept_invalid_certs: bool,
    },

    /// Remove a secondary instance by hostname (and port, if given)
    Remove {
        host: String,
        #[arg(short, long)]
        port: Option<u16>,
    },
}

pub fn run_instances_cmd(
    instances_cmd: Instances,
    config: &mut Config,
    config_path: &str,
) -> Result<()> {
    match instances_cmd {
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

        // TODO: Make this a dialogue with dialoguer
        Instances::Add {
            host,
            schema,
            port,
            api_key,
            update_gravity,
            accept_invalid_certs,
        } => {
            config.secondary.push(InstanceConfig {
                host,
                schema,
                port,
                api_key,
                accept_invalid_certs,
                api_sync_options: None,
                config_api_sync_options: None,
                config_sync: None,
                update_gravity: Some(update_gravity),
                sync_mode: None,
                import_options: None,
                teleporter_sync_options: Some(crate::config::TeleporterImportOptions::default()),
                teleporter_options: None,
            });
            config.save(config_path)?;
            info!("Instance added successfully!");
        }

        Instances::Remove { host, port } => {
            let original_len = config.secondary.len();
            config.secondary.retain(|instance| {
                instance.host != host || port.is_some_and(|p| p != instance.port)
            });
            if config.secondary.len() < original_len {
                config.save(config_path)?;
                info!("Instance '{}' removed successfully!", host);
            } else {
                info!("No instance found with hostname '{}'.", host);
            }
        }
    }

    Ok(())
}
