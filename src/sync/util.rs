use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::Hasher;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;
use tokio::{process::Command, sync::Mutex};
use tracing::warn;

pub const FILE_WATCH_DEBOUNCE: Duration = Duration::from_millis(750);
// Pi-hole doesn't expose rate-limit settings; throttle writes to stay well below typical defaults.
pub const API_WRITE_THROTTLE: Duration = Duration::from_millis(250);

pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(bytes);
    hasher.finish()
}

pub fn hash_value<T: Serialize>(value: &T) -> Result<u64> {
    Ok(hash_bytes(serde_json::to_string(value)?.as_bytes()))
}

#[derive(Clone, Default)]
pub struct HashTracker {
    inner: Arc<Mutex<HashMap<String, u64>>>,
}

impl HashTracker {
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

/// Matches a `pgrep -af` line (`<pid> <cmdline>`) of `pihole -up`/`pihole updatePihole`
/// (the `pihole` script dispatches both to `/opt/pihole/update.sh`).
fn is_update_cmdline(line: &str) -> bool {
    let mut tokens = line.split_whitespace().skip(1).peekable();
    while let Some(tok) = tokens.next() {
        if tok.ends_with("/pihole/update.sh") {
            return true;
        }
        if tok.rsplit('/').next() == Some("pihole")
            && matches!(tokens.peek(), Some(&"-up" | &"updatePihole"))
        {
            return true;
        }
    }
    false
}

pub async fn is_pihole_update_running() -> Result<bool> {
    let output = Command::new("pgrep").args(["-af", "pihole"]).output().await;

    match output {
        Ok(out) => {
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                return Ok(stdout.lines().any(is_update_cmdline));
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

#[cfg(test)]
mod tests {
    use super::is_update_cmdline;

    #[test]
    fn detects_only_pihole_update_cmdlines() {
        for (line, want) in [
            ("12 pihole -up", true),
            ("12 /usr/local/bin/pihole -up", true),
            ("12 pihole updatePihole --check-only", true),
            ("12 bash /opt/pihole/update.sh", true),
            ("12 pihole-updatelists", false),
            ("12 pihole-sync --config /x/pihole-up.yaml sync", false),
            ("12 pihole -g", false),
        ] {
            assert_eq!(is_update_cmdline(line), want, "{line}");
        }
    }
}
