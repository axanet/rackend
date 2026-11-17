//! Tokio implementation of the core `Spawner` port.

use rackend_core::ports::{AnyBox, BlockingJob, BoxFuture, Spawner};

/// Spawns on the current Tokio runtime.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioSpawner;

impl Spawner for TokioSpawner {
    fn spawn(&self, name: &'static str, task: BoxFuture<'static, ()>) {
        let span = tracing::debug_span!("task", name);
        tokio::spawn(tracing::Instrument::instrument(task, span));
    }

    fn run_blocking(&self, job: BlockingJob) -> BoxFuture<'static, AnyBox> {
        Box::pin(async move {
            match tokio::task::spawn_blocking(job).await {
                Ok(v) => v,
                // A panicking job: surface as a value of the wrong type, which
                // the caller reports as an internal error.
                Err(e) => Box::new(format!("blocking task failed: {e}")) as AnyBox,
            }
        })
    }
}
