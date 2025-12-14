use std::path::Path;
use std::time::Duration;

use anyhow::{Error, Result};
use tracing::{error, info, warn};

use crate::config::{Config, ConfigApiSyncMode, SyncMode, SyncTriggerMode};
use crate::pihole::client::PiHoleClient;
use crate::pihole::config_filter::{ConfigFilter, FilterMode};
use crate::sync::triggers::{run_interval_mode, watch_config_api_main, watch_config_file};
use crate::sync::util::hash_config;

pub async fn run_sync(config_path: &str, run_once: bool, disable_initial_sync: bool) -> Result<()> {
    // Load config
    let config = Config::load(config_path)?;
    let trigger_mode = config.sync.trigger_mode;
    let sync_interval = Duration::from_secs(config.sync.interval * 60);
    let api_poll_interval = Duration::from_secs(
        config
            .sync
            .api_poll_interval
            .unwrap_or(config.sync.interval)
            * 60,
    );
    let config_watch_path = std::path::PathBuf::from(config.sync.config_path.clone());

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

            match std::fs::create_dir_all(&config.sync.cache_location) {
                Ok(_) => info!("Directory created successfully"),
                Err(e) => {
                    error!("Error creating directory: {}", e);
                    panic!("Failed to create cache directory. Please ensure the process has the necessary permissions for {}", &config.sync.cache_location);
                }
            }
        }
    }

    info!("Running in sync mode...");

    if run_once {
        perform_sync(
            &main_pihole,
            &secondary_piholes,
            &backup_path,
            has_teleporter_secondaries,
            has_config_api_secondaries,
            None,
        )
        .await?;
        logout_all(&main_pihole, &secondary_piholes).await;
        return Ok(());
    }

    let mut last_main_config_hash: Option<u64> = None;

    if !disable_initial_sync {
        let main_config_used = perform_sync(
            &main_pihole,
            &secondary_piholes,
            &backup_path,
            has_teleporter_secondaries,
            has_config_api_secondaries,
            None,
        )
        .await?;

        if let Some(config_value) = main_config_used {
            last_main_config_hash = hash_config(&config_value).ok();
        }
        logout_all(&main_pihole, &secondary_piholes).await;
    } else if matches!(trigger_mode, SyncTriggerMode::WatchConfigApi) {
        // Seed baseline without running an initial sync so we don't sync immediately.
        match main_pihole.get_config().await {
            Ok(main_config) => {
                if let Ok(hash) = hash_config(&main_config) {
                    last_main_config_hash = Some(hash);
                    info!(
                        "Seeded baseline config hash from main instance without initial sync: {}",
                        hash
                    );
                }
            }
            Err(e) => warn!(
                "[{}] Failed to fetch config for baseline: {:?}",
                main_pihole.config.host, e
            ),
        }
    }

    match trigger_mode {
        SyncTriggerMode::Interval => {
            let main_clone = main_pihole.clone();
            let secondaries_clone = secondary_piholes.clone();
            let backup_clone = backup_path.clone();
            run_interval_mode(
                sync_interval,
                move || {
                    let main = main_clone.clone();
                    let secondaries = secondaries_clone.clone();
                    let backup = backup_clone.clone();
                    async move {
                        perform_sync(
                            &main,
                            &secondaries,
                            &backup,
                            has_teleporter_secondaries,
                            has_config_api_secondaries,
                            None,
                        )
                        .await?;
                        logout_all(&main, &secondaries).await;
                        Ok(())
                    }
                },
                None,
            )
            .await?;
        }
        SyncTriggerMode::WatchConfigFile => {
            let main_clone = main_pihole.clone();
            let secondaries_clone = secondary_piholes.clone();
            let backup_path_clone = backup_path.clone();
            watch_config_file(&config_watch_path, move || {
                let main = main_clone.clone();
                let secondaries = secondaries_clone.clone();
                let backup = backup_path_clone.clone();
                async move {
                    perform_sync(
                        &main,
                        &secondaries,
                        &backup,
                        has_teleporter_secondaries,
                        has_config_api_secondaries,
                        None,
                    )
                    .await?;
                    logout_all(&main, &secondaries).await;
                    Ok(())
                }
            })
            .await?;
        }
        SyncTriggerMode::WatchConfigApi => {
            let main_for_sync = main_pihole.clone();
            let secondaries_clone = secondary_piholes.clone();
            let backup_path_clone = backup_path.clone();
            watch_config_api_main(
                main_pihole.clone(),
                api_poll_interval,
                last_main_config_hash,
                move |main_config| {
                    let main = main_for_sync.clone();
                    let secondaries = secondaries_clone.clone();
                    let backup = backup_path_clone.clone();
                    async move {
                        perform_sync(
                            &main,
                            &secondaries,
                            &backup,
                            has_teleporter_secondaries,
                            has_config_api_secondaries,
                            Some(main_config),
                        )
                        .await?;
                        logout_all(&main, &secondaries).await;
                        Ok(())
                    }
                },
            )
            .await?;
        }
    }

    Ok(())
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

