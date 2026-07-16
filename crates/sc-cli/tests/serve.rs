//! Smoke test for `saltcorn serve` (Phase 7): the CLI's boot path brings the
//! data layer up against a **real Postgres database** and the assembled server
//! answers its health route.
//!
//! This drives the same steps the binary does — [`DbConfig`] → [`connect_catalog`]
//! (connect + introspect + ensure the users table) → the admin router — but
//! stops short of binding a TCP port, exercising the router with a `oneshot`
//! request instead.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use sc_auth::SessionStore;
use sc_cli::{DbConfig, connect_catalog};
use sc_server::{AppMounts, ServerConfig, admin_handlers, build_router};
use sc_test_harness::TestDb;
use tower::ServiceExt;

/// Matches the harness fallback so a bare `cargo test` works in CI.
const DEFAULT_URL: &str = "postgres://saltcorn:saltcorn@localhost:5432/saltcorn_test";

/// Build a connection URL for the per-test database by replacing the database
/// name in the base `DATABASE_URL` (which the harness also reads).
fn url_for(db: &TestDb) -> String {
    let base = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_URL.to_owned());
    // Strip any query string, then replace the final `/dbname` path segment.
    let (authority_and_path, query) = match base.split_once('?') {
        Some((head, q)) => (head, Some(q)),
        None => (base.as_str(), None),
    };
    let cut = authority_and_path.rfind('/').expect("URL has a path");
    let mut url = format!("{}/{}", &authority_and_path[..cut], db.name());
    if let Some(q) = query {
        url.push('?');
        url.push_str(q);
    }
    url
}

#[tokio::test]
async fn serve_boots_against_a_db_and_answers_health() -> sc_error::Result<()> {
    let db = TestDb::new().await?;

    // The full CLI boot path: parse a URL, connect, init the catalog, bootstrap.
    let cfg = DbConfig::from_url(url_for(&db));
    let catalog = connect_catalog(&cfg).await?;

    // The users table now exists (bootstrap ran as part of connect_catalog).
    assert!(
        catalog.get(sc_auth::USERS_TABLE)?.is_some(),
        "connect_catalog should have bootstrapped the users table"
    );

    // Assemble the server exactly as `serve` does, then hit /health.
    let apps = Arc::new(AppMounts::new(catalog.clone()));
    let router = build_router(
        &sc_api::admin_endpoints(),
        admin_handlers(catalog, apps),
        Arc::new(SessionStore::default()),
        &ServerConfig::default(),
    )?;

    let response = router
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["status"], "ok");

    Ok(())
}

#[tokio::test]
async fn connect_to_an_unreachable_database_fails_loudly() {
    // A port nothing listens on: connecting must return an error, and it must
    // carry the redacted target for context (no silent half-boot).
    let cfg = DbConfig::from_url("postgres://saltcorn:secret@127.0.0.1:1/saltcorn_test");
    // `Arc<Catalog>` is not `Debug`, so match rather than `expect_err`.
    let msg = match connect_catalog(&cfg).await {
        Ok(_) => panic!("connecting to a dead port must fail"),
        Err(e) => e.to_string(),
    };
    assert!(
        msg.contains("127.0.0.1:1/saltcorn_test"),
        "error should name the target: {msg}"
    );
    assert!(!msg.contains("secret"), "error leaked the password: {msg}");
}
