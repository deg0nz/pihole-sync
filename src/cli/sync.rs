use std::{fs, path::Path, time::Duration};

use tokio::time::sleep;
use tracing::{error, info};

use crate::pihole::client::PiHoleClient;
use crate::pihole::config_filter::FilterMode;
use crate::{config::Config, pihole::config_filter::ConfigFilter};
use anyhow::{Error, Result};

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
            error!(
                "[{}] Failed to download backup: {:?}",
                main_pihole.config.host, e
            );
        } else {
            for secondary_pihole in &secondary_piholes {
                info!("[{}] Uploading backup", secondary_pihole.config.host);
                if let Err(e) = secondary_pihole.upload_backup(&backup_path).await {
                    error!(
                        "Failed to upload backup to {}: {:?}",
                        secondary_pihole.config.host, e
                    );
                    continue;
                } else if secondary_pihole.config.update_gravity.unwrap_or(false) {
                    info!("[{}] Updating gravity", secondary_pihole.config.host);
                    if let Err(e) = secondary_pihole.trigger_gravity_update().await {
                        error!(
                            "Failed to update gravity on {}: {:?}",
                            secondary_pihole.config.host, e
                        );
                    }
                }

                if secondary_pihole.has_config_filters() {
                    info!("[{}] Syncing config", secondary_pihole.config.host);
                    if let Err(e) =
                        sync_pihole_config_filtered(&main_pihole, &secondary_pihole).await
                    {
                        error!("{}", e);
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

async fn sync_pihole_config_filtered(
    main: &PiHoleClient,
    secondary: &PiHoleClient,
) -> Result<(), Error> {
    let config = main.get_config().await?;

    if let Some(config_sync) = secondary.config.config_sync.clone() {
        let mut filter_mode = FilterMode::OptIn;

        if config_sync.exclude {
            filter_mode = FilterMode::OptOut;
        }

        let filter = ConfigFilter::new(&config_sync.filter_keys, filter_mode);
        let filtered_config = filter.filter_json(config.clone());

        dbg!(&filtered_config);

        let res = secondary.patch_config(filtered_config).await?;
        dbg!(res)
    }

    Ok(())
}
