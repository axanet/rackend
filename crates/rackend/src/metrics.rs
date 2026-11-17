//! The Prometheus recorder for the `metrics` facade (SPEC §14.3, §14.4).
//! Installed once per process; `/metrics` renders it.

use std::sync::OnceLock;

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};

static HANDLE: OnceLock<Option<PrometheusHandle>> = OnceLock::new();

/// Latency buckets in seconds: 1 ms to 10 s.
const BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Installs the recorder (first call) and returns its handle.
pub fn recorder() -> Option<PrometheusHandle> {
    HANDLE
        .get_or_init(|| {
            let builder = PrometheusBuilder::new()
                .set_buckets_for_metric(Matcher::Suffix("_seconds".into()), BUCKETS)
                .ok()?;
            match builder.install_recorder() {
                Ok(h) => Some(h),
                Err(e) => {
                    tracing::warn!(error = %e, "cannot install the Prometheus recorder; /metrics is off");
                    None
                }
            }
        })
        .clone()
}
