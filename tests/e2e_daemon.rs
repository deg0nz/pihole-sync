//! Runs the real `pihole-sync sync` binary as a long-lived daemon and checks that a change
//! on main reaches the secondary without restarting the process.
//!
//! `interval` / `api_poll_interval` are whole minutes, so the two polling tests use the
//! smallest real value (1 minute) and take a bit over a minute each.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pihole_sync::{
    config::{
        ApiSyncOptions, Config, ConfigApiSyncMode, ConfigSyncOptions, InstanceConfig, SyncConfig,
        SyncMode, SyncTriggerMode,
    },
    pihole::client::{Group, List, PiHoleClient},
};
use serde_json::json;
use tempfile::TempDir;

mod common;
use common::{ensure_docker_host, spawn_pihole};

const MAIN_WEBPASSWORD: &str = "admin-main";
const SECONDARY_WEBPASSWORD: &str = "admin-secondary";
/// One poll interval (60s) plus sync time and slack.
const SYNC_TIMEOUT: Duration = Duration::from_secs(150);

/// Kills the daemon on drop (also on panic) and dumps its output; libtest only shows it on failure.
struct Daemon {
    child: Child,
    output: Arc<Mutex<String>>,
}

impl Daemon {
    fn start(config_path: &Path, extra_args: &[&str]) -> Result<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_pihole-sync"))
            .arg("--config")
            .arg(config_path)
            .arg("sync")
            .args(extra_args)
            .env("RUST_LOG", "info")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn pihole-sync")?;
        let output = Arc::new(Mutex::new(String::new()));
        pipe_into(child.stdout.take(), &output);
        pipe_into(child.stderr.take(), &output);
        Ok(Self { child, output })
    }

    fn log(&self) -> String {
        self.output.lock().unwrap().clone()
    }

    async fn wait_for_log(&mut self, needle: &str) -> Result<()> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(30) {
            if self.log().contains(needle) {
                return Ok(());
            }
            self.assert_running()?;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        bail!("daemon never logged {needle:?}")
    }

    fn assert_running(&mut self) -> Result<()> {
        if let Some(status) = self.child.try_wait()? {
            bail!("daemon exited early with {status}");
        }
        Ok(())
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        println!("----- pihole-sync output -----\n{}", self.log());
    }
}

fn pipe_into<R: Read + Send + 'static>(pipe: Option<R>, output: &Arc<Mutex<String>>) {
    let Some(pipe) = pipe else { return };
    let output = Arc::clone(output);
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            let mut out = output.lock().unwrap();
            out.push_str(&line);
            out.push('\n');
        }
    });
}

fn api_secondary(client: &PiHoleClient) -> InstanceConfig {
    let mut cfg = client.config.clone();
    cfg.sync_mode = Some(SyncMode::Api);
    cfg.api_sync_options = Some(ApiSyncOptions {
        sync_config: Some(ConfigSyncOptions {
            mode: Some(ConfigApiSyncMode::Include),
            filter_keys: vec!["dns.upstreams".into()],
        }),
        sync_groups: Some(true),
        sync_lists: Some(true),
    });
    cfg
}

fn write_config(
    dir: &TempDir,
    trigger_mode: SyncTriggerMode,
    config_path: &Path,
    main: InstanceConfig,
    secondary: InstanceConfig,
) -> Result<PathBuf> {
    let config = Config {
        sync: SyncConfig {
            interval: 1,
            cache_location: dir.path().join("cache").to_str().unwrap().into(),
            trigger_mode,
            config_path: config_path.to_str().unwrap().into(),
            api_poll_interval: Some(1),
            trigger_api_readiness_timeout_secs: 60,
        },
        main,
        secondary: vec![secondary],
    };
    let path = dir.path().join("config.yaml");
    config.save(&path)?;
    Ok(path)
}

fn upstream(tag: &str) -> String {
    match tag {
        "one" => "9.9.9.9".into(),
        "two" => "1.0.0.1".into(),
        _ => "8.8.4.4".into(),
    }
}

