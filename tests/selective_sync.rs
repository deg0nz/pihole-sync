use std::path::PathBuf;

use anyhow::{Context, Result};
use pihole_sync::{
    config::{
        Config, ConfigApiSyncMode, ConfigSyncOptions, InstanceConfig, SyncConfig, SyncMode,
        SyncTriggerMode,
    },
    pihole::client::PiHoleClient,
    sync::run_sync,
};
use serde_json::{json, Value};
use tempfile::TempDir;

mod common;
use common::pihole::PiHoleInstance;
use common::{ensure_docker_host, spawn_pihole};
use tracing::debug;

const MAIN_WEBPASSWORD: &str = "admin-main";
const SECONDARY_WEBPASSWORD: &str = "admin-secondary";

#[derive(Clone)]
struct SeedData {
    upstreams: Value,
    hosts: Value,
    cname_records: Value,
    session_timeout: Value,
}

impl SeedData {
    fn main_seed() -> Self {
        Self {
            upstreams: json!(["1.1.1.1", "1.0.0.1"]),
            hosts: json!(["10.0.0.10 main.test"]),
            cname_records: json!(["alias.main,target.main"]),
            session_timeout: json!(1337),
        }
    }

    fn secondary_seed() -> Self {
        Self {
            upstreams: json!(["9.9.9.9"]),
            hosts: json!(["10.0.0.99 secondary.test"]),
            cname_records: json!(["alias.secondary,target.secondary"]),
            session_timeout: json!(99),
        }
    }
}

async fn spawn_test_pihole(password: &str) -> Result<PiHoleInstance> {
    spawn_pihole(password, None, |_| {}).await
}

async fn seed_config(client: &PiHoleClient, seed: &SeedData) -> Result<Value> {
    let mut config = client.get_config().await?;

    set_path(&mut config, &["dns", "upstreams"], seed.upstreams.clone());
    set_path(&mut config, &["dns", "hosts"], seed.hosts.clone());
    set_path(
        &mut config,
        &["dns", "cnameRecords"],
        seed.cname_records.clone(),
    );
    set_path(
        &mut config,
        &["webserver", "session", "timeout"],
        seed.session_timeout.clone(),
    );

    client
        .patch_config_and_wait_for_ftl_readiness(json!({ "config": config }))
        .await
        .context("failed to patch config with seed data")?;

    client
        .get_config()
        .await
        .context("failed to fetch config after seeding")
}

fn set_path(config: &mut Value, path: &[&str], new_value: Value) {
    let mut current = config;
    for segment in path.iter().take(path.len().saturating_sub(1)) {
        if !current.is_object() {
            *current = json!({});
        }

        let obj = current.as_object_mut().unwrap();
        current = obj
            .entry((*segment).to_string())
            .or_insert_with(|| Value::Object(Default::default()));
    }

    if let Some(last) = path.last() {
        if !current.is_object() {
            *current = json!({});
        }
        current
            .as_object_mut()
            .unwrap()
            .insert((*last).to_string(), new_value);
    }
}

fn read_path(config: &Value, path: &[&str]) -> Option<Value> {
    let pointer = format!("/{}", path.join("/"));
    config.pointer(&pointer).cloned()
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
        },
        main,
        secondary: vec![secondary],
    };

    let path = dir.path().join("config.yaml");
    config.save(&path)?;
    Ok(path)
}

#[tokio::test()]
async fn config_api_selective_sync_include_and_exclude() -> Result<()> {
    common::init_logging();
    ensure_docker_host()?;
    run_include_mode().await?;
    // run_exclude_mode().await?;

    Ok(())
}

