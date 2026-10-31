# Changelog

## 1.0.0-beta.8

### Breaking
See [MIGRATION.md](./MIGRATION.md).
- Removed deprecated config names (`config_sync`, `config_api_sync_options`, `teleporter_options`, `import_options`, `sync_mode: config_api`, `api_sync_options.config`); unknown fields are rejected, removed ones with a message naming the replacement.
- TLS certificates are verified by default; self-signed setups need `accept_invalid_certs: true`.
- API sync deletes groups and lists that exist only on a secondary.
- `sync.interval` and `sync.api_poll_interval` must be at least 1.

### Fixed
- `watch_config_api` syncs only when main config, groups, lists, domains or clients change; skipped syncs are retried on the next poll; idle polls log out the main session.
- Config sync never overwrites the secondary's API credentials, `app_sudo` or `totp_secret`; main's password hashes are never synced.
- Group and list names/addresses are percent-encoded in API paths.
- Sync hash keys and instance removal include the port.
- Group/list writes reported in `processed.errors` fail and are retried; list sync is skipped when group sync failed.
- Teleporter backups are compared by content; waits for gravity.db and pending FTL restarts after imports.
- Pi-hole update processes are matched precisely; re-authentication happens only on 401.

### Added
- `accept_invalid_certs` option and `--accept-invalid-certs` / `instances remove --port` CLI flags.
- `watch_config_file` also watches `gravity.db`.
- Log line for each group and list removed from a secondary.
- Docker env vars `SECONDARY_SYNC_GROUPS_<i>`, `SECONDARY_SYNC_LISTS_<i>`, `*_ACCEPT_INVALID_CERTS*`.
- Config validation tests and end-to-end tests.

### Changed
- Docker image builds from local source on a debian trixie runtime.
- Dead and duplicate code removed.
