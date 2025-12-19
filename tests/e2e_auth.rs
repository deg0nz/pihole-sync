use std::time::Duration;

use anyhow::{anyhow, Result};
use pihole_sync::pihole::client::{AppPassword, PiHoleClient};
use tokio::time::sleep;

mod common;
use common::{ensure_docker_host, spawn_pihole, STARTUP_TIMEOUT};

const WEBPASSWORD: &str = "admin";

#[tokio::test(flavor = "multi_thread")]
async fn e2e_auth_with_app_password() -> Result<()> {
    common::init_logging();
    ensure_docker_host()?;
    let mut instance = spawn_pihole(WEBPASSWORD, None, |_| {}).await?;

    tracing::debug!(
        "[test] pihole at {}:{}",
        instance.client.config.host,
        instance.client.config.port
    );

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
