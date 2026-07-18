//! Router assembly and endpoint dispatch (technical design §13.1, §16).
//!
//! [`build_router`] turns an [`EndpointSet`] into an axum [`Router`]. Rather than
//! registering one axum route per endpoint, all routes — the compile-time admin
//! API and any runtime-registered application routes — are dispatched through a
//! single [`matchit`] router built from the endpoint values, exactly the "routes
//! need not be known at compile time" machinery the design calls for. A request
//! is matched to an endpoint, its [`AuthRequirement`] is enforced against the
//! session, and its handler is resolved from the [`HandlerRegistry`]; anything
//! that isn't an API route falls through to the static `ui/admin` bundle (via
//! `tower-http`'s [`ServeDir`]) or the minimal bootstrap document.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path as AxumPath, Request, State};
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::Cookie;
use sc_api::{
    ApiRequest, AuthRequirement, Endpoint, EndpointSet, HandlerRef, Method as ApiMethod,
    SessionAction,
};
use sc_app::AppRequest;
use sc_auth::{SessionStore, User};
use sc_error::{Error, ErrorKind, Repr, Result};
use serde_json::Value;
use tower::ServiceExt;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::apps::{AppMounts, MountedApp, subdomain_of};
use crate::config::ServerConfig;
use crate::handler::{HandlerCtx, HandlerRegistry, HandlerResponse};
use crate::security::{
    CONTENT_SECURITY_POLICY, CSRF_HEADER, SESSION_COOKIE, build_cookie, csrf_middleware,
};

/// The minimal bootstrap document served for non-API navigations. It has **no
/// server-rendered admin markup** and no inline script/style (so it satisfies
/// the strict CSP): just the SPA mount point and the bundle's stable entry
/// points. The `ui/admin` build pins these to `/main.js` + `/main.css` (see its
/// `vite.config.ts`), so a request that falls back to this document loads the
/// same assets the built `index.html` links — both same-origin, `'self'`-clean.
pub const BOOTSTRAP_HTML: &str = "<!doctype html>\n\
<html lang=\"en\">\n\
<head>\n\
<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<title>Saltcorn</title>\n\
<link rel=\"stylesheet\" href=\"/main.css\">\n\
</head>\n\
<body>\n\
<div id=\"root\"></div>\n\
<script type=\"module\" src=\"/main.js\"></script>\n\
</body>\n\
</html>\n";

/// Shared server state threaded through dispatch.
#[derive(Clone)]
struct AppState {
    /// Path pattern → the endpoints registered at that path (one per method).
    routes: Arc<matchit::Router<Vec<Endpoint>>>,
    /// Name → handler resolution for `HandlerRef::Named`.
    handlers: Arc<HandlerRegistry>,
    /// The session store backing login/logout and per-request auth.
    sessions: Arc<SessionStore>,
    /// Directory holding the built `ui/admin` bundle, if configured.
    static_dir: Option<Arc<PathBuf>>,
    /// Whether to set `Secure` on the session cookie.
    secure_cookies: bool,
    /// The applications served on their own subdomains, if any.
    apps: Arc<AppMounts>,
    /// The domain apps are served under; `None` disables app routing.
    base_domain: Option<Arc<String>>,
}

/// Build the axum router for an endpoint set, serving no applications.
///
/// Fails only if the endpoint paths cannot be assembled into a [`matchit`]
/// router (a duplicate/conflicting route pattern — a registration bug).
pub fn build_router(
    endpoints: &EndpointSet,
    handlers: HandlerRegistry,
    sessions: Arc<SessionStore>,
    config: &ServerConfig,
) -> Result<Router> {
    build_router_with_apps(
        endpoints,
        handlers,
        sessions,
        config,
        Arc::new(AppMounts::none()),
    )
}

