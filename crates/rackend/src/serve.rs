//! `rackend serve`: the startup sequence (SPEC §5.4) and graceful shutdown (SPEC §5.6).

use std::sync::Arc;
use std::time::Duration;

use rackend_store_sqlite::{DataDbPaths, DbOptions, SqliteStore, open_data_db, open_logs_db};
use time::OffsetDateTime;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::config::Config;
use crate::datadir::{DataDir, network_filesystem};
use crate::exit::Fatal;
use crate::wiring;

/// How long in-flight requests may take after shutdown starts.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
/// `PRAGMA optimize` interval (SPEC §6.2).
const OPTIMIZE_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// Trash purge, audit retention and session cleanup interval.
const MAINTENANCE_EVERY: Duration = Duration::from_secs(60 * 60);
/// File GC (orphans, removed files) and thumbnail cache GC: at start, then daily.
const FILES_GC_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Runs the server until SIGTERM/SIGINT.
pub async fn run(cfg: Config, version: &'static str) -> Result<(), Fatal> {
    if cfg.dev {
        tracing::warn!(
            "dev mode is on (RACKEND_DEV): pretty logs and permissive localhost handling; never use it in production"
        );
    }
    for var in &cfg.not_yet_supported {
        tracing::warn!(
            variable = var,
            "environment variable is set but not used by this version yet"
        );
    }

    // Steps 2–3: data dir, write check, exclusive lock, tmp wipe.
    let dir = DataDir::prepare(&cfg.data_dir)?;
    tracing::info!(data_dir = %dir.root().display(), "data directory locked");

    // Step 4: network filesystem warning.
    if let Some(kind) = network_filesystem(dir.root()) {
        tracing::warn!(
            data_dir = %dir.root().display(),
            filesystem = kind,
            "the data directory is on a network filesystem; SQLite locking is unreliable there and data can be corrupted. Use a local disk. See https://github.com/axanet/rackend/blob/main/docs/deployment.md#storage"
        );
    }

    // Steps 5–6: open data.db, PRAGMAs, migration procedure.
    let now = OffsetDateTime::now_utc();
    let paths = DataDbPaths {
        db: dir.data_db(),
        backups_dir: dir.backups(),
    };
    let sync = cfg.sqlite_synchronous;
    let (data, report) = tokio::task::spawn_blocking(move || {
        open_data_db(&paths, DbOptions::data(sync), version, now)
    })
    .await
    .map_err(|e| Fatal::startup(format!("startup task failed: {e}")))?
    .map_err(|e| Fatal::migration(&e))?;
    if report.applied.is_empty() {
        tracing::info!(schema_version = report.to, "data.db is up to date");
    } else {
        tracing::info!(
            from = report.from,
            to = report.to,
            applied = ?report.applied,
            backup = ?report.backup,
            "data.db migrated"
        );
    }
    let logs_path = dir.logs_db();
    let logs = tokio::task::spawn_blocking(move || {
        open_logs_db(&logs_path, DbOptions::logs(sync), version, now)
    })
    .await
    .map_err(|e| Fatal::startup(format!("startup task failed: {e}")))?
    .map_err(|e| Fatal::migration(&e))?;

    // Step 7: settings, schema cache, compiled rules.
    let app = wiring::app(
        SqliteStore::new(data.clone()),
        wiring::AppOptions {
            version,
            public_url: cfg.public_url.clone(),
            encryption_key: cfg.encryption_key,
            reset_secrets: cfg.reset_secrets,
            data_dir: dir.root().to_path_buf(),
        },
    )
    .await
    .map_err(Fatal::startup)?;
    tracing::info!(
        collections = app.schema.get().schema.all().len(),
        "schema loaded"
    );

    // Step 8: first superuser (SPEC §5.5 step 4). The instance id and the
    // default `users` collection were created while loading.
    let mut setup_token = None;
    if let Some((email, password)) = &cfg.superuser {
        match app.ensure_superuser(email, password).await {
            Ok(true) => {
                tracing::info!(email = %email, "created the first superuser from the environment")
            }
            Ok(false) => {}
            Err(e) => {
                return Err(Fatal::startup(format!(
                    "RACKEND_SUPERUSER_EMAIL/PASSWORD: {e}"
                )));
            }
        }
    } else {
        setup_token = app
            .setup_token_if_needed()
            .await
            .map_err(|e| Fatal::startup(format!("cannot create the setup token: {e}")))?;
    }

    // Step 9: background jobs. Signing keys first: RSA generation is slow,
    // and nothing in this version needs the keys during the first requests.
    let keys = {
        let app = Arc::clone(&app);
        tokio::spawn(async move {
            if let Err(e) = app.ensure_signing_keys().await {
                tracing::error!(error = %e, "cannot generate signing keys");
            }
        })
    };
    let optimize = {
        let (data, logs) = (data.clone(), logs.db().clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(OPTIMIZE_EVERY);
            tick.tick().await; // the first tick fires immediately
            loop {
                tick.tick().await;
                for db in [&data, &logs] {
                    if let Err(e) = db.optimize().await {
                        tracing::warn!(error = %e, db = %db.path().display(), "PRAGMA optimize failed");
                    }
                }
            }
        })
    };
    let files_gc = {
        let app = Arc::clone(&app);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(FILES_GC_EVERY);
            loop {
                tick.tick().await;
                match app
                    .files_gc(rackend_core::usecases::files::ORPHAN_MIN_AGE)
                    .await
                {
                    Ok(report) => tracing::info!(%report, "file garbage collection done"),
                    Err(e) => tracing::warn!(error = %e, "file garbage collection failed"),
                }
                match app.thumbs_gc().await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(deleted = n, "trimmed the thumbnail cache"),
                    Err(e) => tracing::warn!(error = %e, "thumbnail cache GC failed"),
                }
            }
        })
    };
    let maintenance = {
        let app = Arc::clone(&app);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(MAINTENANCE_EVERY);
            loop {
                tick.tick().await;
                match app.job_purge_trash().await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(purged = n, "purged trashed fields/collections"),
                    Err(e) => tracing::warn!(error = %e, "trash purge failed"),
                }
                match app.job_audit_retention().await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(deleted = n, "deleted expired audit entries"),
                    Err(e) => tracing::warn!(error = %e, "audit retention failed"),
                }
                if let Err(e) = app.job_session_gc().await {
                    tracing::warn!(error = %e, "session cleanup failed");
                }
            }
        })
    };

    // Step 10: bind HTTP.
    let listener = TcpListener::bind(cfg.http_addr)
        .await
        .map_err(|e| Fatal::startup(format!("cannot listen on {}: {e}", cfg.http_addr)))?;
    let addr = listener.local_addr().unwrap_or(cfg.http_addr);
    let router = wiring::router(
        Arc::clone(&app),
        rackend_http::HttpOptions {
            dev: cfg.dev,
            trusted_proxies: cfg.trusted_proxies.clone(),
            tmp_dir: Some(dir.tmp()),
        },
    );
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let mut server = tokio::spawn(rackend_http::serve(listener, router, async {
        let _ = stop_rx.await;
    }));
    tracing::info!(%addr, version, "rackend is ready");
    if let Some(token) = setup_token {
        let host = if addr.ip().is_unspecified() {
            format!("localhost:{}", addr.port())
        } else {
            addr.to_string()
        };
        tracing::warn!("Open http://{host}/_/setup?token={token} to create the first superuser.");
    }

    let server_result = tokio::select! {
        () = shutdown_signal() => None,
        res = &mut server => Some(res),
    };

    // Shutdown: stop accepting, wait for in-flight requests (bounded), drain
    // the writer queues, checkpoint, release the lock.
    tracing::info!("shutting down");
    let mut failure = None;
    match server_result {
        Some(Ok(Ok(()))) => {}
        Some(Ok(Err(e))) => failure = Some(Fatal::startup(format!("HTTP server failed: {e}"))),
        Some(Err(e)) => failure = Some(Fatal::startup(format!("HTTP server task failed: {e}"))),
        None => {
            let _ = stop_tx.send(());
            if tokio::time::timeout(SHUTDOWN_GRACE, &mut server)
                .await
                .is_err()
            {
                tracing::warn!("in-flight requests did not finish within 10 s; closing them");
                server.abort();
            }
        }
    }
    keys.abort();
    files_gc.abort();
    optimize.abort();
    maintenance.abort();
    if let Err(e) = data.shutdown().await {
        tracing::error!(error = %e, "data.db shutdown failed");
    }
    if let Err(e) = logs.db().shutdown().await {
        tracing::warn!(error = %e, "logs.db shutdown failed");
    }
    dir.release();
    match failure {
        Some(f) => Err(f),
        None => {
            tracing::info!("stopped");
            Ok(())
        }
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot listen for SIGTERM; only Ctrl-C stops the server");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
