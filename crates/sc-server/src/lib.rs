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

mod apps;
mod config;
mod handler;
mod handlers;
mod router;
mod security;
mod serve;
mod triggers;

pub use apps::{AppMounts, MountedApp, build_and_mount, mount_all, subdomain_of};

/// The server's JavaScript evaluator: the `deno_core`-backed engine ownership
/// formulas' reified path runs on (§7.3). Constructed once at boot and shared —
/// `AppMounts::with_evaluator(default_js_evaluator())` — so every provider of
/// every mount evaluates on one isolate.
pub fn default_js_evaluator() -> std::sync::Arc<dyn sc_expr::JsEvaluator> {
    std::sync::Arc::new(sc_expr::DenoEvaluator::new())
}
pub use config::{DEFAULT_BIND, ServerConfig};
pub use handler::{
    BoxFuture, HandlerCtx, HandlerFn, HandlerRegistry, HandlerResponse, SessionAction,
};
pub use handlers::admin_handlers;
pub use router::{BOOTSTRAP_HTML, CSRF_REQUEST_HEADER, build_router, build_router_with_apps};
pub use security::{CONTENT_SECURITY_POLICY, CSRF_COOKIE, CSRF_HEADER, SESSION_COOKIE};
pub use serve::serve;
pub use triggers::{fire_startup, install_triggers, start_scheduler};
