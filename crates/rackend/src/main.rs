//! The `rackend` binary: CLI and composition root (SPEC §4.1, §5.1).

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use rackend::config::Config;
use rackend::{exit, healthcheck, logging, serve};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_COMMIT: &str = env!("RACKEND_GIT_COMMIT");
const BUILD_DATE: &str = env!("RACKEND_BUILD_DATE");

/// rackend: a self-hosted, single-binary backend.
///
/// Configuration comes from a few RACKEND_* environment variables; everything
/// else is configured at runtime in the admin UI.
#[derive(Debug, Parser)]
#[command(name = "rackend", version = VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the server (default).
    Serve,
    /// Exit 0 if GET /api/health returns 200 (for Docker HEALTHCHECK).
    Healthcheck,
    /// Print version, git commit, build date, SQLite version and features.
    Version,
    /// Inspect database migrations.
    Migrate {
        #[command(subcommand)]
        command: MigrateCommand,
    },
    /// Create, list and restore backups.
    Backup {
        #[command(subcommand)]
        command: BackupCommand,
    },
    /// Database integrity, foreign keys and stored files. Read only.
    Check,
    /// The OpenAPI document of this instance.
    Openapi {
        #[command(subcommand)]
        command: OpenapiCommand,
    },
}

#[derive(Debug, Subcommand)]
enum OpenapiCommand {
    /// Write the document to stdout. Opens the database read only, so it
    /// also works while the server runs.
    Export {
        /// OpenAPI version: 3.1 or 3.0.
        #[arg(long, default_value = "3.1")]
        version: String,
        /// Include superuser and admin endpoints.
        #[arg(long)]
        include_admin: bool,
        /// json or yaml.
        #[arg(long, default_value = "json")]
        format: String,
    },
}

#[derive(Debug, Subcommand)]
enum BackupCommand {
    /// Write a backup now (works while the server runs).
    Create {
        /// Leave out storage/ (uploaded files).
        #[arg(long)]
        no_storage: bool,
    },
    /// List backups, newest first.
    List,
    /// Restore a backup. The server must be stopped.
    Restore {
        /// Archive path, or a name from `backup list`.
        file: std::path::PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum MigrateCommand {
    /// Show applied and pending migrations. Never applies anything.
    Status,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(),
        Command::Healthcheck => healthcheck(),
        Command::Version => {
            print_version();
            exit::OK
        }
        Command::Migrate {
            command: MigrateCommand::Status,
        } => migrate_status(),
        Command::Backup { command } => backup(command),
        Command::Check => check(),
        Command::Openapi {
            command:
                OpenapiCommand::Export {
                    version,
                    include_admin,
                    format,
                },
        } => openapi_export(&version, include_admin, &format),
    };
    ExitCode::from(code)
}

fn openapi_export(version: &str, include_admin: bool, format: &str) -> u8 {
    use rackend::openapi_export::{Format, Options, export};
    let cfg = match load_config() {
        Ok(c) => c,
        Err(code) => return code,
    };
    let Some(version) = rackend_core::openapi::Version::parse(version) else {
        eprintln!("rackend openapi export: --version must be 3.1 or 3.0");
        return exit::FAILURE;
    };
    let format = match format {
        "json" => Format::Json,
        "yaml" | "yml" => Format::Yaml,
        _ => {
            eprintln!("rackend openapi export: --format must be json or yaml");
            return exit::FAILURE;
        }
    };
    let opts = Options {
        version,
        include_admin,
        format,
        public_url: cfg.public_url.clone(),
        app_version: VERSION.to_owned(),
    };
    match export(&cfg.data_dir, &opts) {
        Ok(text) => {
            print!("{text}");
            exit::OK
        }
        Err(e) => {
            eprintln!("rackend openapi export: {e}");
            exit::FAILURE
        }
    }
}

fn load_config() -> Result<Config, u8> {
    Config::from_env().map_err(|e| {
        eprintln!("rackend: invalid configuration: {e:#}");
        exit::FAILURE
    })
}

fn serve() -> u8 {
    let cfg = match load_config() {
        Ok(c) => c,
        Err(code) => return code,
    };
    if let Err(e) = logging::init(&cfg.log_level, cfg.log_format, cfg.otlp_endpoint.as_deref()) {
        eprintln!("rackend: {e:#}");
        return exit::FAILURE;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "cannot start the async runtime");
            return exit::FAILURE;
        }
    };
    match runtime.block_on(serve::run(cfg, VERSION)) {
        Ok(()) => exit::OK,
        Err(fatal) => {
            tracing::error!(exit_code = fatal.code, "{}", fatal.message);
            fatal.code
        }
    }
}

fn healthcheck() -> u8 {
    let cfg = match load_config() {
        Ok(c) => c,
        Err(code) => return code,
    };
    match healthcheck::check(cfg.http_addr) {
        Ok(()) => exit::OK,
        Err(e) => {
            eprintln!("rackend healthcheck: {e}");
            exit::FAILURE
        }
    }
}

fn print_version() {
    let features: Vec<&str> = Vec::new();
    println!("rackend {VERSION}");
    println!("commit:   {GIT_COMMIT}");
    println!("built:    {BUILD_DATE}");
    println!("sqlite:   {}", rackend_store_sqlite::sqlite_version());
    println!(
        "features: {}",
        if features.is_empty() {
            "none".to_owned()
        } else {
            features.join(", ")
        }
    );
}

