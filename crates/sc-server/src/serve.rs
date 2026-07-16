//! Binding the listener and running the server with graceful shutdown
//! (technical design §16).

use std::sync::Arc;

use sc_api::EndpointSet;
use sc_auth::SessionStore;
use sc_error::{Context, Error, Result};

use crate::apps::AppMounts;
use crate::config::ServerConfig;
use crate::handler::HandlerRegistry;
use crate::router::build_router_with_apps;

/// Build the router, bind the configured address, and serve until a shutdown
/// signal (Ctrl-C or, on Unix, `SIGTERM`) arrives.
///
/// The session store is supplied by the caller so its lifetime (and, later, a
/// shared handle for the handlers) is under the caller's control. `apps` is the
/// live [`AppMounts`] registry the caller has already mounted the stored
/// applications into (see [`mount_all`](crate::mount_all)); the caller keeps a
/// clone of the handle to mount/unmount apps at runtime.
pub async fn serve(
    config: ServerConfig,
    endpoints: EndpointSet,
    handlers: HandlerRegistry,
    sessions: Arc<SessionStore>,
    apps: Arc<AppMounts>,
) -> Result<()> {
    let app = build_router_with_apps(&endpoints, handlers, sessions, &config, apps)?;

    let listener = tokio::net::TcpListener::bind(config.addr)
        .await
        .with_context(|| format!("binding to {}", config.addr))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| Error::msg(format!("server error: {e}")))
}

/// Resolve when the process is asked to stop: Ctrl-C on every platform, plus
/// `SIGTERM` on Unix (the signal an orchestrator sends).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            // If the handler can't be installed, fall back to Ctrl-C only.
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
