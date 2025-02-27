# pihole-sync

A small utility to sync your PiHole v6 configuration to multiple PiHole instances.

The sync goes one-to-many. One main instance is specified and it's configuration is distributed to all other (secondary) instances.

## Features

- Syncs everything that is contained in PiHole's Teleporter backups
- Aquire PiHole API App passwords
- CLI for modifying and adding PiHole instances
