use std::collections::HashMap;

use anyhow::Result;
use tracing::{debug, error, info, warn};

use crate::config::{ConfigApiSyncMode, SyncMode};
use crate::pihole::client::{Group, List, PiHoleClient};
use crate::pihole::config_filter::{ConfigFilter, FilterMode};
use crate::sync::groups::sync_groups;
use crate::sync::lists::sync_lists;
use crate::sync::util::{hash_config, HashTracker};

#[derive(Debug, Clone, Copy, Default)]
pub struct ApiSyncNeeds {
    pub config: bool,
    pub groups: bool,
    pub lists: bool,
}

impl ApiSyncNeeds {
    pub fn any(&self) -> bool {
        self.config || self.groups || self.lists
    }
}

#[derive(Debug, Default, Clone)]
pub struct ApiSyncPayload {
    pub main_config: Option<serde_json::Value>,
    pub main_groups: Vec<Group>,
    pub main_group_lookup: HashMap<u32, String>,
    pub main_lists: Vec<List>,
    pub main_groups_hash: Option<u64>,
    pub main_lists_hash: Option<u64>,
}

pub fn determine_api_sync_needs(secondary_piholes: &[PiHoleClient]) -> ApiSyncNeeds {
    let api_secondaries: Vec<&PiHoleClient> = secondary_piholes
        .iter()
        .filter(|secondary| matches!(secondary.config.sync_mode, Some(SyncMode::Api)))
        .collect();
    debug!("api_secondaries count: {}", api_secondaries.len());

    let config = api_secondaries.iter().any(|secondary| {
        secondary
            .config
            .api_sync_options
            .as_ref()
            .and_then(|o| o.sync_config.as_ref())
            .is_some()
    });

    let mut groups = api_secondaries.iter().any(|s| {
        s.config
            .api_sync_options
            .as_ref()
            .and_then(|o| o.sync_groups)
            .unwrap_or(false)
    });
    let lists = api_secondaries.iter().any(|s| {
        s.config
            .api_sync_options
            .as_ref()
            .and_then(|o| o.sync_lists)
            .unwrap_or(false)
    });

    if lists {
        groups = true; // list sync requires group mapping
    }

    ApiSyncNeeds {
        config,
        groups,
        lists,
    }
}

/// Sync configuration to a single secondary instance
async fn sync_config_for_secondary(
    secondary: &PiHoleClient,
    main_config: &serde_json::Value,
    config_sync: &crate::config::ConfigSyncOptions,
    hash_tracker: &HashTracker,
) -> Result<()> {
    let filter_mode = match config_sync.mode.unwrap_or(ConfigApiSyncMode::Include) {
        ConfigApiSyncMode::Include => FilterMode::OptIn,
        ConfigApiSyncMode::Exclude => FilterMode::OptOut,
    };

    let filter = ConfigFilter::new(&config_sync.filter_keys, filter_mode);
    let filtered_config = filter.filter_json(main_config.clone());
    let host_key = secondary.config.host.clone();

    let filtered_hash = hash_config(&filtered_config)?;

    if !hash_tracker
        .has_changed(&format!("config:{}", host_key), filtered_hash)
        .await
    {
        info!(
            "[{}] Skipping config_api sync; filtered config unchanged since last run",
            host_key
        );
        return Ok(());
    }

    info!("[{}] Syncing config via API", host_key);
    secondary
        .patch_config_and_wait_for_ftl_readiness(filtered_config)
        .await?;

    hash_tracker
        .update(&format!("config:{}", host_key), filtered_hash)
        .await;
    Ok(())
}

/// Sync groups to a single secondary instance
async fn sync_groups_for_secondary(
    secondary: &PiHoleClient,
    main_groups: &[Group],
    main_groups_hash: u64,
    hash_tracker: &HashTracker,
) -> Result<()> {
    let host_key = format!("groups:{}", secondary.config.host);

    if !hash_tracker.has_changed(&host_key, main_groups_hash).await {
        info!(
            "[{}] Skipping groups sync; groups unchanged since last run",
            secondary.config.host
        );
        return Ok(());
    }

    let secondary_groups = secondary.get_groups().await?;
    sync_groups(main_groups, &secondary_groups, secondary).await?;

    hash_tracker.update(&host_key, main_groups_hash).await;
    Ok(())
}

