//! The file-store IDE's route (design §12.1): who may fetch the bundle, and under
//! which Content-Security-Policy.
//!
//! These are the two properties that cannot be seen from `ui/ide` itself. The
//! workbench is admin-only — unlike the SPA bundle, which must be public because
//! the login screen is in it — and it is served with a *relaxed* policy that must
//! not leak onto any other route: VS Code injects its own styles and runs workers
//! from blobs, and buying that for the IDE while keeping the admin UI strict is the
//! whole reason the IDE is a separate page on a separate prefix.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sc_api::{AuthRequirement, Endpoint, EndpointSet, Method, PathSpec};
use sc_auth::{ROLE_ADMIN, ROLE_PUBLIC, SessionStore, User};
use sc_server::{
    CONTENT_SECURITY_POLICY, HandlerRegistry, IDE_CONTENT_SECURITY_POLICY, SESSION_COOKIE,
    ServerConfig, build_router,
};
use tower::ServiceExt;
use uuid::Uuid;

/// One public endpoint, so the router has something to build from: these tests are
/// about the static IDE route, which is outside the endpoint set entirely.
fn endpoints() -> EndpointSet {
    let mut set = EndpointSet::new();
    set.register(
        Endpoint::new("ping", Method::Get, PathSpec::root().lit("api/ping"))
            .auth(AuthRequirement::Public),
    );
    set
}

/// A temporary directory standing in for `ui/ide/dist`, holding the bundle's entry
/// point and cleaned up by the caller.
fn ide_bundle(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sc-ide-bundle-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.js"), "export const workbench = 1;\n").unwrap();
    std::fs::write(
        dir.join("index.html"),
        "<!doctype html><div id=\"workbench\"></div>\n",
    )
    .unwrap();
    dir
}

fn router_with_ide(dir: Option<std::path::PathBuf>) -> (Router, Arc<SessionStore>) {
    let sessions = Arc::new(SessionStore::default());
    let config = ServerConfig {
        ide_dir: dir,
        ..ServerConfig::default()
    };
    let router = build_router(
        &endpoints(),
        HandlerRegistry::new(),
        sessions.clone(),
        &config,
    )
    .expect("build router");
    (router, sessions)
}

/// A logged-in session for a user of `role`.
fn session_for(sessions: &SessionStore, role: u8) -> String {
    let user = User::new(Uuid::new_v4(), role).unwrap();
    sessions.login(user).unwrap()
}

fn get(path: &str, session: Option<&str>, html: bool) -> Request<Body> {
    let mut request = Request::get(path);
    if let Some(token) = session {
        request = request.header(header::COOKIE, format!("{SESSION_COOKIE}={token}"));
    }
    if html {
        request = request.header(header::ACCEPT, "text/html,*/*");
    }
    request.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn an_admin_gets_the_ide_bundle_under_the_relaxed_policy() {
    let dir = ide_bundle("admin");
    let (router, sessions) = router_with_ide(Some(dir.clone()));
    let token = session_for(&sessions, ROLE_ADMIN);

    // The document: `/ide/` is the bundle's own index.html.
    let response = router
        .clone()
        .oneshot(get("/ide/", Some(&token), true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap(),
        IDE_CONTENT_SECURITY_POLICY,
        "the IDE is served under its own policy, not the strict one"
    );
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("id=\"workbench\""));

    // And its assets, addressed under the prefix.
    let response = router
        .clone()
        .oneshot(get("/ide/main.js", Some(&token), false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("export const workbench"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_ide_is_not_served_to_anyone_but_an_admin() {
    let dir = ide_bundle("denied");
    let (router, sessions) = router_with_ide(Some(dir.clone()));

    // An anonymous navigation is sent to the admin UI, which is where logging in
    // is possible — a 401 page would be a dead end.
    let response = router
        .clone()
        .oneshot(get("/ide/?store=app-source", None, true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers().get(header::LOCATION).unwrap(), "/");

    // An asset fetch with no session gets the ordinary rejection, and no bundle.
    let response = router
        .clone()
        .oneshot(get("/ide/main.js", None, false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("export const workbench"));

    // A logged-in non-admin is refused too: this milestone's IDE is admin-only.
    let public = session_for(&sessions, ROLE_PUBLIC);
    let response = router
        .clone()
        .oneshot(get("/ide/main.js", Some(&public), false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_relaxed_policy_stays_on_the_ide_route() {
    let dir = ide_bundle("policy");
    let (router, sessions) = router_with_ide(Some(dir.clone()));
    let token = session_for(&sessions, ROLE_ADMIN);

    // The admin SPA, fetched by the same admin in the same session, is still
    // strict — no inline styles, no blob workers, no eval.
    for path in ["/", "/tables", "/api/ping"] {
        let response = router
            .clone()
            .oneshot(get(path, Some(&token), true))
            .await
            .unwrap();
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .unwrap(),
            CONTENT_SECURITY_POLICY,
            "{path} must keep the strict policy"
        );
    }

    // `/ideas` is not the IDE's, despite the prefix: it falls through to the SPA.
    let response = router
        .clone()
        .oneshot(get("/ideas", None, true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap(),
        CONTENT_SECURITY_POLICY
    );

    // The relaxations are the ones §12.1 justifies, and nothing wider: no remote
    // script origin, and connections still only to this server.
    assert!(IDE_CONTENT_SECURITY_POLICY.contains("style-src 'self' 'unsafe-inline'"));
    assert!(IDE_CONTENT_SECURITY_POLICY.contains("worker-src 'self' blob:"));
    assert!(IDE_CONTENT_SECURITY_POLICY.contains("connect-src 'self' data: blob:"));
    assert!(!IDE_CONTENT_SECURITY_POLICY.contains("https:"));
    assert!(!IDE_CONTENT_SECURITY_POLICY.contains('*'));

    let _ = std::fs::remove_dir_all(&dir);
}

/// With no bundle, every path under the prefix is a 404 — **including the
/// document**, and never HTML.
///
/// This is the shape of a bug that already happened once. Answering a request for
/// `/ide/main.js` with a document gives the browser HTML where it expected a
/// module: it refuses it on its MIME type and renders a blank page, so the
/// fallback hides the very thing it was meant to explain. There is nothing to fall
/// back *for* — the IDE has no client-side routes — so there is no fallback.
#[tokio::test]
async fn without_a_bundle_nothing_under_the_prefix_is_html() {
    let (router, sessions) = router_with_ide(None);
    let token = session_for(&sessions, ROLE_ADMIN);

    for path in [
        "/ide/",
        "/ide/?store=app-source",
        "/ide/main.js",
        "/ide/main.css",
        "/ide/assets/onig.wasm",
    ] {
        let response = router
            .clone()
            .oneshot(get(path, Some(&token), true))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            !content_type.contains("text/html"),
            "{path} answered with {content_type}: HTML where a module was expected is \
             what renders as a blank page"
        );
    }
}

/// The same rule when a bundle *is* configured: a path the bundle does not contain
/// is missing, not the document.
#[tokio::test]
async fn an_asset_outside_the_bundle_is_a_404() {
    let dir = ide_bundle("missing-asset");
    let (router, sessions) = router_with_ide(Some(dir.clone()));
    let token = session_for(&sessions, ROLE_ADMIN);

    let response = router
        .clone()
        .oneshot(get("/ide/assets/not-there.js", Some(&token), false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(&dir);
}
