use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Read};
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

/// Hashes entry names and contents of a Teleporter zip, ignoring the entry
/// timestamps (Pi-hole sets them to the download time).
pub fn teleporter_content_hash(bytes: &[u8]) -> Result<u64> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    let mut names = archive
        .file_names()
        .map(|name| name.map(|n| n.into_owned()))
        .collect::<Result<Vec<_>, _>>()?;
    names.sort();

    let mut hasher = DefaultHasher::new();
    for name in names {
        // Network/client stats change with every DNS query, and Teleporter import never reads this file.
        if name == "etc/pihole/pihole-FTL.db" {
            continue;
        }
        let mut contents = Vec::new();
        archive.by_name(&name)?.read_to_end(&mut contents)?;
        (name, contents).hash(&mut hasher);
    }
    Ok(hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, DateTime, ZipWriter};

    fn build_zip(entries: &[(&str, &[u8])], minute: u8) -> Vec<u8> {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(DateTime::from_date_and_time(2026, 1, 1, 12, minute, 0).unwrap());
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, contents) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn content_hash_ignores_timestamps_and_volatile_entries() {
        let a = build_zip(
            &[
                ("etc/pihole/pihole.toml", b"config"),
                ("etc/pihole/pihole-FTL.db", b"stats 1"),
            ],
            1,
        );
        let b = build_zip(
            &[
                ("etc/pihole/pihole.toml", b"config"),
                ("etc/pihole/pihole-FTL.db", b"stats 2"),
            ],
            2,
        );
        assert_ne!(a, b);
        assert_eq!(
            teleporter_content_hash(&a).unwrap(),
            teleporter_content_hash(&b).unwrap()
        );
    }

    #[test]
    fn content_hash_detects_changed_contents() {
        let a = build_zip(&[("etc/pihole/pihole.toml", b"config")], 1);
        let b = build_zip(&[("etc/pihole/pihole.toml", b"changed")], 1);
        assert_ne!(
            teleporter_content_hash(&a).unwrap(),
            teleporter_content_hash(&b).unwrap()
        );
    }

    #[test]
    fn content_hash_rejects_non_zip() {
        assert!(teleporter_content_hash(b"not a zip").is_err());
    }
}
