# Build stage
FROM cgr.dev/chainguard/rust:latest AS builder

WORKDIR /app

# Install dependencies including protoc
# Wolfi-based images use apk
USER root
RUN apk update && apk add protobuf-dev
USER 1000

# Copy the workspace configuration and lock file
COPY Cargo.toml Cargo.lock ./

# Copy all crates to handle dependencies
COPY crates/ ./crates/

# Build the aura-server
# We specifically build the server crate
RUN cargo build --release -p aura-server

# Runtime stage
# We use glibc-dynamic to support standard Rust linking and SQLite bundled behaviors
FROM cgr.dev/chainguard/glibc-dynamic:latest

WORKDIR /app

# Copy the binary from the builder
COPY --from=builder /app/target/release/aura-server /app/aura-server

# Create data directory for persistence (Fly.io will mount a volume here)
# Chainguard images are non-root, so we need to ensure the app has permissions
# but we can rely on fly.io volume permissions or local data folder.
USER root
RUN mkdir -p /app/data && chown -R 1000:1000 /app/data
USER 1000

# Expose QUIC (UDP) and ACME (TCP) ports
EXPOSE 8443/udp
EXPOSE 443/tcp

# Run the server
# We point the database to the persistent data volume
ENTRYPOINT ["/app/aura-server"]
CMD ["--bind", "0.0.0.0:8443"]
