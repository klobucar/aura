# Build stage
FROM cgr.dev/chainguard/rust:latest-dev AS builder

USER root
# Create app directory and set permissions before switching user
RUN mkdir -p /app && chown -R 1000:1000 /app
WORKDIR /app

# Install dependencies including protoc
# Wolfi-based images use apk
RUN apk update && apk add protobuf-dev

# Switch to non-root user for the build
USER 1000
# Ensure cargo home is in a writable location inside /app
ENV CARGO_HOME=/app/.cargo

# Copy the workspace configuration and lock file
# We use --chown to ensure the copied files are owned by the build user
COPY --chown=1000:1000 Cargo.toml Cargo.lock ./

# Copy all crates to handle dependencies
COPY --chown=1000:1000 crates/ ./crates/

# Build the aura-server
# We specifically build the server crate
RUN cargo build --release -p aura-server

# Runtime stage
# We use glibc-dynamic to support standard Rust linking and SQLite bundled behaviors
FROM cgr.dev/chainguard/glibc-dynamic:latest

WORKDIR /app

# Switch to non-root user for the runtime
# Fly.io uses this to determine volume ownership at runtime
USER 1000

# Copy the binary from the builder
COPY --from=builder --chown=1000:1000 /app/target/release/aura-server /app/aura-server

# Expose QUIC (UDP) and ACME (TCP) ports
EXPOSE 8443/udp
EXPOSE 443/tcp

# Run the server
# Static binary execution without a shell
ENTRYPOINT ["/app/aura-server"]
CMD ["--bind", "0.0.0.0:8443"]