async fn perform_sync(
    main_pihole: &PiHoleClient,
    secondary_piholes: &[PiHoleClient],
    backup_path: &Path,
    has_teleporter_secondaries: bool,
    has_config_api_secondaries: bool,
    provided_main_config: Option<serde_json::Value>,
) -> Result<Option<serde_json::Value>> {
    let mut main_config_used = provided_main_config;

    if has_teleporter_secondaries {
        sync_teleporter(main_pihole, secondary_piholes, backup_path).await;
    }

    if has_config_api_secondaries {
        main_config_used = sync_config_api(main_pihole, secondary_piholes, main_config_used).await;
    }

    Ok(main_config_used)
}

async fn sync_teleporter(
    main_pihole: &PiHoleClient,
    secondary_piholes: &[PiHoleClient],
    backup_path: &Path,
) {
    info!("Downloading backup from main instance...");
    if let Err(e) = main_pihole.download_backup(backup_path).await {
        error!(
            "[{}] Failed to download backup: {:?}",
            main_pihole.config.host, e
        );
        return;
    }

    for secondary_pihole in secondary_piholes {
        if !matches!(
            secondary_pihole.config.sync_mode,
            Some(SyncMode::Teleporter) | None
        ) {
            continue;
        }

        info!("[{}] Uploading backup", secondary_pihole.config.host);
        if let Err(e) = secondary_pihole.upload_backup(backup_path).await {
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

async fn sync_config_api(
    main_pihole: &PiHoleClient,
    secondary_piholes: &[PiHoleClient],
    mut main_config_used: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    if main_config_used.is_none() {
        match main_pihole.get_config().await {
            Ok(config_value) => main_config_used = Some(config_value),
            Err(e) => {
                error!(
                    "[{}] Failed to fetch config from main instance: {:?}",
                    main_pihole.config.host, e
                );
            }
        }
    }

    if let Some(main_config) = &main_config_used {
        for secondary_pihole in secondary_piholes {
            if !matches!(secondary_pihole.config.sync_mode, Some(SyncMode::ConfigApi)) {
                continue;
            }

            info!("[{}] Syncing config via API", secondary_pihole.config.host);
            if let Err(e) = sync_pihole_config_filtered(main_config, secondary_pihole).await {
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

    main_config_used
}

async fn logout_all(main: &PiHoleClient, secondaries: &[PiHoleClient]) {
    if let Err(e) = main.logout().await {
        error!(
            "[{}] Failed to logout from main instance: {:?}",
            main.config.host, e
        );
    }
    for secondary in secondaries {
        if let Err(e) = secondary.logout().await {
            error!(
                "[{}] Failed to logout from secondary instance: {:?}",
                secondary.config.host, e
            );
        }
    }
}