fn migrate_status() -> u8 {
    let cfg = match load_config() {
        Ok(c) => c,
        Err(code) => return code,
    };
    let path = cfg.data_dir.join("data.db");
    if !path.is_file() {
        println!(
            "No database at {} (a new instance starts at schema v0).",
            path.display()
        );
        return exit::OK;
    }
    match rackend_store_sqlite::data_migration_status(&path) {
        Ok(s) => {
            println!("database:       {}", path.display());
            println!("schema version: v{}", s.schema_version);
            println!("binary knows:   v{}", s.binary_max);
            if s.schema_version > s.binary_max {
                println!(
                    "WARNING: the database is newer than this binary; it will refuse to start (exit 78)."
                );
            }
            println!("applied:");
            for a in &s.applied {
                println!(
                    "  {:04}_{}  {}  (by {})",
                    a.id, a.name, a.applied_at, a.binary_version
                );
            }
            println!("pending:");
            if s.pending.is_empty() {
                println!("  none");
            }
            for m in &s.pending {
                println!("  {:04}_{}", m.id, m.name);
            }
            exit::OK
        }
        Err(e) => {
            eprintln!("rackend migrate status: {e}");
            e.exit_code()
        }
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, u8> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            eprintln!("rackend: cannot start the async runtime: {e}");
            exit::FAILURE
        })
}

fn backup(command: BackupCommand) -> u8 {
    let cfg = match load_config() {
        Ok(c) => c,
        Err(code) => return code,
    };
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let backups = rackend::backup::FsBackups::new(&cfg.data_dir, None, VERSION);
    match command {
        BackupCommand::Create { no_storage } => {
            if !cfg.data_dir.join("data.db").is_file() {
                eprintln!("rackend: no database in {}", cfg.data_dir.display());
                return exit::FAILURE;
            }
            match rt.block_on(backups.create_now(!no_storage)) {
                Ok(info) => {
                    println!(
                        "{} ({} bytes{})",
                        info.name,
                        info.size,
                        if info.includes_storage {
                            ", with storage/"
                        } else {
                            ""
                        }
                    );
                    exit::OK
                }
                Err(e) => {
                    eprintln!("rackend: backup failed: {e}");
                    exit::FAILURE
                }
            }
        }
        BackupCommand::List => {
            use rackend_core::ports::Backups as _;
            match rt.block_on(backups.list()) {
                Ok(items) if items.is_empty() => {
                    println!("No backups in {}.", cfg.data_dir.join("backups").display());
                    exit::OK
                }
                Ok(items) => {
                    for b in items {
                        println!(
                            "{}  {:>12} bytes  {}{}",
                            b.name,
                            b.size,
                            b.created,
                            if b.includes_storage { "  storage" } else { "" }
                        );
                    }
                    exit::OK
                }
                Err(e) => {
                    eprintln!("rackend: {e}");
                    exit::FAILURE
                }
            }
        }
        BackupCommand::Restore { file } => {
            let archive = if file.is_file() {
                file
            } else {
                cfg.data_dir.join("backups").join(&file)
            };
            if !archive.is_file() {
                eprintln!("rackend: no backup at {}", archive.display());
                return exit::FAILURE;
            }
            // The lock proves the server is stopped (SPEC §18.4).
            let dir = match rackend::datadir::DataDir::prepare(&cfg.data_dir) {
                Ok(d) => d,
                Err(fatal) => {
                    eprintln!(
                        "rackend: {} (stop the server before restoring)",
                        fatal.message
                    );
                    return fatal.code;
                }
            };
            let out = rackend::backup::restore(dir.root(), &archive);
            dir.release();
            match out {
                Ok(r) => {
                    println!("Restored {} files from {}.", r.files, archive.display());
                    println!("The replaced files are in {}.", r.replaced.display());
                    println!("Start the server; an older database is migrated on start.");
                    exit::OK
                }
                Err(e) => {
                    eprintln!("rackend: restore failed, nothing was changed: {e}");
                    exit::FAILURE
                }
            }
        }
    }
}

fn check() -> u8 {
    let cfg = match load_config() {
        Ok(c) => c,
        Err(code) => return code,
    };
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    match rt.block_on(rackend::check::run(&cfg.data_dir, cfg.encryption_key)) {
        Ok(r) => {
            let line = |label: &str, items: &[String]| {
                if items.is_empty() {
                    println!("{label}: ok");
                } else {
                    println!(
                        "{label}: {} problem{}",
                        items.len(),
                        if items.len() == 1 { "" } else { "s" }
                    );
                    for i in items.iter().take(50) {
                        println!("  {i}");
                    }
                }
            };
            line("integrity", &r.integrity);
            line("foreign keys", &r.foreign_keys);
            match &r.storage_error {
                Some(e) => println!("storage: cannot read ({e})"),
                None => {
                    line("files without a stored blob", &r.missing_blobs);
                    println!(
                        "blobs no record uses: {} (the cleanup job removes them)",
                        r.orphan_blobs.len()
                    );
                }
            }
            if r.ok() { exit::OK } else { exit::FAILURE }
        }
        Err(e) => {
            eprintln!("rackend check: {e}");
            exit::FAILURE
        }
    }
}
