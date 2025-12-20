use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;
use tokio::{process::Command, sync::Mutex};
use tracing::warn;

pub const FILE_WATCH_DEBOUNCE: Duration = Duration::from_millis(750);
// Pi-hole doesn't expose rate-limit settings; throttle writes to stay well below typical defaults.
pub const API_WRITE_THROTTLE: Duration = Duration::from_millis(250);

pub fn hash_config(config: &serde_json::Value) -> Result<u64> {
    hash_value(config)
}

pub fn hash_value<T: Serialize>(value: &T) -> Result<u64> {
    let serialized = serde_json::to_string(value)?;
    let mut hasher = DefaultHasher::new();
    serialized.hash(&mut hasher);
    Ok(hasher.finish())
}

#[derive(Clone, Default)]
pub struct HashTracker {
    inner: Arc<Mutex<HashMap<String, u64>>>,
}

impl HashTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true if the given hash differs from the last stored hash for the key.
    pub async fn has_changed(&self, key: &str, current_hash: u64) -> bool {
        let hashes = self.inner.lock().await;
        hashes
            .get(key)
            .is_none_or(|previous| *previous != current_hash)
    }

    pub async fn update(&self, key: &str, hash: u64) {
        let mut hashes = self.inner.lock().await;
        hashes.insert(key.to_string(), hash);
    }
}

pub async fn is_pihole_update_running() -> Result<bool> {
    let output = Command::new("pgrep")
        .args(["-af", "pihole.*-up"])
        .output()
        .await;

    match output {
        Ok(out) => {
            if out.status.success() {
                let stdout_has_content = !out.stdout.is_empty();
                return Ok(stdout_has_content);
            }

            // pgrep exits with 1 when no processes were matched; that's not an error for us.
            if let Some(1) = out.status.code() {
                return Ok(false);
            }

            warn!(
                "pgrep returned non-zero status ({}). stdout: {:?}, stderr: {:?}",
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            Ok(false)
        }
        Err(e) => {
            warn!("Failed to run pgrep to detect \"pihole -up\": {}", e);
            Ok(false)
        }
    }
}
