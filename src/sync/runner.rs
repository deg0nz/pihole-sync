use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use tracing::{error, info, warn};

use crate::config::{Config, ConfigApiSyncMode, SyncMode, SyncTriggerMode};
use crate::pihole::client::{Group, List, PiHoleClient};
use crate::pihole::config_filter::{ConfigFilter, FilterMode};
use crate::sync::triggers::{run_interval_mode, watch_config_api, watch_config_file};
use crate::sync::util::{hash_config, hash_value, is_pihole_update_running, HashTracker};
use tokio::time::sleep;

// Pi-hole doesn't expose rate-limit settings; throttle writes to stay well below typical defaults.
const API_WRITE_THROTTLE: Duration = Duration::from_millis(250);

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

#[derive(serde::Serialize)]
struct NormalizedGroup<'a> {
    name: &'a str,
    comment: &'a Option<String>,
    enabled: bool,
}

#[derive(serde::Serialize)]
struct NormalizedList {
    address: String,
    list_type: String,
    comment: Option<String>,
    enabled: bool,
    groups: Vec<String>,
}

fn normalize_groups(groups: &[Group]) -> Vec<NormalizedGroup<'_>> {
    let mut normalized: Vec<NormalizedGroup<'_>> = groups
        .iter()
        .map(|g| NormalizedGroup {
            name: &g.name,
            comment: &g.comment,
            enabled: g.enabled,
        })
        .collect();
    normalized.sort_by(|a, b| a.name.cmp(b.name));
    normalized
}

fn normalize_lists(lists: &[List], group_lookup: &HashMap<u32, String>) -> Vec<NormalizedList> {
    let mut normalized = Vec::new();

    for list in lists {
        let group_ids = if list.groups.is_empty() {
            vec![0]
        } else {
            list.groups.clone()
        };
        let mut group_names: Vec<String> = group_ids
            .iter()
            .map(|id| {
                group_lookup
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| format!("id:{}", id))
            })
            .collect();
        group_names.sort();
        normalized.push(NormalizedList {
            address: list.address.clone(),
            list_type: list.list_type.clone(),
            comment: list.comment.clone(),
            enabled: list.enabled,
            groups: group_names,
        });
    }

    normalized.sort_by(|a, b| {
        a.address
            .cmp(&b.address)
            .then_with(|| a.list_type.cmp(&b.list_type))
    });
    normalized
}

async fn sync_groups(
    main_groups: &[Group],
    secondary_groups: &[Group],
    secondary: &PiHoleClient,
) -> Result<()> {
    let secondary_by_name: HashMap<&str, &Group> = secondary_groups
        .iter()
        .map(|g| (g.name.as_str(), g))
        .collect();

    for group in main_groups {
        match secondary_by_name.get(group.name.as_str()) {
            Some(existing) => {
                let needs_update =
                    existing.comment != group.comment || existing.enabled != group.enabled;
                if needs_update {
                    secondary.update_group(&existing.name, group).await?;
                    sleep(API_WRITE_THROTTLE).await;
                }
            }
            None => {
                secondary.add_group(group).await?;
                sleep(API_WRITE_THROTTLE).await;
            }
        }
    }

    Ok(())
}

fn groups_for_list(
    list: &List,
    main_group_lookup: &HashMap<u32, String>,
    secondary_group_lookup: &HashMap<String, u32>,
    sync_groups: bool,
    secondary_host: &str,
) -> Vec<u32> {
    let raw_groups: Vec<u32> = if list.groups.is_empty() {
        vec![0]
    } else {
        list.groups.clone()
    };

    if !sync_groups && raw_groups.iter().any(|g| *g != 0) {
        warn!(
            "[{}] sync_lists enabled without sync_groups; assigning list {} to default group because it is assigned to other groups on the main instance ({:?})",
            secondary_host, list.address, raw_groups
        );
        return vec![0];
    }

    let mut mapped = Vec::new();
    for gid in raw_groups {
        let name = main_group_lookup
            .get(&gid)
            .cloned()
            .unwrap_or_else(|| format!("id:{}", gid));
        if let Some(sec_id) = secondary_group_lookup.get(&name) {
            mapped.push(*sec_id);
        } else if gid == 0 {
            mapped.push(0);
        } else {
            warn!(
                "[{}] Group '{}' missing on secondary; assigning list {} to default group 0",
                secondary_host, name, list.address
            );
            mapped.push(0);
        }
    }

    mapped.sort();
    mapped.dedup();
    mapped
}

