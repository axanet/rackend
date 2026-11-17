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
    };
    ExitCode::from(code)
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
    if let Err(e) = logging::init(&cfg.log_level, cfg.log_format) {
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