async fn run_include_mode() -> Result<()> {
    let temp_dir = TempDir::new().context("failed to create temp dir")?;
    let main = spawn_test_pihole(MAIN_WEBPASSWORD).await?;
    let secondary = spawn_test_pihole(SECONDARY_WEBPASSWORD).await?;

    debug!(
        "[test] main at {}:{}",
        main.client.config.host, main.client.config.port
    );
    debug!(
        "[test] secondary at {}:{}",
        secondary.client.config.host, secondary.client.config.port
    );

    let main_seeded = seed_config(&main.client, &SeedData::main_seed())
        .await
        .context("failed to seed main config")?;
    let secondary_seeded = seed_config(&secondary.client, &SeedData::secondary_seed())
        .await
        .context("failed to seed secondary config")?;

    let mut secondary_cfg = secondary.client.config.clone();
    secondary_cfg.sync_mode = Some(SyncMode::ConfigApi);
    secondary_cfg.config_api_sync_options = Some(ConfigSyncOptions {
        mode: Some(ConfigApiSyncMode::Include),
        filter_keys: vec!["dns.upstreams".into(), "webserver.session.timeout".into()],
    });

    let config_path =
        write_test_config(&temp_dir, main.client.config.clone(), secondary_cfg.clone())?;

    run_sync(
        config_path
            .to_str()
            .context("failed to convert config path to str")?,
        true,
        false,
    )
    .await?;

    let synced = secondary.client.get_config().await?;

    assert_eq!(
        read_path(&synced, &["dns", "upstreams"]),
        read_path(&main_seeded, &["dns", "upstreams"]),
        "included upstreams should sync from main"
    );
    assert_eq!(
        read_path(&synced, &["webserver", "session", "timeout"]),
        read_path(&main_seeded, &["webserver", "session", "timeout"]),
        "included webserver session settings should sync from main"
    );
    assert_eq!(
        read_path(&synced, &["dns", "hosts"]),
        read_path(&secondary_seeded, &["dns", "hosts"]),
        "non-included hosts should remain unchanged on secondary"
    );
    assert_eq!(
        read_path(&synced, &["dns", "cnameRecords"]),
        read_path(&secondary_seeded, &["dns", "cnameRecords"]),
        "non-included cnameRecords should remain unchanged on secondary"
    );

    Ok(())
}

#[allow(dead_code)]
async fn run_exclude_mode() -> Result<()> {
    let temp_dir = TempDir::new().context("failed to create temp dir")?;
    let main = spawn_test_pihole(MAIN_WEBPASSWORD).await?;
    let secondary = spawn_test_pihole(SECONDARY_WEBPASSWORD).await?;

    tracing::debug!(
        "[test] main at {}:{}",
        main.client.config.host,
        main.client.config.port
    );
    tracing::debug!(
        "[test] secondary at {}:{}",
        secondary.client.config.host,
        secondary.client.config.port
    );

    let main_seeded = seed_config(&main.client, &SeedData::main_seed())
        .await
        .context("failed to seed main config")?;
    let secondary_seeded = seed_config(&secondary.client, &SeedData::secondary_seed())
        .await
        .context("failed to seed secondary config")?;

    let mut secondary_cfg = secondary.client.config.clone();
    secondary_cfg.sync_mode = Some(SyncMode::ConfigApi);
    secondary_cfg.config_api_sync_options = Some(ConfigSyncOptions {
        mode: Some(ConfigApiSyncMode::Exclude),
        filter_keys: vec!["dns.cnameRecords".into()],
    });

    let config_path =
        write_test_config(&temp_dir, main.client.config.clone(), secondary_cfg.clone())?;

    run_sync(
        config_path
            .to_str()
            .context("failed to convert config path to str")?,
        true,
        false,
    )
    .await?;

    let synced = secondary.client.get_config().await?;

    assert_eq!(
        read_path(&synced, &["dns", "upstreams"]),
        read_path(&main_seeded, &["dns", "upstreams"]),
        "non-excluded upstreams should sync from main"
    );
    assert_eq!(
        read_path(&synced, &["dns", "hosts"]),
        read_path(&main_seeded, &["dns", "hosts"]),
        "non-excluded hosts should sync from main"
    );
    assert_eq!(
        read_path(&synced, &["webserver", "session", "timeout"]),
        read_path(&main_seeded, &["webserver", "session", "timeout"]),
        "non-excluded webserver settings should sync from main"
    );
    assert_eq!(
        read_path(&synced, &["dns", "cnameRecords"]),
        read_path(&secondary_seeded, &["dns", "cnameRecords"]),
        "excluded cnameRecords should remain unchanged on secondary"
    );

    Ok(())
}