/// Build the axum router, also serving `apps` on their own subdomains
/// (design §13.2).
///
/// A request is routed to an app by its `Host`: `blog.<base_domain>` reaches the
/// app whose subdomain is `blog`. Anything else — the base domain, an unknown
/// subdomain, or any host when no base domain is configured — is the admin, so
/// mounting an app cannot take the admin away from an operator.
///
/// `apps` is a **shared, live** [`AppMounts`] handle (design §13.2): the caller
/// keeps a clone to mount/unmount apps at runtime, and this router resolves each
/// request against the registry's current contents — an app mounted after the
/// router was built serves immediately, with no restart.
pub fn build_router_with_apps(
    endpoints: &EndpointSet,
    handlers: HandlerRegistry,
    sessions: Arc<SessionStore>,
    config: &ServerConfig,
    apps: Arc<AppMounts>,
) -> Result<Router> {
    if !apps.is_empty() && config.base_domain.is_none() {
        return Err(Error::config(format!(
            "applications are mounted ({}) but no --base-domain is set, so no request \
             could ever reach them",
            apps.subdomains().join(", ")
        )));
    }
    if !apps.is_empty() && apps.catalog().is_none() {
        return Err(Error::config(
            "applications are mounted but no catalog was given for their APIs to run against",
        ));
    }

    let routes = Arc::new(build_matchit(endpoints)?);
    let state = AppState {
        routes,
        handlers: Arc::new(handlers),
        sessions,
        static_dir: config.static_dir.clone().map(Arc::new),
        secure_cookies: config.secure_cookies,
        apps,
        base_domain: config.base_domain.clone().map(Arc::new),
    };

    let app = Router::new()
        // Operational health check: a fixed, unauthenticated route (outside the
        // typed API) so a CLI smoke test, load balancer, or orchestrator can
        // confirm the process is up. It takes precedence over the SPA fallback.
        .route("/health", axum::routing::get(health))
        // Binary file upload, deliberately **outside** the typed `EndpointSet`.
        //
        // The endpoint model is JSON-only — `TypeSchema` is
        // `Value`/`Struct`/`Array`/`Optional` and `HandlerResponse` carries
        // `Json` — so a raw body cannot be described by it. `writeFile`'s base64
        // path stays for small files and remains in the generated TypeScript
        // client; this exists for the ones that should not be held in memory
        // twice and inflated by a third to cross the wire.
        //
        // The cost, accepted knowingly: this is the first admin operation absent
        // from the generated client, so the SPA hand-writes this one call. It
        // therefore has to do for itself everything `dispatch` does for a typed
        // endpoint — session lookup and the admin check — which is why the auth
        // is repeated here rather than inherited. CSRF is *not* repeated: the
        // middleware wraps every route including this one.
        .route("/upload/{store}/{*path}", axum::routing::post(upload))
        .fallback(dispatch)
        .with_state(state)
        // CSRF runs outside dispatch so it guards every route and can mint the
        // double-submit cookie on the way out.
        .layer(axum::middleware::from_fn_with_state(
            config.secure_cookies,
            csrf_middleware,
        ))
        // Strict security headers on every response (design §16). CSP is
        // `if_not_present`, not `overriding`: an application carries its own
        // `CspPolicy` (§13.2) and sets it on its own responses, and this must not
        // replace it. Everything that does not set one — the whole admin surface —
        // still gets the strict default.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CONTENT_SECURITY_POLICY),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ));

    Ok(app)
}

/// Ceiling on a single upload. Generous enough for the assets a code framework's
/// source tree carries (images, fonts, sample data) while still bounding what one
/// request can allocate.
const MAX_UPLOAD_BYTES: usize = 256 * 1024 * 1024;

