use std::future::Future;
use std::pin::Pin;

use super::AnyBox;

/// A boxed, sendable future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A blocking job returning a type-erased value.
pub type BlockingJob = Box<dyn FnOnce() -> AnyBox + Send>;

/// Spawns background tasks and runs blocking work. Core never spawns tasks
/// itself (SPEC §4.2).
pub trait Spawner: Send + Sync + std::fmt::Debug {
    /// Runs `task` in the background. `name` is used for logs and tracing.
    fn spawn(&self, name: &'static str, task: BoxFuture<'static, ()>);

    /// Runs `job` on a thread that may block and returns its result.
    fn run_blocking(&self, job: BlockingJob) -> BoxFuture<'static, AnyBox>;

    /// Completes after `duration` (timers for background loops).
    fn sleep(&self, duration: std::time::Duration) -> BoxFuture<'static, ()>;
}
