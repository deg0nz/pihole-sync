mod config_sync;
mod groups;
mod lists;
mod teleporter;
pub(crate) mod triggers;
pub(crate) mod util;

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::config::{Config, SyncMode, SyncTriggerMode};
use crate::pihole::client::{Group, PiHoleClient};
use crate::sync::config_sync::{
    determine_api_sync_needs, sync_config_api, ApiSyncNeeds, ApiSyncPayload,
};
use crate::sync::groups::normalize_groups;
use crate::sync::lists::normalize_lists;
use crate::sync::teleporter::{download_backup, upload_backup};
use crate::sync::util::{hash_config, hash_value, is_pihole_update_running, HashTracker};

pub use triggers::{run_interval_mode, watch_config_api, watch_config_file};

const TRIGGER_API_READINESS_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone, Copy)]
struct SyncModes {
    teleporter: bool,
    api: bool,
}

#[derive(Debug, Default)]
struct SyncState {
    last_main_config_hash: Option<u64>,
    last_teleporter_hash: Option<u64>,
    last_teleporter_applied: bool,
}

type SharedSyncState = Arc<Mutex<SyncState>>;

fn determine_sync_modes(secondary_piholes: &[PiHoleClient]) -> SyncModes {
    let teleporter = secondary_piholes.iter().any(|secondary| {
        matches!(
            secondary.config.sync_mode,
            Some(SyncMode::Teleporter) | None
        )
    });
    let api = secondary_piholes
        .iter()
        .any(|secondary| matches!(secondary.config.sync_mode, Some(SyncMode::Api)));

    SyncModes { teleporter, api }
}

fn teleporter_backup_hash(bytes: &[u8]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hasher;

    let mut hasher = DefaultHasher::new();
    hasher.write(bytes);
    hasher.finish()
}

fn build_group_lookup(groups: &[Group]) -> std::collections::HashMap<u32, String> {
    groups
        .iter()
        .filter_map(|g| g.id.map(|id| (id, g.name.clone())))
        .collect()
}

async fn wait_for_api_readiness_after_trigger(
    main_pihole: &PiHoleClient,
    secondary_piholes: &[PiHoleClient],
) -> Result<()> {
    let timeout = Duration::from_secs(TRIGGER_API_READINESS_TIMEOUT_SECS);

    debug!(
        "[{}] Waiting for Pi-hole API to become ready after trigger",
        main_pihole.config.host
    );
    main_pihole.wait_for_ready(timeout).await.with_context(|| {
        format!(
            "[{}] Main Pi-hole API not ready after waiting {}s",
            main_pihole.config.host, TRIGGER_API_READINESS_TIMEOUT_SECS
        )
    })?;

    for secondary in secondary_piholes {
        debug!(
            "[{}] Waiting for Pi-hole API to become ready after trigger",
            secondary.config.host
        );
        secondary.wait_for_ready(timeout).await.with_context(|| {
            format!(
                "[{}] Secondary Pi-hole API not ready after waiting {}s",
                secondary.config.host, TRIGGER_API_READINESS_TIMEOUT_SECS
            )
        })?;
    }

    Ok(())
}

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

    let main_pihole = PiHoleClient::new(config.main.clone())?;
    let secondary_piholes: Vec<PiHoleClient> = config
        .secondary
        .iter()
        .map(|secondary_config| PiHoleClient::new(secondary_config.clone()))
        .collect::<Result<Vec<_>>>()?;
    let sync_modes = determine_sync_modes(&secondary_piholes);

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

    let backup_path = Path::new(&config.sync.cache_location).join("pihole_backup.zip");
    let hash_tracker = HashTracker::new();
    let sync_state: SharedSyncState = Arc::new(Mutex::new(SyncState::default()));

    if sync_modes.teleporter {
        ensure_cache_directory(&config.sync.cache_location, &backup_path)?;
    }

    info!("Running in sync mode...");

    if run_once {
        run_once_mode(
            &main_pihole,
            &secondary_piholes,
            &backup_path,
            sync_modes,
            &sync_state,
            &hash_tracker,
        )
        .await?;
        return Ok(());
    }

    handle_initial_sync(
        &main_pihole,
        &secondary_piholes,
        &backup_path,
        sync_modes,
        &sync_state,
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
                sync_modes,
                sync_state.clone(),
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
                sync_modes,
                sync_state.clone(),
                hash_tracker.clone(),
            )
            .await?;
        }
        SyncTriggerMode::WatchConfigApi => {
            run_watch_config_api_trigger(
                api_poll_interval,
                main_pihole.clone(),
                secondary_piholes.clone(),
                backup_path.to_path_buf(),
                sync_modes,
                sync_state.clone(),
                hash_tracker.clone(),
            )
            .await?;
        }
    }

    Ok(())
}

