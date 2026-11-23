//! Prometheus metrics for operators, in the text exposition format. On Fly.io
//! the managed Prometheus scrapes them and fly-metrics.net graphs them.
//!
//! Aggregate counts only. No label names a user, channel, session, address
//! or key, and every label takes one of a fixed list of values (anything
//! unexpected is counted as `other`), so cardinality is bounded too.
//!
//! Served only when `AURA_METRICS_ADDR` is set. On Fly that port is
//! deliberately not in `[[services]]`, so only Fly's scraper reaches it.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use crate::state::ServerState;

/// Login outcomes: `AuthError::code()` values plus the connection-level ones.
const LOGIN_OUTCOMES: &[&str] = &[
    "ok",
    "invalid_password",
    "username_taken",
    "invalid_username",
    "invalid_public_key",
    "identity_key_mismatch",
    "invalid_signature",
    "banned",
    "verification_required",
    "invalid_session",
    "permission_denied",
    "user_not_found",
    "internal_error",
    "challenge_mismatch",
    "protocol_error",
    "other",
];

/// Control-message types, as named by `connection::message_type_name`.
const MESSAGE_TYPES: &[&str] = &[
    "join_channel",
    "text_packet",
    "create_channel",
    "update_channel",
    "update_profile",
    "delete_channel",
    "delete_user",
    "update_status",
    "list_users",
    "verify_user",
    "ban_user",
    "grant_admin",
    "kick_user",
    "mls_join",
    "mls_commit_welcome",
    "mls_ratchet_commit",
    "other",
];

/// Why a connection was dropped before it got going.
const REJECT_REASONS: &[&str] = &["rate_limited", "handshake_failed", "other"];

const MAX_LABELS: usize = 24;

/// A counter split by one label whose values come from a fixed list.
struct LabeledCounter {
    values: &'static [&'static str],
    counts: [AtomicU64; MAX_LABELS],
}

impl LabeledCounter {
    const fn new(values: &'static [&'static str]) -> Self {
        assert!(values.len() <= MAX_LABELS);
        Self {
            values,
            counts: [const { AtomicU64::new(0) }; MAX_LABELS],
        }
    }

    /// Count `value`, or `other` if it isn't one of the expected values.
    fn inc(&self, value: &str) {
        let i = self
            .values
            .iter()
            .position(|v| *v == value)
            .or_else(|| self.values.iter().position(|v| *v == "other"))
            .expect("every label list ends with other");
        self.counts[i].fetch_add(1, Ordering::Relaxed);
    }

    fn get(&self, value: &str) -> u64 {
        self.values
            .iter()
            .position(|v| *v == value)
            .map_or(0, |i| self.counts[i].load(Ordering::Relaxed))
    }

    fn render(&self, out: &mut String, name: &str, label: &str, extra: &str) {
        for (i, value) in self.values.iter().enumerate() {
            let _ = writeln!(
                out,
                "{name}{{{label}=\"{value}\"{extra}}} {}",
                self.counts[i].load(Ordering::Relaxed)
            );
        }
    }
}

/// Process-wide counters. Updated from the connection and state code.
pub struct Metrics {
    connections_total: AtomicU64,
    connections_active: AtomicI64,
    connections_rejected: LabeledCounter,
    logins: LabeledCounter,
    control_ok: LabeledCounter,
    control_error: LabeledCounter,
    audio_datagrams: AtomicU64,
    audio_bytes: AtomicU64,
    text_replays_rejected: AtomicU64,
}

pub static METRICS: Metrics = Metrics {
    connections_total: AtomicU64::new(0),
    connections_active: AtomicI64::new(0),
    connections_rejected: LabeledCounter::new(REJECT_REASONS),
    logins: LabeledCounter::new(LOGIN_OUTCOMES),
    control_ok: LabeledCounter::new(MESSAGE_TYPES),
    control_error: LabeledCounter::new(MESSAGE_TYPES),
    audio_datagrams: AtomicU64::new(0),
    audio_bytes: AtomicU64::new(0),
    text_replays_rejected: AtomicU64::new(0),
};

/// Keeps `aura_connections_active` up to date for one connection's lifetime.
pub struct ConnectionGuard(());

impl Metrics {
    /// A QUIC connection completed its handshake. Hold the guard while it lives.
    pub fn connection_opened(&self) -> ConnectionGuard {
        self.connections_total.fetch_add(1, Ordering::Relaxed);
        self.connections_active.fetch_add(1, Ordering::Relaxed);
        ConnectionGuard(())
    }

    pub fn connection_rejected(&self, reason: &str) {
        self.connections_rejected.inc(reason);
    }

    pub fn login(&self, outcome: &str) {
        self.logins.inc(outcome);
    }

    pub fn control_message(&self, msg_type: &str, ok: bool) {
        if ok {
            self.control_ok.inc(msg_type);
        } else {
            self.control_error.inc(msg_type);
        }
    }

