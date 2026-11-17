//! Ports: traits implemented by adapters (SPEC §4.3).

mod clock;
mod id;
mod spawner;
mod store;

pub use clock::{Clock, SystemClock};
pub use id::{IdGenerator, UuidV7Generator};
pub use spawner::{BoxFuture, Spawner};
pub use store::{
    AnyBox, ReadJob, ReadTx, SettingRow, Store, StoreExt, StoreFuture, WriteJob, WriteTx,
};
