//! Binding the listener and running the server with graceful shutdown
//! (technical design §16), over TLS where it is configured (§13.5).
//!
//! Two signals matter to a running server: `SIGTERM` (or Ctrl-C) stops it, and
//! `SIGHUP` reloads the catalog and the applications in place without building
//! anything — see [`crate::reload`].
//!
//! The service manager is told when both of those are true: `READY=1` goes out
//! once every listener is bound and accepting, and `STOPPING=1` when the
//! shutdown signal arrives, so a unit may say `Type=notify` and `WatchdogSec`
//! and mean them (see [`crate::systemd`]). Where there is no service manager —
//! every development run, and every platform without one — the notifications are
//! no-ops and this function behaves exactly as it did before.
//!
//! With TLS on there are **two** listeners: the configured bind address, which
//! answers plain HTTP, and the TLS port beside it. What the plain one serves is
//! the admin's choice — a permanent redirect to HTTPS (the default) or the
//! application itself, for a deployment that is also reachable over a private
//! network. Both stop together on the one shutdown signal.

use std::sync::Arc;

use sc_api::EndpointSet;
use sc_auth::SessionStore;
use sc_error::{Context, Error, Result};

use crate::apps::AppMounts;
use crate::config::ServerConfig;
use crate::handler::HandlerRegistry;
use crate::router::build_router_with_apps;
use crate::systemd::ServiceManager;
use crate::tls::{TlsSettings, graceful_shutdown, https_addr, redirect_router, serve_https};

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
    // `SIGHUP` reloads the catalog and the applications in place, building
    // nothing (see `crate::reload`). Installed here beside the shutdown signals
    // because this is the function that owns the process's signal behaviour, and
    // it holds the registry a reload mutates.
    crate::reload::spawn_sighup_reload(apps.clone());

    let app = build_router_with_apps(&endpoints, handlers, sessions, &config, apps)?;

    // The service manager that started this process, if one did. Read here
    // rather than passed in: it is a property of the process's environment, and
    // every caller of `serve` would otherwise have to plumb the same thing.
    let service = ServiceManager::from_env();

    let TlsSettings::Off = &config.tls else {
        return serve_with_tls(config, app, service).await;
    };

    let listener = tokio::net::TcpListener::bind(config.addr)
        .await
        .with_context(|| format!("binding to {}", config.addr))?;

    // Bound, therefore ready: a connection to the address now completes, so this
    // is the moment `systemctl start` may return and dependent units may run.
    let watchdog = service.spawn_watchdog();
    service.notify_ready(&format!("serving on http://{}", config.addr));

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(service))
        .await
        .map_err(|e| Error::msg(format!("server error: {e}")));
    if let Some(watchdog) = watchdog {
        watchdog.abort();
    }
    result
}

/// Serve `app` over both listeners: TLS on the configured HTTPS port, and plain
/// HTTP on the bind address.
///
/// Both are bound **before** either serves, so a port that cannot be had — 443
/// without the capability to bind it is the usual one — is an error at startup
/// naming the address, rather than a server that is up on one protocol and
/// silently missing on the other.
async fn serve_with_tls(
    config: ServerConfig,
    app: axum::Router,
    service: ServiceManager,
) -> Result<()> {
    let port = config
        .tls
        .port()
        .ok_or_else(|| Error::config("TLS is enabled but has no port"))?;
    let tls_addr = https_addr(config.addr, port);

    let tls_listener = std::net::TcpListener::bind(tls_addr)
        .with_context(|| format!("binding the TLS listener to {tls_addr}"))?;
    let http_listener = tokio::net::TcpListener::bind(config.addr)
        .await
        .with_context(|| format!("binding to {}", config.addr))?;

    // What the plain-HTTP port answers with. A redirect is the default; serving
    // the application there too is the deployment that is reachable both ways on
    // purpose.
    let http_app = if config.tls.redirect_http() {
        redirect_router(port)
    } else {
        app.clone()
    };

    let handle = axum_server::Handle::new();
    // One signal, both servers: the TLS server stops through its handle and the
    // plain one through this channel, so a `SIGTERM` does not leave half the
    // process listening.
    let (stop_http, http_stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn({
        let handle = handle.clone();
        let service = service.clone();
        async move {
            shutdown_signal(service).await;
            let _ = stop_http.send(());
            graceful_shutdown(&handle);
        }
    });

    eprintln!("saltcorn: serving TLS on https://{tls_addr}");
    // Both listeners are bound by now — the TLS one synchronously above, the
    // plain one just after it — so readiness is not a claim about the TLS
    // handshake (whose certificate may still be being ordered from an ACME CA)
    // but about the ports, which is what a dependent unit waits on.
    let watchdog = service.spawn_watchdog();
    service.notify_ready(&format!(
        "serving TLS on https://{tls_addr} and http://{}",
        config.addr
    ));
    let http = tokio::spawn(async move {
        axum::serve(http_listener, http_app)
            .with_graceful_shutdown(async move {
                let _ = http_stopped.await;
            })
            .await
    });

    let tls_result = serve_https(tls_listener, app, &config.tls, handle).await;
    if let Some(watchdog) = watchdog {
        watchdog.abort();
    }
    // The plain listener is awaited whichever way the TLS one ended, so its
    // error is reported rather than dropped with the task.
    match http.await {
        Ok(Ok(())) => tls_result,
        Ok(Err(e)) => {
            tls_result?;
            Err(Error::msg(format!("server error: {e}")))
        }
        Err(e) => {
            tls_result?;
            Err(Error::msg(format!("the HTTP listener panicked: {e}")))
        }
    }
}

/// Resolve when the process is asked to stop: Ctrl-C on every platform, plus
/// `SIGTERM` on Unix (the signal an orchestrator sends).
///
/// The service manager is told before this resolves, so the unit shows
/// `deactivating` for the length of the graceful drain that follows rather than
/// looking hung — and so a `WatchdogSec` unit is not restarted for the drain.
async fn shutdown_signal(service: ServiceManager) {
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

    service.notify_stopping("draining in-flight requests");
}
