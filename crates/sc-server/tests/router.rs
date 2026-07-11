//! End-to-end tests for the server's dispatch, auth, CSRF, session, and static
//! serving — driven through the assembled router with `tower`'s `oneshot`, so no
//! network or database is required. The concrete admin handlers and their
//! DB-backed tests arrive in the second half of the Phase 6 server subphase.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sc_api::{AuthRequirement, Endpoint, EndpointSet, Method, PathSpec};
use sc_auth::{ROLE_ADMIN, SessionStore, User};
use sc_server::{
    CONTENT_SECURITY_POLICY, CSRF_COOKIE, CSRF_HEADER, HandlerRegistry, HandlerResponse,
    SESSION_COOKIE, ServerConfig, build_router,
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

/// A small endpoint set exercising every dispatch path.
fn test_endpoints() -> EndpointSet {
    let mut set = EndpointSet::new();
    set.register(
        Endpoint::new("ping", Method::Get, PathSpec::root().lit("api/ping"))
            .auth(AuthRequirement::Public),
    );
    set.register(
        Endpoint::new("echo", Method::Post, PathSpec::root().lit("api/echo"))
            .auth(AuthRequirement::Public),
    );
    set.register(
        Endpoint::new("secret", Method::Get, PathSpec::root().lit("api/secret"))
            .auth(AuthRequirement::admin()),
    );
    set.register(
        Endpoint::new("login", Method::Post, PathSpec::root().lit("api/login"))
            .auth(AuthRequirement::Public),
    );
    set.register(
        Endpoint::new("logout", Method::Post, PathSpec::root().lit("api/logout"))
            .auth(AuthRequirement::LoggedIn),
    );
    set
}

fn test_registry() -> HandlerRegistry {
    let mut reg = HandlerRegistry::new();
    reg.register("ping", |_ctx| async {
        Ok(HandlerResponse::ok(json!({ "pong": true })))
    });
    reg.register(
        "echo",
        |ctx| async move { Ok(HandlerResponse::ok(ctx.body)) },
    );
    reg.register("secret", |_ctx| async {
        Ok(HandlerResponse::ok(json!({ "ok": true })))
    });
    reg.register("login", |_ctx| async {
        let admin = User::new(Uuid::new_v4(), ROLE_ADMIN)?;
        Ok(HandlerResponse::start_session(
            admin,
            json!({ "role": ROLE_ADMIN }),
        ))
    });
    reg.register("logout", |_ctx| async {
        Ok(HandlerResponse::end_session(json!({ "ok": true })))
    });
    reg
}

/// Build a router over the test endpoints, returning it alongside the shared
/// session store so tests can seed sessions directly.
fn test_router() -> (Router, Arc<SessionStore>) {
    let sessions = Arc::new(SessionStore::default());
    let router = build_router(
        &test_endpoints(),
        test_registry(),
        sessions.clone(),
        &ServerConfig::default(),
    )
    .expect("build router");
    (router, sessions)
}

/// Run one request and return status, the response's Set-Cookie values, and the
/// body as text.
async fn call(router: &Router, request: Request<Body>) -> (StatusCode, Vec<String>, String) {
    let response = router.clone().oneshot(request).await.expect("dispatch");
    let status = response.status();
    let cookies: Vec<String> = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(str::to_owned)
        .collect();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read body");
    (
        status,
        cookies,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// Extract a cookie value by name from a set of Set-Cookie header values.
fn cookie_value(cookies: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    cookies.iter().find_map(|c| {
        c.strip_prefix(&prefix)
            .map(|rest| rest.split(';').next().unwrap_or("").to_owned())
    })
}

#[tokio::test]
async fn serves_bootstrap_document_with_security_headers() {
    let (router, _) = test_router();
    let response = router
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    // Strict CSP and hardening headers on every response (design §16).
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap(),
        CONTENT_SECURITY_POLICY
    );
    assert_eq!(
        response.headers().get("x-content-type-options").unwrap(),
        "nosniff"
    );
    assert_eq!(
        response.headers().get(header::X_FRAME_OPTIONS).unwrap(),
        "DENY"
    );

    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("<div id=\"root\"></div>"));
    // No server-rendered admin markup and no inline script.
    assert!(html.contains("<script type=\"module\" src=\"/main.js\">"));
    assert!(!html.contains("onclick"));
}

#[tokio::test]
async fn public_endpoint_reaches_its_handler() {
    let (router, _) = test_router();
    let (status, cookies, body) = call(
        &router,
        Request::get("/api/ping").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({ "pong": true })
    );
    // The CSRF double-submit cookie is minted on first contact.
    assert!(cookie_value(&cookies, CSRF_COOKIE).is_some());
}

#[tokio::test]
async fn unregistered_handler_is_not_implemented() {
    // Admin endpoint set with an *empty* registry: routes mount, but handlers 501.
    let sessions = Arc::new(SessionStore::default());
    let router = build_router(
        &sc_api::admin_endpoints(),
        HandlerRegistry::new(),
        sessions,
        &ServerConfig::default(),
    )
    .unwrap();
    let (status, _, _) = call(
        &router,
        Request::get("/api/auth/status")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn unknown_method_on_route_is_405() {
    let (router, _) = test_router();
    // /api/echo is POST-only; a GET matches the path but no endpoint's method,
    // so it is method-not-allowed (GET is CSRF-safe, so it reaches dispatch).
    let (status, _, _) = call(
        &router,
        Request::get("/api/echo").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn admin_route_requires_authentication() {
    let (router, _) = test_router();
    let (status, _, _) = call(
        &router,
        Request::get("/api/secret").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn insufficient_role_is_forbidden() {
    let (router, sessions) = test_router();
    // Seed a session for a non-admin user (role 40) directly in the store.
    let editor = User::new(Uuid::new_v4(), 40).unwrap();
    let token = sessions.login(editor).unwrap();

    let request = Request::get("/api/secret")
        .header(header::COOKIE, format!("{SESSION_COOKIE}={token}"))
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = call(&router, request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn csrf_blocks_mutations_without_a_token() {
    let (router, _) = test_router();
    // POST with neither cookie nor header is rejected before reaching the handler.
    let request = Request::post("/api/echo")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{\"a\":1}"))
        .unwrap();
    let (status, _, _) = call(&router, request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn csrf_allows_mutations_with_a_matching_token() {
    let (router, _) = test_router();
    // 1. A GET mints the CSRF cookie.
    let (_, cookies, _) = call(
        &router,
        Request::get("/api/ping").body(Body::empty()).unwrap(),
    )
    .await;
    let csrf = cookie_value(&cookies, CSRF_COOKIE).expect("csrf cookie");

    // 2. The mutation echoes it in both cookie and header, and reaches the handler.
    let request = Request::post("/api/echo")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, format!("{CSRF_COOKIE}={csrf}"))
        .header(CSRF_HEADER, &csrf)
        .body(Body::from("{\"a\":1}"))
        .unwrap();
    let (status, _, body) = call(&router, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({ "a": 1 })
    );
}

#[tokio::test]
async fn login_starts_a_session_that_unlocks_admin_routes() {
    let (router, _) = test_router();
    // 1. GET to obtain the CSRF token.
    let (_, cookies, _) = call(
        &router,
        Request::get("/api/ping").body(Body::empty()).unwrap(),
    )
    .await;
    let csrf = cookie_value(&cookies, CSRF_COOKIE).expect("csrf cookie");

    // 2. POST login → sets a session cookie.
    let login = Request::post("/api/login")
        .header(header::COOKIE, format!("{CSRF_COOKIE}={csrf}"))
        .header(CSRF_HEADER, &csrf)
        .body(Body::empty())
        .unwrap();
    let (status, cookies, _) = call(&router, login).await;
    assert_eq!(status, StatusCode::OK);
    let session = cookie_value(&cookies, SESSION_COOKIE).expect("session cookie");

    // 3. The session grants access to the admin-only route.
    let secret = Request::get("/api/secret")
        .header(header::COOKIE, format!("{SESSION_COOKIE}={session}"))
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = call(&router, secret).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({ "ok": true })
    );
}
