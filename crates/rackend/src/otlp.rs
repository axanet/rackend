//! OTLP export (SPEC §14.4): when `RACKEND_OTLP_ENDPOINT` is set, traces
//! (one span per request, database spans below it) and metrics go to an
//! OpenTelemetry collector over HTTP/protobuf. Exporters run on their own
//! threads; nothing here blocks requests.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::SdkTracerProvider;
use rackend_core::usecases::App;

static TRACES: OnceLock<SdkTracerProvider> = OnceLock::new();
static METRICS: OnceLock<SdkMeterProvider> = OnceLock::new();

fn resource() -> Resource {
    Resource::builder_empty()
        .with_service_name("rackend")
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .build()
}

fn url(endpoint: &str, path: &str) -> String {
    format!("{}/{path}", endpoint.trim_end_matches('/'))
}

/// A tracing layer exporting spans to `{endpoint}/v1/traces`.
pub fn tracing_layer<S>(
    endpoint: &str,
) -> Result<tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::Tracer>, String>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(url(endpoint, "v1/traces"))
        .build()
        .map_err(|e| format!("OTLP traces: {e}"))?;
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource())
        .build();
    let tracer = provider.tracer("rackend");
    let _ = TRACES.set(provider);
    Ok(tracing_opentelemetry::layer().with_tracer(tracer))
}

/// Exports the dashboard numbers to `{endpoint}/v1/metrics` every 60 s.
pub fn start_metrics(endpoint: &str, app: &Arc<App>) -> Result<(), String> {
    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(url(endpoint, "v1/metrics"))
        .build()
        .map_err(|e| format!("OTLP metrics: {e}"))?;
    let reader = PeriodicReader::builder(exporter)
        .with_interval(Duration::from_secs(60))
        .build();
    let provider = SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(resource())
        .build();
    let meter = provider.meter("rackend");
    let weak = Arc::downgrade(app);
    macro_rules! gauge {
        ($name:literal, $unit:literal, |$a:ident| $value:expr) => {{
            let w = weak.clone();
            let _ = meter
                .f64_observable_gauge($name)
                .with_unit($unit)
                .with_callback(move |o| {
                    if let Some($a) = w.upgrade() {
                        o.observe($value, &[]);
                    }
                })
                .build();
        }};
    }
    macro_rules! counter {
        ($name:literal, |$a:ident| $value:expr) => {{
            let w = weak.clone();
            let _ = meter
                .u64_observable_counter($name)
                .with_callback(move |o| {
                    if let Some($a) = w.upgrade() {
                        o.observe($value, &[]);
                    }
                })
                .build();
        }};
    }
    use std::sync::atomic::Ordering::Relaxed;
    counter!("rackend.http.requests", |a| a
        .counters()
        .http_requests
        .load(Relaxed));
    counter!("rackend.http.server_errors", |a| a
        .counters()
        .http_server_errors
        .load(Relaxed));
    counter!("rackend.realtime.events", |a| a
        .hub
        .dispatched
        .load(Relaxed));
    counter!("rackend.thumbnails.generated", |a| a.thumb_generations());
    gauge!(
        "rackend.realtime.connections",
        "{connection}",
        |a| a.hub.connections() as f64
    );
    gauge!(
        "rackend.db.write_queue",
        "{job}",
        |a| a.store.stats().write_queue_depth as f64
    );
    gauge!("rackend.db.size", "By", |a| {
        let s = a.store.stats();
        (s.db_size + s.wal_size) as f64
    });
    gauge!("rackend.process.rss", "By", |a| a.host_sample().process_rss
        as f64);
    gauge!("rackend.host.cpu", "%", |a| a.host_sample().cpu_percent);
    gauge!(
        "rackend.host.memory.used",
        "By",
        |a| a.host_sample().memory_used as f64
    );
    gauge!(
        "rackend.host.memory.total",
        "By",
        |a| a.host_sample().memory_total as f64
    );
    gauge!(
        "rackend.host.disk.free",
        "By",
        |a| a.host_sample().disk_free as f64
    );
    let _ = METRICS.set(provider);
    Ok(())
}

/// Flushes and stops the exporters (shutdown).
pub fn shutdown() {
    if let Some(p) = TRACES.get() {
        let _ = p.shutdown_with_timeout(Duration::from_secs(5));
    }
    if let Some(p) = METRICS.get() {
        let _ = p.shutdown_with_timeout(Duration::from_secs(5));
    }
}
