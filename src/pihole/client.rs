use anyhow::{Context, Result};
use reqwest::{
    multipart::{Form, Part},
    Client, ClientBuilder, Response, StatusCode,
};
use serde::Deserialize;
use serde_json::Value;
use std::{path::Path, sync::Arc};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

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
    sid: Option<String>,
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

const X_FTL_SID_HEADER: &str = "sid";
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

    /// **Authenticate and get session token**
    async fn authenticate(&self, password: Option<String>) -> Result<()> {
        debug!("Authenticating");
        let auth_url = format!("{}/auth", self.base_url);
        let body = serde_json::json!({ "password": if let Some(pw) = password { pw } else { self.config.api_key.clone() } });

        let response = self
            .client
            .post(&auth_url)
            .json(&body)
            .send()
            .await?
            .json::<AuthResponse>()
            .await?;

        debug!("Auth Response: {:?}", response);

        if let Some(token) = response.session.sid {
            self.set_token(token.clone()).await?;
        } else {
            anyhow::bail!("Failed to authenticate: No session ID received. This probably means that the API password is invalid.");
        }
        Ok(())
    }

    pub async fn fetch_app_password(&self, password: String) -> Result<AppPassword> {
        self.authenticate(Some(password)).await?;

        let app_auth_url = format!("{}/auth/app", self.base_url);

        let response = self
            .client
            .get(&app_auth_url)
            .header(X_FTL_SID_HEADER, self.get_session_token().await?)
            .send()
            .await?;

        if response.status().is_client_error() {
            anyhow::bail!("Failed to fetch app password: {}", response.text().await?);
        }

        let password_res = response.json::<AppPasswordResponse>().await?;

        Ok(password_res.app)
    }

    /// **Check if session is still valid**
    async fn is_logged_in(&self) -> Result<bool> {
        debug!("Checking login status");
        let url = format!("{}/auth", self.base_url);
        let response = self
            .client
            .get(&url)
            .header(X_FTL_SID_HEADER, self.get_session_token().await?)
            .send()
            .await?;

        if response.status() == StatusCode::UNAUTHORIZED {
            return Ok(false);
        }

        let auth_response = response.json::<AuthResponse>().await?;

        // Update token if we get a new one
        if let Some(token) = auth_response.session.sid {
            if &token != self.session_token.lock().await.as_ref().unwrap() {
                self.set_token(token).await?
            }
        }

        debug!("Authenticated? {:?}", auth_response.session.valid);

        Ok(auth_response.session.valid)
    }

    async fn set_token(&self, token: String) -> Result<()> {
        debug!("Updating token");
        let mut local_token = self.session_token.lock().await;
        *local_token = Some(token);

        Ok(())
    }

    /// **Ensure authentication before making requests**
    async fn ensure_authenticated(&self) -> Result<()> {
        if !self.is_logged_in().await? {
            self.authenticate(None).await?;
        }
        Ok(())
    }

    /// **Make an authenticated GET request**
    async fn get(&self, endpoint: &str) -> Result<Response> {
        self.ensure_authenticated().await?;
        let url = format!("{}{}", self.base_url, endpoint);

        let request = self
            .client
            .get(&url)
            .header(X_FTL_SID_HEADER, self.get_session_token().await?)
            .send()
            .await?;

        Ok(request)
    }

    /// Sends an authenticated POST request to the Pi-hole API.
    async fn post(&self, endpoint: &str) -> Result<Response> {
        self.ensure_authenticated().await?;

        let url = format!("{}{}", self.base_url, endpoint);

        self.client
            .post(&url)
            .header(X_FTL_SID_HEADER, self.get_session_token().await?)
            .send()
            .await?
            .error_for_status()
            .context(format!("POST request failed: {}", url))
    }

    /// Sends an authenticated POST request to the Pi-hole API.
    async fn patch(&self, endpoint: &str, data: Value) -> Result<Response> {
        self.ensure_authenticated().await?;

        let url = format!("{}{}", self.base_url, endpoint);

        self.client
            .patch(&url)
            .json(&data)
            .header(X_FTL_SID_HEADER, self.get_session_token().await?)
            .send()
            .await?
            .error_for_status()
            .context(format!("POST request failed: {}", url))
    }

    /// Sends an authenticated POST request to the Pi-hole API.
    async fn delete(&self, endpoint: &str) -> Result<Response> {
        self.ensure_authenticated().await?;

        let url = format!("{}{}", self.base_url, endpoint);

        let res = self
            .client
            .post(&url)
            .header(X_FTL_SID_HEADER, self.get_session_token().await?)
            .send()
            .await?;

        Ok(res)
    }

    /// Downloads a backup from the Teleporter API.
    pub async fn download_backup(&self, output_path: &Path) -> Result<()> {
        self.ensure_authenticated().await?;

        let response = self.get("/teleporter").await?;
        let bytes = response.bytes().await?;

        tokio::fs::write(output_path, &bytes)
            .await
            .context("Failed to write backup file")?;

        info!("Successfully downloaded backup archive");
        Ok(())
    }

    /// Uploads a backup to the Teleporter API.
    pub async fn upload_backup(&self, file_path: &Path) -> Result<()> {
        self.ensure_authenticated().await?;

        let file_bytes = tokio::fs::read(file_path).await?;
        let url = format!("{}/teleporter", self.base_url);

        let file_part = Part::bytes(file_bytes).file_name("pihole_backup.zip");

        let mut form = Form::new()
            .text("resourceName", "pihole_backup.zip")
            .part("file", file_part);

        if let Some(import_options) = self.config.import_options.clone() {
            let import_options_part = Part::text(serde_json::to_string(&import_options)?);
            form = form.part("import", import_options_part);
        }

        let response = self
            .client
            .post(&url)
            .header(X_FTL_SID_HEADER, self.get_session_token().await?)
            .multipart(form)
            .header("Content-Type", "application/zip")
            .send()
            .await?;

        match response.error_for_status() {
            Ok(res) => {
                info!("Successfully uploaded backup to {}", self.base_url);
                info!("Processed:");
                res.json::<BackupUploadProcessedResponse>()
                    .await?
                    .files
                    .iter()
                    .for_each(|file| info!("  {}", file));
            }
            Err(err) => {
                debug!("Error: {}", err.to_string());
                return Err(err.into());
            }
        }

        Ok(())
    }

    /// Triggers a gravity update.
    pub async fn trigger_gravity_update(&self) -> Result<()> {
        self.post("/action/gravity").await?;
        info!("Triggered gravity update on {}", self.base_url);
        Ok(())
    }

    async fn get_session_token(&self) -> Result<String> {
        let session_token = self.session_token.lock().await.clone();
        Ok(session_token.unwrap_or("".to_string()))
    }

    pub async fn logout(&self) -> Result<()> {
        self.delete("/auth").await?;
        info!("Logged out from {}", self.base_url);
        Ok(())
    }

    /// Retrieves session timeout in seconds
    async fn get_session_timeout(&self) -> Result<u64> {
        let response = self.get("/config/webserver/session/timeout").await?;
        let v: Value = response.json().await?;
        if let Some(timeout) = v
            .get("config")
            .and_then(|config| config.get("webserver"))
            .and_then(|webserver| webserver.get("session"))
            .and_then(|session| session.get("timeout"))
            .and_then(|timeout| timeout.as_u64())
        {
            return Ok(timeout);
        }

        Ok(0)
    }

    pub async fn get_config(&self) -> Result<Value> {
        let response = self.get("/config").await?;
        let v: Value = response.json().await?;

        // TODO: Remove unwrap, handle None
        Ok(v.get("config").unwrap().to_owned())
    }

    pub async fn patch_config(&self, config: Value) -> Result<()> {
        self.patch("/config", config).await?;
        Ok(())
    }

    async fn start_session_keepalive(&self, interval_seconds: u64) -> Result<()> {
        // Ensure we have a valid session before starting the keepalive loop
        self.ensure_authenticated().await?;

        // Get the initial session token that we'll be maintaining
        let initial_token = self.get_session_token().await?;
        debug!("Starting keepalive for session token: {}", initial_token);

        let client = self.clone();

        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_secs(interval_seconds));

            loop {
                interval.tick().await;
                debug!("Performing session keepalive check");

                match client.get("/auth").await {
                    Ok(response) => match response.json::<AuthResponse>().await {
                        Ok(auth_response) => {
                            if !auth_response.session.valid {
                                warn!("Session became invalid during keepalive. Cancelling keepalive.");
                                return;
                            } else {
                                debug!("Session keepalive successful");
                            }
                        }
                        Err(e) => error!("Failed to parse keepalive response: {}", e),
                    },
                    Err(e) => error!("Session keepalive check failed: {}", e),
                }
            }
        });

        Ok(())
    }

    pub async fn init_session_keepalive(&self, sync_interval_seconds: u64) -> Result<()> {
        let session_timeout = self.get_session_timeout().await?;
        let keepalive_interval = sync_interval_seconds - 30;

        if session_timeout == 0 {
            warn!("Couldn't retrieve session timeout correctly. Not starting keepalive interval.");
            return Ok(());
        }

        if session_timeout < sync_interval_seconds {
            info!("{}: Sync interval is greater than PiHole's session timeout. Starting session keepalive interval.", self.config.host);
            debug!("Sync interval: {} seconds", sync_interval_seconds);
            debug!("Session Timeout: {} seconds", session_timeout);
            debug!("Session Keepalive interval: {} seconds", keepalive_interval);
            self.start_session_keepalive(keepalive_interval).await?;
        }

        Ok(())
    }

    pub fn has_config_filters(&self) -> bool {
        self.config.config_excludes.is_some()
    }
}
