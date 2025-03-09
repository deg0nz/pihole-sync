use anyhow::{Context, Result};
use reqwest::{
    multipart::{Form, Part},
    Client, ClientBuilder, Response, StatusCode,
};
use serde::Deserialize;
use std::{path::Path, sync::Arc};
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::config::{Instance, SyncImportOptions};

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

#[derive(Debug)]
pub struct PiHoleClient {
    client: Client,
    base_url: String,
    api_key: String,
    session_token: Arc<Mutex<Option<String>>>,
    import_options: Option<SyncImportOptions>,
}

const X_FTL_SID_HEADER: &str = "sid";

impl PiHoleClient {
    pub fn new(
        schema: &str,
        host: &str,
        port: u16,
        api_key: &str,
        import_options: Option<SyncImportOptions>,
    ) -> Self {
        let base_url = format!("{}://{}:{}/api", schema, host, port);
        Self {
            client: ClientBuilder::new()
                .danger_accept_invalid_certs(true)
                .build()
                .unwrap(),
            base_url,
            api_key: api_key.to_string(),
            session_token: Arc::new(Mutex::new(None)),
            import_options,
        }
    }

    pub fn from_instance(instance: Instance) -> Self {
        let base_url = format!(
            "{}://{}:{}/api",
            instance.schema, instance.host, instance.port
        );
        Self {
            client: ClientBuilder::new()
                .danger_accept_invalid_certs(true)
                .build()
                .unwrap(),
            base_url,
            api_key: instance.api_key.to_string(),
            session_token: Arc::new(Mutex::new(None)),
            import_options: instance.import_options,
        }
    }

    /// **Authenticate and get session token**
    async fn authenticate(&self, password: Option<String>) -> Result<()> {
        debug!("Authenticating");
        let auth_url = format!("{}/auth", self.base_url);
        let body = serde_json::json!({ "password": if let Some(pw) = password { pw } else { self.api_key.clone() } });

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

        if let Some(import_options) = self.import_options.clone() {
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
}