/// Adds a group, a list in that group and a new dns.upstreams value on main.
async fn change_main(main: &PiHoleClient, tag: &str) -> Result<()> {
    let group_name = format!("Daemon Group {tag}");
    main.add_group(&Group {
        name: group_name.clone(),
        comment: Some(tag.into()),
        enabled: true,
        id: None,
    })
    .await?;
    let gid = main
        .get_groups()
        .await?
        .into_iter()
        .find(|g| g.name == group_name)
        .and_then(|g| g.id)
        .context("group id")?;
    main.add_list(&List {
        address: format!("https://example.com/daemon-{tag}.txt"),
        list_type: "block".into(),
        comment: Some(tag.into()),
        groups: vec![gid],
        enabled: true,
        id: None,
    })
    .await?;
    main.patch_config_and_wait_for_ftl_readiness(json!({ "dns": { "upstreams": [upstream(tag)] } }))
        .await
}

async fn is_synced(secondary: &PiHoleClient, tag: &str) -> Result<bool> {
    let group_name = format!("Daemon Group {tag}");
    let Some(gid) = secondary
        .get_groups()
        .await?
        .into_iter()
        .find(|g| g.name == group_name)
        .and_then(|g| g.id)
    else {
        return Ok(false);
    };
    let address = format!("https://example.com/daemon-{tag}.txt");
    let list_ok = secondary
        .get_lists()
        .await?
        .iter()
        .any(|l| l.address == address && l.groups == vec![gid]);
    let upstreams = secondary.get_config().await?["dns"]["upstreams"].clone();
    Ok(list_ok && upstreams == json!([upstream(tag)]))
}

async fn wait_synced(daemon: &mut Daemon, secondary: &PiHoleClient, tag: &str) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < SYNC_TIMEOUT {
        daemon.assert_running()?;
        // The secondary restarts FTL while the daemon patches it; treat errors as "not yet".
        if is_synced(secondary, tag).await.unwrap_or(false) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!("change {tag:?} did not reach the secondary within {SYNC_TIMEOUT:?}")
}

/// Initial sync, then a later change picked up by the next poll of the same process.
async fn run_polling_daemon(trigger_mode: SyncTriggerMode) -> Result<()> {
    common::init_logging();
    ensure_docker_host()?;
    let dir = TempDir::new()?;
    let main = spawn_pihole(MAIN_WEBPASSWORD, None, |_| {}).await?;
    let secondary = spawn_pihole(SECONDARY_WEBPASSWORD, None, |_| {}).await?;

    change_main(&main.client, "one").await?;
    let config = write_config(
        &dir,
        trigger_mode,
        Path::new("/etc/pihole/pihole.toml"),
        main.client.config.clone(),
        api_secondary(&secondary.client),
    )?;
    let mut daemon = Daemon::start(&config, &[])?;
    wait_synced(&mut daemon, &secondary.client, "one").await?;

    change_main(&main.client, "two").await?;
    wait_synced(&mut daemon, &secondary.client, "two").await
}

#[tokio::test]
async fn e2e_daemon_interval_mode_syncs_later_changes() -> Result<()> {
    run_polling_daemon(SyncTriggerMode::Interval).await
}

#[tokio::test]
async fn e2e_daemon_watch_config_api_syncs_later_changes() -> Result<()> {
    run_polling_daemon(SyncTriggerMode::WatchConfigApi).await
}

/// The main Pi-hole runs in a container; the watcher only needs a local pihole.toml path,
/// so touching a host file stands in for FTL rewriting its config / gravity.db.
#[tokio::test]
async fn e2e_daemon_watch_config_file_syncs_on_toml_and_gravity_changes() -> Result<()> {
    common::init_logging();
    ensure_docker_host()?;
    let dir = TempDir::new()?;
    let watched = dir.path().join("pihole.toml");
    std::fs::write(&watched, "# v0\n")?;
    let main = spawn_pihole(MAIN_WEBPASSWORD, None, |_| {}).await?;
    let secondary = spawn_pihole(SECONDARY_WEBPASSWORD, None, |_| {}).await?;

    change_main(&main.client, "one").await?;
    let config = write_config(
        &dir,
        SyncTriggerMode::WatchConfigFile,
        &watched,
        main.client.config.clone(),
        api_secondary(&secondary.client),
    )?;
    let mut daemon = Daemon::start(&config, &["--no-initial-sync"])?;
    daemon.wait_for_log("Watching Pi-hole config file").await?;
    assert!(
        !is_synced(&secondary.client, "one").await?,
        "--no-initial-sync must not sync before a file change"
    );

    std::fs::write(&watched, "# v1\n")?;
    wait_synced(&mut daemon, &secondary.client, "one").await?;

    change_main(&main.client, "two").await?;
    std::fs::write(dir.path().join("gravity.db"), "changed")?;
    wait_synced(&mut daemon, &secondary.client, "two").await
}
