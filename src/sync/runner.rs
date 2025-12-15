use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::config::{Config, ConfigApiSyncMode, SyncMode, SyncTriggerMode};
use crate::pihole::client::PiHoleClient;
use crate::pihole::config_filter::{ConfigFilter, FilterMode};
use crate::sync::triggers::{run_interval_mode, watch_config_api, watch_config_file};
use crate::sync::util::{filtered_config_has_changed, hash_config, is_pihole_update_running};

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
    let last_filtered_config_hashes: Arc<Mutex<HashMap<String, u64>>> =
        Arc::new(Mutex::new(HashMap::new()));

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
        info!("Sync trigger mode: run-once (no watcher).");
        perform_sync(
            &main_pihole,
            &secondary_piholes,
            &backup_path,
            has_teleporter_secondaries,
            has_config_api_secondaries,
            &last_filtered_config_hashes,
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
            &last_filtered_config_hashes,
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
            let last_filtered_hashes_clone = last_filtered_config_hashes.clone();
            info!(
                "Sync trigger mode: interval. Running every {} minute(s).",
                sync_interval.as_secs() / 60
            );
            run_interval_mode(
                sync_interval,
                move || {
                    let main = main_clone.clone();
                    let secondaries = secondaries_clone.clone();
                    let backup = backup_clone.clone();
                    let hashes = last_filtered_hashes_clone.clone();
                    async move {
                        perform_sync(
                            &main,
                            &secondaries,
                            &backup,
                            has_teleporter_secondaries,
                            has_config_api_secondaries,
                            &hashes,
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
            let last_filtered_hashes_clone = last_filtered_config_hashes.clone();
            info!(
                "Sync trigger mode: watch_config_file. Watching {}.",
                config_watch_path.display()
            );
            watch_config_file(&config_watch_path, move || {
                let main = main_clone.clone();
                let secondaries = secondaries_clone.clone();
                let backup = backup_path_clone.clone();
                let hashes = last_filtered_hashes_clone.clone();
                async move {
                    if is_pihole_update_running().await? {
                        warn!("Detected running \"pihole -up\"; skipping sync until update completes.");
                        return Ok(());
                    }
                    perform_sync(
                        &main,
                        &secondaries,
                        &backup,
                        has_teleporter_secondaries,
                        has_config_api_secondaries,
                        &hashes,
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
            let main_for_fetch = main_pihole.clone();
            let main_for_sync = main_pihole.clone();
            let secondaries_clone = secondary_piholes.clone();
            let backup_path_clone = backup_path.clone();
            let last_filtered_hashes_clone = last_filtered_config_hashes.clone();
            info!(
                "Sync trigger mode: watch_config_api. Polling every {} minute(s).",
                api_poll_interval.as_secs() / 60
            );
            watch_config_api(
                api_poll_interval,
                last_main_config_hash,
                move || {
                    let main = main_for_fetch.clone();
                    async move { main.get_config().await }
                },
                move |main_config| {
                    let main = main_for_sync.clone();
                    let secondaries = secondaries_clone.clone();
                    let backup = backup_path_clone.clone();
                    let hashes = last_filtered_hashes_clone.clone();
                    async move {
                        perform_sync(
                            &main,
                            &secondaries,
                            &backup,
                            has_teleporter_secondaries,
                            has_config_api_secondaries,
                            &hashes,
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

async fn perform_sync(
    main_pihole: &PiHoleClient,
    secondary_piholes: &[PiHoleClient],
    backup_path: &Path,
    has_teleporter_secondaries: bool,
    has_config_api_secondaries: bool,
    last_filtered_config_hashes: &Arc<Mutex<HashMap<String, u64>>>,
    provided_main_config: Option<serde_json::Value>,
) -> Result<Option<serde_json::Value>> {
    let mut main_config_used = provided_main_config;

    if has_teleporter_secondaries {
        sync_teleporter(main_pihole, secondary_piholes, backup_path).await;
    }

    if has_config_api_secondaries {
        main_config_used = sync_config_api(
            main_pihole,
            secondary_piholes,
            main_config_used,
            last_filtered_config_hashes,
        )
        .await;
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
    last_filtered_config_hashes: &Arc<Mutex<HashMap<String, u64>>>,
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

            let Some(config_sync) = secondary_pihole.config.config_api_sync_options.clone() else {
                continue;
            };

            let filter_mode = match config_sync.mode.unwrap_or(ConfigApiSyncMode::Include) {
                ConfigApiSyncMode::Include => FilterMode::OptIn,
                ConfigApiSyncMode::Exclude => FilterMode::OptOut,
            };

            let filter = ConfigFilter::new(&config_sync.filter_keys, filter_mode);
            let filtered_config = filter.filter_json(main_config.clone());
            let host_key = secondary_pihole.config.host.clone();

            let filtered_hash = match hash_config(&filtered_config) {
                Ok(hash) => hash,
                Err(e) => {
                    error!(
                        "[{}] Failed to hash filtered config: {:?}",
                        secondary_pihole.config.host, e
                    );
                    continue;
                }
            };

            if !filtered_config_has_changed(&host_key, filtered_hash, last_filtered_config_hashes)
                .await
            {
                info!(
                    "[{}] Skipping config_api sync; filtered config unchanged since last run",
                    host_key
                );
                continue;
            }

            info!("[{}] Syncing config via API", host_key);
            if let Err(e) = secondary_pihole
                .patch_config_and_wait_for_ftl_readiness(filtered_config.clone())
                .await
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

            let mut hashes = last_filtered_config_hashes.lock().await;
            hashes.insert(host_key, filtered_hash);
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
