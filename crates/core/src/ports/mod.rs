//! Ports: traits implemented by adapters (SPEC §4.3).

mod auth_ext;
mod blob;
mod clock;
mod id;
mod observe;
mod spawner;
mod store;

pub use auth_ext::{
    Email, IdpClient, IdpConfig, IdpError, IdpStart, IdpUser, MailError, Mailer, MailerFactory,
    SmtpConfig, SmtpTls,
};
pub use blob::{
    BlobChunks, BlobError, BlobListItem, BlobMeta, BlobReader, BlobStore, BlobStoreFactory,
    ByteRange, ImageError, ImageInfo, ImageProcessor, NormalizedImage, S3Config, StorageConfig,
};
pub use clock::{Clock, SystemClock};
pub use id::{IdGenerator, UuidV7Generator};
pub use observe::{
    BackupInfo, Backups, HostMetrics, HostSample, LogsStore, MetricPoint, RequestLog,
    RequestLogQuery, RequestStats, RouteStat,
};
pub use spawner::{BlockingJob, BoxFuture, Spawner};
pub use store::{
    AnyBox, AuditQuery, ReadJob, ReadTx, RecordPage, RecordQuery, SettingRow, Sort, Store,
    StoreExt, StoreFuture, StoreStats, WriteJob, WriteTx,
};
