# `pihole-sync` (for Pi-hole v6)

A (currently) quick and dirty utility to sync your *Pi-hole v6* configuration to multiple Pi-hole instances.

The sync goes one-to-many. One main instance is specified and it's configuration is distributed to all other (secondary) instances.

> **1.0.0-beta announcement (main branch)**
> - Per-secondary sync mode choice: Teleporter or Config API (with include/exclude filters).
> - New sync triggers: interval, watch Pi-hole config file, or watch `/api/config` with polling.
> - Watch-mode guard: skip sync while `pihole -up` is running.
> - Config is YAML-only; TOML configs are no longer supported.
>
> See the 1.0.0-beta.1 release: https://github.com/deg0nz/pihole-sync/releases/tag/1.0.0-beta.1
> For fuller docs, check the README on the development branch.

## Features

- Syncs everything contained in Pi-hole's Teleporter backups
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

- Configure your main and secondary instances in the configuration file. (Please [refer to example config](./config-examples/config.example.yaml))
  - Leave the password free for now. You can generate one via the CLI command `pihole-sync app-password` (add `--config /path/to/config.toml` if you don't use the default path ;))
  - Add the printed **password hash** to your respective Pi-hole instance under Settings > Webserver and API > webserver.api.app_pwhash  (Refer to Pi-hole API documentation for more information: https://ftl.pi-hole.net/master/docs/#get-/auth/app)
  - Add the printed **app password** to the config.toml
- Run `pihole-sync sync` for running in sync mode
  - You can also run `pihole-sync sync --once` to run the sync once and exit.


# Disclaimer

This project is not associated with the official Pi-hole project.
Pi-hole is a registered trademark of Pi-hole LLC.