/// The operational health check. Always `200 {"status":"ok"}`; reaching it at
/// all is the signal that the server booted and is accepting requests.
async fn health() -> Response {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

/// Stream a request body straight into a file store (see the route's comment for
/// why this lives outside the typed endpoint set).
///
/// The store and destination come from the URL rather than a JSON body, because
/// there is no JSON body — the body *is* the file. `{*path}` is a greedy capture,
/// so a nested destination like `assets/img/logo.png` arrives whole.
///
/// The work itself is done by the `uploadFile` handler in the registry, not here.
/// That is deliberate: the handler already closes over the catalog and applies
/// the same access rule as every other file operation, so routing around the
/// `EndpointSet` does not also mean routing around the access model. All this
/// function adds is the plumbing dispatch would otherwise have done — session
/// lookup, the admin check, and reading the body.
async fn upload(
    State(state): State<AppState>,
    jar: CookieJar,
    AxumPath((store_name, path)): AxumPath<(String, String)>,
    body: axum::body::Body,
) -> Response {
    let session_token = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    let user = match &session_token {
        Some(token) => match state.sessions.user_for(token) {
            Ok(user) => user,
            Err(e) => {
                log_failure("session lookup failed", &e);
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "session lookup failed");
            }
        },
        None => None,
    };
    if let Some(rejection) = enforce_auth(&AuthRequirement::admin(), user.as_ref()) {
        return rejection;
    }

    let Some(handler) = state.handlers.get("uploadFile").cloned() else {
        return json_error(
            StatusCode::NOT_FOUND,
            "this server has no file-upload handler registered",
        );
    };

    // Bounded rather than unbounded: the cap is what keeps a single request from
    // exhausting memory. True streaming to disk would remove the ceiling and is
    // the next step if it ever binds; this is already far above what
    // base64-in-JSON could carry.
    let bytes = match axum::body::to_bytes(body, MAX_UPLOAD_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return json_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("upload exceeds the {MAX_UPLOAD_BYTES} byte limit"),
            );
        }
    };

    let ctx = HandlerCtx {
        raw_body: Some(bytes),
        path_params: HashMap::from([("store".to_owned(), store_name), ("path".to_owned(), path)]),
        query: HashMap::new(),
        body: serde_json::Value::Null,
        user,
    };
    match handler(ctx).await {
        Ok(resp) => apply_response(&state, jar, session_token, resp),
        Err(e) => error_response(&e, Audience::Admin),
    }
}

/// Group endpoints by path pattern and insert them into a `matchit` router.
fn build_matchit(endpoints: &EndpointSet) -> Result<matchit::Router<Vec<Endpoint>>> {
    // Preserve registration order while grouping same-path endpoints together
    // (e.g. GET and POST on `/api/tables`).
    let mut by_path: Vec<(String, Vec<Endpoint>)> = Vec::new();
    for ep in endpoints {
        let pattern = ep.path.pattern();
        match by_path.iter_mut().find(|(p, _)| *p == pattern) {
            Some((_, eps)) => eps.push(ep.clone()),
            None => by_path.push((pattern, vec![ep.clone()])),
        }
    }

    let mut router = matchit::Router::new();
    for (pattern, eps) in by_path {
        router
            .insert(pattern.clone(), eps)
            .map_err(|e| Error::config(format!("cannot mount route `{pattern}`: {e}")))?;
    }
    Ok(router)
}

/// The single fallback that dispatches every request: API routes via `matchit`,
/// everything else to the static bundle / bootstrap document.
async fn dispatch(
    State(state): State<AppState>,
    method: axum::http::Method,
    uri: Uri,
    headers: axum::http::HeaderMap,
    jar: CookieJar,
    body: Bytes,
) -> Response {
    // An application claims the whole of its subdomain, so this comes first: on
    // `blog.example.com` every path is the blog's, not the admin's.
    if let Some(app) = resolve_app(&state, &headers) {
        return dispatch_app(&state, &app, method, &uri, jar, &body).await;
    }

    match state.routes.at(uri.path()) {
        Ok(matched) => {
            let params: HashMap<String, String> = matched
                .params
                .iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect();
            let endpoints = matched.value;

            let Some(api_method) = map_method(method.as_str()) else {
                return json_error(StatusCode::METHOD_NOT_ALLOWED, "unsupported method");
            };
            match endpoints.iter().find(|e| e.method == api_method) {
                Some(ep) => handle_api(&state, ep, params, &uri, jar, &body).await,
                None => json_error(
                    StatusCode::METHOD_NOT_ALLOWED,
                    "method not allowed for this route",
                ),
            }
        }
        // Not an API route: serve the SPA bundle / bootstrap for navigations.
        Err(_) => {
            if method == axum::http::Method::GET || method == axum::http::Method::HEAD {
                serve_static(&state, &uri).await
            } else {
                json_error(StatusCode::NOT_FOUND, "not found")
            }
        }
    }
}

