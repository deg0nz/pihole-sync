# `pihole-sync` (for Pi-hole v6)

A (currently) quick and dirty utility to sync your *Pi-hole v6* configuration to multiple Pi-hole instances.
The sync goes one-to-many. One main instance is specified and it's configuration is distributed to all other (secondary) instances.

> Warning: Config and API are considered *unstable* until v1.0.0 and may change at any time

## Features

- Syncs everything contained in Pi-hole's Teleporter backups
- Syncs selected config keys via Pi-hole's `/config` API (filtered)
- Acquire app passwords for Pi-hole API
- Modify and add Pi-hole instances via CLI

## Installation

I'm trying to provide pre-compiled binaries in the near future.
But for the time being, installation is only available via `git clone` and `cargo build`, so to install, run the following commands:

```bash
git clone https://github.com/deg0nz/pihole-sync.git
cd pihole-sync
cargo build --release
```

> Note: On Debian, you may need to install `libssl-dev` and `pkg-config` before building!

## How to Use

> Note: Config file schema changed to YAML. (TOML is currentlystill supported, but will be dropped in 1.0)

The default config location is `/etc/pihole-sync/config.yaml`, you need to specify `--config /path/to/config.toml` via CLI, if you don't use the default path.

- Configure your main and secondary instances in the configuration file. (Please [refer to example config](./config/example.config.yaml))
  - Leave the password free for now. You can generate one via the CLI command `pihole-sync app-password` (add `--config /path/to/config.toml` if you don't use the default path ;))
  - Add the printed **password hash** to your respective Pi-hole instance under Settings > Webserver and API > webserver.api.app_pwhash  (Refer to Pi-hole API documentation for more information: https://ftl.pi-hole.net/master/docs/#get-/auth/app)
  - Add the printed **app password** to the config.toml
- Per secondary, choose a sync mechanism using `sync_mode`:
  - `teleporter` uses `/teleporter`
  - `config_api` uses `/config` and requires `config_api_sync_options` filters
- Run `pihole-sync sync` for running in sync mode
  - You can also run `pihole-sync sync --once` to run the sync once and exit.


# Disclaimer

This project is not associated with the official Pi-hole project.
Pi-hole is a registered trademark of Pi-hole LLC.
