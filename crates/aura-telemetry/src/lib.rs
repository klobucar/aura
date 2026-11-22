//! Opt-in OpenTelemetry tracing for Aura, shared by the server and the
//! client core (`aura-core`, which both apps load).
//!
//! Aura is a privacy tool, so what leaves the process is deliberately narrow:
//!
//! - **Off unless configured.** Nothing is exported without an OTLP endpoint
//!   ([`Config::from_env`] returns `None`, or a client never calls start).
//! - **Only Aura's own spans.** The exporter sees spans whose target starts
//!   with [`TARGET_PREFIX`] (`aura::`). Spans from libraries (QUIC, TLS,
//!   OpenMLS, HTTP) and every log *event* are filtered out before the
//!   OpenTelemetry layer, so log lines (which can name users, channels and
//!   addresses) never become span events.
//! - **No identities in attributes.** Aura's spans carry only things like a
//!   message type or an outcome code: never a user id, channel id, IP
//!   address, display name, key or message content. That is a rule for code
//!   that declares `aura::` spans, enforced in review; this crate can't check
//!   it. Traces still show *when* activity happens.
//!
//! Export is OTLP over HTTP/protobuf from a background thread (no async
//! runtime needed), over TLS with the operating system's trust store.

use std::collections::HashMap;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{WithExportConfig as _, WithHttpConfig as _};
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use tracing::Subscriber;
use tracing_subscriber::filter::{filter_fn, FilterFn};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Spans whose target starts with this are exported; nothing else is.
pub const TARGET_PREFIX: &str = "aura::";

/// Path appended to a base OTLP/HTTP endpoint for traces.
const TRACES_PATH: &str = "/v1/traces";

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("telemetry endpoint must be an http(s) URL, got {0:?}")]
    InvalidEndpoint(String),
    #[error("could not build the OTLP exporter: {0}")]
    Exporter(String),
}

/// Where and how to export.
#[derive(Debug, Clone)]
pub struct Config {
    /// Full OTLP/HTTP traces URL. `None` lets the exporter read the standard
    /// `OTEL_EXPORTER_OTLP_*` environment variables itself.
    endpoint: Option<String>,
    headers: HashMap<String, String>,
    service_name: String,
    service_version: String,
}

impl Config {
    /// Telemetry configured by the standard OpenTelemetry environment
    /// variables, or `None` (off) when neither `OTEL_EXPORTER_OTLP_ENDPOINT`
    /// nor `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` is set, or when
    /// `OTEL_SDK_DISABLED=true`. Headers, timeout and sampling
    /// (`OTEL_TRACES_SAMPLER`, `OTEL_TRACES_SAMPLER_ARG`) come from the
    /// environment too.
    pub fn from_env(service_name: &str, service_version: &str) -> Option<Self> {
        Self::from_vars(|k| std::env::var(k).ok(), service_name, service_version)
    }

    fn from_vars(
        var: impl Fn(&str) -> Option<String>,
        service_name: &str,
        service_version: &str,
    ) -> Option<Self> {
        let set = |k: &str| var(k).is_some_and(|v| !v.trim().is_empty());
        if var("OTEL_SDK_DISABLED").is_some_and(|v| v.trim().eq_ignore_ascii_case("true")) {
            return None;
        }
        if !set("OTEL_EXPORTER_OTLP_ENDPOINT") && !set("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT") {
            return None;
        }
        Some(Self {
            endpoint: None,
            headers: HashMap::new(),
            service_name: service_name.to_owned(),
            service_version: service_version.to_owned(),
        })
    }

    /// Telemetry to an explicit OTLP/HTTP base URL (for example
    /// `https://otlp.example.com`; `/v1/traces` is appended unless already
    /// present), with request headers such as an API key.
    pub fn with_endpoint(
        base_url: &str,
        headers: HashMap<String, String>,
        service_name: &str,
        service_version: &str,
    ) -> Result<Self, TelemetryError> {
        let base = base_url.trim().trim_end_matches('/');
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return Err(TelemetryError::InvalidEndpoint(base_url.to_owned()));
        }
        let endpoint = if base.ends_with(TRACES_PATH) {
            base.to_owned()
        } else {
            format!("{base}{TRACES_PATH}")
        };
        Ok(Self {
            endpoint: Some(endpoint),
            headers,
            service_name: service_name.to_owned(),
            service_version: service_version.to_owned(),
        })
    }

    /// The traces URL, when set explicitly.
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }
}