/// The application a request's `Host` names, if any.
///
/// Returns an owned [`Arc`] so the live registry's read lock is released before
/// the request is served: a concurrent mount/unmount never blocks on an in-flight
/// request, and a request in flight against a since-replaced app keeps serving the
/// version it resolved.
fn resolve_app(state: &AppState, headers: &axum::http::HeaderMap) -> Option<Arc<MountedApp>> {
    let base = state.base_domain.as_ref()?;
    let host = headers.get(header::HOST)?.to_str().ok()?;
    let subdomain = subdomain_of(host, Some(base.as_str()))?;
    state.apps.get(subdomain)
}

/// Serve one request against an application: its API providers first, then its
/// framework (design §13.2/§13.3).
///
/// Providers win over the framework for the paths they claim, so an app's
/// `/api/*` is its data and everything else is its UI. Both answers carry the
/// app's own CSP.
async fn dispatch_app(
    state: &AppState,
    app: &MountedApp,
    method: axum::http::Method,
    uri: &Uri,
    jar: CookieJar,
    body: &Bytes,
) -> Response {
    let csp = app.app.csp.header_value();
    let Some(api_method) = map_method(method.as_str()) else {
        return with_csp(
            json_error(StatusCode::METHOD_NOT_ALLOWED, "unsupported method"),
            &csp,
        );
    };
    let path = uri.path();

    // The app's data: an API provider that claims this path.
    if let Some(provider) = app.provider_for(path) {
        // An app is mounted only with a catalog (checked at build time), so this
        // is a server bug rather than a request problem.
        let Some(catalog) = state.apps.catalog() else {
            return with_csp(
                json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "no catalog for application APIs",
                ),
                &csp,
            );
        };

        let session_token = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
        let user = match &session_token {
            Some(token) => match state.sessions.user_for(token) {
                Ok(u) => u,
                Err(e) => {
                    log_failure("session lookup failed", &e);
                    return with_csp(
                        json_error(StatusCode::INTERNAL_SERVER_ERROR, "session lookup failed"),
                        &csp,
                    );
                }
            },
            None => None,
        };

        let parsed_body = if body.is_empty() {
            Value::Null
        } else {
            match serde_json::from_slice(body) {
                Ok(v) => v,
                Err(e) => {
                    return with_csp(
                        json_error(StatusCode::BAD_REQUEST, format!("invalid JSON body: {e}")),
                        &csp,
                    );
                }
            }
        };

        let req = ApiRequest {
            method: api_method,
            path: path.to_owned(),
            query: parse_query(uri),
            body: parsed_body,
        };

        // The provider enforces the endpoint's auth itself (§7), so unlike the
        // admin path there is no separate check here.
        return match provider.handle(req, catalog, user.as_ref()).await {
            // A provider's response is shaped exactly like a handler's — body,
            // status, session change — so it goes out through the same
            // `apply_response`: an app's `login` sets its session cookie the very
            // same way the admin's does.
            Ok(resp) => with_csp(
                apply_response(
                    state,
                    jar,
                    session_token,
                    HandlerResponse {
                        body: resp.body,
                        status: resp.status,
                        session: resp.session,
                    },
                ),
                &csp,
            ),
            Err(e) => with_csp(error_response(&e, Audience::App), &csp),
        };
    }

    // The app's UI: its framework serves the built bundle. No catalog access
    // happens here for a code framework — the app reaches data only through the
    // API above.
    let Some(catalog) = state.apps.catalog() else {
        return with_csp(
            json_error(StatusCode::INTERNAL_SERVER_ERROR, "no catalog"),
            &csp,
        );
    };
    let req = AppRequest {
        method: api_method,
        path: path.to_owned(),
    };
    match app.framework.handle(req, catalog).await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status).unwrap_or(StatusCode::OK);
            let mut out = (status, resp.body).into_response();
            if let Ok(ct) = HeaderValue::from_str(&resp.content_type) {
                out.headers_mut().insert(header::CONTENT_TYPE, ct);
            }
            with_csp(out, &csp)
        }
        Err(e) => with_csp(error_response(&e, Audience::App), &csp),
    }
}

