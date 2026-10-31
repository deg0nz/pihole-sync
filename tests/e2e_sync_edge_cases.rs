use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use pihole_sync::{
    config::{
        ApiSyncOptions, Config, ConfigApiSyncMode, ConfigSyncOptions, InstanceConfig, SyncConfig,
        SyncMode, SyncTriggerMode,
    },
    pihole::client::{Group, List, PiHoleClient},
    sync::{run_sync, teleporter_content_hash},
};
use serde_json::{json, Value};
use tempfile::TempDir;

mod common;
use common::pihole::{raw_session, wait_for_ready};
use common::{ensure_docker_host, spawn_pihole};

const MAIN_WEBPASSWORD: &str = "admin-main";
const SECONDARY_WEBPASSWORD: &str = "admin-secondary";

const TRICKY_ADDRESS: &str = "https://example.com/path/hosts.txt?format=adblock&x=1";
const SPACE_GROUP: &str = "Space Group";

fn group(name: &str, comment: &str) -> Group {
    Group {
        name: name.into(),
        comment: Some(comment.into()),
        enabled: true,
        id: None,
    }
}

fn list(address: &str, comment: &str, enabled: bool, groups: Vec<u32>) -> List {
    List {
        address: address.into(),
        list_type: "block".into(),
        comment: Some(comment.into()),
        groups,
        enabled,
        id: None,
    }
}

async fn group_id(client: &PiHoleClient, name: &str) -> Result<u32> {
    client
        .get_groups()
        .await?
        .into_iter()
        .find(|g| g.name == name)
        .and_then(|g| g.id)
        .with_context(|| format!("group {name} not found"))
}

fn api_secondary(client: &PiHoleClient, sync_config: Option<ConfigSyncOptions>) -> InstanceConfig {
    let mut cfg = client.config.clone();
    cfg.sync_mode = Some(SyncMode::Api);
    cfg.api_sync_options = Some(ApiSyncOptions {
        sync_config,
        sync_groups: Some(true),
        sync_lists: Some(true),
    });
    cfg
}

async fn sync_once(
    dir: &TempDir,
    main: InstanceConfig,
    secondary: Vec<InstanceConfig>,
) -> Result<()> {
    let cache_location = dir.path().join("cache");
    let config = Config {
        sync: SyncConfig {
            interval: 1,
            cache_location: cache_location.to_str().context("cache path")?.into(),
            trigger_mode: SyncTriggerMode::Interval,
            config_path: "/etc/pihole/pihole.toml".into(),
            api_poll_interval: None,
            trigger_api_readiness_timeout_secs: 60,
        },
        main,
        secondary,
    };
    let path: PathBuf = dir.path().join("config.yaml");
    config.save(&path)?;
    run_sync(path.to_str().context("config path")?, true, false).await
}

/// PUT /lists/{address} and PUT /groups/{name} must work with characters that need encoding.
#[tokio::test]
async fn e2e_api_updates_list_with_query_address_and_group_with_space() -> Result<()> {
    common::init_logging();
    ensure_docker_host()?;
    let dir = TempDir::new()?;
    let main = spawn_pihole(MAIN_WEBPASSWORD, None, |_| {}).await?;
    let secondary = spawn_pihole(SECONDARY_WEBPASSWORD, None, |_| {}).await?;

    main.client
        .add_group(&group(SPACE_GROUP, "main comment"))
        .await?;
    let main_gid = group_id(&main.client, SPACE_GROUP).await?;
    main.client
        .add_list(&list(TRICKY_ADDRESS, "main", false, vec![main_gid]))
        .await?;

    secondary
        .client
        .add_group(&group(SPACE_GROUP, "old comment"))
        .await?;
    secondary
        .client
        .add_list(&list(TRICKY_ADDRESS, "old", true, vec![0]))
        .await?;

    sync_once(
        &dir,
        main.client.config.clone(),
        vec![api_secondary(&secondary.client, None)],
    )
    .await?;

    let sec_gid = group_id(&secondary.client, SPACE_GROUP).await?;
    let groups = secondary.client.get_groups().await?;
    let g = groups.iter().find(|g| g.name == SPACE_GROUP).unwrap();
    assert_eq!(g.comment.as_deref(), Some("main comment"));

    let lists = secondary.client.get_lists().await?;
    let matching: Vec<_> = lists
        .iter()
        .filter(|l| l.address == TRICKY_ADDRESS)
        .collect();
    assert_eq!(matching.len(), 1, "list must be updated, not duplicated");
    assert_eq!(matching[0].comment.as_deref(), Some("main"));
    assert!(!matching[0].enabled);
    assert_eq!(matching[0].groups, vec![sec_gid]);
    Ok(())
}

