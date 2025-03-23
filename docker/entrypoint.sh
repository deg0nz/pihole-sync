#!/bin/bash
set -e

# Create config directory if it doesn't exist
mkdir -p /etc/pihole-sync

# Generate config.yaml from environment variables
cat > /etc/pihole-sync/config.yaml << EOF
sync:
  interval: ${SYNC_INTERVAL:-60}
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
EOF

  i=$((i+1))
done

# If no secondaries were added, ensure the YAML is valid with an empty list
if [ "$i" -eq 1 ]; then
  echo "  []" >> /etc/pihole-sync/config.yaml
fi

# Execute the original command
exec "$@"