/// Stamp an application's own CSP onto its response (design §13.2).
fn with_csp(mut resp: Response, csp: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(csp) {
        resp.headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, value);
    }
    resp
}

/// Enforce auth, parse the request, run the handler, and apply its session
/// action — the full API request lifecycle for one matched endpoint.
async fn handle_api(
    state: &AppState,
    ep: &Endpoint,
    path_params: HashMap<String, String>,
    uri: &Uri,
    jar: CookieJar,
    body: &Bytes,
) -> Response {
    // Recover the authenticated user (if any) from the session cookie.
    let session_token = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    let user = match &session_token {
        Some(token) => match state.sessions.user_for(token) {
            Ok(u) => u,
            Err(e) => {
                log_failure("session lookup failed", &e);
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "session lookup failed");
            }
        },
        None => None,
    };

    // Enforce the endpoint's authorization requirement (design §7).
    if let Some(rejection) = enforce_auth(&ep.auth, user.as_ref()) {
        return rejection;
    }

    // Parse the JSON body (empty body → null).
    let parsed_body = if body.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => {
                return json_error(StatusCode::BAD_REQUEST, format!("invalid JSON body: {e}"));
            }
        }
    };

    // Resolve the handler; unimplemented / non-Rust handlers are 501.
    let handler = match &ep.handler {
        HandlerRef::Named(name) => match state.handlers.get(name) {
            Some(h) => h.clone(),
            None => return json_error(StatusCode::NOT_IMPLEMENTED, "handler not implemented"),
        },
        HandlerRef::GuestCode { .. } | HandlerRef::Sql(_) => {
            return json_error(
                StatusCode::NOT_IMPLEMENTED,
                "custom handlers not yet supported",
            );
        }
    };

    let ctx = HandlerCtx {
        raw_body: None,
        path_params,
        query: parse_query(uri),
        body: parsed_body,
        user,
    };

    match handler(ctx).await {
        Ok(resp) => apply_response(state, jar, session_token, resp),
        Err(e) => error_response(&e, Audience::Admin),
    }
}

/// Check a user against an [`AuthRequirement`]. Returns `Some(rejection)` when
/// the request is not authorized, `None` when it may proceed.
fn enforce_auth(auth: &AuthRequirement, user: Option<&User>) -> Option<Response> {
    let unauthenticated = || json_error(StatusCode::UNAUTHORIZED, "authentication required");
    match auth {
        AuthRequirement::Public => None,
        AuthRequirement::LoggedIn => user.is_none().then(unauthenticated),
        AuthRequirement::MinRole(min) => match user {
            None => Some(unauthenticated()),
            Some(u) if u.meets_role(*min) => None,
            Some(_) => Some(json_error(StatusCode::FORBIDDEN, "insufficient privilege")),
        },
    }
}

