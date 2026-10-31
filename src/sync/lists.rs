use std::collections::{HashMap, HashSet};

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

    for orphan in orphan_lists(main_lists, secondary_lists) {
        secondary
            .delete_list(&orphan.address, &orphan.list_type)
            .await?;
        sleep(API_WRITE_THROTTLE).await;
    }

    Ok(())
}

/// Secondary lists whose (address, type) is missing on main.
fn orphan_lists<'a>(main_lists: &[List], secondary_lists: &'a [List]) -> Vec<&'a List> {
    let main_keys: HashSet<(&str, &str)> = main_lists
        .iter()
        .map(|l| (l.address.as_str(), l.list_type.as_str()))
        .collect();
    secondary_lists
        .iter()
        .filter(|l| !main_keys.contains(&(l.address.as_str(), l.list_type.as_str())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(address: &str, list_type: &str) -> List {
        List {
            address: address.into(),
            list_type: list_type.into(),
            comment: None,
            groups: vec![0],
            enabled: true,
            id: None,
        }
    }

    fn with_groups(groups: Vec<u32>) -> List {
        List {
            groups,
            ..list("https://a", "block")
        }
    }

    #[test]
    fn groups_for_list_maps_by_name_not_id() {
        let main = HashMap::from([(0, "Default".to_string()), (3, "kids".to_string())]);
        let secondary = HashMap::from([("Default".to_string(), 0), ("kids".to_string(), 7)]);
        let map = |l: &List, sync_groups| groups_for_list(l, &main, &secondary, sync_groups, "s");

        assert_eq!(map(&with_groups(vec![3, 0, 3]), true), vec![0, 7]);
        // Empty means default group; ids missing on either side fall back to 0.
        assert_eq!(map(&with_groups(vec![]), true), vec![0]);
        assert_eq!(map(&with_groups(vec![9]), true), vec![0]);
        // Without group sync, secondary ids can't be trusted to match.
        assert_eq!(map(&with_groups(vec![3]), false), vec![0]);
    }

    #[test]
    fn lists_equal_ignores_group_order_and_empty_default() {
        assert!(lists_equal(&with_groups(vec![0]), &with_groups(vec![])));
        assert!(lists_equal(
            &with_groups(vec![1, 2]),
            &with_groups(vec![2, 1])
        ));
        assert!(!lists_equal(&with_groups(vec![1]), &with_groups(vec![2])));
        let disabled = List {
            enabled: false,
            ..with_groups(vec![0])
        };
        assert!(!lists_equal(&disabled, &with_groups(vec![0])));
        let commented = List {
            comment: Some("c".into()),
            ..with_groups(vec![0])
        };
        assert!(!lists_equal(&commented, &with_groups(vec![0])));
    }

    #[test]
    fn normalize_lists_hash_ignores_ids_and_order() {
        // Same names under different ids must hash the same (else every run resyncs);
        // a different group must not.
        let a = HashMap::from([(1, "x".to_string()), (2, "y".to_string())]);
        let b = HashMap::from([(5, "y".to_string()), (6, "x".to_string())]);
        let lists_a = vec![list("https://b", "block"), with_groups(vec![1, 2])];
        let lists_b = vec![with_groups(vec![5, 6]), list("https://b", "block")];
        let hash = |l: &[List], g| crate::sync::util::hash_value(&normalize_lists(l, g)).unwrap();
        assert_eq!(hash(&lists_a, &a), hash(&lists_b, &b));
        assert_ne!(
            hash(&[with_groups(vec![1])], &a),
            hash(&[with_groups(vec![2])], &a)
        );
        assert_eq!(
            normalize_lists(&[with_groups(vec![4])], &a)[0].groups,
            vec!["id:4"]
        );
    }

    #[test]
    fn orphan_lists_matches_on_address_and_type() {
        let main = vec![list("https://a", "block")];
        let secondary = vec![list("https://a", "block"), list("https://a", "allow")];
        let orphans = orphan_lists(&main, &secondary);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].list_type, "allow");
    }
}
