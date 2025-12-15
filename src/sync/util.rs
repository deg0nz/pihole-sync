use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use anyhow::Result;
use tokio::{process::Command, sync::Mutex};
use tracing::warn;

pub const FILE_WATCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(750);

pub fn hash_config(config: &serde_json::Value) -> Result<u64> {
    let serialized = serde_json::to_string(config)?;
    let mut hasher = DefaultHasher::new();
    serialized.hash(&mut hasher);
    Ok(hasher.finish())
}

pub async fn filtered_config_has_changed(
    host_key: &str,
    filtered_hash: u64,
    last_filtered_config_hashes: &Arc<Mutex<HashMap<String, u64>>>,
) -> bool {
    let hashes = last_filtered_config_hashes.lock().await;
    if let Some(previous_hash) = hashes.get(host_key) {
        if *previous_hash == filtered_hash {
            return false;
        }
    }
    true
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