fn ensure_cache_directory(cache_location: &str, backup_path: &Path) -> Result<()> {
    // Check cache directory (teleporter ZIP)
    info!("Checking cache directory: {}", backup_path.display());
    let path = Path::new(cache_location);

    if !path.exists() {
        info!("Cache directory does not exist. Trying to create it.");
        std::fs::create_dir_all(cache_location).with_context(|| {
            format!(
                "Failed to create cache directory at {}. Please ensure the process has write permissions",
                cache_location
            )
        })?;
        info!("Directory created successfully");
    }
    Ok(())
}

async fn run_once_mode(
    main_pihole: &PiHoleClient,
    secondary_piholes: &[PiHoleClient],
    backup_path: &Path,
    sync_modes: SyncModes,
    sync_state: &SharedSyncState,
    hash_tracker: &HashTracker,
) -> Result<()> {
    info!("Sync trigger mode: run-once (no watcher).");
    perform_sync(
        main_pihole,
        secondary_piholes,
        backup_path,
        sync_modes,
        sync_state,
        hash_tracker,
        None,
    )
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_initial_sync(
    main_pihole: &PiHoleClient,
    secondary_piholes: &[PiHoleClient],
    backup_path: &Path,
    sync_modes: SyncModes,
    sync_state: &SharedSyncState,
    hash_tracker: &HashTracker,
    disable_initial_sync: bool,
    trigger_mode: SyncTriggerMode,
) -> Result<()> {
    if !disable_initial_sync {
        perform_sync(
            main_pihole,
            secondary_piholes,
            backup_path,
            sync_modes,
            sync_state,
            hash_tracker,
            None,
        )
        .await?;
    } else if matches!(trigger_mode, SyncTriggerMode::WatchConfigApi) {
        // Seed baseline without running an initial sync so we don't sync immediately.
        match main_pihole.get_config().await {
            Ok(main_config) => {
                if let Ok(hash) = hash_config(&main_config) {
                    let mut state = sync_state.lock().await;
                    state.last_main_config_hash = Some(hash);
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

    Ok(())
}

async fn run_interval_trigger(
    sync_interval: Duration,
    main_pihole: PiHoleClient,
    secondary_piholes: Vec<PiHoleClient>,
    backup_path: PathBuf,
    sync_modes: SyncModes,
    sync_state: SharedSyncState,
    hash_tracker: HashTracker,
) -> Result<()> {
    let main_clone = main_pihole.clone();
    let secondaries_clone = secondary_piholes.clone();
    let backup_clone = backup_path.clone();
    let last_filtered_hashes_clone = hash_tracker.clone();
    let state_clone = sync_state.clone();
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
            let state = state_clone.clone();
            async move {
                perform_sync(
                    &main,
                    &secondaries,
                    &backup,
                    sync_modes,
                    &state,
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
    sync_modes: SyncModes,
    sync_state: SharedSyncState,
    hash_tracker: HashTracker,
) -> Result<()> {
    let main_clone = main_pihole.clone();
    let secondaries_clone = secondary_piholes.clone();
    let backup_path_clone = backup_path.to_path_buf();
    let last_filtered_hashes_clone = hash_tracker.clone();
    let state_clone = sync_state.clone();
    info!(
        "Sync trigger mode: watch_config_file. Watching {}.",
        config_watch_path.display()
    );
    watch_config_file(&config_watch_path, move || {
        let main = main_clone.clone();
        let secondaries = secondaries_clone.clone();
        let backup = backup_path_clone.clone();
        let hashes = last_filtered_hashes_clone.clone();
        let state = state_clone.clone();
        async move {
            if is_pihole_update_running().await? {
                warn!("Detected running \"pihole -up\"; skipping sync until update completes.");
                return Ok(());
            }
            if let Err(e) = wait_for_api_readiness_after_trigger(&main, &secondaries).await {
                warn!("Skipping triggered sync; Pi-hole API not ready yet: {}", e);
                return Ok(());
            }
            perform_sync(
                &main,
                &secondaries,
                &backup,
                sync_modes,
                &state,
                &hashes,
                None,
            )
            .await?;
            Ok(())
        }
    })
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_watch_config_api_trigger(
    api_poll_interval: Duration,
    main_pihole: PiHoleClient,
    secondary_piholes: Vec<PiHoleClient>,
    backup_path: std::path::PathBuf,
    sync_modes: SyncModes,
    sync_state: SharedSyncState,
    hash_tracker: HashTracker,
) -> Result<()> {
    let main_for_fetch = main_pihole.clone();
    let main_for_sync = main_pihole.clone();
    let secondaries_clone = secondary_piholes.clone();
    let backup_path_clone = backup_path.clone();
    let last_filtered_hashes_clone = hash_tracker.clone();
    let state_clone = sync_state.clone();
    let baseline_hash = { sync_state.lock().await.last_main_config_hash };
    info!(
        "Sync trigger mode: watch_config_api. Polling every {} minute(s).",
        api_poll_interval.as_secs() / 60
    );
    watch_config_api(
        api_poll_interval,
        baseline_hash,
        move || {
            let main = main_for_fetch.clone();
            async move { main.get_config().await }
        },
        move |main_config| {
            let main = main_for_sync.clone();
            let secondaries = secondaries_clone.clone();
            let backup = backup_path_clone.clone();
            let hashes = last_filtered_hashes_clone.clone();
            let state = state_clone.clone();
            async move {
                if let Err(e) = wait_for_api_readiness_after_trigger(&main, &secondaries).await {
                    warn!("Skipping triggered sync; Pi-hole API not ready yet: {}", e);
                    return Ok(());
                }
                perform_sync(
                    &main,
                    &secondaries,
                    &backup,
                    sync_modes,
                    &state,
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
    sync_modes: SyncModes,
    sync_state: &SharedSyncState,
    hash_tracker: &HashTracker,
    provided_main_config: Option<serde_json::Value>,
) -> Result<()> {
    let mut main_config = provided_main_config;

    let (previous_teleporter_hash, previous_teleporter_applied) = {
        let state = sync_state.lock().await;
        (state.last_teleporter_hash, state.last_teleporter_applied)
    };

    if sync_modes.teleporter {
        match download_backup(main_pihole, backup_path).await {
            Ok(_) => match tokio::fs::read(backup_path).await {
                Ok(backup_bytes) => {
                    let backup_hash = teleporter_backup_hash(&backup_bytes);
                    let should_upload = Some(backup_hash) != previous_teleporter_hash
                        || !previous_teleporter_applied;

                    let applied = if should_upload {
                        upload_teleporter_to_secondaries(secondary_piholes, backup_path).await
                    } else {
                        info!(
                            "Teleporter backup unchanged since last run; skipping upload to secondary instances."
                        );
                        true
                    };
                    let mut state = sync_state.lock().await;
                    state.last_teleporter_hash = Some(backup_hash);
                    state.last_teleporter_applied = applied;
                }
                Err(e) => error!(
                    "[{}] Failed to read downloaded backup for hashing: {}",
                    main_pihole.config.host, e
                ),
            },
            Err(e) => error!(
                "[{}] Failed to download backup: {:?}",
                main_pihole.config.host, e
            ),
        }
    }

    let api_needs = if sync_modes.api {
        determine_api_sync_needs(secondary_piholes)
    } else {
        ApiSyncNeeds::default()
    };

    if sync_modes.api && api_needs.any() {
        if api_needs.config && main_config.is_none() {
            match main_pihole.get_config().await {
                Ok(config_value) => main_config = Some(config_value),
                Err(e) => {
                    error!(
                        "[{}] Failed to fetch config from main instance: {:?}",
                        main_pihole.config.host, e
                    );
                }
            }
        }

        let mut payload = ApiSyncPayload::default();
        payload.main_config = main_config.clone();

        if api_needs.groups {
            match main_pihole.get_groups().await {
                Ok(groups) => {
                    payload.main_group_lookup = build_group_lookup(&groups);
                    payload.main_groups_hash = hash_value(&normalize_groups(&groups)).ok();
                    payload.main_groups = groups;
                }
                Err(e) => error!(
                    "[{}] Failed to fetch groups from main instance: {:?}",
                    main_pihole.config.host, e
                ),
            }
        }

        if api_needs.lists {
            match main_pihole.get_lists().await {
                Ok(lists) => {
                    payload.main_lists_hash =
                        hash_value(&normalize_lists(&lists, &payload.main_group_lookup)).ok();
                    payload.main_lists = lists;
                }
                Err(e) => error!(
                    "[{}] Failed to fetch lists from main instance: {:?}",
                    main_pihole.config.host, e
                ),
            }
        }

        sync_config_api(secondary_piholes, &payload, &api_needs, hash_tracker).await;
    }

    if let Some(hash) = main_config.as_ref().and_then(|cfg| hash_config(cfg).ok()) {
        let mut state = sync_state.lock().await;
        state.last_main_config_hash = Some(hash);
    }

    logout_all(main_pihole, secondary_piholes).await;
    Ok(())
}

async fn upload_teleporter_to_secondaries(
    secondary_piholes: &[PiHoleClient],
    backup_path: &Path,
) -> bool {
    let mut all_ok = true;
    for secondary_pihole in secondary_piholes.iter().filter(|secondary| {
        matches!(
            secondary.config.sync_mode,
            Some(SyncMode::Teleporter) | None
        )
    }) {
        if let Err(e) = upload_backup(secondary_pihole, backup_path).await {
            error!(
                "Failed to upload backup to {}: {:?}",
                secondary_pihole.config.host, e
            );
            all_ok = false;
        }
    }
    all_ok
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
