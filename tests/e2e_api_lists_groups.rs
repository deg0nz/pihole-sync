use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use pihole_sync::{
    config::{ApiSyncOptions, Config, InstanceConfig, SyncConfig, SyncMode, SyncTriggerMode},
    pihole::client::{Group, List, PiHoleClient},
    sync::run_sync,
};
use tempfile::TempDir;
use tracing::debug;

mod common;
use common::{ensure_docker_host, spawn_pihole};

const MAIN_WEBPASSWORD: &str = "admin-main";
const SECONDARY_WEBPASSWORD: &str = "admin-secondary";

#[tokio::test()]
async fn e2e_api_sync_groups_and_lists() -> Result<()> {
    common::init_logging();
    ensure_docker_host()?;

    let temp_dir = TempDir::new().context("failed to create temp dir")?;
    let main = spawn_pihole(MAIN_WEBPASSWORD, None, |_| {}).await?;
    let secondary = spawn_pihole(SECONDARY_WEBPASSWORD, None, |_| {}).await?;

    debug!(
        "[test] main at {}:{}",
        main.client.config.host, main.client.config.port
    );
    debug!(
        "[test] secondary at {}:{}",
        secondary.client.config.host, secondary.client.config.port
    );

    let main_groups = seed_groups(&main.client).await?;
    let list_type = derive_list_type(&main.client).await?;
    seed_lists(&main.client, &main_groups, &list_type).await?;

    let mut secondary_cfg = secondary.client.config.clone();
    secondary_cfg.sync_mode = Some(SyncMode::Api);
    secondary_cfg.api_sync_options = Some(ApiSyncOptions {
        sync_groups: Some(true),
        sync_lists: Some(true),
        ..Default::default()
    });

    let config_path = write_test_config(&temp_dir, main.client.config.clone(), secondary_cfg)?;

    run_sync(
        config_path
            .to_str()
            .context("failed to convert config path to str")?,
        true,
        false,
    )
    .await?;

    let secondary_groups = secondary.client.get_groups().await?;
    assert_groups_synced(
        &main_groups,
        &secondary_groups,
        &["E2E Group Alpha", "E2E Group Beta"],
    );

    let secondary_lists = secondary.client.get_lists().await?;
    assert_lists_synced(
        &secondary_lists,
        &secondary_groups,
        &list_type,
        &[
            ExpectedList {
                address: "https://example.com/blocklist-alpha.txt",
                comment: "E2E alpha list",
                enabled: true,
                groups: &["E2E Group Alpha"],
            },
            ExpectedList {
                address: "https://example.com/blocklist-beta.txt",
                comment: "E2E beta list",
                enabled: false,
                groups: &["E2E Group Alpha", "E2E Group Beta"],
            },
        ],
    );

    Ok(())
}

#[tokio::test()]
async fn e2e_api_sync_many_groups_and_lists_respects_rate_limit() -> Result<()> {
    common::init_logging();
    ensure_docker_host()?;

    let temp_dir = TempDir::new().context("failed to create temp dir")?;
    let main = spawn_pihole(MAIN_WEBPASSWORD, None, |_| {}).await?;
    let secondary = spawn_pihole(SECONDARY_WEBPASSWORD, None, |_| {}).await?;

    debug!(
        "[test-many] main at {}:{}",
        main.client.config.host, main.client.config.port
    );
    debug!(
        "[test-many] secondary at {}:{}",
        secondary.client.config.host, secondary.client.config.port
    );

    let seeded_groups = seed_many_groups(&main.client, 25).await?;
    let list_type = derive_list_type(&main.client).await?;
    seed_many_lists(&main.client, &list_type, 30).await?;

    let mut secondary_cfg = secondary.client.config.clone();
    secondary_cfg.sync_mode = Some(SyncMode::Api);
    secondary_cfg.api_sync_options = Some(ApiSyncOptions {
        sync_groups: Some(true),
        sync_lists: Some(true),
        ..Default::default()
    });

    let config_path = write_test_config(&temp_dir, main.client.config.clone(), secondary_cfg)?;

    run_sync(
        config_path
            .to_str()
            .context("failed to convert config path to str")?,
        true,
        false,
    )
    .await?;

    let secondary_groups = secondary.client.get_groups().await?;
    let secondary_group_names: Vec<_> = secondary_groups.iter().map(|g| g.name.as_str()).collect();
    for group in &seeded_groups {
        assert!(
            secondary_group_names.contains(&group.name.as_str()),
            "missing bulk group {} on secondary",
            group.name
        );
    }

    let secondary_lists = secondary.client.get_lists().await?;
    let secondary_addresses: Vec<_> = secondary_lists.iter().map(|l| l.address.as_str()).collect();
    for idx in 0..30 {
        let address = format!("https://example.com/bulk-list-{idx}.txt");
        assert!(
            secondary_addresses.contains(&address.as_str()),
            "missing bulk list {} on secondary",
            address
        );
    }

    Ok(())
}

