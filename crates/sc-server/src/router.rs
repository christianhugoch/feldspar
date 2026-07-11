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
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::Cookie;
use sc_api::{AuthRequirement, Endpoint, EndpointSet, HandlerRef, Method as ApiMethod};
use sc_auth::{SessionStore, User};
use sc_error::{Error, Result};
use serde_json::Value;
use tower::ServiceExt;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::config::ServerConfig;
use crate::handler::{HandlerCtx, HandlerRegistry, HandlerResponse, SessionAction};
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
}

/// Build the axum router for an endpoint set.
///
/// Fails only if the endpoint paths cannot be assembled into a [`matchit`]
/// router (a duplicate/conflicting route pattern — a registration bug).
pub fn build_router(
    endpoints: &EndpointSet,
    handlers: HandlerRegistry,
    sessions: Arc<SessionStore>,
    config: &ServerConfig,
) -> Result<Router> {
    let routes = Arc::new(build_matchit(endpoints)?);
    let state = AppState {
        routes,
        handlers: Arc::new(handlers),
        sessions,
        static_dir: config.static_dir.clone().map(Arc::new),
        secure_cookies: config.secure_cookies,
    };

    let app = Router::new()
        .fallback(dispatch)
        .with_state(state)
        // CSRF runs outside dispatch so it guards every route and can mint the
        // double-submit cookie on the way out.
        .layer(axum::middleware::from_fn_with_state(
            config.secure_cookies,
            csrf_middleware,
        ))
        // Strict security headers on every response (design §16).
        .layer(SetResponseHeaderLayer::overriding(
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
    jar: CookieJar,
    body: Bytes,
) -> Response {
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
            Err(_) => {
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
        path_params,
        query: parse_query(uri),
        body: parsed_body,
        user,
    };

    match handler(ctx).await {
        Ok(resp) => apply_response(state, jar, session_token, resp),
        Err(e) => json_error(error_status(&e), e.to_string()),
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
            Err(_) => {
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
fn error_status(err: &Error) -> StatusCode {
    match err {
        Error::NotFound(_) => StatusCode::NOT_FOUND,
        Error::Invalid(_) => StatusCode::BAD_REQUEST,
        Error::Auth(_) => StatusCode::UNAUTHORIZED,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
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

/// The header the SPA must echo the CSRF cookie in (re-exported for callers/tests).
pub const CSRF_REQUEST_HEADER: &str = CSRF_HEADER;
