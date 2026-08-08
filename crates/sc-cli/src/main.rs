//! The saltcorn binary: serve and management commands (layer 10).
//!
//! The entry point runs on the **tokio** runtime (the workspace-wide runtime
//! decision), since the server it launches is async top to bottom. It exposes
//! the `serve` command, which connects the primary database, initialises the
//! [`Catalog`](sc_catalog::Catalog), and starts the HTTP server with the admin
//! API mounted. The database connection is configured by [`DbConfig`] — flags,
//! the environment, or the environment selected with `--environment` out of the
//! `saltcorn.toml` configuration file; the remaining flags configure the HTTP
//! server itself ([`ServerConfig`]). The reusable boot logic lives in the crate
//! library ([`sc_cli`]).

use std::process::ExitCode;
use std::sync::Arc;

use sc_api::admin_endpoints;
use sc_app::{
    app_source_from_config, build_application, load_application_by_subdomain, save_application,
};
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
        Some("api") => api_command(&args[1..]).await,
        Some(other) => Err(sc_error::Error::config(format!(
            "unknown command `{other}`"
        ))),
        None => {
            print_usage();
            Ok(())
        }
    }
}

/// The file-store IDE's bundle, in the checkout this binary was built in.
///
/// A hard-coded path, deliberately. The IDE is not a deployment choice — it is
/// where an admin edits an application's source, reached from a button in the
/// admin UI — so there is nothing for an operator to decide and no flag to forget:
/// the default build carries the bundle's path, and one built with
/// `SC_BUILD_ADMIN=0` finds `ui/ide/dist` next to the source it was compiled from.
const IDE_BUNDLE_IN_CHECKOUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../ui/ide/dist");

/// Where the IDE bundle is: the one built into this binary, else the checkout's.
///
/// `None` only when neither exists — a binary built with `SC_BUILD_ADMIN=0` and run
/// away from its source tree, which has no admin UI to reach the IDE from either.
fn ide_bundle_dir() -> Option<std::path::PathBuf> {
    let candidate = option_env!("SC_IDE_BUNDLE_DIR").unwrap_or(IDE_BUNDLE_IN_CHECKOUT);
    let path = std::path::PathBuf::from(candidate);
    path.join("index.html").exists().then_some(path)
}

