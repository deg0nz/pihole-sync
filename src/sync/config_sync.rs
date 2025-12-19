use std::collections::HashMap;

use tracing::{debug, error, info, warn};

use crate::config::{ConfigApiSyncMode, SyncMode};
use crate::pihole::client::{Group, List, PiHoleClient};
use crate::pihole::config_filter::{ConfigFilter, FilterMode};
use crate::sync::groups::{normalize_groups, sync_groups};
use crate::sync::lists::{normalize_lists, sync_lists};
use crate::sync::util::{hash_config, hash_value, HashTracker};

pub async fn sync_config_api(
    main_pihole: &PiHoleClient,
    secondary_piholes: &[PiHoleClient],
    mut main_config_used: Option<serde_json::Value>,
    hash_tracker: &HashTracker,
) -> Option<serde_json::Value> {
    let api_secondaries: Vec<&PiHoleClient> = secondary_piholes
        .iter()
        .filter(|secondary| matches!(secondary.config.sync_mode, Some(SyncMode::Api)))
        .collect();
    debug!("api_secondaries count: {}", api_secondaries.len());

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
    debug!(
        "API sync needs_config_sync={}, needs_groups={}, needs_lists={}",
        needs_config_sync, needs_groups, needs_lists
    );

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
                    debug!(
                        "Fetched {} group(s) from main; hash={}",
                        main_groups.len(),
                        hash
                    );
                } else {
                    debug!(
                        "Fetched {} group(s) from main but failed to compute hash",
                        main_groups.len()
                    );
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
                    debug!(
                        "Fetched {} list(s) from main; hash={}",
                        main_lists.len(),
                        hash
                    );
                } else {
                    debug!(
                        "Fetched {} list(s) from main but failed to compute hash",
                        main_lists.len()
                    );
                }
            }
            Err(e) => error!(
                "[{}] Failed to fetch lists from main instance: {:?}",
                main_pihole.config.host, e
            ),
        }
    }

    for secondary_pihole in &api_secondaries {
        debug!(
            "[{}] raw api_sync_options: {:?}",
            secondary_pihole.config.host, secondary_pihole.config.api_sync_options
        );
        let Some(api_options) = secondary_pihole.config.api_sync_options.clone() else {
            continue;
        };
        debug!(
            "[{}] api_sync_options: groups={:?}, lists={:?}, config={:?}",
            secondary_pihole.config.host,
            api_options.sync_groups,
            api_options.sync_lists,
            api_options.sync_config.as_ref().map(|c| c.mode)
        );

        if let Some(config_sync) = api_options.sync_config {
            if let Some(main_config) = &main_config_used {
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
            } else {
                debug!(
                    "[{}] Skipping config sync because no main config is available",
                    secondary_pihole.config.host
                );
            }
        }

        if api_options.sync_groups.unwrap_or(false) {
            if main_groups.is_empty() {
                warn!(
                    "[{}] Skipping group sync: no groups fetched from main instance",
                    secondary_pihole.config.host
                );
            } else if let Some(groups_hash) = main_groups_hash {
                let changed = hash_tracker
                    .has_changed(
                        &format!("groups:{}", secondary_pihole.config.host),
                        groups_hash,
                    )
                    .await;
                debug!(
                    "[{}] groups hash on main: {}; has_changed={}",
                    secondary_pihole.config.host, groups_hash, changed
                );
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
                let changed = hash_tracker
                    .has_changed(
                        &format!("lists:{}", secondary_pihole.config.host),
                        lists_hash,
                    )
                    .await;
                debug!(
                    "[{}] lists hash on main: {}; has_changed={}",
                    secondary_pihole.config.host, lists_hash, changed
                );
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

    main_config_used
}