/// An empty (but successfully fetched) main list set removes all secondary lists,
/// including ones whose address needs encoding; orphan groups go, group 0 stays.
#[tokio::test]
async fn e2e_api_mirrors_deletions_with_tricky_address_and_empty_main() -> Result<()> {
    common::init_logging();
    ensure_docker_host()?;
    let dir = TempDir::new()?;
    let main = spawn_pihole(MAIN_WEBPASSWORD, None, |_| {}).await?;
    let secondary = spawn_pihole(SECONDARY_WEBPASSWORD, None, |_| {}).await?;

    for l in main.client.get_lists().await? {
        main.client.delete_list(&l.address, &l.list_type).await?;
    }
    assert!(main.client.get_lists().await?.is_empty());

    secondary
        .client
        .add_list(&list(TRICKY_ADDRESS, "extra", true, vec![0]))
        .await?;
    secondary
        .client
        .add_group(&group("Extra Group", "extra"))
        .await?;
    assert!(secondary.client.get_lists().await?.len() >= 2);

    sync_once(
        &dir,
        main.client.config.clone(),
        vec![api_secondary(&secondary.client, None)],
    )
    .await?;

    let lists = secondary.client.get_lists().await?;
    assert!(lists.is_empty(), "secondary lists not removed: {lists:?}");
    let groups = secondary.client.get_groups().await?;
    assert!(!groups.iter().any(|g| g.name == "Extra Group"));
    assert!(groups.iter().any(|g| g.id == Some(0)));
    Ok(())
}

/// Two secondaries on the same host (different ports) must both be synced in one run.
#[tokio::test]
async fn e2e_api_syncs_two_secondaries_on_same_host() -> Result<()> {
    common::init_logging();
    ensure_docker_host()?;
    let dir = TempDir::new()?;
    let main = spawn_pihole(MAIN_WEBPASSWORD, None, |_| {}).await?;
    let sec_a = spawn_pihole(SECONDARY_WEBPASSWORD, None, |_| {}).await?;
    let sec_b = spawn_pihole(SECONDARY_WEBPASSWORD, None, |_| {}).await?;
    assert_eq!(sec_a.client.config.host, sec_b.client.config.host);

    main.client.add_group(&group("Shared Group", "g")).await?;
    let address = "https://example.com/multi-port.txt";
    main.client
        .add_list(&list(address, "l", true, vec![0]))
        .await?;

    sync_once(
        &dir,
        main.client.config.clone(),
        vec![
            api_secondary(&sec_a.client, None),
            api_secondary(&sec_b.client, None),
        ],
    )
    .await?;

    for sec in [&sec_a, &sec_b] {
        let port = sec.client.config.port;
        assert!(
            sec.client
                .get_groups()
                .await?
                .iter()
                .any(|g| g.name == "Shared Group"),
            "group missing on secondary port {port}"
        );
        assert!(
            sec.client
                .get_lists()
                .await?
                .iter()
                .any(|l| l.address == address),
            "list missing on secondary port {port}"
        );
    }
    Ok(())
}

/// The readiness check can pass before FTL starts restarting, so wait a bit first.
async fn patch_and_settle(client: &PiHoleClient, config: Value) -> Result<()> {
    client
        .patch_config_and_wait_for_ftl_readiness(config)
        .await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    wait_for_ready(client).await
}

/// Activates an app password; `app_sudo` lets app-password sessions modify config.
async fn enable_app_password(
    client: &PiHoleClient,
    webpassword: &str,
    app_sudo: bool,
) -> Result<String> {
    let app = client.fetch_app_password(webpassword.into()).await?;
    patch_and_settle(
        client,
        json!({ "webserver": { "api": { "app_pwhash": app.hash, "app_sudo": app_sudo } } }),
    )
    .await?;
    client.logout().await?;
    Ok(app.password)
}

/// Exclude mode with no keys syncs everything except the secondary's API access settings:
/// it keeps accepting its own app password, and repeated config syncs keep working.
#[tokio::test]
async fn e2e_config_exclude_all_keeps_secondary_credentials() -> Result<()> {
    common::init_logging();
    ensure_docker_host()?;
    let dir = TempDir::new()?;
    let main = spawn_pihole(MAIN_WEBPASSWORD, None, |_| {}).await?;
    let secondary = spawn_pihole(SECONDARY_WEBPASSWORD, None, |_| {}).await?;

    enable_app_password(&main.client, MAIN_WEBPASSWORD, false).await?;
    let sec_app_pw = enable_app_password(&secondary.client, SECONDARY_WEBPASSWORD, true).await?;
    let sec_api_before = secondary.client.get_config().await?["webserver"]["api"].clone();

    let mut sec_cfg = api_secondary(
        &secondary.client,
        Some(ConfigSyncOptions {
            mode: Some(ConfigApiSyncMode::Exclude),
            filter_keys: vec![],
        }),
    );
    sec_cfg.api_key = sec_app_pw;
    let app_client = PiHoleClient::new(sec_cfg.clone())?;

    // Two rounds: the second only arrives if the first left the secondary's API access intact.
    for upstream in ["1.1.1.1", "9.9.9.9"] {
        patch_and_settle(&main.client, json!({ "dns": { "upstreams": [upstream] } })).await?;
        sync_once(&dir, main.client.config.clone(), vec![sec_cfg.clone()]).await?;
        tokio::time::sleep(Duration::from_secs(3)).await;
        wait_for_ready(&app_client).await?;

        let config = app_client
            .get_config()
            .await
            .context("secondary must still accept its own app password")?;
        assert_eq!(config.pointer("/dns/upstreams"), Some(&json!([upstream])));
        for key in ["app_pwhash", "app_sudo", "pwhash"] {
            assert_eq!(
                config["webserver"]["api"][key], sec_api_before[key],
                "secondary webserver.api.{key} was overwritten"
            );
        }
    }
    Ok(())
}

