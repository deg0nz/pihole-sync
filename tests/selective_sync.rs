use std::{
    env,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use pihole_sync::{
    cli::sync::run_sync,
    config::{Config, ConfigSyncOptions, InstanceConfig, SyncConfig, TeleporterImportOptions},
    pihole::client::PiHoleClient,
};
use serde_json::{json, Value};
use tempfile::TempDir;
use testcontainers::core::IntoContainerPort;
use testcontainers::{runners::AsyncRunner, ContainerAsync, GenericImage, ImageExt};
use tokio::time::sleep;

mod common;

const WEBPASSWORD: &str = "admin";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(90);

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

struct PiHoleInstance {
    _container: ContainerAsync<GenericImage>,
    client: PiHoleClient,
}

async fn spawn_pihole() -> Result<PiHoleInstance> {
    let image = GenericImage::new("pihole/pihole", "latest")
        .with_exposed_port(80.tcp())
        .with_env_var("WEBPASSWORD", WEBPASSWORD)
        .with_env_var("FTLCONF_webserver_api_password", WEBPASSWORD)
        .with_env_var("DNSMASQ_LISTENING", "all")
        .with_env_var("FTLCONF_LOCAL_IPV4", "0.0.0.0")
        .with_env_var("TZ", "UTC");

    let container = image.start().await?;
    let host_port = container.get_host_port_ipv4(80).await?;

    let base_config = InstanceConfig {
        host: "127.0.0.1".into(),
        schema: "http".into(),
        port: host_port,
        api_key: WEBPASSWORD.into(),
        update_gravity: Some(false),
        config_sync: None,
        import_options: None,
        teleporter_options: Some(TeleporterImportOptions::default()),
    };

    let client = PiHoleClient::new(base_config);
    wait_for_ready(&client).await?;

    Ok(PiHoleInstance {
        _container: container,
        client,
    })
}

async fn wait_for_ready(client: &PiHoleClient) -> Result<()> {
    let start = std::time::Instant::now();
    let mut last_err: Option<anyhow::Error> = None;

    while start.elapsed() < STARTUP_TIMEOUT {
        match client.get_config().await {
            Ok(_) => return Ok(()),
            Err(err) => {
                last_err = Some(err);
                sleep(Duration::from_secs(3)).await;
            }
        }
    }

    Err(anyhow!(
        "Pi-hole API not ready after {:?}: {:?}",
        STARTUP_TIMEOUT,
        last_err.map(|e| e.to_string())
    ))
}

fn ensure_docker_host() -> Result<()> {
    let env_host = env::var("DOCKER_HOST").ok();
    let candidates: Vec<String> = if let Some(host) = env_host.clone() {
        vec![host]
    } else {
        vec!["unix:///var/run/docker.sock".into()]
    };

    for host in &candidates {
        if let Some(socket) = host.strip_prefix("unix://") {
            if UnixStream::connect(socket).is_ok() {
                return Ok(());
            }
        }
    }

    if env_host.is_none() {
        if let Some(home) = env::var_os("HOME") {
            let colima = Path::new(&home).join(".colima/default/docker.sock");
            if colima.exists() {
                return Err(anyhow!(
                    "Docker not reachable. Set DOCKER_HOST=unix://{}",
                    colima.display()
                ));
            }
        }
    }

    Err(anyhow!(
        "Docker not reachable at {:?}. Set DOCKER_HOST to your Docker socket (e.g. unix:///var/run/docker.sock or your Colima socket).",
        candidates
    ))
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
        .patch_config(json!({ "config": config }))
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
    let cache_dir = dir.path().join("cache");
    let config = Config {
        sync: SyncConfig {
            interval: 1,
            cache_location: cache_dir
                .to_str()
                .context("failed to convert cache path to string")?
                .to_string(),
        },
        main,
        secondary: vec![secondary],
    };

    let path = dir.path().join("config.yaml");
    config.save(&path)?;
    Ok(path)
}

#[tokio::test(flavor = "multi_thread")]
async fn selective_sync_opt_in_and_opt_out() -> Result<()> {
    common::init_logging();
    ensure_docker_host()?;
    run_opt_in().await?;
    run_opt_out().await?;

    Ok(())
}

async fn run_opt_in() -> Result<()> {
    let temp_dir = TempDir::new().context("failed to create temp dir")?;
    let main = spawn_pihole().await?;
    let secondary = spawn_pihole().await?;

    let main_seeded = seed_config(&main.client, &SeedData::main_seed())
        .await
        .context("failed to seed main config")?;
    let secondary_seeded = seed_config(&secondary.client, &SeedData::secondary_seed())
        .await
        .context("failed to seed secondary config")?;

    let mut secondary_cfg = secondary.client.config.clone();
    secondary_cfg.config_sync = Some(ConfigSyncOptions {
        exclude: false,
        filter_keys: vec!["dns.upstreams".into(), "webserver.session".into()],
    });

    let config_path =
        write_test_config(&temp_dir, main.client.config.clone(), secondary_cfg.clone())?;

    run_sync(
        config_path
            .to_str()
            .context("failed to convert config path to str")?,
        true,
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

async fn run_opt_out() -> Result<()> {
    let temp_dir = TempDir::new().context("failed to create temp dir")?;
    let main = spawn_pihole().await?;
    let secondary = spawn_pihole().await?;

    let main_seeded = seed_config(&main.client, &SeedData::main_seed())
        .await
        .context("failed to seed main config")?;
    let secondary_seeded = seed_config(&secondary.client, &SeedData::secondary_seed())
        .await
        .context("failed to seed secondary config")?;

    let mut secondary_cfg = secondary.client.config.clone();
    secondary_cfg.config_sync = Some(ConfigSyncOptions {
        exclude: true,
        filter_keys: vec!["dns.cnameRecords".into()],
    });

    let config_path =
        write_test_config(&temp_dir, main.client.config.clone(), secondary_cfg.clone())?;

    run_sync(
        config_path
            .to_str()
            .context("failed to convert config path to str")?,
        true,
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
