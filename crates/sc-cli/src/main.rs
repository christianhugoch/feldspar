//! The saltcorn binary: serve and management commands (layer 10).
//!
//! The entry point runs on the **tokio** runtime (the workspace-wide runtime
//! decision), since the server it launches is async top to bottom. It exposes
//! the `serve` command, which connects the primary database, initialises the
//! [`Catalog`](sc_catalog::Catalog), and starts the HTTP server with the admin
//! API mounted. The database connection is configured by [`DbConfig`] (flags or
//! the environment); the remaining flags configure the HTTP server itself
//! ([`ServerConfig`]). The reusable boot logic lives in the crate library
//! ([`sc_cli`]).

use std::process::ExitCode;
use std::sync::Arc;

use sc_api::admin_endpoints;
use sc_auth::SessionStore;
use sc_cli::DbConfig;
use sc_cli::connect_catalog;
use sc_error::Result;
use sc_server::{ServerConfig, admin_handlers, serve};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Dispatch a subcommand.
async fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("serve") => serve_command(&args[1..]).await,
        Some(other) => Err(sc_error::Error::config(format!(
            "unknown command `{other}`"
        ))),
        None => {
            print_usage();
            Ok(())
        }
    }
}

/// `saltcorn serve [--database-url URL | --db-host H ...] [--bind ADDR] [...]`.
///
/// Database flags are consumed by [`DbConfig::extract`]; whatever is left over is
/// parsed as [`ServerConfig`], so a typo in either still fails loudly.
async fn serve_command(args: &[String]) -> Result<()> {
    let (db, server_args) = DbConfig::extract(args)?;
    let mut config = ServerConfig::from_args(server_args)?;
    // When the binary was built with the admin bundle (`SC_BUILD_ADMIN=1`, see
    // `build.rs`) and no explicit `--static-dir` was given, serve that bundle.
    if config.static_dir.is_none() {
        if let Some(dir) = option_env!("SC_ADMIN_BUNDLE_DIR") {
            config.static_dir = Some(std::path::PathBuf::from(dir));
        }
    }

    // Bring the data layer up before binding: connect the database, load the
    // catalog, and ensure the users table exists. A bad connection fails here
    // with a clear message rather than a server that boots then 500s.
    let catalog = connect_catalog(&db).await?;

    let sessions = Arc::new(SessionStore::default());
    eprintln!("saltcorn: listening on http://{}", config.addr);
    serve(config, admin_endpoints(), admin_handlers(catalog), sessions).await
}

/// Print the short usage summary.
fn print_usage() {
    eprintln!("saltcorn — usage:");
    eprintln!("  saltcorn serve [database flags] [server flags]");
    eprintln!();
    eprintln!("  database (or the DATABASE_URL / PG* environment variables):");
    eprintln!("    --database-url URL   full connection string (takes precedence)");
    eprintln!("    --db-host H  --db-port N  --db-user U  --db-password P  --db-name D");
    eprintln!();
    eprintln!("  server:");
    eprintln!("    --bind ADDR  --static-dir DIR  --session-ttl-hours N  --secure-cookies");
}