/// A server-side invalidated session is transparently re-established on 401.
#[tokio::test]
async fn e2e_client_reauthenticates_after_session_invalidated() -> Result<()> {
    common::init_logging();
    ensure_docker_host()?;
    let instance = spawn_pihole(SECONDARY_WEBPASSWORD, None, |_| {}).await?;
    let client = &instance.client;
    client.add_group(&group(SPACE_GROUP, "before")).await?;

    // Kill every session except a fresh raw one, including the client's cached session.
    let (http, base, sid) = raw_session(&client.config).await?;
    let sessions: Value = http
        .get(format!("{base}/auth/sessions"))
        .header("X-FTL-SID", &sid)
        .send()
        .await?
        .json()
        .await?;
    let mut killed = 0;
    for s in sessions["sessions"].as_array().context("sessions")? {
        if s["current_session"] == json!(false) {
            http.delete(format!("{base}/auth/session/{}", s["id"]))
                .header("X-FTL-SID", &sid)
                .send()
                .await?
                .error_for_status()?;
            killed += 1;
        }
    }
    assert!(killed >= 1, "expected the client's session to be killed");

    assert!(client
        .get_groups()
        .await?
        .iter()
        .any(|g| g.name == SPACE_GROUP));
    client.get_lists().await?;
    client
        .update_group(SPACE_GROUP, &group(SPACE_GROUP, "after"))
        .await?;
    let groups = client.get_groups().await?;
    let g = groups.iter().find(|g| g.name == SPACE_GROUP).unwrap();
    assert_eq!(g.comment.as_deref(), Some("after"));
    Ok(())
}

/// Teleporter mode copies groups, lists and domains to the secondary.
#[tokio::test]
async fn e2e_teleporter_sync_copies_gravity() -> Result<()> {
    common::init_logging();
    ensure_docker_host()?;
    let dir = TempDir::new()?;
    let main = spawn_pihole(MAIN_WEBPASSWORD, None, |_| {}).await?;
    let secondary = spawn_pihole(SECONDARY_WEBPASSWORD, None, |_| {}).await?;

    let domain = "blocked.teleporter.test";
    let address = "https://example.com/teleporter.txt";
    main.client.add_group(&group("Teleported", "tp")).await?;
    main.client
        .add_list(&list(address, "tp", true, vec![0]))
        .await?;
    let (http, base, sid) = raw_session(&main.client.config).await?;
    http.post(format!("{base}/domains/deny/exact"))
        .header("X-FTL-SID", &sid)
        .json(&json!({ "domain": domain, "comment": "tp", "groups": [0], "enabled": true }))
        .send()
        .await?
        .error_for_status()?;

    let mut sec_cfg = secondary.client.config.clone();
    sec_cfg.sync_mode = Some(SyncMode::Teleporter);
    sync_once(&dir, main.client.config.clone(), vec![sec_cfg]).await?;

    // The import may restart FTL on the secondary.
    tokio::time::sleep(Duration::from_secs(2)).await;
    wait_for_ready(&secondary.client).await?;

    assert!(secondary
        .client
        .get_groups()
        .await?
        .iter()
        .any(|g| g.name == "Teleported"));
    assert!(secondary
        .client
        .get_lists()
        .await?
        .iter()
        .any(|l| l.address == address));

    let (http, base, sid) = raw_session(&secondary.client.config).await?;
    let domains: Value = http
        .get(format!("{base}/domains/deny/exact"))
        .header("X-FTL-SID", &sid)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        domains["domains"]
            .as_array()
            .context("domains")?
            .iter()
            .any(|d| d["domain"] == json!(domain)),
        "domain missing on secondary: {domains}"
    );
    Ok(())
}

/// Two downloads of an unchanged main differ in bytes (entry timestamps) but not in content hash.
#[tokio::test]
async fn e2e_teleporter_content_hash_stable_for_unchanged_main() -> Result<()> {
    common::init_logging();
    ensure_docker_host()?;
    let dir = TempDir::new()?;
    let main = spawn_pihole(MAIN_WEBPASSWORD, None, |_| {}).await?;

    let (first, second) = (dir.path().join("a.zip"), dir.path().join("b.zip"));
    main.client.download_backup(&first).await?;
    // Zip timestamps have 2s resolution.
    tokio::time::sleep(Duration::from_secs(3)).await;
    main.client.download_backup(&second).await?;

    let (a, b) = (std::fs::read(&first)?, std::fs::read(&second)?);
    assert_ne!(a, b, "raw backups expected to differ by entry timestamps");
    assert_eq!(teleporter_content_hash(&a)?, teleporter_content_hash(&b)?);
    Ok(())
}