/// A running exporter. Dropping it (or [`Telemetry::shutdown`]) flushes and
/// stops the export thread.
pub struct Telemetry {
    provider: SdkTracerProvider,
}

impl Telemetry {
    /// Start exporting according to `config`.
    pub fn start(config: Config) -> Result<Self, TelemetryError> {
        let mut builder = opentelemetry_otlp::SpanExporter::builder().with_http();
        if let Some(endpoint) = &config.endpoint {
            builder = builder.with_endpoint(endpoint.clone());
        }
        if !config.headers.is_empty() {
            builder = builder.with_headers(config.headers.clone());
        }
        let exporter = builder
            .build()
            .map_err(|e| TelemetryError::Exporter(e.to_string()))?;

        let resource = Resource::builder()
            .with_service_name(config.service_name.clone())
            .with_attribute(opentelemetry::KeyValue::new(
                "service.version",
                config.service_version.clone(),
            ))
            .build();

        let provider = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource)
            .build();
        Ok(Self::from_provider(provider))
    }

    fn from_provider(provider: SdkTracerProvider) -> Self {
        Self { provider }
    }

    /// The `tracing` layer that feeds Aura's spans, and only those, to the
    /// exporter. Add it to the process's subscriber next to any log layer.
    pub fn layer<S>(&self) -> impl Layer<S> + Send + Sync + 'static
    where
        S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
    {
        tracing_opentelemetry::layer()
            .with_tracer(self.provider.tracer("aura"))
            // No source locations, thread names, levels or targets: noise, and
            // not something an operator needs from a trace.
            .with_location(false)
            .with_threads(false)
            .with_level(false)
            .with_target(false)
            .with_tracked_inactivity(false)
            .with_filter(export_filter())
    }

    /// Flush buffered spans and stop exporting.
    pub fn shutdown(self) {
        // Drop does the work.
    }
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        let _ = self.provider.shutdown();
    }
}

/// Only spans (never events) whose target is under [`TARGET_PREFIX`].
fn export_filter() -> FilterFn<impl Fn(&tracing::Metadata<'_>) -> bool> {
    filter_fn(|meta| meta.is_span() && meta.target().starts_with(TARGET_PREFIX))
}

/// Test support: the real filter and layer, recording spans in memory.
#[cfg(feature = "testing")]
pub mod testing {
    pub use opentelemetry_sdk::trace::{InMemorySpanExporter, SpanData};

