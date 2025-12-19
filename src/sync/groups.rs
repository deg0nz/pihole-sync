use std::collections::HashMap;

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

    Ok(())
}
