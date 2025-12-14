use std::{env, os::unix::net::UnixStream, path::Path, time::Duration};

use anyhow::{anyhow, Result};
use pihole_sync::{config::InstanceConfig, pihole::client::PiHoleClient};
use testcontainers::core::IntoContainerPort;
use testcontainers::{runners::AsyncRunner, ContainerAsync, GenericImage, ImageExt};
use tokio::time::sleep;

pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(90);

pub struct PiHoleInstance {
    _container: ContainerAsync<GenericImage>,
    pub client: PiHoleClient,
}

pub fn ensure_docker_host() -> Result<()> {
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

pub async fn spawn_pihole<F>(
    webpassword: &str,
    extra_env: Option<&[(&str, &str)]>,
    configure: F,
) -> Result<PiHoleInstance>
where
    F: FnOnce(&mut InstanceConfig),
{
    let mut image = GenericImage::new("pihole/pihole", "latest")
        .with_exposed_port(80.tcp())
        .with_env_var("FTLCONF_webserver_api_password", webpassword)
        .with_env_var("FTLCONF_dns_listeningMode", "all")
        .with_env_var("TZ", "UTC");

    if let Some(extra_env) = extra_env {
        for (key, value) in extra_env {
            image = image.with_env_var(*key, *value);
        }
    }

    let container = image.start().await?;
    let host_port = container.get_host_port_ipv4(80).await?;

    let mut config = InstanceConfig {
        host: "127.0.0.1".into(),
        schema: "http".into(),
        port: host_port,
        api_key: webpassword.into(),
        update_gravity: Some(false),
        sync_mode: None,
        config_api_sync_options: None,
        config_sync: None,
        teleporter_sync_options: None,
        teleporter_options: None,
        import_options: None,
    };

    configure(&mut config);

    let client = PiHoleClient::new(config);
    wait_for_ready(&client).await?;

    Ok(PiHoleInstance {
        _container: container,
        client,
    })
}

pub async fn wait_for_ready(client: &PiHoleClient) -> Result<()> {
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