/// Turn a [`HandlerResponse`] into an HTTP response, applying its session action
/// (start/end) to the cookie jar and the store.
fn apply_response(
    state: &AppState,
    jar: CookieJar,
    session_token: Option<String>,
    resp: HandlerResponse,
) -> Response {
    let status = StatusCode::from_u16(resp.status).unwrap_or(StatusCode::OK);
    let jar = match resp.session {
        SessionAction::Keep => jar,
        SessionAction::Start(user) => match state.sessions.login(user) {
            Ok(token) => jar.add(build_cookie(
                SESSION_COOKIE,
                token,
                true,
                state.secure_cookies,
            )),
            Err(e) => {
                log_failure("could not start session", &e);
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "could not start session");
            }
        },
        SessionAction::End => {
            if let Some(token) = &session_token {
                let _ = state.sessions.logout(token);
            }
            jar.remove(Cookie::build((SESSION_COOKIE, "")).path("/").build())
        }
    };
    (status, jar, Json(resp.body)).into_response()
}

/// Serve a file from the static bundle, falling back to the SPA bootstrap
/// document (history fallback) when there is no matching file.
async fn serve_static(state: &AppState, uri: &Uri) -> Response {
    if let Some(dir) = &state.static_dir {
        if let Ok(request) = Request::builder().uri(uri.clone()).body(Body::empty()) {
            // `ServeDir`'s error type is `Infallible`, so a match (not `if let`)
            // keeps the compiler from flagging an irrefutable pattern.
            match ServeDir::new(dir.as_ref().as_path()).oneshot(request).await {
                Ok(response) if response.status() != StatusCode::NOT_FOUND => {
                    return response.map(Body::new);
                }
                _ => {}
            }
        }
    }
    (StatusCode::OK, Html(BOOTSTRAP_HTML)).into_response()
}

/// Map an HTTP method token to the endpoint model's [`ApiMethod`].
fn map_method(method: &str) -> Option<ApiMethod> {
    match method {
        "GET" => Some(ApiMethod::Get),
        "POST" => Some(ApiMethod::Post),
        "PUT" => Some(ApiMethod::Put),
        "PATCH" => Some(ApiMethod::Patch),
        "DELETE" => Some(ApiMethod::Delete),
        _ => None,
    }
}

/// Map a domain error to an HTTP status.
///
/// The specific request-level variants get their conventional codes; everything
/// else is decided by the §16 [`ErrorKind`] split, so an **Application** error
/// (bad configuration or app code the admin must fix — e.g. a failed build,
/// carrying the bundler's diagnostics) is a client-fixable `422`, while a
/// **System** error (a bug or infrastructure failure to report) is a `500`.
fn error_status(err: &Error) -> StatusCode {
    match err.repr() {
        Repr::NotFound(_) => StatusCode::NOT_FOUND,
        Repr::Invalid(_) => StatusCode::BAD_REQUEST,
        Repr::Auth(_) => StatusCode::UNAUTHORIZED,
        _ => match err.kind() {
            ErrorKind::Application => StatusCode::UNPROCESSABLE_ENTITY,
            ErrorKind::System => StatusCode::INTERNAL_SERVER_ERROR,
        },
    }
}

/// Parse a query string into key/value pairs (no percent-decoding; MVP simple).
fn parse_query(uri: &Uri) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(query) = uri.query() {
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let mut kv = pair.splitn(2, '=');
            let key = kv.next().unwrap_or("").to_owned();
            let value = kv.next().unwrap_or("").to_owned();
            out.insert(key, value);
        }
    }
    out
}

/// A JSON error body with the given status.
fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": message.into() }))).into_response()
}

