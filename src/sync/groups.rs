use std::collections::{HashMap, HashSet};

use anyhow::Result;
use tokio::time::sleep;

use crate::pihole::client::{Group, PiHoleClient};
use crate::sync::util::API_WRITE_THROTTLE;

#[derive(serde::Serialize)]
pub struct NormalizedGroup<'a> {
    pub name: &'a str,
    pub comment: &'a Option<String>,
    pub enabled: bool,
}

pub fn normalize_groups(groups: &[Group]) -> Vec<NormalizedGroup<'_>> {
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

pub async fn sync_groups(
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

    for orphan in orphan_groups(main_groups, secondary_groups) {
        secondary.delete_group(&orphan.name).await?;
        sleep(API_WRITE_THROTTLE).await;
    }

    Ok(())
}

/// Secondary groups missing on main. Group 0 ("Default") is never deleted.
fn orphan_groups<'a>(main_groups: &[Group], secondary_groups: &'a [Group]) -> Vec<&'a Group> {
    let main_names: HashSet<&str> = main_groups.iter().map(|g| g.name.as_str()).collect();
    secondary_groups
        .iter()
        .filter(|g| g.id != Some(0) && !main_names.contains(g.name.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(id: u32, name: &str) -> Group {
        Group {
            name: name.into(),
            comment: None,
            enabled: true,
            id: Some(id),
        }
    }

    #[test]
    fn orphan_groups_keeps_default_and_main_groups() {
        let main = vec![group(0, "Default"), group(1, "kids")];
        let secondary = vec![
            group(0, "Renamed default"),
            group(5, "kids"),
            group(6, "old"),
        ];
        let orphans: Vec<&str> = orphan_groups(&main, &secondary)
            .iter()
            .map(|g| g.name.as_str())
            .collect();
        assert_eq!(orphans, vec!["old"]);
    }
}
