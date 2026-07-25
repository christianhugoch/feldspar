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
use sc_app::{app_source_from_config, build_application, load_application_by_subdomain};
use sc_auth::SessionStore;
use sc_cli::DbConfig;
use sc_cli::{
    connect_catalog, connect_file_stores, connect_stored_file_stores, extract_file_stores,
};
use sc_error::Result;
use sc_server::{AppMounts, ServerConfig, admin_handlers, mount_all, serve};

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
        Some("build-app") => build_app_command(&args[1..]).await,
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
    let (db, rest) = DbConfig::extract(args)?;
    let (file_store_specs, server_args) = extract_file_stores(rest)?;
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
    // Connect the file stores configured in the admin UI. One that fails — a
    // disk unmounted since it was defined — is logged and skipped, not fatal;
    // it stays listed and editable so the admin can repoint it.
    connect_stored_file_stores(&catalog).await?;
    // Then any requested with `--file-store NAME=PATH`, which are ephemeral and
    // must not silently shadow a configured store of the same name.
    connect_file_stores(&catalog, &file_store_specs)?;

    // Bring the applications up. `--base-domain` is what makes them addressable
    // (an app is served at `<subdomain>.<base-domain>`), so mounting is gated on
    // it: without a base domain no request could ever reach an app, and mounting
    // one would make the router refuse to build. With one, every stored app is
    // built and mounted now — a build that fails is logged and skipped, never
    // fatal (§13.2), and can be fixed and rebuilt without a restart.
    // The JS engine ownership formulas evaluate on (§7.3): one isolate for the
    // whole server, shared by every mounted app's providers — and by the trigger
    // dispatcher, whose `only_if` formulas and action configuration are the same
    // language evaluated the same way.
    let evaluator = sc_server::default_js_evaluator();

    // Triggers: the built-in actions, the stored trigger set, and the dispatcher
    // installed into the catalog — after which a row write raises an event.
    // Before it, nothing observes writes, which is what keeps `build-app` and
    // every other command from firing anything. It comes before the mounts
    // because the mount registry carries the dispatcher: an app's login and its
    // errors raise events through the same router the admin API's do.
    let triggers = sc_server::install_triggers(&catalog, evaluator.clone()).await?;

    let apps = Arc::new(
        AppMounts::new(catalog.clone())
            .with_evaluator(evaluator)
            .with_triggers(triggers.clone()),
    );
    if config.base_domain.is_some() {
        mount_all(&apps).await;
    }

    // Everything is up — catalog, file stores, applications, triggers — and the
    // listener has not been announced yet, which is exactly what the `startup`
    // event means.
    sc_server::fire_startup(&catalog, &triggers).await;

    let sessions = Arc::new(SessionStore::default());
    eprintln!("saltcorn: listening on http://{}", config.addr);
    let handlers = admin_handlers(catalog, apps.clone());
    serve(config, admin_endpoints(), handlers, sessions, apps).await
}

/// `saltcorn build-app SUBDOMAIN [database flags] [--file-store NAME=PATH]`.
///
/// Builds one application from the command line, printing the tool output as it
/// goes and failing with the bundler's own diagnostics.
///
/// The admin UI can already build an app, and this does the same work — so why
/// have it? Because when a build fails, the UI shows the *result* and this shows
/// the *run*: it is scriptable, it is what a deploy step or a CI job calls, and
/// its output goes to a terminal where it can be piped, grepped and kept. It also
/// works when the app cannot be reached in a browser at all, which is precisely
/// the state a failing build tends to leave a deployment in.
///
/// Deliberately **builds without mounting**: nothing is served by this process,
/// so running it against a live deployment's database cannot disturb what that
/// server is serving. The next build or restart there picks up the output.
async fn build_app_command(args: &[String]) -> Result<()> {
    let (subdomain, rest) = match args.split_first() {
        Some((first, rest)) if !first.starts_with('-') => (first.clone(), rest.to_vec()),
        _ => {
            return Err(sc_error::Error::config(
                "build-app requires the application's subdomain: \
                 saltcorn build-app SUBDOMAIN [database flags]",
            ));
        }
    };
    let (db, rest) = DbConfig::extract(rest)?;
    let (file_store_specs, leftover) = extract_file_stores(rest)?;
    if let Some(unknown) = leftover.first() {
        return Err(sc_error::Error::config(format!(
            "unknown build-app argument `{unknown}`"
        )));
    }

    let catalog = connect_catalog(&db).await?;
    connect_stored_file_stores(&catalog).await?;
    connect_file_stores(&catalog, &file_store_specs)?;

    let app = load_application_by_subdomain(&catalog, &subdomain)
        .await?
        .ok_or_else(|| {
            sc_error::Error::not_found(format!("no application with subdomain `{subdomain}`"))
        })?;

    eprintln!(
        "saltcorn: building application `{}` ({})",
        app.name, subdomain
    );
    let source = app_source_from_config(&app.framework)?;
    let report = build_application(&catalog, &app, &source).await?;

    // The tool output is the point of running this here rather than clicking
    // Build, so it goes to stdout whole — not the tail an error message can
    // carry, and not summarised.
    if let Some(log) = &report.install_log {
        print!("{log}");
    }
    print!("{}", report.stdout);
    eprint!("{}", report.stderr);

    eprintln!(
        "saltcorn: built {} file{} into {}{}",
        report.bundle.len(),
        if report.bundle.len() == 1 { "" } else { "s" },
        report.output_dir.display(),
        if report.installed {
            " (dependencies installed)"
        } else {
            ""
        }
    );
    Ok(())
}

/// Print the short usage summary.
fn print_usage() {
    eprintln!("saltcorn — usage:");
    eprintln!("  saltcorn serve [database flags] [server flags]");
    eprintln!("  saltcorn build-app SUBDOMAIN [database flags] [--file-store NAME=PATH]");
    eprintln!();
    eprintln!("  database (or the DATABASE_URL / PG* environment variables):");
    eprintln!("    --database-url URL   full connection string (takes precedence)");
    eprintln!("    --db-host H  --db-port N  --db-user U  --db-password P  --db-name D");
    eprintln!();
    eprintln!(
        "  build-app: builds one application and prints the bundler's output.

  server:"
    );
    eprintln!("    --bind ADDR  --static-dir DIR  --session-ttl-hours N  --secure-cookies");
    eprintln!(
        "    --file-store NAME=PATH   connect a local directory as a named file store (repeatable)"
    );
}
