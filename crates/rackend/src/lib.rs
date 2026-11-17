//! The `rackend` composition root, as a library so integration tests can
//! build the same application the binary runs.

pub mod backup;
pub mod check;
pub mod config;
pub mod datadir;
pub mod exit;
pub mod healthcheck;
pub mod logging;
pub mod metrics;
#[cfg(feature = "otlp")]
pub mod otlp;
pub mod serve;
pub mod spawner;
pub mod ui_assets;
pub mod wiring;