    pub fn audio_datagram(&self, bytes: usize) {
        self.audio_datagrams.fetch_add(1, Ordering::Relaxed);
        self.audio_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn text_replay_rejected(&self) {
        self.text_replays_rejected.fetch_add(1, Ordering::Relaxed);
    }

    /// Logins with `outcome` so far (for tests).
    pub fn logins_with(&self, outcome: &str) -> u64 {
        self.logins.get(outcome)
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        METRICS.connections_active.fetch_sub(1, Ordering::Relaxed);
    }
}

fn header(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

/// The exposition text: counters, plus gauges read from `state`.
pub fn render(state: &ServerState) -> String {
    let m = &METRICS;
    let mut out = String::new();

    header(&mut out, "aura_build_info", "gauge", "Server build.");
    let _ = writeln!(
        out,
        "aura_build_info{{version=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION")
    );

    header(
        &mut out,
        "aura_connections_total",
        "counter",
        "QUIC connections that completed the TLS handshake.",
    );
    let _ = writeln!(
        out,
        "aura_connections_total {}",
        m.connections_total.load(Ordering::Relaxed)
    );
    header(
        &mut out,
        "aura_connections_active",
        "gauge",
        "Open QUIC connections.",
    );
    let _ = writeln!(
        out,
        "aura_connections_active {}",
        m.connections_active.load(Ordering::Relaxed)
    );
    header(
        &mut out,
        "aura_connections_rejected_total",
        "counter",
        "Connections dropped before login, by reason.",
    );
    m.connections_rejected
        .render(&mut out, "aura_connections_rejected_total", "reason", "");

    header(
        &mut out,
        "aura_logins_total",
        "counter",
        "Login attempts, by outcome.",
    );
    m.logins
        .render(&mut out, "aura_logins_total", "outcome", "");

    header(
        &mut out,
        "aura_control_messages_total",
        "counter",
        "Control-stream messages handled, by type and outcome.",
    );
    m.control_ok.render(
        &mut out,
        "aura_control_messages_total",
        "type",
        ",outcome=\"ok\"",
    );
    m.control_error.render(
        &mut out,
        "aura_control_messages_total",
        "type",
        ",outcome=\"error\"",
    );

    header(
        &mut out,
        "aura_audio_datagrams_total",
        "counter",
        "Encrypted audio datagrams received for relay.",
    );
    let _ = writeln!(
        out,
        "aura_audio_datagrams_total {}",
        m.audio_datagrams.load(Ordering::Relaxed)
    );
    header(
        &mut out,
        "aura_audio_bytes_total",
        "counter",
        "Bytes of encrypted audio received for relay.",
    );
    let _ = writeln!(
        out,
        "aura_audio_bytes_total {}",
        m.audio_bytes.load(Ordering::Relaxed)
    );

    header(
        &mut out,
        "aura_text_replays_rejected_total",
        "counter",
        "Text messages refused as replays.",
    );
    let _ = writeln!(
        out,
        "aura_text_replays_rejected_total {}",
        m.text_replays_rejected.load(Ordering::Relaxed)
    );

    header(
        &mut out,
        "aura_sessions_active",
        "gauge",
        "Logged-in sessions.",
    );
    let _ = writeln!(out, "aura_sessions_active {}", state.sessions.len());
    header(
        &mut out,
        "aura_channels",
        "gauge",
        "Channels on the server.",
    );
    let _ = writeln!(out, "aura_channels {}", state.channel_metadata.len());
    header(
        &mut out,
        "aura_seen_message_ids",
        "gauge",
        "Text message IDs held for replay protection.",
    );
    let _ = writeln!(
        out,
        "aura_seen_message_ids {}",
        state.seen_messages.message_count()
    );

    out
}

/// Serve `GET /metrics` on `addr` until the process exits.
pub async fn serve(addr: SocketAddr, state: Arc<ServerState>) -> std::io::Result<()> {
    use axum::http::header::CONTENT_TYPE;
    use axum::routing::get;

    let app = axum::Router::new().route(
        "/metrics",
        get(move || {
            let state = Arc::clone(&state);
            async move {
                (
                    [(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
                    render(&state),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Arc<ServerState> {
        let db = Arc::new(crate::db::Database::open_in_memory().unwrap());
        Arc::new(ServerState::new(db, crate::config::Config::default()))
    }

    #[test]
    fn unexpected_label_values_count_as_other() {
        static C: LabeledCounter = LabeledCounter::new(REJECT_REASONS);
        C.inc("rate_limited");
        C.inc("some-ip-or-name");
        assert_eq!(C.get("rate_limited"), 1);
        assert_eq!(C.get("other"), 1);
        assert_eq!(C.get("some-ip-or-name"), 0);
    }

    #[test]
    fn label_lists_fit_and_end_with_other() {
        for list in [LOGIN_OUTCOMES, MESSAGE_TYPES, REJECT_REASONS] {
            assert!(list.len() <= MAX_LABELS);
            assert_eq!(list.last(), Some(&"other"));
        }
    }

    #[test]
    fn renders_every_metric_in_exposition_format() {
        let text = render(&state());
        for name in [
            "aura_build_info",
            "aura_connections_total",
            "aura_connections_active",
            "aura_connections_rejected_total",
            "aura_logins_total",
            "aura_control_messages_total",
            "aura_audio_datagrams_total",
            "aura_audio_bytes_total",
            "aura_text_replays_rejected_total",
            "aura_sessions_active",
            "aura_channels",
            "aura_seen_message_ids",
        ] {
            assert!(text.contains(&format!("# TYPE {name} ")), "{name} missing");
        }
        assert!(text.contains("aura_logins_total{outcome=\"invalid_signature\"} "));
        assert!(text.contains("aura_control_messages_total{type=\"mls_join\",outcome=\"error\"} "));
        // Every sample line is `name{labels} value` or `name value`.
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let value = line.rsplit(' ').next().unwrap();
            assert!(value.parse::<f64>().is_ok(), "bad sample line: {line}");
        }
    }

    #[tokio::test]
    async fn serves_metrics_over_http() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = probe.local_addr().unwrap();
        drop(probe);
        tokio::spawn(serve(addr, state()));

        let mut response = String::new();
        for _ in 0..50 {
            if let Ok(mut s) = tokio::net::TcpStream::connect(addr).await {
                s.write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
                s.read_to_string(&mut response).await.unwrap();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response
            .to_ascii_lowercase()
            .contains("content-type: text/plain; version=0.0.4"));
        assert!(response.contains("aura_build_info{version="));
    }
}