/// Log a domain [`Error`] at the HTTP boundary, then map it to a JSON response.
///
/// Principle 5 — no silent failures: an operator watching the process must see
/// *why* a request failed, not just the terse body the client receives. Every
/// error is printed with its full source chain ([`Error::chain`]) so a wrapped
/// driver error (e.g. the real SQL error behind `tokio_postgres`'s `"db error"`)
/// is visible on the console. System errors (`500`) are the ones that most need
/// eyes, so they are flagged accordingly.
fn error_response(err: &Error, audience: Audience) -> Response {
    let status = error_status(err);
    let label = if status == StatusCode::INTERNAL_SERVER_ERROR {
        "internal error"
    } else {
        "request error"
    };
    // The log always gets everything, including the failing line.
    eprintln!("saltcorn: {label}: {}", err.chain());
    let message = match audience {
        // An admin needs the cause. `Display` on a context error renders only the
        // outermost layer — "connecting file store `docs`" with no hint that the
        // directory is missing — which is a message that says something failed
        // and nothing about what to do, on the one screen whose job is to say
        // what to do.
        Audience::Admin => err.causes(),
        // An application's callers are its ordinary users, not operators. The
        // outermost message is the part deliberately written to be shown; the
        // causes below it are internals — SQL, paths, driver text — and belong
        // in the log, which already has them.
        Audience::App => err.to_string(),
    };
    json_error(status, message)
}

/// Who will read an error message, which decides how much of it to send.
///
/// This is a trust boundary, not a formatting preference: the same
/// [`error_response`] serves the admin API and every application's API, and the
/// two have different readers. Making it an explicit argument rather than a
/// default means adding a route forces the question to be answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Audience {
    /// The admin API — an operator, who needs the whole causal chain.
    Admin,
    /// An application's own API — its end users, who get the top-level message.
    App,
}

/// Log a discarded lower-level failure at a call site that only knows "it broke"
/// (no [`Error`] value to forward). Keeps those paths from failing silently.
fn log_failure(context: &str, err: &(dyn std::error::Error + 'static)) {
    eprintln!("saltcorn: {context}: {}", sc_error::format_chain(err));
}

/// The header the SPA must echo the CSRF cookie in (re-exported for callers/tests).
pub const CSRF_REQUEST_HEADER: &str = CSRF_HEADER;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body_json(resp: Response) -> Value {
        let bytes = to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        serde_json::from_slice(&bytes).expect("body is JSON")
    }

    #[tokio::test]
    async fn system_error_maps_to_500_and_reports_the_message() {
        // A database failure is a System error (§16): the client gets a 500 and
        // the top-level message in the body. (The full cause chain goes to the
        // console via `error_response`'s log line.)
        let err = Error::database(
            "query failed: db error\n  caused by: relation \"apps\" does not exist",
        );
        let resp = error_response(&err, Audience::App);
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_json(resp).await;
        assert_eq!(
            body["error"],
            Value::String(err.to_string()),
            "client body carries the error's Display"
        );
    }

    /// The audience split. An admin gets the cause; an application's users get
    /// only the top-level message.
    #[tokio::test]
    async fn an_admin_sees_the_cause_and_an_app_user_does_not() {
        use sc_error::Context;

        // The shape that motivated this: a context layer whose own message says
        // nothing useful, wrapping the one that does.
        let inner: std::result::Result<(), std::io::Error> = Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "No such file or directory",
        ));
        let err = inner.context("connecting file store `docs`").unwrap_err();

        let admin = body_json(error_response(&err, Audience::Admin)).await;
        let admin_text = admin["error"].as_str().unwrap();
        assert!(admin_text.contains("connecting file store"), "{admin_text}");
        assert!(
            admin_text.contains("No such file or directory"),
            "an admin must be told what actually went wrong: {admin_text}"
        );

        let app = body_json(error_response(&err, Audience::App)).await;
        let app_text = app["error"].as_str().unwrap();
        assert!(app_text.contains("connecting file store"), "{app_text}");
        assert!(
            !app_text.contains("No such file or directory"),
            "an app's users must not be shown internals: {app_text}"
        );
    }

    #[tokio::test]
    async fn application_error_maps_to_422() {
        // A bad app config is the admin's to fix — a client-fixable 422, not 500.
        let resp = error_response(&Error::config("bad framework config"), Audience::Admin);
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn request_level_errors_keep_their_conventional_codes() {
        assert_eq!(
            error_response(&Error::not_found("app"), Audience::Admin).status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            error_response(&Error::auth("nope"), Audience::Admin).status(),
            StatusCode::UNAUTHORIZED
        );
    }
}
