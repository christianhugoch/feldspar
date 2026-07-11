//! The saltcorn binary: serve and management commands (layer 10).
//!
//! The entry point runs on the **tokio** runtime (the workspace-wide runtime
//! decision), since the server it launches is async top to bottom. So far it
//! exposes the `serve` command, which builds a [`ServerConfig`] from the CLI and
//! starts the HTTP server. The admin API endpoints are mounted through
//! `sc-api`'s [`admin_endpoints`]; their handlers land in the second half of the
//! Phase 6 server subphase, so until then unimplemented routes answer `501`.

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
    let config = ServerConfig::from_args(args)?;
    let sessions = Arc::new(SessionStore::default());
    eprintln!("saltcorn: listening on http://{}", config.addr);
    serve(config, admin_endpoints(), HandlerRegistry::new(), sessions).await
}

/// Print the short usage summary.
fn print_usage() {
    eprintln!("saltcorn — usage:");
    eprintln!("  saltcorn serve [--bind ADDR] [--static-dir DIR] \\");
    eprintln!("                 [--session-ttl-hours N] [--secure-cookies]");
}
