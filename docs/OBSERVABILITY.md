---
title: Observability
description: Prometheus metrics and opt-in OpenTelemetry tracing for the Aura server and clients, and what they do and don't expose.
---
# Observability

Two independent, opt-in signals:

- **Prometheus metrics** from the server: aggregate counts, scraped from a
  `/metrics` endpoint. On Fly.io they show up in Fly's managed Grafana.
- **OpenTelemetry traces** from the server and both clients, exported over
  OTLP/HTTP (protobuf) to a backend you choose, such as Grafana Cloud (Tempo),
  Honeycomb, or your own OpenTelemetry Collector.

Both are **off by default**: nothing is served or exported until you
configure it.

## Metrics

The server serves Prometheus metrics at `GET /metrics` on `AURA_METRICS_ADDR`
(for example `[::]:9091`, which accepts IPv4 and IPv6; off when unset). On
Fly.io, `fly.toml` sets it and a `[metrics]` section, so Fly's managed
Prometheus scrapes it and the numbers show up in Fly's managed Grafana at
[fly-metrics.net](https://fly-metrics.net). The port isn't in `[[services]]`,
so only Fly's scraper can reach it.

Everything is an **aggregate count**. No label names a user, channel,
session, address or key, and every label has a fixed set of values (anything
unexpected is counted as `other`).

| Metric | Type | Labels |
|---|---|---|
| `aura_connections_total` | counter | — (QUIC connections past the TLS handshake) |
| `aura_connections_active` | gauge | — |
| `aura_connections_rejected_total` | counter | `reason`: `rate_limited`, `handshake_failed` |
| `aura_logins_total` | counter | `outcome`: `ok`, `invalid_password`, `invalid_signature`, `username_taken`, `challenge_mismatch`, `protocol_error`, … |
| `aura_control_messages_total` | counter | `type` (for example `mls_commit_welcome`, `text_packet`), `outcome`: `ok`, `error` |
| `aura_audio_datagrams_total`, `aura_audio_bytes_total` | counter | — (encrypted audio received for relay) |
| `aura_text_replays_rejected_total` | counter | — |
| `aura_sessions_active`, `aura_channels`, `aura_seen_message_ids` | gauge | — |
| `aura_build_info` | gauge | `version` |

Example queries: `sum by (outcome) (rate(aura_logins_total[5m]))` for login
outcomes, and `rate(aura_audio_bytes_total[1m]) * 8` for relayed audio in
bits per second.

## Traces

### What traces export, and what they don't

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

### Server

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

**On Fly.io.** Fly's managed Grafana only has Prometheus metrics, not
traces, so point the server at
your own. Keep the endpoint and credentials out of `fly.toml` (the repository
is public) and set them as secrets:

```sh
fly secrets set -a aura-server \
  OTEL_EXPORTER_OTLP_ENDPOINT=https://otlp.example.com \
  OTEL_EXPORTER_OTLP_HEADERS="Authorization=Basic <token>"
```

### Clients

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
