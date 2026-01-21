use std::collections::HashMap;
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
use crate::sync::triggers::{run_interval_mode, watch_config_api, watch_config_file};
use crate::sync::util::{hash_config, hash_value, is_pihole_update_running, HashTracker};

#[derive(Debug, Clone, Copy)]
pub(super) struct SyncModes {
    pub teleporter: bool,
    pub api: bool,
}

#[derive(Debug, Default)]
struct SyncState {
    last_main_config_hash: Option<u64>,
    last_teleporter_hash: Option<u64>,
    last_teleporter_applied: bool,
}

type SharedSyncState = Arc<Mutex<SyncState>>;

/// Central context for sync operations. Owns all state and clients,
/// eliminating the need to pass multiple parameters through function calls.
#[derive(Clone)]
pub struct SyncContext {
    // Clients
    main_pihole: PiHoleClient,
    secondary_piholes: Vec<PiHoleClient>,

    // Paths
    backup_path: PathBuf,
    config_watch_path: PathBuf,

    // Timing
    sync_interval: Duration,
    api_poll_interval: Duration,
    trigger_api_readiness_timeout: Duration,

    // Mode
    trigger_mode: SyncTriggerMode,
    sync_modes: SyncModes,

    // State (shared, mutable)
    sync_state: SharedSyncState,
    hash_tracker: HashTracker,

    // Cache location (for directory creation)
    cache_location: String,
}

impl SyncContext {
    /// Create a new SyncContext from the application configuration.
    pub fn from_config(config: &Config) -> Result<Self> {
        let main_pihole = PiHoleClient::new(config.main.clone())?;
        let secondary_piholes: Vec<PiHoleClient> = config
            .secondary
            .iter()
            .map(|secondary_config| PiHoleClient::new(secondary_config.clone()))
            .collect::<Result<Vec<_>>>()?;

        let sync_modes = determine_sync_modes(&secondary_piholes);

        let sync_interval = Duration::from_secs(config.sync.interval * 60);
        let api_poll_interval = Duration::from_secs(
            config
                .sync
                .api_poll_interval
                .unwrap_or(config.sync.interval)
                * 60,
        );
        let trigger_api_readiness_timeout =
            Duration::from_secs(config.sync.trigger_api_readiness_timeout_secs);

        let backup_path = Path::new(&config.sync.cache_location).join("pihole_backup.zip");
        let config_watch_path = PathBuf::from(&config.sync.config_path);

        Ok(Self {
            main_pihole,
            secondary_piholes,
            backup_path,
            config_watch_path,
            sync_interval,
            api_poll_interval,
            trigger_api_readiness_timeout,
            trigger_mode: config.sync.trigger_mode,
            sync_modes,
            sync_state: Arc::new(Mutex::new(SyncState::default())),
            hash_tracker: HashTracker::new(),
            cache_location: config.sync.cache_location.clone(),
        })
    }

    /// Main entry point. Runs the sync loop based on trigger mode.
    pub async fn run(&self, run_once: bool, disable_initial_sync: bool) -> Result<()> {
        self.log_configuration();

        if self.sync_modes.teleporter {
            self.ensure_cache_directory()?;
        }

        info!("Running in sync mode...");

        if run_once {
            info!("Sync trigger mode: run-once (no watcher).");
            return self.perform_sync(None).await;
        }

        // Handle initial sync or baseline seeding
        self.handle_initial_sync(disable_initial_sync).await?;

        // Run the appropriate trigger loop
        match self.trigger_mode {
            SyncTriggerMode::Interval => self.run_interval_loop().await,
            SyncTriggerMode::WatchConfigFile => self.run_file_watch_loop().await,
            SyncTriggerMode::WatchConfigApi => self.run_api_watch_loop().await,
        }
    }

    fn log_configuration(&self) {
        info!(
            "Configured secondary instances ({}): {}",
            self.secondary_piholes.len(),
            self.secondary_piholes
                .iter()
                .map(|s| {
                    let mode = s.config.sync_mode.unwrap_or(SyncMode::Teleporter);
                    format!("{}:{} ({:?})", s.config.host, s.config.port, mode)
                })
                .collect::<Vec<_>>()
                .join(", ")
        );

        for secondary in &self.secondary_piholes {
            debug!(
                "[{}:{}] sync_mode={:?}, api_sync_options={:?}",
                secondary.config.host,
                secondary.config.port,
                secondary.config.sync_mode,
                secondary.config.api_sync_options
            );
        }
    }

