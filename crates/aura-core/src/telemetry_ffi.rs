//! Opt-in OpenTelemetry tracing for the Swift and C# clients.
//!
//! Off unless the app calls [`telemetry_start`] or [`telemetry_start_from_env`]
//! (the user configured an OTLP endpoint). What is exported is decided by
//! `aura_telemetry`: only spans with `aura::` targets, never log lines. The
//! spans here, and the MLS spans in `mls.rs`, carry an operation name and an
//! outcome, never a user, channel, peer, key or message. App-level spans go
//! through [`TelemetrySpan`] with a fixed [`ClientOperation`] list, so the
//! apps can't attach free-form strings.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tracing::field::Empty;

/// The running exporter, kept alive for the life of the process.
static TELEMETRY: Mutex<Option<aura_telemetry::Telemetry>> = Mutex::new(None);

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum TelemetryError {
    #[error("invalid telemetry endpoint: {message}")]
    InvalidEndpoint { message: String },
    #[error("could not start telemetry: {message}")]
    Exporter { message: String },
    /// Telemetry was already started in this process (it can start once).
    #[error("telemetry already started")]
    AlreadyStarted,
}

impl From<aura_telemetry::TelemetryError> for TelemetryError {
    fn from(e: aura_telemetry::TelemetryError) -> Self {
        match e {
            aura_telemetry::TelemetryError::InvalidEndpoint(_) => TelemetryError::InvalidEndpoint {
                message: e.to_string(),
            },
            aura_telemetry::TelemetryError::Exporter(_) => TelemetryError::Exporter {
                message: e.to_string(),
            },
        }
    }
}

fn install(config: aura_telemetry::Config) -> Result<(), TelemetryError> {
    use tracing_subscriber::layer::SubscriberExt as _;

    let mut slot = TELEMETRY.lock().unwrap_or_else(|p| p.into_inner());
    if slot.is_some() {
        return Err(TelemetryError::AlreadyStarted);
    }
    let telemetry = aura_telemetry::Telemetry::start(config)?;
    let subscriber = tracing_subscriber::registry().with(telemetry.layer());
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|_| TelemetryError::AlreadyStarted)?;
    *slot = Some(telemetry);
    Ok(())
}

/// Start exporting Aura's client spans to an OTLP/HTTP endpoint: a base URL
/// such as `https://otlp.example.com` (`/v1/traces` is appended), with request
/// headers such as an API key. Call only when the user has opted in.
#[uniffi::export]
pub fn telemetry_start(
    endpoint: String,
    headers: HashMap<String, String>,
    service_name: String,
) -> Result<(), TelemetryError> {
    let config = aura_telemetry::Config::with_endpoint(
        &endpoint,
        headers,
        &service_name,
        env!("CARGO_PKG_VERSION"),
    )?;
    install(config)
}

/// Start exporting if the standard `OTEL_EXPORTER_OTLP_ENDPOINT` (or
/// `..._TRACES_ENDPOINT`) environment variable is set; returns whether it did.
#[uniffi::export]
pub fn telemetry_start_from_env(service_name: String) -> Result<bool, TelemetryError> {
    match aura_telemetry::Config::from_env(&service_name, env!("CARGO_PKG_VERSION")) {
        Some(config) => install(config).map(|()| true),
        None => Ok(false),
    }
}

/// Flush buffered spans and stop exporting (for app shutdown).
#[uniffi::export]
pub fn telemetry_shutdown() {
    let mut slot = TELEMETRY.lock().unwrap_or_else(|p| p.into_inner());
    slot.take();
}

/// App-level operations a client may trace. A fixed list, so a span's name
/// is never something the app made up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ClientOperation {
    /// Opening the QUIC connection (handshake included).
    Connect,
    /// Logging in over an open connection.
    Login,
    /// Joining a channel, up to having MLS keys for it.
    JoinChannel,
}

/// How an operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum OperationOutcome {
    Ok,
    Failed,
    Cancelled,
}

impl OperationOutcome {
    fn as_str(self) -> &'static str {
        match self {
            OperationOutcome::Ok => "ok",
            OperationOutcome::Failed => "error",
            OperationOutcome::Cancelled => "cancelled",
        }
    }
}

/// A traced app-level operation: create it when the operation starts and
/// call [`TelemetrySpan::finish`] when it ends. A no-op when telemetry is off.
#[derive(uniffi::Object)]
pub struct TelemetrySpan {
    span: Mutex<Option<tracing::Span>>,
}

#[uniffi::export]
impl TelemetrySpan {
    #[uniffi::constructor]
    pub fn new(operation: ClientOperation) -> Arc<Self> {
        // Root spans: each operation is its own trace.
        let span = match operation {
            ClientOperation::Connect => {
                tracing::info_span!(target: "aura::client", parent: None, "connect", outcome = Empty)
            }
            ClientOperation::Login => {
                tracing::info_span!(target: "aura::client", parent: None, "login", outcome = Empty)
            }
            ClientOperation::JoinChannel => {
                tracing::info_span!(target: "aura::client", parent: None, "join_channel", outcome = Empty)
            }
        };
        Arc::new(Self {
            span: Mutex::new(Some(span)),
        })
    }

    /// End the operation. Later calls do nothing.
    pub fn finish(&self, outcome: OperationOutcome) {
        let mut slot = self.span.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(span) = slot.take() {
            span.record("outcome", outcome.as_str());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt as _;

    #[test]
    fn client_spans_export_name_and_outcome_only() {
        let (telemetry, exporter) = aura_telemetry::testing::in_memory();
        let subscriber = tracing_subscriber::registry().with(telemetry.layer());
        tracing::subscriber::with_default(subscriber, || {
            let login = TelemetrySpan::new(ClientOperation::Login);
            login.finish(OperationOutcome::Failed);
            login.finish(OperationOutcome::Ok); // ignored: already finished
            TelemetrySpan::new(ClientOperation::Connect).finish(OperationOutcome::Ok);
        });
        let spans = exporter.get_finished_spans().unwrap();
        let got: Vec<(String, Vec<(String, String)>)> = spans
            .iter()
            .map(|s| {
                (
                    s.name.to_string(),
                    s.attributes
                        .iter()
                        .map(|kv| (kv.key.to_string(), kv.value.as_str().into_owned()))
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (
                    "login".to_owned(),
                    vec![("outcome".to_owned(), "error".to_owned())]
                ),
                (
                    "connect".to_owned(),
                    vec![("outcome".to_owned(), "ok".to_owned())]
                ),
            ]
        );
        drop(telemetry);
    }

    #[test]
    fn spans_are_no_ops_without_telemetry() {
        let span = TelemetrySpan::new(ClientOperation::JoinChannel);
        span.finish(OperationOutcome::Ok);
    }
}
