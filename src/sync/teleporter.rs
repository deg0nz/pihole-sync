use std::path::Path;

use anyhow::Result;
use tracing::{error, info};

use crate::config::SyncMode;
use crate::pihole::client::PiHoleClient;

pub async fn download_backup(main_pihole: &PiHoleClient, backup_path: &Path) -> Result<()> {
    info!("Downloading backup from main instance...");
    main_pihole.download_backup(backup_path).await
}

pub async fn upload_backup(secondary_pihole: &PiHoleClient, backup_path: &Path) -> Result<()> {
    info!("[{}] Uploading backup", secondary_pihole.config.host);
    secondary_pihole.upload_backup(backup_path).await?;

    if secondary_pihole.config.update_gravity.unwrap_or(false) {
        info!("[{}] Updating gravity", secondary_pihole.config.host);
        secondary_pihole.trigger_gravity_update().await?;
    }

    Ok(())
}

#[allow(dead_code)]
pub async fn sync_teleporter(
    main_pihole: &PiHoleClient,
    secondary_piholes: &[PiHoleClient],
    backup_path: &Path,
) {
    if let Err(e) = download_backup(main_pihole, backup_path).await {
        error!(
            "[{}] Failed to download backup: {:?}",
            main_pihole.config.host, e
        );
        return;
    }

    for secondary_pihole in secondary_piholes {
        if !matches!(
            secondary_pihole.config.sync_mode,
            Some(SyncMode::Teleporter) | None
        ) {
            continue;
        }

        if let Err(e) = upload_backup(secondary_pihole, backup_path).await {
            error!(
                "Failed to upload backup to {}: {:?}",
                secondary_pihole.config.host, e
            );
            continue;
        }
    }
}
