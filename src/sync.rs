mod config_sync;
mod context;
mod groups;
mod lists;
mod teleporter;
pub(crate) mod triggers;
pub(crate) mod util;

pub use context::SyncContext;
pub use teleporter::teleporter_content_hash;

use anyhow::Result;

use crate::config::Config;

/// Main entry point for sync operations.
pub async fn run_sync(config_path: &str, run_once: bool, disable_initial_sync: bool) -> Result<()> {
    let config = Config::load(config_path)?;
    let context = SyncContext::from_config(&config)?;
    context.run(run_once, disable_initial_sync).await
}

#[cfg(test)]
mod tests;