/// Sync lists to a single secondary instance
async fn sync_lists_for_secondary(
    secondary: &PiHoleClient,
    main_lists: &[List],
    main_group_lookup: &HashMap<u32, String>,
    main_lists_hash: u64,
    sync_groups_enabled: bool,
    hash_tracker: &HashTracker,
) -> Result<bool> {
    let host_key = format!("lists:{}", secondary.config.host);

    if !hash_tracker.has_changed(&host_key, main_lists_hash).await {
        info!(
            "[{}] Skipping lists sync; lists unchanged since last run",
            secondary.config.host
        );
        return Ok(false);
    }

    let secondary_groups = secondary.get_groups().await?;
    let secondary_lists = secondary.get_lists().await?;

    sync_lists(
        main_lists,
        main_group_lookup,
        &secondary_groups,
        &secondary_lists,
        secondary,
        sync_groups_enabled,
    )
    .await?;

    hash_tracker.update(&host_key, main_lists_hash).await;
    Ok(true)
}

pub async fn sync_config_api(
    secondary_piholes: &[PiHoleClient],
    payload: &ApiSyncPayload,
    needs: &ApiSyncNeeds,
    hash_tracker: &HashTracker,
) {
    if !needs.any() {
        return;
    }

    let api_secondaries: Vec<&PiHoleClient> = secondary_piholes
        .iter()
        .filter(|secondary| matches!(secondary.config.sync_mode, Some(SyncMode::Api)))
        .collect();

    if api_secondaries.is_empty() {
        return;
    }

    debug!(
        "API sync needs_config_sync={}, needs_groups={}, needs_lists={}",
        needs.config, needs.groups, needs.lists
    );

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

        // Sync configuration
        if let Some(config_sync) = &api_options.sync_config {
            if let Some(main_config) = &payload.main_config {
                if let Err(e) = sync_config_for_secondary(
                    secondary_pihole,
                    main_config,
                    config_sync,
                    hash_tracker,
                )
                .await
                {
                    error!(
                        "[{}] Config sync failed: {}",
                        secondary_pihole.config.host, e
                    );
                }
            } else {
                warn!(
                    "[{}] Skipping config sync because no main config is available",
                    secondary_pihole.config.host
                );
            }
        }

        // Sync groups
        if api_options.sync_groups.unwrap_or(false) {
            if payload.main_groups.is_empty() {
                warn!(
                    "[{}] Skipping group sync: no groups fetched from main instance",
                    secondary_pihole.config.host
                );
            } else if let Some(groups_hash) = payload.main_groups_hash {
                debug!(
                    "[{}] groups hash on main: {}",
                    secondary_pihole.config.host, groups_hash
                );
                if let Err(e) = sync_groups_for_secondary(
                    secondary_pihole,
                    &payload.main_groups,
                    groups_hash,
                    hash_tracker,
                )
                .await
                {
                    error!(
                        "[{}] Group sync failed: {}",
                        secondary_pihole.config.host, e
                    );
                }
            } else {
                warn!(
                    "[{}] Skipping group sync: failed to compute groups hash on main instance",
                    secondary_pihole.config.host
                );
            }
        }

        // Sync lists
        if api_options.sync_lists.unwrap_or(false) {
            if payload.main_lists.is_empty() {
                warn!(
                    "[{}] Skipping lists sync: no lists fetched from main instance",
                    secondary_pihole.config.host
                );
            } else if let Some(lists_hash) = payload.main_lists_hash {
                debug!(
                    "[{}] lists hash on main: {}",
                    secondary_pihole.config.host, lists_hash
                );
                match sync_lists_for_secondary(
                    secondary_pihole,
                    &payload.main_lists,
                    &payload.main_group_lookup,
                    lists_hash,
                    api_options.sync_groups.unwrap_or(false),
                    hash_tracker,
                )
                .await
                {
                    Ok(updated) => {
                        if updated && secondary_pihole.config.update_gravity.unwrap_or(false) {
                            info!(
                                "[{}] Lists changed; triggering gravity update",
                                secondary_pihole.config.host
                            );
                            if let Err(e) = secondary_pihole.trigger_gravity_update().await {
                                error!(
                                    "[{}] Failed to trigger gravity update: {}",
                                    secondary_pihole.config.host, e
                                );
                            }
                        }
                    }
                    Err(e) => error!("[{}] List sync failed: {}", secondary_pihole.config.host, e),
                }
            } else {
                warn!(
                    "[{}] Skipping list sync: failed to compute lists hash on main instance",
                    secondary_pihole.config.host
                );
            }
        }
    }
}
