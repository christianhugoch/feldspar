//! The saltcorn binary: serve and management commands (layer 10).
//!
//! The entry point runs on the **tokio** runtime (the workspace-wide runtime
//! decision), since the server it launches is async top to bottom. So far it
//! exposes the `serve` command, which builds a [`ServerConfig`] from the CLI and
//! starts the HTTP server. The admin API endpoints are mounted through
//! `sc-api`'s [`admin_endpoints`]; their handlers exist in `sc-server`
//! ([`admin_handlers`](sc_server::admin_handlers)) but need a connected
//! [`Catalog`](sc_catalog::Catalog) to run. Wiring the DB connection config into
//! the CLI is Phase 9, so for now `serve` mounts an empty registry and the admin
//! routes answer `501` until that lands.

use std::process::ExitCode;
use std::sync::Arc;

use sc_api::admin_endpoints;
use sc_auth::SessionStore;
use sc_error::Result;
use sc_server::{HandlerRegistry, ServerConfig, serve};

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

/// `saltcorn serve [--bind ADDR] [--static-dir DIR] [...]`.
async fn serve_command(args: &[String]) -> Result<()> {
    let mut config = ServerConfig::from_args(args)?;
    // When the binary was built with the admin bundle (`SC_BUILD_ADMIN=1`, see
    // `build.rs`) and no explicit `--static-dir` was given, serve that bundle.
    if config.static_dir.is_none() {
        if let Some(dir) = option_env!("SC_ADMIN_BUNDLE_DIR") {
            config.static_dir = Some(std::path::PathBuf::from(dir));
        }
    }
    let sessions = Arc::new(SessionStore::default());
    eprintln!("saltcorn: listening on http://{}", config.addr);
    // Handlers need a connected Catalog; DB-connection config is Phase 9, so the
    // registry is empty for now (admin routes answer 501 until then).
    serve(config, admin_endpoints(), HandlerRegistry::new(), sessions).await
}

/// Print the short usage summary.
fn print_usage() {
    eprintln!("saltcorn — usage:");
    eprintln!("  saltcorn serve [--bind ADDR] [--static-dir DIR] \\");
    eprintln!("                 [--session-ttl-hours N] [--secure-cookies]");
}
