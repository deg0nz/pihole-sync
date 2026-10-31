# Build stage
FROM rust:latest AS builder

WORKDIR /usr/src/pihole-sync

# Build the local source
COPY . .
RUN cargo build --release

# Runtime stage
FROM debian:trixie-slim

# Install runtime dependencies
RUN apt-get update && \
    apt-get install -y ca-certificates && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

# Create config directory and cache directory
RUN mkdir -p /etc/pihole-sync /var/cache/pihole-sync

WORKDIR /opt/pihole-sync

# Copy the binary from the builder stage
COPY --from=builder /usr/src/pihole-sync/target/release/pihole-sync .

# Add the startup script
COPY docker/entrypoint.sh .
RUN chmod +x entrypoint.sh

# Create volumes for persistent data
VOLUME ["/etc/pihole-sync", "/var/cache/pihole-sync"]

# Set entrypoint to our script
ENTRYPOINT ["/opt/pihole-sync/entrypoint.sh"]

# Default command (will be passed to the entrypoint script)
CMD ["/opt/pihole-sync/pihole-sync", "--config", "/etc/pihole-sync/config.yaml", "sync"]