async fn seed_groups(client: &PiHoleClient) -> Result<Vec<Group>> {
    let to_create = vec![
        Group {
            name: "E2E Group Alpha".to_string(),
            comment: Some("primary sync group".to_string()),
            enabled: true,
            id: None,
        },
        Group {
            name: "E2E Group Beta".to_string(),
            comment: Some("secondary sync group".to_string()),
            enabled: false,
            id: None,
        },
    ];

    for group in &to_create {
        client
            .add_group(group)
            .await
            .context("failed to create seed group on main")?;
    }

    client
        .get_groups()
        .await
        .context("failed to fetch groups after seeding")
}

async fn derive_list_type(client: &PiHoleClient) -> Result<String> {
    let lists = client.get_lists().await?;
    if let Some(list) = lists.first() {
        return Ok(list.list_type.clone());
    }
    Ok("adlist".to_string())
}

async fn seed_lists(client: &PiHoleClient, groups: &[Group], list_type: &str) -> Result<()> {
    let group_ids = group_id_lookup(groups)?;
    let group_alpha = *group_ids
        .get("E2E Group Alpha")
        .context("missing seeded group alpha id")?;
    let group_beta = *group_ids
        .get("E2E Group Beta")
        .context("missing seeded group beta id")?;

    let lists = vec![
        List {
            address: "https://example.com/blocklist-alpha.txt".to_string(),
            list_type: list_type.to_string(),
            comment: Some("E2E alpha list".to_string()),
            groups: vec![group_alpha],
            enabled: true,
            id: None,
        },
        List {
            address: "https://example.com/blocklist-beta.txt".to_string(),
            list_type: list_type.to_string(),
            comment: Some("E2E beta list".to_string()),
            groups: vec![group_alpha, group_beta],
            enabled: false,
            id: None,
        },
    ];

    for list in &lists {
        client
            .add_list(list)
            .await
            .with_context(|| format!("failed to seed list {}", list.address))?;
    }

    Ok(())
}

async fn seed_many_groups(client: &PiHoleClient, count: usize) -> Result<Vec<Group>> {
    let mut created = Vec::new();
    for idx in 0..count {
        let group = Group {
            name: format!("Bulk Group {idx:02}"),
            comment: Some(format!("bulk group {idx}")),
            enabled: idx % 2 == 0,
            id: None,
        };
        client
            .add_group(&group)
            .await
            .with_context(|| format!("failed to create bulk group {idx}"))?;
        created.push(group);
    }

    client
        .get_groups()
        .await
        .context("failed to fetch groups after bulk seeding")
}

async fn seed_many_lists(client: &PiHoleClient, list_type: &str, count: usize) -> Result<()> {
    for idx in 0..count {
        let list = List {
            address: format!("https://example.com/bulk-list-{idx}.txt"),
            list_type: list_type.to_string(),
            comment: Some(format!("bulk list {idx}")),
            groups: Vec::new(), // default group 0
            enabled: idx % 2 == 0,
            id: None,
        };
        client
            .add_list(&list)
            .await
            .with_context(|| format!("failed to seed bulk list {}", list.address))?;
    }

    Ok(())
}

