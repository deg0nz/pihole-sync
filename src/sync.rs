mod config_sync;
mod groups;
mod lists;
mod teleporter;
pub(crate) mod triggers;
pub(crate) mod util;

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use tracing::{debug, error, info, warn};

use crate::config::{Config, SyncMode, SyncTriggerMode};
use crate::pihole::client::PiHoleClient;
use crate::sync::config_sync::sync_config_api;
use crate::sync::teleporter::sync_teleporter;
use crate::sync::util::{hash_config, is_pihole_update_running, HashTracker};

pub use triggers::{run_interval_mode, watch_config_api, watch_config_file};

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

    info!(
        "Configured secondary instances ({}): {}",
        secondary_piholes.len(),
        secondary_piholes
            .iter()
            .map(|s| {
                let mode = s.config.sync_mode.unwrap_or(SyncMode::Teleporter);
                format!("{}:{} ({:?})", s.config.host, s.config.port, mode)
            })
            .collect::<Vec<_>>()
            .join(", ")
    );
    for secondary in &secondary_piholes {
        debug!(
            "[{}:{}] sync_mode={:?}, api_sync_options={:?}",
            secondary.config.host,
            secondary.config.port,
            secondary.config.sync_mode,
            secondary.config.api_sync_options
        );
    }

    let has_teleporter_secondaries = secondary_piholes.iter().any(|secondary| {
        matches!(
            secondary.config.sync_mode,
            Some(SyncMode::Teleporter) | None
        )
    });
    let has_api_secondaries = secondary_piholes
        .iter()
        .any(|secondary| matches!(secondary.config.sync_mode, Some(SyncMode::Api)));

    let backup_path = Path::new(&config.sync.cache_location).join("pihole_backup.zip");
    let hash_tracker = HashTracker::new();

    if has_teleporter_secondaries {
        ensure_cache_directory(&config.sync.cache_location, &backup_path);
    }

    info!("Running in sync mode...");

    if run_once {
        run_once_mode(
            &main_pihole,
            &secondary_piholes,
            &backup_path,
            has_teleporter_secondaries,
            has_api_secondaries,
            &hash_tracker,
        )
        .await?;
        return Ok(());
    }

    let last_main_config_hash = handle_initial_sync(
        &main_pihole,
        &secondary_piholes,
        &backup_path,
        has_teleporter_secondaries,
        has_api_secondaries,
        &hash_tracker,
        disable_initial_sync,
        trigger_mode,
    )
    .await?;

    match trigger_mode {
        SyncTriggerMode::Interval => {
            run_interval_trigger(
                sync_interval,
                main_pihole.clone(),
                secondary_piholes.clone(),
                backup_path.to_path_buf(),
                has_teleporter_secondaries,
                has_api_secondaries,
                hash_tracker.clone(),
            )
            .await?;
        }
        SyncTriggerMode::WatchConfigFile => {
            run_watch_config_file_trigger(
                config_watch_path.clone(),
                main_pihole.clone(),
                secondary_piholes.clone(),
                backup_path.to_path_buf(),
                has_teleporter_secondaries,
                has_api_secondaries,
                hash_tracker.clone(),
            )
            .await?;
        }
        SyncTriggerMode::WatchConfigApi => {
            run_watch_config_api_trigger(
                api_poll_interval,
                last_main_config_hash,
                main_pihole.clone(),
                secondary_piholes.clone(),
                backup_path.to_path_buf(),
                has_teleporter_secondaries,
                has_api_secondaries,
                hash_tracker.clone(),
            )
            .await?;
        }
    }

    Ok(())
}

fn ensure_cache_directory(cache_location: &str, backup_path: &Path) {
    // Check cache directory (teleporter ZIP)
    info!("Checking cache directory: {}", backup_path.display());
    let path = Path::new(cache_location);

    if !path.exists() {
        info!("Cache directory does not exist. Trying to create it.");

        match std::fs::create_dir_all(cache_location) {
            Ok(_) => info!("Directory created successfully"),
            Err(e) => {
                error!("Error creating directory: {}", e);
                panic!("Failed to create cache directory. Please ensure the process has the necessary permissions for {}", cache_location);
            }
        }
    }
}

