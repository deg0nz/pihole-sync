use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use anyhow::Result;

pub const FILE_WATCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(750);

pub fn hash_config(config: &serde_json::Value) -> Result<u64> {
    let serialized = serde_json::to_string(config)?;
    let mut hasher = DefaultHasher::new();
    serialized.hash(&mut hasher);
    Ok(hasher.finish())
}
