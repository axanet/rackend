use std::future::Future;
use std::pin::Pin;

/// A boxed, sendable future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Spawns background tasks. Core never spawns tasks itself (SPEC §4.2).
pub trait Spawner: Send + Sync + std::fmt::Debug {
    /// Runs `task` in the background. `name` is used for logs and tracing.
    fn spawn(&self, name: &'static str, task: BoxFuture<'static, ()>);
}
