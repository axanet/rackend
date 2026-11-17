use uuid::Uuid;

/// Generates record and entity ids. Mockable in tests.
pub trait IdGenerator: Send + Sync + std::fmt::Debug {
    /// A new UUIDv7 (time-ordered) id.
    fn new_id(&self) -> Uuid;
}

/// Default generator producing UUIDv7 ids (SPEC §6.4).
#[derive(Debug, Default, Clone, Copy)]
pub struct UuidV7Generator;

impl IdGenerator for UuidV7Generator {
    fn new_id(&self) -> Uuid {
        Uuid::now_v7()
    }
}
