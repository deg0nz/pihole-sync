use std::path::Path;

use anyhow::Result;
use tracing::info;

use crate::pihole::client::PiHoleClient;

pub async fn upload_backup(secondary_pihole: &PiHoleClient, backup_path: &Path) -> Result<()> {
    info!("[{}] Uploading backup", secondary_pihole.config.host);
    secondary_pihole.upload_backup(backup_path).await?;

    if secondary_pihole.config.update_gravity.unwrap_or(false) {
        info!("[{}] Updating gravity", secondary_pihole.config.host);
        secondary_pihole.trigger_gravity_update().await?;
    }

    Ok(())
}
