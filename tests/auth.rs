use std::{env, os::unix::net::UnixStream, path::Path, time::Duration};

use anyhow::{anyhow, Result};
use pihole_sync::{
    config::InstanceConfig,
    pihole::client::{AppPassword, PiHoleClient},
};
use testcontainers::core::IntoContainerPort;
use testcontainers::{runners::AsyncRunner, ContainerAsync, GenericImage, ImageExt};
use tokio::time::sleep;

mod common;

const WEBPASSWORD: &str = "admin";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(90);

struct PiHoleInstance {
    _container: ContainerAsync<GenericImage>,
    client: PiHoleClient,
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_with_app_password() -> Result<()> {
    common::init_logging();
    ensure_docker_host()?;
    let mut instance = spawn_pihole().await?;

    // Wrong password should fail to authenticate
    let bad_attempt = instance
        .client
        .fetch_app_password("wrong".to_string())
        .await;
    assert!(bad_attempt.is_err(), "expected bad password to fail");

    // Correct web password should return an app password
    let app_pw = wait_for_app_password(&mut instance.client).await?;
    assert!(!app_pw.password.is_empty());
    assert!(!app_pw.hash.is_empty());

    let cfg = instance.client.get_config().await?;
    assert!(
        cfg.get("dns").is_some(),
        "expected dns section in returned config"
    );

    Ok(())
}

async fn spawn_pihole() -> Result<PiHoleInstance> {
    let image = GenericImage::new("pihole/pihole", "latest")
        .with_exposed_port(80.tcp())
        .with_env_var("FTLCONF_webserver_api_password", WEBPASSWORD)
        .with_env_var("FTLCONF_dns_listeningMode", "all")
        .with_env_var("TZ", "UTC");

    let container = image.start().await?;
    let host_port = container.get_host_port_ipv4(80).await?;

    let client = PiHoleClient::new(InstanceConfig {
        host: "127.0.0.1".into(),
        schema: "http".into(),
        port: host_port,
        api_key: WEBPASSWORD.into(),
        update_gravity: Some(false),
        config_sync: None,
        import_options: None,
        teleporter_options: None,
    });

    Ok(PiHoleInstance {
        _container: container,
        client,
    })
}

async fn wait_for_app_password(client: &mut PiHoleClient) -> Result<AppPassword> {
    let start = std::time::Instant::now();
    let mut last_err: Option<anyhow::Error> = None;

    while start.elapsed() < STARTUP_TIMEOUT {
        match client.fetch_app_password(WEBPASSWORD.to_string()).await {
            Ok(app) => return Ok(app),
            Err(err) => {
                last_err = Some(err.into());
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