fn lists_equal(target: &List, existing: &List) -> bool {
    let mut target_groups = target.groups.clone();
    let mut existing_groups = if existing.groups.is_empty() {
        vec![0]
    } else {
        existing.groups.clone()
    };
    target_groups.sort();
    existing_groups.sort();

    target.comment == existing.comment
        && target.enabled == existing.enabled
        && target_groups == existing_groups
}

async fn sync_lists(
    main_lists: &[List],
    main_group_lookup: &HashMap<u32, String>,
    secondary_groups: &[Group],
    secondary_lists: &[List],
    secondary: &PiHoleClient,
    sync_groups: bool,
) -> Result<()> {
    let secondary_group_lookup: HashMap<String, u32> = secondary_groups
        .iter()
        .filter_map(|g| g.id.map(|id| (g.name.clone(), id)))
        .collect();

    let secondary_list_lookup: HashMap<(String, String), &List> = secondary_lists
        .iter()
        .map(|l| ((l.address.clone(), l.list_type.clone()), l))
        .collect();

    for list in main_lists {
        let desired_groups = groups_for_list(
            list,
            main_group_lookup,
            &secondary_group_lookup,
            sync_groups,
            &secondary.config.host,
        );

        let mut desired_list = list.clone();
        desired_list.groups = desired_groups;

        let key = (list.address.clone(), list.list_type.clone());
        match secondary_list_lookup.get(&key) {
            Some(existing) => {
                if !lists_equal(&desired_list, existing) {
                    secondary.update_list(&desired_list).await?;
                    sleep(API_WRITE_THROTTLE).await;
                }
            }
            None => {
                secondary.add_list(&desired_list).await?;
                sleep(API_WRITE_THROTTLE).await;
            }
        }
    }

    Ok(())
}

