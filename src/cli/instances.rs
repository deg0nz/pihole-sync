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
    },

    /// Remove a secondary instance by hostname
    Remove { host: String },
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
        } => {
            config.secondary.push(InstanceConfig {
                host,
                schema,
                port,
                api_key,
                update_gravity: Some(update_gravity),
                import_options: Some(crate::config::SyncImportOptions::default()),
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
    }

    Ok(())
}
