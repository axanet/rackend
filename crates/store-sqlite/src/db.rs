//! Connection model (SPEC §6.3): one writer connection on a dedicated OS
//! thread fed by a bounded job queue, and a pool of read-only connections
//! used via `spawn_blocking`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use tokio::sync::{Semaphore, oneshot};

use crate::Error;
use crate::conn::{DbOptions, open_reader};

type WriterJob = Box<dyn FnOnce(&mut Connection) + Send>;

enum WriterMsg {
    Job(WriterJob),
    Shutdown(oneshot::Sender<Result<(), Error>>),
}

/// One SQLite database file with a single writer and a reader pool.
#[derive(Debug, Clone)]
pub struct SqliteDb {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    opts: DbOptions,
    writer: SyncSender<WriterMsg>,
    closing: AtomicBool,
    pending: Arc<AtomicU64>,
    readers: Mutex<Vec<Connection>>,
    reader_permits: Arc<Semaphore>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteDb")
            .field("path", &self.path)
            .field("opts", &self.opts)
            .finish_non_exhaustive()
    }
}

impl SqliteDb {
    /// Starts the writer thread on an already opened (and migrated) writer
    /// connection and opens the reader pool.
    pub fn start(writer: Connection, path: &Path, opts: DbOptions) -> Result<Self, Error> {
        let mut readers = Vec::with_capacity(opts.readers);
        for _ in 0..opts.readers {
            readers.push(open_reader(path, &opts)?);
        }
        let (tx, rx) = sync_channel::<WriterMsg>(opts.writer_queue);
        let name = format!(
            "sqlite-writer-{}",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("db")
        );
        std::thread::Builder::new()
            .name(name)
            .spawn(move || writer_loop(writer, rx))?;
        Ok(Self {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                reader_permits: Arc::new(Semaphore::new(opts.readers.max(1))),
                opts,
                writer: tx,
                closing: AtomicBool::new(false),
                pending: Arc::new(AtomicU64::new(0)),
                readers: Mutex::new(readers),
            }),
        })
    }

    /// The database file path.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Write jobs queued or running.
    pub fn queue_depth(&self) -> u64 {
        self.inner.pending.load(Ordering::Relaxed)
    }

    /// Sizes of the database file and its WAL.
    pub fn file_sizes(&self) -> (u64, u64) {
        let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let mut wal = self.inner.path.as_os_str().to_owned();
        wal.push("-wal");
        (size(&self.inner.path), size(Path::new(&wal)))
    }

    /// Writes a consistent copy of the database to `dest` (`VACUUM INTO`),
    /// on the writer so no write is half applied.
    pub async fn snapshot_to(&self, dest: &Path) -> Result<(), Error> {
        let dest = dest.to_string_lossy().into_owned();
        self.with_writer(move |conn| conn.execute("VACUUM INTO ?1", [dest]).map(drop))
            .await?
            .map_err(Error::from)
    }

    /// Runs `f` on the writer connection. Fails fast with [`Error::QueueFull`]
    /// when the queue is full and [`Error::Closed`] while shutting down.
    pub async fn with_writer<R, F>(&self, f: F) -> Result<R, Error>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> R + Send + 'static,
    {
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        let pending = Arc::clone(&self.inner.pending);
        let job: WriterJob = Box::new(move |conn| {
            pending.fetch_sub(1, Ordering::Relaxed);
            // The receiver may be gone if the caller was cancelled; the job
            // still ran (and committed or rolled back) as one unit.
            let _ = reply_tx.send(f(conn));
        });
        self.inner.pending.fetch_add(1, Ordering::Relaxed);
        match self.inner.writer.try_send(WriterMsg::Job(job)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.inner.pending.fetch_sub(1, Ordering::Relaxed);
                return Err(Error::QueueFull);
            }
            Err(TrySendError::Disconnected(_)) => {
                self.inner.pending.fetch_sub(1, Ordering::Relaxed);
                return Err(Error::Closed);
            }
        }
        tracing::Instrument::instrument(
            reply_rx,
            tracing::info_span!("db.write", db = %self.name()),
        )
        .await
        .map_err(|_| Error::Closed)
    }

    fn name(&self) -> String {
        self.inner
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Runs `f` on a read-only connection from the pool.
    pub async fn with_reader<R, F>(&self, f: F) -> Result<R, Error>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> R + Send + 'static,
    {
        let permit = Arc::clone(&self.inner.reader_permits)
            .acquire_owned()
            .await
            .map_err(|_| Error::Closed)?;
        let inner = Arc::clone(&self.inner);
        let span = tracing::info_span!("db.read", db = %self.name());
        let joined = tokio::task::spawn_blocking(move || -> Result<R, Error> {
            let _span = span.enter();
            let _permit = permit;
            let pooled = lock(&inner.readers).pop();
            let mut conn = match pooled {
                Some(c) => c,
                // A connection is lost only if a job panicked; replace it.
                None => open_reader(&inner.path, &inner.opts)?,
            };
            let out = f(&mut conn);
            lock(&inner.readers).push(conn);
            Ok(out)
        })
        .await;
        joined.map_err(|e| Error::Worker(e.to_string()))?
    }

    /// Drains the writer queue, then runs `PRAGMA optimize` and
    /// `PRAGMA wal_checkpoint(TRUNCATE)` and stops the writer (SPEC §5.6).
    /// Later writes fail with [`Error::Closed`].
    pub async fn shutdown(&self) -> Result<(), Error> {
        if self.inner.closing.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        // Close idle readers first so the checkpoint can truncate the WAL.
        let _all = self
            .inner
            .reader_permits
            .acquire_many(self.inner.opts.readers.max(1) as u32)
            .await;
        lock(&self.inner.readers).clear();
        self.inner.reader_permits.close();

        let (done_tx, done_rx) = oneshot::channel();
        let sender = self.inner.writer.clone();
        // A blocking send waits for queue space, so queued jobs drain first.
        tokio::task::spawn_blocking(move || sender.send(WriterMsg::Shutdown(done_tx)))
            .await
            .map_err(|e| Error::Worker(e.to_string()))?
            .map_err(|_| Error::Closed)?;
        done_rx.await.map_err(|_| Error::Closed)?
    }

    /// Runs `PRAGMA optimize` on the writer (SPEC §6.2, every 6 h).
    pub async fn optimize(&self) -> Result<(), Error> {
        self.with_writer(|conn| conn.execute_batch("PRAGMA optimize;"))
            .await?
            .map_err(Error::from)
    }
}

fn writer_loop(mut conn: Connection, rx: std::sync::mpsc::Receiver<WriterMsg>) {
    while let Ok(msg) = rx.recv() {
        match msg {
            WriterMsg::Job(job) => {
                // A panicking job must not take the writer down; its caller
                // sees a closed reply channel.
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&mut conn)));
                if result.is_err() {
                    tracing::error!("database writer job panicked");
                    // Make sure no transaction is left open.
                    if !conn.is_autocommit() {
                        let _ = conn.execute_batch("ROLLBACK");
                    }
                }
            }
            WriterMsg::Shutdown(done) => {
                let result = conn
                    .execute_batch("PRAGMA optimize; PRAGMA wal_checkpoint(TRUNCATE);")
                    .map_err(Error::from);
                let _ = done.send(result);
                return;
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // A poisoned pool only means a job panicked while holding the lock; the
    // Vec itself is still valid.
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
