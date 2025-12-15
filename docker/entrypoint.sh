#!/bin/bash
set -e

# Create config directory if it doesn't exist
mkdir -p /etc/pihole-sync

# Generate config.yaml from environment variables
cat > /etc/pihole-sync/config.yaml << EOF
sync:
  trigger_mode: ${SYNC_TRIGGER_MODE:-interval}
  interval: ${SYNC_INTERVAL:-60}
  config_path: ${SYNC_CONFIG_PATH:-/etc/pihole/pihole.toml}
  $( [ -n "${SYNC_API_POLL_INTERVAL}" ] && echo "api_poll_interval: ${SYNC_API_POLL_INTERVAL}" )
  cache_location: /var/cache/pihole-sync

main:
  host: ${MAIN_HOST:-main-pihole.example.com}
  schema: ${MAIN_SCHEMA:-https}
  port: ${MAIN_PORT:-443}
  api_key: ${MAIN_API_KEY:-YOUR_MAIN_PIHOLE_APP_PASSWORD}

secondary:
EOF

# Add secondary Pi-holes from environment variables
i=1
while true; do
  HOST_VAR="SECONDARY_HOST_$i"
  SCHEMA_VAR="SECONDARY_SCHEMA_$i"
  PORT_VAR="SECONDARY_PORT_$i"
  API_KEY_VAR="SECONDARY_API_KEY_$i"
  UPDATE_GRAVITY_VAR="SECONDARY_UPDATE_GRAVITY_$i"
  SYNC_MODE_VAR="SECONDARY_SYNC_MODE_$i"
  CONFIG_MODE_VAR="SECONDARY_CONFIG_MODE_$i"
  FILTER_KEYS_VAR="SECONDARY_FILTER_KEYS_$i"

  # Check if this secondary Pi-hole is defined
  if [ -z "${!HOST_VAR}" ]; then
    break
  fi

  # Add this secondary Pi-hole to the config
  cat >> /etc/pihole-sync/config.yaml << EOF
  - host: ${!HOST_VAR}
    schema: ${!SCHEMA_VAR:-https}
    port: ${!PORT_VAR:-443}
    api_key: ${!API_KEY_VAR}
    update_gravity: ${!UPDATE_GRAVITY_VAR:-true}
    sync_mode: ${!SYNC_MODE_VAR:-teleporter}
EOF

  # If config_api mode is requested, add filter configuration
  if [ "${!SYNC_MODE_VAR}" = "config_api" ]; then
    CONFIG_MODE_VALUE=${!CONFIG_MODE_VAR:-include}
    FILTER_KEYS_VALUE=${!FILTER_KEYS_VAR}

    cat >> /etc/pihole-sync/config.yaml << EOF
    config_api_sync_options:
      mode: ${CONFIG_MODE_VALUE}
      filter_keys:
EOF

    if [ -n "${FILTER_KEYS_VALUE}" ]; then
      IFS=',' read -ra FILTER_KEYS <<< "${FILTER_KEYS_VALUE}"
      for KEY in "${FILTER_KEYS[@]}"; do
        TRIMMED_KEY="$(echo "$KEY" | xargs)"
        if [ -n "$TRIMMED_KEY" ]; then
          echo "        - ${TRIMMED_KEY}" >> /etc/pihole-sync/config.yaml
        fi
      done
    else
      echo "        []" >> /etc/pihole-sync/config.yaml
    fi
  fi

  i=$((i+1))
done

# If no secondaries were added, ensure the YAML is valid with an empty list
if [ "$i" -eq 1 ]; then
  echo "  []" >> /etc/pihole-sync/config.yaml
fi

# Execute the original command
exec "$@"