    fn ensure_cache_directory(&self) -> Result<()> {
        info!("Checking cache directory: {}", self.backup_path.display());
        let path = Path::new(&self.cache_location);

        if !path.exists() {
            info!("Cache directory does not exist. Trying to create it.");
            std::fs::create_dir_all(&self.cache_location).with_context(|| {
                format!(
                    "Failed to create cache directory at {}. Please ensure the process has write permissions",
                    self.cache_location
                )
            })?;
            info!("Directory created successfully");
        }
        Ok(())
    }

    async fn handle_initial_sync(&self, disable_initial_sync: bool) -> Result<()> {
        if !disable_initial_sync {
            self.perform_sync(None).await?;
        } else if matches!(self.trigger_mode, SyncTriggerMode::WatchConfigApi) {
            // Seed baseline without running an initial sync so we don't sync immediately.
            self.seed_baseline_hash().await;
        }
        Ok(())
    }

    async fn seed_baseline_hash(&self) {
        match self.main_pihole.get_config().await {
            Ok(main_config) => {
                if let Ok(hash) = hash_config(&main_config) {
                    let mut state = self.sync_state.lock().await;
                    state.last_main_config_hash = Some(hash);
                    info!(
                        "Seeded baseline config hash from main instance without initial sync: {}",
                        hash
                    );
                }
            }
            Err(e) => warn!(
                "[{}] Failed to fetch config for baseline: {:?}",
                self.main_pihole.config.host, e
            ),
        }
        self.logout_all().await;
    }

    async fn run_interval_loop(&self) -> Result<()> {
        info!(
            "Sync trigger mode: interval. Running every {} minute(s).",
            self.sync_interval.as_secs() / 60
        );

        let ctx = self.clone();
        run_interval_mode(
            self.sync_interval,
            move || {
                let ctx = ctx.clone();
                async move { ctx.perform_sync(None).await }
            },
            None,
        )
        .await
    }

    async fn run_file_watch_loop(&self) -> Result<()> {
        info!(
            "Sync trigger mode: watch_config_file. Watching {}.",
            self.config_watch_path.display()
        );

        let ctx = self.clone();
        watch_config_file(&self.config_watch_path, move || {
            let ctx = ctx.clone();
            async move {
                if is_pihole_update_running().await? {
                    warn!("Detected running \"pihole -up\"; skipping sync until update completes.");
                    return Ok(());
                }
                if let Err(e) = ctx.wait_for_api_readiness().await {
                    warn!("Skipping triggered sync; Pi-hole API not ready yet: {}", e);
                    return Ok(());
                }
                ctx.perform_sync(None).await
            }
        })
        .await
    }

    async fn run_api_watch_loop(&self) -> Result<()> {
        info!(
            "Sync trigger mode: watch_config_api. Polling every {} minute(s).",
            self.api_poll_interval.as_secs() / 60
        );

        let baseline = self.sync_state.lock().await.last_main_config_hash;
        let main_for_fetch = self.main_pihole.clone();
        let ctx = self.clone();

        watch_config_api(
            self.api_poll_interval,
            baseline,
            move || {
                let main = main_for_fetch.clone();
                async move { main.get_config().await }
            },
            move |main_config| {
                let ctx = ctx.clone();
                async move {
                    if let Err(e) = ctx.wait_for_api_readiness().await {
                        warn!("Skipping triggered sync; Pi-hole API not ready yet: {}", e);
                        return Ok(());
                    }
                    ctx.perform_sync(Some(main_config)).await
                }
            },
        )
        .await
    }

    async fn wait_for_api_readiness(&self) -> Result<()> {
        let timeout = self.trigger_api_readiness_timeout;

        debug!(
            "[{}] Waiting up to {:?} for Pi-hole API to become ready after trigger",
            self.main_pihole.config.host, timeout
        );
        self.main_pihole
            .wait_for_ready(timeout)
            .await
            .with_context(|| {
                format!(
                    "[{}] Main Pi-hole API not ready after waiting {:?}",
                    self.main_pihole.config.host, timeout
                )
            })?;

        for secondary in &self.secondary_piholes {
            debug!(
                "[{}] Waiting up to {:?} for Pi-hole API to become ready after trigger",
                secondary.config.host, timeout
            );
            secondary.wait_for_ready(timeout).await.with_context(|| {
                format!(
                    "[{}] Secondary Pi-hole API not ready after waiting {:?}",
                    secondary.config.host, timeout
                )
            })?;
        }

        Ok(())
    }

