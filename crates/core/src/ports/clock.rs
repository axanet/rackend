use time::OffsetDateTime;

/// Source of the current time. Mockable in tests.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// The current time in UTC.
    fn now(&self) -> OffsetDateTime;
}

/// The real system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}
