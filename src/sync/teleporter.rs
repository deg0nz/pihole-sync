use std::path::Path;

use tracing::{error, info};

use crate::config::SyncMode;
use crate::pihole::client::PiHoleClient;

pub async fn sync_teleporter(
    main_pihole: &PiHoleClient,
    secondary_piholes: &[PiHoleClient],
    backup_path: &Path,
) {
    info!("Downloading backup from main instance...");
    if let Err(e) = main_pihole.download_backup(backup_path).await {
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

        info!("[{}] Uploading backup", secondary_pihole.config.host);
        if let Err(e) = secondary_pihole.upload_backup(backup_path).await {
            error!(
                "Failed to upload backup to {}: {:?}",
                secondary_pihole.config.host, e
            );
            continue;
        }

        if secondary_pihole.config.update_gravity.unwrap_or(false) {
            info!("[{}] Updating gravity", secondary_pihole.config.host);
            if let Err(e) = secondary_pihole.trigger_gravity_update().await {
                error!(
                    "Failed to update gravity on {}: {:?}",
                    secondary_pihole.config.host, e
                );
            }
        }
    }
}
