//! Application constants and default values

/// Default configuration file path for pihole-sync
pub const DEFAULT_CONFIG_PATH: &str = "/etc/pihole-sync/config.yaml";

/// Default Pi-hole configuration file path (for file watching)
pub const DEFAULT_PIHOLE_CONFIG_PATH: &str = "/etc/pihole/pihole.toml";

/// Default systemd service installation path
pub const DEFAULT_SYSTEMD_PATH: &str = "/etc/systemd/system/pihole-sync.service";

/// Default sync interval in minutes
pub const DEFAULT_SYNC_INTERVAL_MINUTES: u64 = 60;
