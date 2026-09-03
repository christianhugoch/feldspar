//! HTTP server (axum): mounts the typed API, serves the `ui/admin` React SPA
//! bundle, sessions + strict CSP (layer 9; technical design §12, §16).
//!
//! The admin UI is a React SPA over a typed JSON API — there is **no**
//! server-rendered admin HTML. This crate provides the server half:
//!
//! - [`ServerConfig`] — bind address, static bundle directory, cookie/session
//!   settings, parsed from the CLI ([`ServerConfig::from_args`]).
//! - [`HandlerRegistry`] / [`HandlerCtx`] / [`HandlerResponse`] — the name→code
//!   map that endpoint dispatch resolves against; handlers stay free of HTTP
//!   plumbing and manage sessions declaratively via [`SessionAction`].
//! - [`build_router`] — assembles an axum [`Router`](axum::Router) that dispatches
//!   every [`sc_api::Endpoint`] through a single [`matchit`] router (runtime
//!   routes included), enforces per-endpoint auth, applies strict CSP/security
//!   headers and CSRF protection, and serves the static bundle / bootstrap doc.
//! - [`serve`] — binds the listener and runs with graceful shutdown.
//!
//! The concrete admin handlers (login, tables, rows, users) live in
//! [`admin_handlers`]; they are resolved by name against the [`sc_api`] admin
//! endpoint set at dispatch time.

mod agents;
mod apps;
mod backup;
mod chat;
mod config;
mod handler;
mod handlers;
mod logging;
mod lsp;
mod modules;
mod reload;
mod router;
mod security;
mod serve;
mod systemd;
mod tls;
mod triggers;

pub use agents::{AgentServices, install_agents};
pub use apps::{AppMounts, MountedApp, build_and_mount, mount_all, subdomain_of};
pub use backup::{
    Available as BackupContents, BACKUP_CREATE_ROUTE, BACKUP_UPLOAD_ROUTE, BackupPreferences,
    RestoreReport, Selection as BackupSelection,
};
pub use chat::AGENT_CHAT_ROUTE;
pub use sc_agent::{ProviderConnector, StoredProviders};

/// The server's JavaScript evaluator: the `deno_core`-backed engine ownership
/// formulas' reified path runs on (§7.3). Constructed once at boot and shared —
/// `AppMounts::with_evaluator(default_js_evaluator())` — so every provider of
/// every mount evaluates on one isolate.
///
/// The code pool it lazily builds for `run_js_code` bodies takes its defaults;
/// [`js_evaluator`] is the same thing with a [`ServerConfig`]'s knobs applied.
pub fn default_js_evaluator() -> std::sync::Arc<dyn sc_expr::JsEvaluator> {
    js_evaluator(&ServerConfig::default())
}

/// The server's JavaScript evaluator, configured: `--code-workers` isolates in
/// the code pool, each admitting `--code-max-inflight` runs at once (§10.1).
///
/// The formula isolate is unaffected by either — it is one isolate serving
/// evaluations serially and has no host to wait on. The code pool is built on
/// first use, so a server that never fires a `run_js_code` trigger pays for
/// neither knob.
pub fn js_evaluator(config: &ServerConfig) -> std::sync::Arc<dyn sc_expr::JsEvaluator> {
    std::sync::Arc::new(
        sc_expr::DenoEvaluator::new()
            .with_code_workers(config.code_workers)
            .with_max_inflight(config.code_max_inflight),
    )
}
/// The server's **Python** adapter: the runtime a `run_python_code` body runs
/// on, with this process's knobs applied (§15, §7).
///
/// Built at boot and registered on the dispatcher by [`install_triggers`] —
/// whether or not this binary has an interpreter linked in, and whether or not
/// this process was started with `--python off`. Registering it either way is
/// what keeps a Python trigger's stored configuration meaningful across
/// deployments: what changes between the three cases is the sentence firing it
/// answers with, and every one of those sentences names its own remedy.
///
/// Constructing one starts nothing. The interpreter is started by the first body
/// that needs it, exactly as the code isolate pool is built by the first
/// `run_js_code` — so a server that fires no Python pays for no interpreter.
/// The concrete runtime rather than an `Arc<dyn CodeAdapter>`, because the
/// diagnostics screen (§7's three states, phase 4.2) asks it questions the
/// adapter trait has no business carrying: which state this process is in, where
/// its environment is, and how many runs are resident. It coerces to the trait
/// object where a dispatcher wants one.
pub fn python_adapter(config: &ServerConfig) -> std::sync::Arc<sc_python::PythonRuntime> {
    std::sync::Arc::new(
        sc_python::PythonRuntime::with_bounds(config.python_max_inflight, config.python_max_stuck)
            .with_enabled(config.python == config::PythonMode::Auto)
            .with_env(config.python_env.clone()),
    )
}

/// The Python adapter with this process's defaults — [`python_adapter`] of a
/// [`ServerConfig::default`], for the callers that have no configuration to hand
/// (tests, and any tool that boots a dispatcher without parsing flags).
pub fn default_python_adapter() -> std::sync::Arc<sc_python::PythonRuntime> {
    python_adapter(&ServerConfig::default())
}
pub use config::{DEFAULT_BIND, PythonMode, ServerConfig};
/// The guest-language adapter trait, re-exported: a caller that hands
/// [`install_triggers_with_adapters`] a set of adapters needs to name it, and
/// `sc-expr` is not otherwise its dependency.
pub use sc_expr::CodeAdapter;
pub use handler::{
    BoxFuture, HandlerCtx, HandlerFn, HandlerRegistry, HandlerResponse, SessionAction,
};
pub use handlers::admin_handlers;
pub use logging::log_requests;
pub use lsp::{LSP_ROUTE, MAX_LANGUAGE_SERVERS};
pub use modules::ModuleServices;
pub use reload::{ReloadReport, reload_all, spawn_sighup_reload};
pub use router::{
    BOOTSTRAP_HTML, CSRF_REQUEST_HEADER, IDE_PREFIX, build_router, build_router_with_apps,
};
pub use security::IDE_CONTENT_SECURITY_POLICY;
pub use security::{CONTENT_SECURITY_POLICY, CSRF_COOKIE, CSRF_HEADER, SESSION_COOKIE};
pub use serve::serve;
pub use systemd::ServiceManager;
pub use tls::{
    TlsHandle, TlsSettings, check_certificate, https_addr, install_crypto_provider,
    redirect_router, serve_https, tls_domains,
};
pub use triggers::{
    base_action_registry, fire_startup, install_triggers, install_triggers_with_adapters,
    start_scheduler, start_workflow_engine,
};
