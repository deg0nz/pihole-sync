use anyhow::{anyhow, Context, Result};
use reqwest::{
    multipart::{Form, Part},
    Client, ClientBuilder, RequestBuilder, Response, StatusCode,
};
use serde::Deserialize;
use serde_json::Value;
use std::{path::Path, sync::Arc};
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration, Instant};
use tracing::{debug, info, trace};

use crate::config::InstanceConfig;

#[derive(Debug, Deserialize)]
struct AuthResponse {
    session: Session,
}

#[derive(Debug, Deserialize)]
pub struct AppPassword {
    pub password: String,
    pub hash: String,
}

#[derive(Debug, Deserialize)]
struct AppPasswordResponse {
    app: AppPassword,
}

#[derive(Debug, Deserialize)]
struct Session {
    valid: bool,
    #[allow(dead_code)]
    totp: Option<bool>,
    sid: Option<String>,
    #[allow(dead_code)]
    validity: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct BackupUploadProcessedResponse {
    files: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PiHoleClient {
    base_url: String,
    client: Client,
    session_token: Arc<Mutex<Option<String>>>,
    pub config: InstanceConfig,
}

const X_FTL_SID_HEADER: &str = "X-FTL-SID";
static APP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

impl PiHoleClient {
    pub fn new(config: InstanceConfig) -> Self {
        let base_url = format!("{}://{}:{}/api", config.schema, config.host, config.port);
        Self {
            client: ClientBuilder::new()
                .user_agent(APP_USER_AGENT)
                .danger_accept_invalid_certs(true)
                .build()
                .unwrap(),
            base_url,
            session_token: Arc::new(Mutex::new(None)),
            config,
        }
    }

    fn instance_label(&self) -> (&str, u16) {
        (&self.config.host, self.config.port)
    }

    /// **Authenticate and get session token**
    async fn authenticate(&self, password: Option<String>) -> Result<()> {
        let (host, port) = self.instance_label();
        debug!("[{}:{}] Authenticating", host, port);
        let auth_url = format!("{}/auth", self.base_url);
        let body = serde_json::json!({ "password": if let Some(pw) = password { pw } else { self.config.api_key.clone() } });

        let response = self.client.post(&auth_url).json(&body).send().await?;

        let res_json = response.json::<AuthResponse>().await?;

        if let Some(token) = res_json.session.sid {
            debug!("[{}:{}] Authentication successful.", host, port);
            self.set_token(token).await?;
        } else {
            anyhow::bail!("[{}:{}] Failed to authenticate: No session ID received. This probably means that the API password is invalid.", host, port);
        }
        Ok(())
    }

    pub async fn fetch_app_password(&self, password: String) -> Result<AppPassword> {
        let (host, port) = self.instance_label();
        debug!("[{}:{}] Fetching app password", host, port);
        self.authenticate(Some(password)).await?;

        let app_auth_url = format!("{}/auth/app", self.base_url);

        let response = self
            .authorized_request(self.client.get(&app_auth_url))
            .await?;

        if response.status().is_client_error() {
            anyhow::bail!("Failed to fetch app password: {}", response.text().await?);
        }

        let password_res = response.json::<AppPasswordResponse>().await?;

        Ok(password_res.app)
    }

    /// **Check if session is still valid**
    async fn is_logged_in(&self) -> Result<bool> {
        let (host, port) = self.instance_label();
        trace!("[{}:{}] Checking login status", host, port);
        let url = format!("{}/auth", self.base_url);
        let response = self.authorized_request(self.client.get(&url)).await?;

        if response.status() == StatusCode::UNAUTHORIZED {
            debug!("[{}:{}] Not authenticated", host, port);
            return Ok(false);
        }

        let auth_response = response.json::<AuthResponse>().await?;

        // Update token if we get a new one
        if let Some(token) = auth_response.session.sid {
            trace!("[{}:{}] Received token", host, port);
            let cached = self.session_token.lock().await.clone();
            match cached {
                Some(cached_token) if cached_token == token => {
                    trace!("[{}:{}] Re-using cached token", host, port);
                }
                _ => {
                    debug!("[{}:{}] Updating cached token", host, port);
                    self.set_token(token).await?;
                }
            };
        }

        trace!(
            "[{}:{}] Authenticated? {:?}",
            host,
            port,
            auth_response.session.valid
        );

        Ok(auth_response.session.valid)
    }

    async fn set_token(&self, token: String) -> Result<()> {
        let (host, port) = self.instance_label();
        debug!("[{}:{}] Caching token", host, port);
        let mut local_token = self.session_token.lock().await;
        *local_token = Some(token);

        Ok(())
    }

    /// Downloads a backup from the Teleporter API.
    pub async fn download_backup(&self, output_path: &Path) -> Result<()> {
        let (host, port) = self.instance_label();
        debug!("[{}:{}] Downloading Teleporter backup", host, port);
        self.ensure_authenticated().await?;

        let response = self.get("/teleporter").await?;
        let bytes = response.bytes().await?;

        tokio::fs::write(output_path, &bytes)
            .await
            .context("Failed to write backup file")?;

        info!("[{}:{}] Successfully downloaded backup archive", host, port);
        Ok(())
    }

    /// Uploads a backup to the Teleporter API.
    pub async fn upload_backup(&self, file_path: &Path) -> Result<()> {
        let (host, port) = self.instance_label();
        debug!("[{}:{}] Uploading Teleporter backup", host, port);
        self.ensure_authenticated().await?;

        let file_bytes = tokio::fs::read(file_path).await?;
        let url = format!("{}/teleporter", self.base_url);

        let file_part = Part::bytes(file_bytes).file_name("pihole_backup.zip");

        let mut form = Form::new()
            .text("resourceName", "pihole_backup.zip")
            .part("file", file_part);

        if let Some(teleporter_options) = self.config.teleporter_sync_options.clone() {
            let teleporter_options_part = Part::text(serde_json::to_string(&teleporter_options)?);
            form = form.part("import", teleporter_options_part);
        }

        let response = self
            .authorized_request(
                self.client
                    .post(&url)
                    .multipart(form)
                    .header("Content-Type", "application/zip"),
            )
            .await?;

        match response.error_for_status() {
            Ok(res) => {
                info!("[{}:{}] Successfully uploaded backup", host, port);
                info!("[{}:{}] Processed:", host, port);
                res.json::<BackupUploadProcessedResponse>()
                    .await?
                    .files
                    .iter()
                    .for_each(|file| info!("[{}:{}]   {}", host, port, file));
            }
            Err(err) => {
                debug!("[{}:{}] Error: {}", host, port, err.to_string());
                return Err(err.into());
            }
        }

        Ok(())
    }

    /// Triggers a gravity update.
    pub async fn trigger_gravity_update(&self) -> Result<()> {
        let (host, port) = self.instance_label();
        trace!("[{}:{}] Triggering gravity update", host, port);
        self.post("/action/gravity").await?;
        info!("[{}:{}] Triggered gravity update", host, port);
        Ok(())
    }

    async fn get_session_token(&self) -> Result<String> {
        let session_token = self.session_token.lock().await.clone();
        Ok(session_token.unwrap_or("".to_string()))
    }

    pub async fn logout(&self) -> Result<()> {
        let (host, port) = self.instance_label();
        trace!("[{}:{}] Logging out", host, port);
        let cached_token = self.session_token.lock().await.clone();
        if cached_token.is_none() {
            trace!("[{}:{}] No cached token; skipping logout", host, port);
            return Ok(());
        }

        let url = format!("{}/auth", self.base_url);
        let response = self.authorized_request(self.client.delete(&url)).await?;
        // Pi-hole returns 410 Gone on successful logout (no content).
        if response.status() == StatusCode::GONE
            || response.status().is_success()
            || response.status() == StatusCode::UNAUTHORIZED
        {
            trace!("[{}:{}] Already logged out", host, port);
        } else {
            response
                .error_for_status()
                .context(format!("Logout request failed: {}", url))?;
        }
        *self.session_token.lock().await = None;
        info!("[{}:{}] Logged out", host, port);
        Ok(())
    }

    pub async fn get_config(&self) -> Result<Value> {
        let (host, port) = self.instance_label();
        trace!("[{}:{}] Fetching /config", host, port);
        let response = self.get("/config").await?;
        let v: Value = response.json().await?;

        // TODO: Remove unwrap, handle None
        Ok(v.get("config").unwrap().to_owned())
    }

    pub async fn patch_config(&self, config: Value) -> Result<()> {
        let (host, port) = self.instance_label();
        trace!("[{}:{}] Patching /config", host, port);

        // dbg!(&config);

        self.patch("/config", config).await?;
        Ok(())
    }

    /// Patch the config, wait for the API to become ready again, and verify the change took effect.
    pub async fn patch_config_and_wait_for_ftl_readiness(&self, config: Value) -> Result<()> {
        let (host, port) = self.instance_label();
        trace!("[{}:{}] Patching /config with verification", host, port);

        self.patch_config(serde_json::json!({ "config": config.clone() }))
            .await?;

        // Wait for FTL/api to come back after the restart window.
        self.wait_for_ready(Duration::from_secs(10)).await?;

        // let ftl_info = self.get("/info/ftl").await?;

        // let ftl_info: Value = ftl_info.json().await?;

        // dbg!(ftl_info);

        Ok(())
    }

    /// Wait until the Pi-hole API responds again (covering the FTL restart window).
    pub async fn wait_for_ready(&self, timeout: Duration) -> Result<()> {
        let (host, port) = self.instance_label();
        let start = Instant::now();
        let mut attempt: u32 = 0;

        // sleep(Duration::from_millis(1_000)).await;

        loop {
            attempt += 1;
            match self.ensure_authenticated().await {
                Ok(_) => {
                    debug!("[{}:{}] API ready after {} attempt(s)", host, port, attempt);
                    return Ok(());
                }
                Err(e) => {
                    let elapsed = start.elapsed();
                    if elapsed >= timeout {
                        return Err(anyhow!(
                            "[{}:{}] API not ready after {:?}: {}",
                            host,
                            port,
                            elapsed,
                            e
                        ));
                    }

                    // Jittered exponential backoff capped at 1s.
                    let backoff_ms = (2u64.pow(attempt.min(6)) * 50).min(1_000);
                    trace!(
                        "[{}:{}] API not ready yet ({}). Retrying in {}ms",
                        host,
                        port,
                        e,
                        backoff_ms
                    );
                    sleep(Duration::from_millis(backoff_ms)).await;
                }
            }
        }
    }

    /////////////////////////
    /// HTTP Request helpers
    /////////////////////////

    async fn authorized_request(&self, request: RequestBuilder) -> Result<Response> {
        let token = self.get_session_token().await?;
        let request = request.header(X_FTL_SID_HEADER, &token);

        request.send().await.map_err(Into::into)
    }

    /// **Ensure authentication before making requests**
    async fn ensure_authenticated(&self) -> Result<()> {
        // If we don't have a cached SID yet, avoid the extra round-trip to `/auth`
        // with an empty token (which will always yield 401).
        if self.session_token.lock().await.is_none() {
            self.authenticate(None).await?;
            return Ok(());
        }

        if !self.is_logged_in().await? {
            self.authenticate(None).await?;
        }
        Ok(())
    }

    /// **Make an authenticated GET request**
    async fn get(&self, endpoint: &str) -> Result<Response> {
        self.ensure_authenticated().await?;
        let url = format!("{}{}", self.base_url, endpoint);

        let request = self.authorized_request(self.client.get(&url)).await?;

        Ok(request)
    }

    /// Sends an authenticated POST request to the Pi-hole API.
    async fn post(&self, endpoint: &str) -> Result<Response> {
        self.ensure_authenticated().await?;

        let url = format!("{}{}", self.base_url, endpoint);

        self.authorized_request(self.client.post(&url))
            .await?
            .error_for_status()
            .context(format!("POST request failed: {}", url))
    }

    /// Sends an authenticated POST request to the Pi-hole API.
    async fn patch(&self, endpoint: &str, data: Value) -> Result<Response> {
        self.ensure_authenticated().await?;

        let url = format!("{}{}", self.base_url, endpoint);

        self.authorized_request(self.client.patch(&url).json(&data))
            .await?
            .error_for_status()
            .context(format!("PATCH request failed: {}", url))
    }
}