    async fn perform_sync(&self, provided_main_config: Option<serde_json::Value>) -> Result<()> {
        let mut main_config = provided_main_config;

        let (previous_teleporter_hash, previous_teleporter_applied) = {
            let state = self.sync_state.lock().await;
            (state.last_teleporter_hash, state.last_teleporter_applied)
        };

        if self.sync_modes.teleporter {
            match download_backup(&self.main_pihole, &self.backup_path).await {
                Ok(_) => match tokio::fs::read(&self.backup_path).await {
                    Ok(backup_bytes) => {
                        let backup_hash = teleporter_backup_hash(&backup_bytes);
                        let should_upload = Some(backup_hash) != previous_teleporter_hash
                            || !previous_teleporter_applied;

                        let applied = if should_upload {
                            self.upload_teleporter_to_secondaries().await
                        } else {
                            info!(
                                "Teleporter backup unchanged since last run; skipping upload to secondary instances."
                            );
                            true
                        };

                        let mut state = self.sync_state.lock().await;
                        state.last_teleporter_hash = Some(backup_hash);
                        state.last_teleporter_applied = applied;
                    }
                    Err(e) => error!(
                        "[{}] Failed to read downloaded backup for hashing: {}",
                        self.main_pihole.config.host, e
                    ),
                },
                Err(e) => error!(
                    "[{}] Failed to download backup: {:?}",
                    self.main_pihole.config.host, e
                ),
            }
        }

        let api_needs = if self.sync_modes.api {
            determine_api_sync_needs(&self.secondary_piholes)
        } else {
            ApiSyncNeeds::default()
        };

        if self.sync_modes.api && api_needs.any() {
            if api_needs.config && main_config.is_none() {
                match self.main_pihole.get_config().await {
                    Ok(config_value) => main_config = Some(config_value),
                    Err(e) => {
                        error!(
                            "[{}] Failed to fetch config from main instance: {:?}",
                            self.main_pihole.config.host, e
                        );
                    }
                }
            }

            let mut payload = ApiSyncPayload {
                main_config: main_config.clone(),
                ..Default::default()
            };

            if api_needs.groups {
                match self.main_pihole.get_groups().await {
                    Ok(groups) => {
                        payload.main_group_lookup = build_group_lookup(&groups);
                        payload.main_groups_hash = hash_value(&normalize_groups(&groups)).ok();
                        payload.main_groups = groups;
                    }
                    Err(e) => error!(
                        "[{}] Failed to fetch groups from main instance: {:?}",
                        self.main_pihole.config.host, e
                    ),
                }
            }

            if api_needs.lists {
                match self.main_pihole.get_lists().await {
                    Ok(lists) => {
                        payload.main_lists_hash =
                            hash_value(&normalize_lists(&lists, &payload.main_group_lookup)).ok();
                        payload.main_lists = lists;
                    }
                    Err(e) => error!(
                        "[{}] Failed to fetch lists from main instance: {:?}",
                        self.main_pihole.config.host, e
                    ),
                }
            }

            sync_config_api(
                &self.secondary_piholes,
                &payload,
                &api_needs,
                &self.hash_tracker,
            )
            .await;
        }

        if let Some(hash) = main_config.as_ref().and_then(|cfg| hash_config(cfg).ok()) {
            let mut state = self.sync_state.lock().await;
            state.last_main_config_hash = Some(hash);
        }

        self.logout_all().await;
        Ok(())
    }

    async fn upload_teleporter_to_secondaries(&self) -> bool {
        let mut all_ok = true;
        for secondary_pihole in self.secondary_piholes.iter().filter(|secondary| {
            matches!(
                secondary.config.sync_mode,
                Some(SyncMode::Teleporter) | None
            )
        }) {
            if let Err(e) = upload_backup(secondary_pihole, &self.backup_path).await {
                error!(
                    "Failed to upload backup to {}: {:?}",
                    secondary_pihole.config.host, e
                );
                all_ok = false;
            }
        }
        all_ok
    }

    async fn logout_all(&self) {
        if let Err(e) = self.main_pihole.logout().await {
            error!(
                "[{}] Failed to logout from main instance: {:?}",
                self.main_pihole.config.host, e
            );
        }
        for secondary in &self.secondary_piholes {
            if let Err(e) = secondary.logout().await {
                error!(
                    "[{}] Failed to logout from secondary instance: {:?}",
                    secondary.config.host, e
                );
            }
        }
    }
}

pub(super) fn determine_sync_modes(secondary_piholes: &[PiHoleClient]) -> SyncModes {
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

fn build_group_lookup(groups: &[Group]) -> HashMap<u32, String> {
    groups
        .iter()
        .filter_map(|g| g.id.map(|id| (id, g.name.clone())))
        .collect()
}
