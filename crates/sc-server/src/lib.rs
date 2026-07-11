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

mod config;
mod convert;
mod handler;
mod handlers;
mod router;
mod security;
mod serve;

pub use config::{DEFAULT_BIND, ServerConfig};
pub use handler::{
    BoxFuture, HandlerCtx, HandlerFn, HandlerRegistry, HandlerResponse, SessionAction,
};
pub use handlers::admin_handlers;
pub use router::{BOOTSTRAP_HTML, CSRF_REQUEST_HEADER, build_router};
pub use security::{CONTENT_SECURITY_POLICY, CSRF_COOKIE, CSRF_HEADER, SESSION_COOKIE};
pub use serve::serve;
