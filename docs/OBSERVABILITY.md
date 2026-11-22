---
title: Observability
description: Opt-in OpenTelemetry tracing for the Aura server and clients, and what it does and doesn't export.
---
# Observability

The Aura server and both clients can export **OpenTelemetry traces** over
OTLP/HTTP (protobuf) to a backend you choose, such as Grafana Cloud (Tempo),
Honeycomb, or your own OpenTelemetry Collector. It is **off by default**:
nothing is exported unless you configure an endpoint.

## What is exported, and what isn't

Aura is a privacy tool, so tracing is deliberately narrow. The rules live in
`crates/aura-telemetry`, which both the server and the client core use:

- **Only Aura's own spans.** Spans whose target starts with `aura::` are
  exported. Spans from libraries (QUIC, TLS, OpenMLS, HTTP) are dropped.
- **No log lines.** Log *events* never reach the exporter, even inside an
  exported span. Logs can name users, channels and addresses; spans don't.
- **No identities or content.** Span attributes are limited to an operation
  name, a message type and an outcome code. No user id, channel id, IP
  address, display name, key, session token or message content. Tests check
  this for real logins on the server and for client spans.
- **No audio path.** The voice datagram path has no spans; per-packet timing
  would show who is speaking.

| Where | Span | Attributes |
|---|---|---|
| Server | `auth` (one per login attempt) | `outcome`: `ok`, `invalid_signature`, `username_taken`, `invalid_password`, `challenge_mismatch`, `protocol_error`, … |
| Server | `control_message` (one per control message) | `msg_type` (for example `mls_commit_welcome`, `text_packet`), `outcome` |
| Clients | `connect`, `login`, `join_channel` | `outcome`: `ok`, `error`, `cancelled` |
| Clients (core) | `mls.generate_key_package`, `mls.create_group`, `mls.process_welcome`, `mls.add_member`, `mls.process_commit`, `mls.self_update`, `verification.pairwise_fingerprint` | `outcome` |

Every span is its own trace (a root span), so a long-lived connection doesn't
become one huge trace.

**What traces still reveal.** Timing and volume: when logins happen, how many
control messages of each type the server handles and when, how long MLS
operations take. That is operational metadata, without who or where. If you
export to a third party, consider sampling, for example
`OTEL_TRACES_SAMPLER=traceidratio` and `OTEL_TRACES_SAMPLER_ARG=0.1`.

## Server

Set the standard OpenTelemetry environment variables; the server starts
exporting when `OTEL_EXPORTER_OTLP_ENDPOINT` (or
`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`) is set:

| Variable | Example |
|---|---|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `https://otlp.example.com` (`/v1/traces` is appended) |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | Full traces URL, used as is |
| `OTEL_EXPORTER_OTLP_HEADERS` | `Authorization=Basic <token>` (comma-separated `key=value` pairs) |
| `OTEL_TRACES_SAMPLER`, `OTEL_TRACES_SAMPLER_ARG` | `traceidratio`, `0.1` |
| `OTEL_SDK_DISABLED` | `true` turns it off even when an endpoint is set |

Spans are reported as service `aura-server`. Export runs on a background
thread over TLS with the operating system's trust store; it never blocks
request handling.

**On Fly.io.** Fly has no managed tracing backend, so point the server at
your own. Keep the endpoint and credentials out of `fly.toml` (the repository
is public) and set them as secrets:

```sh
fly secrets set -a aura-server \
  OTEL_EXPORTER_OTLP_ENDPOINT=https://otlp.example.com \
  OTEL_EXPORTER_OTLP_HEADERS="Authorization=Basic <token>"
```

## Clients

The macOS and desktop apps start exporting at launch when
`OTEL_EXPORTER_OTLP_ENDPOINT` is set in their environment, as service
`aura-macos` or `aura-desktop`, and flush on quit.

- **Desktop:** set the variables in the shell that launches the app.
- **macOS:** apps opened from Finder don't inherit shell variables. Launch the
  binary from a terminal
  (`OTEL_EXPORTER_OTLP_ENDPOINT=… /Applications/Aura.app/Contents/MacOS/Aura`),
  or set them in the Xcode scheme while developing.

Client tracing goes through aura-core (`telemetry_start_from_env`,
`telemetry_start` and `TelemetrySpan` over FFI), so both apps share one
implementation. `TelemetrySpan` only accepts a fixed list of operations and
outcomes, so an app can't attach free-form strings. `telemetry_start` takes an
explicit endpoint and headers, for a future in-app setting.
