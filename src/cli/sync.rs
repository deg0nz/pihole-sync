use std::{fs, path::Path, time::Duration};

use tokio::time::sleep;
use tracing::{error, info};

use crate::{config::Config, pihole_client::PiHoleClient};
use anyhow::Result;

pub async fn run_sync(config_path: &str, run_once: bool) -> Result<()> {
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

    let main_pihole = PiHoleClient::new(config.main.clone());

    let secondary_piholes = config
        .secondary
        .iter()
        .map(|secondary_config| PiHoleClient::new(secondary_config.clone()))
        .collect::<Vec<_>>();

    if !run_once {
        main_pihole
            .init_session_keepalive(sync_interval.as_secs())
            .await?;

        for secondary_pihole in &secondary_piholes {
            secondary_pihole
                .init_session_keepalive(sync_interval.as_secs())
                .await?;
        }
    }

    info!("Running in sync mode...");
    loop {
        info!("Downloading backup from main instance...");
        if let Err(e) = main_pihole.download_backup(&backup_path).await {
            error!("Failed to download backup: {:?}", e);
        } else {
            for secondary_pihole in &secondary_piholes {
                info!("Uploading backup to {}", secondary_pihole.config.host);
                if let Err(e) = secondary_pihole.upload_backup(&backup_path).await {
                    error!(
                        "Failed to upload backup to {}: {:?}",
                        secondary_pihole.config.host, e
                    );
                    continue;
                } else if secondary_pihole.config.update_gravity.unwrap_or(false) {
                    info!("Updating gravity on {}", secondary_pihole.config.host);
                    if let Err(e) = secondary_pihole.trigger_gravity_update().await {
                        error!(
                            "Failed to update gravity on {}: {:?}",
                            secondary_pihole.config.host, e
                        );
                    }
                }
            }
        }

        if run_once {
            info!("Sync complete. Exiting because --once was specified.");
            main_pihole.logout().await?;
            for secondary in &secondary_piholes {
                secondary.logout().await?;
            }
            return Ok(());
        }

        info!(
            "Sync complete. Sleeping for {} minutes...",
            sync_interval.as_secs() / 60
        );

        sleep(sync_interval).await;
    }
}
