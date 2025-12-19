use std::collections::HashMap;

use anyhow::Result;
use tokio::time::sleep;
use tracing::warn;

use crate::pihole::client::{Group, List, PiHoleClient};
use crate::sync::util::API_WRITE_THROTTLE;

#[derive(serde::Serialize)]
pub struct NormalizedList {
    pub address: String,
    pub list_type: String,
    pub comment: Option<String>,
    pub enabled: bool,
    pub groups: Vec<String>,
}

pub fn normalize_lists(lists: &[List], group_lookup: &HashMap<u32, String>) -> Vec<NormalizedList> {
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

pub async fn sync_lists(
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
