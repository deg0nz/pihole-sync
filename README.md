# `pihole-sync` (for Pi-hole v6)

A (currently) quick and dirty utility to sync your *Pi-hole v6* configuration to multiple Pi-hole instances.
The sync goes one-to-many. One main instance is specified and it's configuration is distributed to all other (secondary) instances.

## Features

- Syncs everything contained in Pi-hole's Teleporter backups
- Syncs selected config keys via Pi-hole's `/config` API (filtered)
- Supports interval-based syncs, watching the Pi-hole config file, or polling `/api/config` for changes
- Acquire app passwords for Pi-hole API
- Modify and add Pi-hole instances via CLI
- Scaffold helper files with `pihole-sync setup default-config` (writes `./config.yaml`) and `pihole-sync setup systemd` (dialog to generate/install a systemd unit)

## Installation

### Pre-built binaries (recommended)

Grab the latest release from [GitHub releases](https://github.com/deg0nz/pihole-sync/releases). Example for Linux (adjust the archive name for your OS/arch):

```bash
arch="$(uname -m)"
curl -L "https://github.com/deg0nz/pihole-sync/releases/latest/download/pihole-sync-${arch}-unknown-linux-gnu.tar.gz" -o pihole-sync.tar.gz
tar -xzf pihole-sync.tar.gz
sudo mv pihole-sync /usr/local/bin/
```

Then scaffold helper files (optional):

```bash
pihole-sync setup default-config   # writes ./config.yaml (asks before overwrite)
pihole-sync setup systemd          # dialog to create/install systemd unit
```

### Build from source

```bash
git clone https://github.com/deg0nz/pihole-sync.git
cd pihole-sync
cargo build --release
```

> Note: On Debian, you may need to install `libssl-dev` and `pkg-config` before building!

## How to Use

> Note: Config file schema is YAML. TOML configs are no longer supported.

The default config location is `/etc/pihole-sync/config.yaml`; use `--config /path/to/config.yaml` via CLI if you don't use the default path.

- Quick-start helpers:
  - `pihole-sync setup default-config` creates `./config.yaml` from the bundled example (asks before overwriting).
  - `pihole-sync setup systemd` walks you through executable/config paths, writes the unit (default `/etc/systemd/system/pihole-sync.service`), and prints the `systemctl daemon-reload/start/enable` steps.

- Configure your main and secondary instances in the YAML configuration file. (Please [refer to example config](./config/example.config.yaml))
  - Leave the password free for now. You can generate one via the CLI command `pihole-sync app-password` (add `--config /path/to/config.yaml` if you don't use the default path ;))
  - Add the printed **password hash** to your respective Pi-hole instance under Settings > Webserver and API > webserver.api.app_pwhash  (Refer to Pi-hole API documentation for more information: https://ftl.pi-hole.net/master/docs/#get-/auth/app)
  - Add the printed **app password** to your config file
- Per secondary, choose a sync mechanism using `sync_mode`:
  - `teleporter` uses `/teleporter`
  - `api` uses Pi-hole API endpoints (`/config`, `/groups`, `/lists`). Configure via `api_sync_options`:
    - `sync_config` (optional): filters for `/config`
      - `mode: include` (default) — only listed `filter_keys` are synced; empty list syncs nothing.
      - `mode: exclude` — everything is synced except the listed `filter_keys`; empty list syncs everything.
    - `sync_groups` (bool, optional): sync group definitions
    - `sync_lists` (bool, optional): sync allow/block lists; if `sync_groups` is false/omitted, lists are assigned to group 0 and a warning is logged when the main Pi-hole uses other groups.
- Choose how syncs are triggered in `sync.trigger_mode`:
  - `interval` (default): run every `sync.interval` minutes
  - `watch_config_file`: watch `/etc/pihole/pihole.toml` (override via `sync.config_path`)
  - `watch_config_api`: poll `/api/config` every `sync.api_poll_interval` minutes (falls back to `sync.interval`)
- For watch-based triggers, set `sync.trigger_api_readiness_timeout_secs` (default `60`) to control how long to wait for Pi-hole's API to come back after a config change before running the sync.
- Run `pihole-sync sync` for running in sync mode (sessions are logged out after each run to avoid occupying Pi-hole session slots)
  - Use `pihole-sync sync --once` to run the sync once and exit.
  - Use `pihole-sync sync --no-initial-sync` to start watchers without an initial sync.


# Disclaimer

This project is not associated with the official Pi-hole project.
Pi-hole is a registered trademark of Pi-hole LLC.
