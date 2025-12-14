use std::{fs, path::Path, time::Duration};

use tokio::time::sleep;
use tracing::{error, info};

use crate::config::{ConfigApiSyncMode, SyncMode};
use crate::pihole::client::PiHoleClient;
use crate::pihole::config_filter::FilterMode;
use crate::{config::Config, pihole::config_filter::ConfigFilter};
use anyhow::{Error, Result};

pub async fn run_sync(config_path: &str, run_once: bool) -> Result<()> {
    // Load config
    let config = Config::load(config_path)?;
    let sync_interval = Duration::from_secs(config.sync.interval * 60);

    let main_pihole = PiHoleClient::new(config.main.clone());

    let secondary_piholes = config
        .secondary
        .iter()
        .map(|secondary_config| PiHoleClient::new(secondary_config.clone()))
        .collect::<Vec<_>>();

    let has_teleporter_secondaries = secondary_piholes.iter().any(|secondary| {
        matches!(
            secondary.config.sync_mode,
            Some(SyncMode::Teleporter) | None
        )
    });
    let has_config_api_secondaries = secondary_piholes
        .iter()
        .any(|secondary| matches!(secondary.config.sync_mode, Some(SyncMode::ConfigApi)));

    let backup_path = Path::new(&config.sync.cache_location).join("pihole_backup.zip");

    if has_teleporter_secondaries {
        // Check cache directory (teleporter ZIP)
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
    }

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
        // Teleporter sync (only for secondaries that opted into teleporter mode)
        if has_teleporter_secondaries {
            info!("Downloading backup from main instance...");
            if let Err(e) = main_pihole.download_backup(&backup_path).await {
                error!(
                    "[{}] Failed to download backup: {:?}",
                    main_pihole.config.host, e
                );
            } else {
                for secondary_pihole in &secondary_piholes {
                    if !matches!(
                        secondary_pihole.config.sync_mode,
                        Some(SyncMode::Teleporter) | None
                    ) {
                        continue;
                    }

                    info!("[{}] Uploading backup", secondary_pihole.config.host);
                    if let Err(e) = secondary_pihole.upload_backup(&backup_path).await {
                        error!(
                            "Failed to upload backup to {}: {:?}",
                            secondary_pihole.config.host, e
                        );
                        continue;
                    }

                    if secondary_pihole.config.update_gravity.unwrap_or(false) {
                        info!("[{}] Updating gravity", secondary_pihole.config.host);
                        if let Err(e) = secondary_pihole.trigger_gravity_update().await {
                            error!(
                                "Failed to update gravity on {}: {:?}",
                                secondary_pihole.config.host, e
                            );
                        }
                    }
                }
            }
        }

        // Config API sync (only for secondaries that opted into config_api mode)
        if has_config_api_secondaries {
            match main_pihole.get_config().await {
                Ok(main_config) => {
                    for secondary_pihole in &secondary_piholes {
                        if !matches!(secondary_pihole.config.sync_mode, Some(SyncMode::ConfigApi)) {
                            continue;
                        }

                        info!("[{}] Syncing config via API", secondary_pihole.config.host);
                        if let Err(e) =
                            sync_pihole_config_filtered(&main_config, secondary_pihole).await
                        {
                            error!("{}", e);
                            continue;
                        }

                        if secondary_pihole.config.update_gravity.unwrap_or(false) {
                            info!("[{}] Updating gravity", secondary_pihole.config.host);
                            if let Err(e) = secondary_pihole.trigger_gravity_update().await {
                                error!(
                                    "Failed to update gravity on {}: {:?}",
                                    secondary_pihole.config.host, e
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "[{}] Failed to fetch config from main instance: {:?}",
                        main_pihole.config.host, e
                    );
                }
            }
        }

        if run_once {
            info!("Sync complete. Exiting because --once was specified.");
            if let Err(e) = main_pihole.logout().await {
                error!(
                    "[{}] Failed to logout from main instance: {:?}",
                    main_pihole.config.host, e
                );
            }
            for secondary in &secondary_piholes {
                if let Err(e) = secondary.logout().await {
                    error!(
                        "[{}] Failed to logout from secondary instance: {:?}",
                        secondary.config.host, e
                    );
                }
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
    main_config: &serde_json::Value,
    secondary: &PiHoleClient,
) -> Result<(), Error> {
    if let Some(config_sync) = secondary.config.config_api_sync_options.clone() {
        let filter_mode = match config_sync.mode.unwrap_or(ConfigApiSyncMode::Include) {
            ConfigApiSyncMode::Include => FilterMode::OptIn,
            ConfigApiSyncMode::Exclude => FilterMode::OptOut,
        };

        let filter = ConfigFilter::new(&config_sync.filter_keys, filter_mode);
        let filtered_config = filter.filter_json(main_config.clone());

        secondary
            .patch_config_and_wait_for_ftl_readiness(filtered_config)
            .await?;
    }

    Ok(())
}
