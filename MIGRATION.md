# Upgrading to 1.0.0-beta.7

For upgrades from 1.0.0-beta.6 and earlier. Old configs are rejected on load with a message naming the replacement.

## Config renames

| Old | New |
|---|---|
| `config_sync`, `config_api_sync_options` | `api_sync_options.sync_config` (with `sync_mode: api`) |
| `teleporter_options`, `import_options` | `teleporter_sync_options` |
| `sync_mode: config_api` | `sync_mode: api` |
| `api_sync_options.config` | `api_sync_options.sync_config` |

API secondary:

```yaml
# before
- host: s1
  sync_mode: config_api
  config_api_sync_options:
    mode: include
    filter_keys: ["dns.hosts"]

# after
- host: s1
  sync_mode: api
  api_sync_options:
    sync_config:
      mode: include
      filter_keys: ["dns.hosts"]
```

Teleporter secondary:

```yaml
# before
- host: s2
  sync_mode: teleporter
  import_options:
    config: true

# after
- host: s2
  sync_mode: teleporter
  teleporter_sync_options:
    config: true
```

## New and changed options

- `accept_invalid_certs` (per instance, default `false`): TLS certificates are now verified. Self-signed setups must set `accept_invalid_certs: true` (CLI: `instances add --accept-invalid-certs`).
- `instances remove --port <port>` removes only the secondary with that port (useful for several instances on one host).
- `sync.interval` and `sync.api_poll_interval` must be at least 1.
- Unknown config fields are rejected instead of ignored.

## Behaviour changes

- **API sync now mirrors deletions: groups and lists that exist only on a secondary are DELETED on the first run (group 0 is kept). Back up/export secondary-only lists first, or add them to the main.** This applies when `sync_groups` / `sync_lists` is enabled.
- Config sync never overwrites the secondary's `webserver.api` credentials (`password`, `pwhash`, `app_pwhash`, `app_sudo`, `totp_secret`).
- With an app password, set `webserver.api.app_sudo = true` on each secondary, otherwise Pi-hole refuses config changes.
- `watch_config_file` also reacts to changes of `gravity.db` (groups, lists, domains).
- `watch_config_api` now detects config, group, list, domain and client changes and syncs only when something changed.
- Teleporter backups are only re-imported when their contents changed.
- Docker image:
  - built from the local source (build context is the repo root) on a debian trixie runtime;
  - new env vars `SECONDARY_SYNC_GROUPS_<i>`, `SECONDARY_SYNC_LISTS_<i>`, `MAIN_ACCEPT_INVALID_CERTS` and `SECONDARY_ACCEPT_INVALID_CERTS_<i>`;
  - `SECONDARY_SYNC_MODE_<i>=config_api` is still accepted and mapped to `api`; use `api`.