async fn sync_config_api(
    main_pihole: &PiHoleClient,
    secondary_piholes: &[PiHoleClient],
    mut main_config_used: Option<serde_json::Value>,
    hash_tracker: &HashTracker,
) -> Option<serde_json::Value> {
    let api_secondaries: Vec<&PiHoleClient> = secondary_piholes
        .iter()
        .filter(|secondary| matches!(secondary.config.sync_mode, Some(SyncMode::Api)))
        .collect();

    if api_secondaries.is_empty() {
        return main_config_used;
    }

    let needs_config_sync = api_secondaries.iter().any(|secondary| {
        secondary
            .config
            .api_sync_options
            .as_ref()
            .and_then(|o| o.sync_config.as_ref())
            .is_some()
    });

    if needs_config_sync && main_config_used.is_none() {
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

    let mut needs_groups = api_secondaries.iter().any(|s| {
        s.config
            .api_sync_options
            .as_ref()
            .and_then(|o| o.sync_groups)
            .unwrap_or(false)
    });
    let needs_lists = api_secondaries.iter().any(|s| {
        s.config
            .api_sync_options
            .as_ref()
            .and_then(|o| o.sync_lists)
            .unwrap_or(false)
    });
    if needs_lists {
        needs_groups = true; // list sync requires group mapping
    }

    let mut main_groups: Vec<Group> = Vec::new();
    let mut main_group_lookup: HashMap<u32, String> = HashMap::new();
    let mut main_groups_hash: Option<u64> = None;
    if needs_groups {
        match main_pihole.get_groups().await {
            Ok(groups) => {
                main_groups = groups;
                main_group_lookup = main_groups
                    .iter()
                    .filter_map(|g| g.id.map(|id| (id, g.name.clone())))
                    .collect();
                if let Ok(hash) = hash_value(&normalize_groups(&main_groups)) {
                    main_groups_hash = Some(hash);
                }
            }
            Err(e) => error!(
                "[{}] Failed to fetch groups from main instance: {:?}",
                main_pihole.config.host, e
            ),
        }
    }

    let mut main_lists: Vec<List> = Vec::new();
    let mut main_lists_hash: Option<u64> = None;
    if needs_lists {
        match main_pihole.get_lists().await {
            Ok(lists) => {
                main_lists = lists;
                if let Ok(hash) = hash_value(&normalize_lists(&main_lists, &main_group_lookup)) {
                    main_lists_hash = Some(hash);
                }
            }
            Err(e) => error!(
                "[{}] Failed to fetch lists from main instance: {:?}",
                main_pihole.config.host, e
            ),
        }
    }

    if let Some(main_config) = &main_config_used {
        for secondary_pihole in &api_secondaries {
            let Some(api_options) = secondary_pihole.config.api_sync_options.clone() else {
                continue;
            };

            if let Some(config_sync) = api_options.sync_config {
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

                if !hash_tracker
                    .has_changed(&format!("config:{}", host_key), filtered_hash)
                    .await
                {
                    info!(
                        "[{}] Skipping config_api sync; filtered config unchanged since last run",
                        host_key
                    );
                } else {
                    info!("[{}] Syncing config via API", host_key);
                    if let Err(e) = secondary_pihole
                        .patch_config_and_wait_for_ftl_readiness(filtered_config.clone())
                        .await
                    {
                        error!("{}", e);
                    } else {
                        if secondary_pihole.config.update_gravity.unwrap_or(false) {
                            info!("[{}] Updating gravity", secondary_pihole.config.host);
                            if let Err(e) = secondary_pihole.trigger_gravity_update().await {
                                error!(
                                    "Failed to update gravity on {}: {:?}",
                                    secondary_pihole.config.host, e
                                );
                            }
                        }

                        hash_tracker
                            .update(&format!("config:{}", host_key), filtered_hash)
                            .await;
                    }
                }
            }

            if api_options.sync_groups.unwrap_or(false) {
                if main_groups.is_empty() {
                    warn!(
                        "[{}] Skipping group sync: no groups fetched from main instance",
                        secondary_pihole.config.host
                    );
                } else if let Some(groups_hash) = main_groups_hash {
                    if !hash_tracker
                        .has_changed(
                            &format!("groups:{}", secondary_pihole.config.host),
                            groups_hash,
                        )
                        .await
                    {
                        info!(
                            "[{}] Skipping groups sync; groups unchanged since last run",
                            secondary_pihole.config.host
                        );
                    } else {
                        let mut groups_failed = false;
                        let secondary_groups = match secondary_pihole.get_groups().await {
                            Ok(groups) => groups,
                            Err(e) => {
                                error!(
                                    "[{}] Failed to fetch groups from secondary: {:?}",
                                    secondary_pihole.config.host, e
                                );
                                groups_failed = true;
                                Vec::new()
                            }
                        };
                        if !groups_failed {
                            if let Err(e) =
                                sync_groups(&main_groups, &secondary_groups, secondary_pihole).await
                            {
                                error!("{}", e);
                                groups_failed = true;
                            }
                        }

                        if !groups_failed {
                            hash_tracker
                                .update(
                                    &format!("groups:{}", secondary_pihole.config.host),
                                    groups_hash,
                                )
                                .await;
                        }
                    }
                }
            }

            if api_options.sync_lists.unwrap_or(false) {
                if main_lists.is_empty() {
                    warn!(
                        "[{}] Skipping lists sync: no lists fetched from main instance",
                        secondary_pihole.config.host
                    );
                } else if let Some(lists_hash) = main_lists_hash {
                    if !hash_tracker
                        .has_changed(
                            &format!("lists:{}", secondary_pihole.config.host),
                            lists_hash,
                        )
                        .await
                    {
                        info!(
                            "[{}] Skipping lists sync; lists unchanged since last run",
                            secondary_pihole.config.host
                        );
                    } else {
                        let mut lists_failed = false;
                        let secondary_groups = match secondary_pihole.get_groups().await {
                            Ok(groups) => groups,
                            Err(e) => {
                                error!(
                                    "[{}] Failed to fetch groups from secondary (needed for list sync): {:?}",
                                    secondary_pihole.config.host, e
                                );
                                lists_failed = true;
                                Vec::new()
                            }
                        };

                        let secondary_lists = if !lists_failed {
                            match secondary_pihole.get_lists().await {
                                Ok(lists) => lists,
                                Err(e) => {
                                    error!(
                                        "[{}] Failed to fetch lists from secondary: {:?}",
                                        secondary_pihole.config.host, e
                                    );
                                    lists_failed = true;
                                    Vec::new()
                                }
                            }
                        } else {
                            Vec::new()
                        };

                        if !lists_failed {
                            if let Err(e) = sync_lists(
                                &main_lists,
                                &main_group_lookup,
                                &secondary_groups,
                                &secondary_lists,
                                secondary_pihole,
                                api_options.sync_groups.unwrap_or(false),
                            )
                            .await
                            {
                                error!("{}", e);
                                lists_failed = true;
                            }
                        }

                        if !lists_failed {
                            hash_tracker
                                .update(
                                    &format!("lists:{}", secondary_pihole.config.host),
                                    lists_hash,
                                )
                                .await;
                        }
                    }
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