/// `saltcorn serve [--database-url URL | --db-host H ...] [--bind ADDR] [...]`.
///
/// Database flags are consumed by [`DbConfig::extract`]; whatever is left over is
/// parsed as [`ServerConfig`], so a typo in either still fails loudly.
async fn serve_command(args: &[String]) -> Result<()> {
    let (db, rest) = DbConfig::extract(args)?;
    let (file_store_specs, server_args) = extract_file_stores(rest)?;
    let mut config = ServerConfig::from_args(server_args)?;
    // When the binary was built with the admin bundle (the default — see
    // `build.rs`) and no explicit `--static-dir` was given, serve that bundle.
    if config.static_dir.is_none() {
        if let Some(dir) = option_env!("SC_ADMIN_BUNDLE_DIR") {
            config.static_dir = Some(std::path::PathBuf::from(dir));
        }
    }
    config.ide_dir = ide_bundle_dir();

    // Which database this process is about to write to is the one fact worth
    // saying out loud before anything happens — an operator running three
    // environments off one binary should be able to see, in the log, that this
    // one is staging.
    if let Some(source) = db.source() {
        eprintln!("saltcorn: database configured from {source}");
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

    // Agents: the built-in trait set and the two tables an agent and its runs
    // live in (§11.2). A stored agent that does not validate is reported and
    // dropped from the live set, exactly as a trigger that does not is — the
    // rest of the server works and the admin can repair it in the UI. It comes
    // before the triggers because `run_agent` is one of the actions a trigger
    // may name (§11.5), and it needs the assembled trait set.
    let agents = sc_server::install_agents(&catalog).await?;

    // Triggers: the built-in actions plus `run_agent`, the stored trigger set,
    // and the dispatcher installed into the catalog — after which a row write
    // raises an event. Before it, nothing observes writes, which is what keeps
    // `build-app` and every other command from firing anything. It comes before
    // the mounts because the mount registry carries the dispatcher: an app's
    // login and its errors raise events through the same router the admin API's
    // do.
    let triggers = sc_server::install_triggers(&catalog, evaluator.clone(), &agents).await?;

    let apps = Arc::new(
        AppMounts::new(catalog.clone())
            .with_evaluator(evaluator)
            .with_triggers(triggers.clone())
            .with_agents(agents),
    );
    if config.base_domain.is_some() {
        mount_all(&apps).await;
    }

    // Everything is up — catalog, file stores, applications, triggers — and the
    // listener has not been announced yet, which is exactly what the `startup`
    // event means.
    sc_server::fire_startup(&catalog, &triggers).await;

    // The clock's turn: from here a periodic trigger fires on its own schedule,
    // and one whose run was missed while the process was down catches up — once —
    // on the first tick. Held for the lifetime of `serve`; the task ends with the
    // process.
    let (_scheduler, _scheduler_task) = sc_server::start_scheduler(&catalog, &triggers);

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

    if let Some(source) = db.source() {
        eprintln!("saltcorn: database configured from {source}");
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
    let report = build_application(&catalog, &app, &source, None).await?;

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

/// `saltcorn api SUBCOMMAND …` — an application's custom SQL queries from the
/// command line (§13.4).
///
/// Three subcommands rather than one, because an add-only command is a trap: the
/// first typo would need a browser to fix, which is precisely the situation this
/// command exists to avoid.
async fn api_command(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("add-query") => add_query_command(&args[1..]).await,
        Some("list-queries") => list_queries_command(&args[1..]).await,
        Some("remove-query") => remove_query_command(&args[1..]).await,
        Some(other) => Err(sc_error::Error::config(format!(
            "unknown api subcommand `{other}`; there are add-query, list-queries \
             and remove-query"
        ))),
        None => Err(sc_error::Error::config(
            "api needs a subcommand: add-query, list-queries or remove-query",
        )),
    }
}

/// Connect the database and the file stores the way `build-app` does, and load
/// the application named by `--app`.
///
/// The file stores are connected because the command **re-emits the generated
/// client**, which is written through the app's source store — a command that
/// changed the API and left the client describing the old one would be the drift
/// §13.1 exists to prevent, introduced by the tool meant to avoid it.
async fn open_app(
    subdomain: &str,
    db: &DbConfig,
    file_stores: &[String],
) -> Result<(std::sync::Arc<sc_catalog::Catalog>, sc_app::Application)> {
    if let Some(source) = db.source() {
        eprintln!("saltcorn: database configured from {source}");
    }
    let catalog = connect_catalog(db).await?;
    connect_stored_file_stores(&catalog).await?;
    connect_file_stores(&catalog, file_stores)?;
    let app = load_application_by_subdomain(&catalog, subdomain)
        .await?
        .ok_or_else(|| {
            sc_error::Error::not_found(format!("no application with subdomain `{subdomain}`"))
        })?;
    Ok((catalog, app))
}

/// Re-emit `app`'s generated client, reporting what was written.
///
/// A failure here is **reported, not fatal**: the query is already saved, and
/// exiting non-zero would say the opposite. What the message has to carry is
/// which half happened, so nobody goes looking for a client method that was
/// never written — an unreachable store or an app with no client path is a
/// configuration to fix, not a query to add again.
async fn reemit_client(catalog: &sc_catalog::Catalog, app: &sc_app::Application) {
    match sc_app::emit_app_client(catalog, app, None).await {
        Ok(written) if written.is_empty() => {
            eprintln!(
                "saltcorn: application `{}` generates no client, so nothing was \
                 rewritten",
                app.subdomain
            );
        }
        Ok(written) => {
            eprintln!("saltcorn: rewrote {}", written.join(", "));
        }
        Err(e) => {
            eprintln!(
                "saltcorn: the query was saved, but the generated client could not \
                 be rewritten: {e}"
            );
        }
    }
}

/// `saltcorn api add-query --app SUBDOMAIN [--api MOUNT] --name … --path … --sql …`.
///
/// Validates by **preparing** — the same call the admin UI's check button and
/// every save make — so a query that will not prepare exits non-zero carrying
/// Postgres's own message, and the stored application is untouched.
async fn add_query_command(args: &[String]) -> Result<()> {
    let (db, rest) = DbConfig::extract(args)?;
    let (file_stores, rest) = extract_file_stores(rest)?;
    let parsed = sc_cli::api::parse_add_query(&rest)?;

    let (catalog, mut app) = open_app(&parsed.app, &db, &file_stores).await?;
    let api = sc_cli::api::select_api(&mut app, parsed.api.as_deref())?;
    let mut queries = sc_api::custom_queries(&api.config)?;
    if queries.iter().any(|q| q.name == parsed.query.name) {
        return Err(sc_error::Error::invalid(format!(
            "application `{}` already has a custom query named `{}`; remove it \
             first (saltcorn api remove-query) or use another name",
            parsed.app, parsed.query.name
        )));
    }
    let name = parsed.query.name.clone();
    let mount = api.mount.clone();
    queries.push(parsed.query);
    sc_api::set_custom_queries(&mut api.config, &queries)?;

    // The save is the validation: it prepares every query the app declares and
    // stores the columns the database reported. A refusal leaves the stored row
    // exactly as it was, because nothing is written until every query prepares.
    let saved = save_application(&catalog, &app).await?;
    eprintln!(
        "saltcorn: added `{name}` to the API at {mount} of `{}`",
        parsed.app
    );
    print_query_columns(&saved, &mount, &name)?;
    reemit_client(&catalog, &saved).await;
    Ok(())
}

/// Print what the database said the newly-saved query returns — the same answer
/// the admin UI shows, because it is the same stored value, and the shape the
/// generated client method now has.
fn print_query_columns(app: &sc_app::Application, mount: &str, name: &str) -> Result<()> {
    let Some(api) = app.apis.iter().find(|a| a.mount == mount) else {
        return Ok(());
    };
    let Some(query) = sc_api::custom_queries(&api.config)?
        .into_iter()
        .find(|q| q.name == name)
    else {
        return Ok(());
    };
    if query.columns.is_empty() {
        println!("{name}: returns no columns");
    } else {
        println!(
            "{name}: returns {}",
            query
                .columns
                .iter()
                .map(|c| format!("{} ({})", c.name, c.ty.name()))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

/// `saltcorn api list-queries --app SUBDOMAIN [--api MOUNT]`.
async fn list_queries_command(args: &[String]) -> Result<()> {
    let (db, rest) = DbConfig::extract(args)?;
    let (file_stores, rest) = extract_file_stores(rest)?;
    let parsed = sc_cli::api::parse_query_ref("list-queries", &rest)?;

    let (_catalog, app) = open_app(&parsed.app, &db, &file_stores).await?;
    let mut found = 0;
    for api in &app.apis {
        if let Some(mount) = &parsed.api
            && api.mount != *mount
        {
            continue;
        }
        for query in sc_api::custom_queries(&api.config)? {
            found += 1;
            println!(
                "{} {}{}  {} (min role {})",
                query.method.as_str(),
                api.mount,
                query.path,
                query.name,
                query.min_role
            );
            if !query.description.is_empty() {
                println!("    {}", query.description);
            }
            if !query.params.is_empty() {
                println!(
                    "    parameters: {}",
                    query
                        .params
                        .iter()
                        .map(|p| format!(
                            "{}: {}{}",
                            p.name,
                            p.ty.name(),
                            if p.required { "" } else { " (optional)" }
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if !query.columns.is_empty() {
                println!(
                    "    returns: {}",
                    query
                        .columns
                        .iter()
                        .map(|c| format!("{}: {}", c.name, c.ty.name()))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }
    if found == 0 {
        eprintln!(
            "saltcorn: application `{}` has no custom SQL queries",
            parsed.app
        );
    }
    Ok(())
}

/// `saltcorn api remove-query --app SUBDOMAIN [--api MOUNT] --name NAME`.
async fn remove_query_command(args: &[String]) -> Result<()> {
    let (db, rest) = DbConfig::extract(args)?;
    let (file_stores, rest) = extract_file_stores(rest)?;
    let parsed = sc_cli::api::parse_query_ref("remove-query", &rest)?;
    let name = parsed.name.clone().ok_or_else(|| {
        sc_error::Error::config("remove-query needs --name: which query to remove")
    })?;

    let (catalog, mut app) = open_app(&parsed.app, &db, &file_stores).await?;
    let api = sc_cli::api::select_api(&mut app, parsed.api.as_deref())?;
    let mount = api.mount.clone();
    let mut queries = sc_api::custom_queries(&api.config)?;
    let before = queries.len();
    queries.retain(|q| q.name != name);
    if queries.len() == before {
        // Naming what is there, because "no such query" with a list is a typo
        // fixed in one step and without it is a second command to find out.
        return Err(sc_error::Error::not_found(format!(
            "the API at {mount} of `{}` has no custom query named `{name}`; it has {}",
            parsed.app,
            if before == 0 {
                "none".to_owned()
            } else {
                sc_api::custom_queries(&api.config)?
                    .iter()
                    .map(|q| format!("`{}`", q.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )));
    }
    sc_api::set_custom_queries(&mut api.config, &queries)?;
    let saved = save_application(&catalog, &app).await?;
    eprintln!(
        "saltcorn: removed `{name}` from the API at {mount} of `{}`",
        parsed.app
    );
    reemit_client(&catalog, &saved).await;
    Ok(())
}

/// Print the short usage summary.
fn print_usage() {
    eprintln!("saltcorn — usage:");
    eprintln!("  saltcorn serve [database flags] [server flags]");
    eprintln!("  saltcorn build-app SUBDOMAIN [database flags] [--file-store NAME=PATH]");
    eprintln!("  saltcorn api add-query --app SUBDOMAIN [--api MOUNT] --name NAME");
    eprintln!(
        "                        [--method GET] --path /sub/path [--min-role N] \
         [--description TEXT]"
    );
    eprintln!("                        [--param name:type[,name:type…]]… --sql TEXT|@FILE");
    eprintln!("  saltcorn api list-queries --app SUBDOMAIN [--api MOUNT]");
    eprintln!("  saltcorn api remove-query --app SUBDOMAIN [--api MOUNT] --name NAME");
    eprintln!();
    eprintln!("  database (or the DATABASE_URL / PG* environment variables):");
    eprintln!("    --database-url URL   full connection string (takes precedence)");
    eprintln!("    --db-host H  --db-port N  --db-user U  --db-password P  --db-name D");
    eprintln!();
    eprintln!("  configuration file (used for whatever the flags and environment leave unset):");
    eprintln!(
        "    --environment NAME   which [environments.NAME] section to connect with \
         (or SALTCORN_ENV);"
    );
    eprintln!("                         naming one makes it outrank DATABASE_URL / PG*");
    eprintln!("    --config PATH        read this file instead of searching (or SALTCORN_CONFIG)");
    for path in sc_cli::config_file::search_paths() {
        eprintln!("                         searched: {}", path.display());
    }
    eprintln!();
    eprintln!(
        "  build-app: builds one application and prints the bundler's output.

  api: adds, lists and removes an application's custom SQL queries. A query is
       validated by preparing it, so one that will not prepare is refused with
       the database's own message and nothing is stored; adding or removing one
       rewrites the application's generated client. A parameter is written
       `name:type`, or `name:type?` when the caller may leave it out. Every
       query has a minimum role, and it is **admin** unless --min-role says
       otherwise: raw SQL does not go through the row layer, so ownership
       formulae do not filter what it returns.

  server:"
    );
    eprintln!("    --bind ADDR  --static-dir DIR  --session-ttl-hours N  --secure-cookies");
    eprintln!(
        "    --file-store NAME=PATH   connect a local directory as a named file store (repeatable)"
    );
}