async fn run_once_mode(
    main_pihole: &PiHoleClient,
    secondary_piholes: &[PiHoleClient],
    backup_path: &Path,
    has_teleporter_secondaries: bool,
    has_api_secondaries: bool,
    hash_tracker: &HashTracker,
) -> Result<()> {
    info!("Sync trigger mode: run-once (no watcher).");
    perform_sync(
        main_pihole,
        secondary_piholes,
        backup_path,
        has_teleporter_secondaries,
        has_api_secondaries,
        hash_tracker,
        None,
    )
    .await?;
    Ok(())
}

async fn handle_initial_sync(
    main_pihole: &PiHoleClient,
    secondary_piholes: &[PiHoleClient],
    backup_path: &Path,
    has_teleporter_secondaries: bool,
    has_api_secondaries: bool,
    hash_tracker: &HashTracker,
    disable_initial_sync: bool,
    trigger_mode: SyncTriggerMode,
) -> Result<Option<u64>> {
    let mut last_main_config_hash: Option<u64> = None;

    if !disable_initial_sync {
        let main_config_used = perform_sync(
            main_pihole,
            secondary_piholes,
            backup_path,
            has_teleporter_secondaries,
            has_api_secondaries,
            hash_tracker,
            None,
        )
        .await?;

        if let Some(config_value) = main_config_used {
            last_main_config_hash = hash_config(&config_value).ok();
        }
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
        logout_all(main_pihole, secondary_piholes).await;
    }

    Ok(last_main_config_hash)
}

async fn run_interval_trigger(
    sync_interval: Duration,
    main_pihole: PiHoleClient,
    secondary_piholes: Vec<PiHoleClient>,
    backup_path: std::path::PathBuf,
    has_teleporter_secondaries: bool,
    has_api_secondaries: bool,
    hash_tracker: HashTracker,
) -> Result<()> {
    let main_clone = main_pihole.clone();
    let secondaries_clone = secondary_piholes.clone();
    let backup_clone = backup_path.clone();
    let last_filtered_hashes_clone = hash_tracker.clone();
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
                    has_api_secondaries,
                    &hashes,
                    None,
                )
                .await?;
                Ok(())
            }
        },
        None,
    )
    .await
}

async fn run_watch_config_file_trigger(
    config_watch_path: std::path::PathBuf,
    main_pihole: PiHoleClient,
    secondary_piholes: Vec<PiHoleClient>,
    backup_path: std::path::PathBuf,
    has_teleporter_secondaries: bool,
    has_api_secondaries: bool,
    hash_tracker: HashTracker,
) -> Result<()> {
    let main_clone = main_pihole.clone();
    let secondaries_clone = secondary_piholes.clone();
    let backup_path_clone = backup_path.to_path_buf();
    let last_filtered_hashes_clone = hash_tracker.clone();
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
                has_api_secondaries,
                &hashes,
                None,
            )
            .await?;
            Ok(())
        }
    })
    .await
}

async fn run_watch_config_api_trigger(
    api_poll_interval: Duration,
    last_main_config_hash: Option<u64>,
    main_pihole: PiHoleClient,
    secondary_piholes: Vec<PiHoleClient>,
    backup_path: std::path::PathBuf,
    has_teleporter_secondaries: bool,
    has_api_secondaries: bool,
    hash_tracker: HashTracker,
) -> Result<()> {
    let main_for_fetch = main_pihole.clone();
    let main_for_sync = main_pihole.clone();
    let secondaries_clone = secondary_piholes.clone();
    let backup_path_clone = backup_path.clone();
    let last_filtered_hashes_clone = hash_tracker.clone();
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
                    has_api_secondaries,
                    &hashes,
                    Some(main_config),
                )
                .await?;
                Ok(())
            }
        },
    )
    .await
}

async fn perform_sync(
    main_pihole: &PiHoleClient,
    secondary_piholes: &[PiHoleClient],
    backup_path: &Path,
    has_teleporter_secondaries: bool,
    has_api_secondaries: bool,
    hash_tracker: &HashTracker,
    provided_main_config: Option<serde_json::Value>,
) -> Result<Option<serde_json::Value>> {
    let mut main_config_used = provided_main_config;

    if has_teleporter_secondaries {
        sync_teleporter(main_pihole, secondary_piholes, backup_path).await;
    }

    if has_api_secondaries {
        main_config_used = sync_config_api(
            main_pihole,
            secondary_piholes,
            main_config_used,
            hash_tracker,
        )
        .await;
    }

    logout_all(main_pihole, secondary_piholes).await;
    Ok(main_config_used)
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

#[cfg(test)]
mod tests;