struct ExpectedList<'a> {
    address: &'a str,
    comment: &'a str,
    enabled: bool,
    groups: &'a [&'a str],
}

fn assert_groups_synced(main: &[Group], secondary: &[Group], expected: &[&str]) {
    let main_by_name: HashMap<&str, &Group> = main.iter().map(|g| (g.name.as_str(), g)).collect();
    let secondary_by_name: HashMap<&str, &Group> =
        secondary.iter().map(|g| (g.name.as_str(), g)).collect();

    for group_name in expected {
        let main_group = main_by_name
            .get(group_name)
            .unwrap_or_else(|| panic!("missing group {} on main instance", group_name));
        let secondary_group = secondary_by_name
            .get(group_name)
            .unwrap_or_else(|| panic!("missing group {} on secondary instance", group_name));

        assert_eq!(
            secondary_group.comment, main_group.comment,
            "comment mismatch for group {}",
            group_name
        );
        assert_eq!(
            secondary_group.enabled, main_group.enabled,
            "enabled flag mismatch for group {}",
            group_name
        );
    }
}

fn assert_lists_synced(
    secondary_lists: &[List],
    secondary_groups: &[Group],
    list_type: &str,
    expected_lists: &[ExpectedList<'_>],
) {
    let secondary_group_ids =
        group_id_lookup(secondary_groups).expect("secondary groups missing IDs");

    for expected in expected_lists {
        let list = secondary_lists
            .iter()
            .find(|l| l.address == expected.address)
            .unwrap_or_else(|| panic!("missing list {} on secondary", expected.address));

        assert_eq!(
            list.list_type, list_type,
            "list type mismatch for {}",
            expected.address
        );
        assert_eq!(
            list.comment.as_deref(),
            Some(expected.comment),
            "comment mismatch for {}",
            expected.address
        );
        assert_eq!(
            list.enabled, expected.enabled,
            "enabled flag mismatch for {}",
            expected.address
        );

        let mut expected_group_ids: Vec<u32> = expected
            .groups
            .iter()
            .map(|name| {
                *secondary_group_ids.get(*name).unwrap_or_else(|| {
                    panic!(
                        "missing group {} on secondary for list {}",
                        name, expected.address
                    )
                })
            })
            .collect();
        expected_group_ids.sort();
        expected_group_ids.dedup();

        let mut actual_group_ids = if list.groups.is_empty() {
            vec![0]
        } else {
            list.groups.clone()
        };
        actual_group_ids.sort();

        assert_eq!(
            actual_group_ids, expected_group_ids,
            "group membership mismatch for {}",
            expected.address
        );
    }
}

fn group_id_lookup(groups: &[Group]) -> Result<HashMap<String, u32>> {
    let mut lookup = HashMap::new();
    for group in groups {
        let Some(id) = group.id else {
            continue;
        };
        lookup.insert(group.name.clone(), id);
    }

    if lookup.is_empty() {
        anyhow::bail!("no group ids found");
    }

    Ok(lookup)
}

fn write_test_config(
    dir: &TempDir,
    main: InstanceConfig,
    secondary: InstanceConfig,
) -> Result<PathBuf> {
    let cache_location = dir.path().join("cache_location_sentinel");
    std::fs::write(&cache_location, "do-not-use-teleporter")
        .context("failed to create cache location sentinel file")?;

    let config = Config {
        sync: SyncConfig {
            interval: 1,
            cache_location: cache_location
                .to_str()
                .context("failed to convert cache path to string")?
                .to_string(),
            trigger_mode: SyncTriggerMode::Interval,
            config_path: "/etc/pihole/pihole.toml".into(),
            api_poll_interval: None,
            trigger_api_readiness_timeout_secs: 60,
        },
        main,
        secondary: vec![secondary],
    };

    let path = dir.path().join("config.yaml");
    config.save(&path)?;
    Ok(path)
}