    /// A [`super::Telemetry`] that exports synchronously into the returned
    /// exporter. Read spans before dropping the `Telemetry`: shutdown clears
    /// the exporter.
    pub fn in_memory() -> (super::Telemetry, InMemorySpanExporter) {
        let exporter = InMemorySpanExporter::default();
        let provider = super::SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        (super::Telemetry::from_provider(provider), exporter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use tracing_subscriber::layer::SubscriberExt as _;

    #[test]
    fn off_unless_an_endpoint_is_set() {
        let none = |_: &str| None;
        assert!(Config::from_vars(none, "s", "1").is_none());

        let base = |k: &str| (k == "OTEL_EXPORTER_OTLP_ENDPOINT").then(|| "https://x".to_owned());
        assert!(Config::from_vars(base, "s", "1").is_some());

        let traces =
            |k: &str| (k == "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").then(|| "https://x".to_owned());
        assert!(Config::from_vars(traces, "s", "1").is_some());

        let blank = |k: &str| (k == "OTEL_EXPORTER_OTLP_ENDPOINT").then(|| "  ".to_owned());
        assert!(Config::from_vars(blank, "s", "1").is_none());

        let disabled = |k: &str| match k {
            "OTEL_EXPORTER_OTLP_ENDPOINT" => Some("https://x".to_owned()),
            "OTEL_SDK_DISABLED" => Some("TRUE".to_owned()),
            _ => None,
        };
        assert!(Config::from_vars(disabled, "s", "1").is_none());
    }

    #[test]
    fn explicit_endpoints_get_the_traces_path_once() {
        let c = |u: &str| Config::with_endpoint(u, HashMap::new(), "s", "1");
        assert_eq!(
            c("https://otlp.example.com").unwrap().endpoint(),
            Some("https://otlp.example.com/v1/traces")
        );
        assert_eq!(
            c("https://otlp.example.com/").unwrap().endpoint(),
            Some("https://otlp.example.com/v1/traces")
        );
        assert_eq!(
            c("https://otlp.example.com/v1/traces").unwrap().endpoint(),
            Some("https://otlp.example.com/v1/traces")
        );
        assert!(c("otlp.example.com").is_err());
        assert!(c("ftp://otlp.example.com").is_err());
    }

    /// The privacy filter: only `aura::` spans reach the exporter, with only
    /// their declared fields; library spans and all events are dropped.
    #[test]
    fn exports_only_aura_spans_and_no_events() {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let telemetry = Telemetry::from_provider(provider);
        let subscriber = tracing_subscriber::registry().with(telemetry.layer());

        tracing::subscriber::with_default(subscriber, || {
            let kept = tracing::info_span!(target: "aura::test", "kept", outcome = "ok");
            kept.in_scope(|| {
                // A log line naming someone, inside an exported span.
                tracing::info!(target: "aura::test", user = "alice", "joined from 10.0.0.1");
                let _lib = tracing::info_span!(target: "quinn::endpoint", "lib_span").entered();
            });
            drop(kept);
            let _other = tracing::info_span!(target: "aurora::x", "not_aura").entered();
        });
        // Read before shutdown: the in-memory exporter clears itself then.
        let spans = exporter.get_finished_spans().unwrap();
        drop(telemetry);
        let names: Vec<_> = spans.iter().map(|s| s.name.to_string()).collect();
        assert_eq!(names, ["kept"]);
        let kept = &spans[0];
        assert!(kept.events.is_empty(), "log events must not be exported");
        let attrs: Vec<_> = kept
            .attributes
            .iter()
            .map(|kv| (kv.key.as_str().to_owned(), kv.value.as_str().into_owned()))
            .collect();
        assert!(attrs.contains(&("outcome".to_owned(), "ok".to_owned())));
        assert!(
            !attrs
                .iter()
                .any(|(_, v)| v.contains("alice") || v.contains("10.0.0.1")),
            "{attrs:?}"
        );
    }

    /// The real export path: OTLP/HTTP protobuf through the blocking reqwest
    /// client, to a local listener, flushed on shutdown.
    #[test]
    fn exports_over_otlp_http() {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let receiver = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            conn.set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            // Read headers and body (Content-Length) before replying.
            loop {
                let n = conn.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len || n == 0 {
                        break;
                    }
                }
            }
            conn.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                .unwrap();
            String::from_utf8_lossy(&buf).into_owned()
        });

        let mut headers = HashMap::new();
        headers.insert("x-test-key".to_owned(), "k1".to_owned());
        let config =
            Config::with_endpoint(&format!("http://{addr}"), headers, "aura-test", "0").unwrap();
        let telemetry = Telemetry::start(config).unwrap();
        let subscriber = tracing_subscriber::registry().with(telemetry.layer());
        tracing::subscriber::with_default(subscriber, || {
            let _s = tracing::info_span!(target: "aura::test", "exported_span", outcome = "ok")
                .entered();
        });
        drop(telemetry); // flushes the batch

        let request = receiver.join().unwrap();
        assert!(request.starts_with("POST /v1/traces "), "{request}");
        let lower = request.to_ascii_lowercase();
        assert!(
            lower.contains("content-type: application/x-protobuf"),
            "{request}"
        );
        assert!(lower.contains("x-test-key: k1"), "{request}");
        assert!(request.contains("exported_span") && request.contains("aura-test"));
    }
}
