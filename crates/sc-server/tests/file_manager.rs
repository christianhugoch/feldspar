//! End-to-end integration tests for the file-manager API (Phase 8, final item):
//! connect a file store to the catalog, then browse/read/write it through the
//! assembled router as a browser-based admin would, and round-trip per-file xattr
//! metadata through the connected store handle.
//!
//! Like `admin_api.rs` this drives the concrete [`admin_handlers`] over a **real
//! Postgres database** (needed only for auth/bootstrap here) with the session and
//! CSRF cookies a browser carries. The file store is a [`LocalFileStore`] rooted
//! at a fresh temp directory, connected to the catalog by name.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use sc_auth::SessionStore;
use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_files::{FileMeta, FileStore, LocalFileStore};
use sc_server::{AppMounts, CSRF_COOKIE, CSRF_HEADER, ServerConfig, admin_handlers, build_router};
use sc_test_harness::TestDb;
use serde_json::{Value, json};
use tower::ServiceExt;

/// A cookie-jar-carrying client over the router (CSRF + session), mirroring the
/// helper in `admin_api.rs`.
struct Client {
    router: Router,
    cookies: HashMap<String, String>,
}

impl Client {
    fn new(router: Router) -> Client {
        Client {
            router,
            cookies: HashMap::new(),
        }
    }

    async fn send(&mut self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if !self.cookies.is_empty() {
            let cookie_header = self
                .cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            builder = builder.header(header::COOKIE, cookie_header);
        }
        if method != "GET" && method != "HEAD" {
            if let Some(csrf) = self.cookies.get(CSRF_COOKIE) {
                builder = builder.header(CSRF_HEADER, csrf);
            }
        }
        let request = match body {
            Some(ref b) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(b).unwrap()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };

        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        for raw in response.headers().get_all(header::SET_COOKIE) {
            if let Ok(text) = raw.to_str() {
                let pair = text.split(';').next().unwrap_or("");
                if let Some((name, value)) = pair.split_once('=') {
                    if value.is_empty() {
                        self.cookies.remove(name);
                    } else {
                        self.cookies.insert(name.to_owned(), value.to_owned());
                    }
                }
            }
        }
        let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, value)
    }
}

/// A fresh, unique temp directory (created on disk) for one store.
fn temp_dir() -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "sc-file-manager-test-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    base
}

/// Build a router whose catalog has one connected file store (`docs`) rooted at a
/// fresh temp directory, log in an admin, and return the client, the store handle,
/// and the `TestDb` (kept in scope: dropping it deletes the database).
async fn setup() -> sc_error::Result<(Client, Arc<dyn FileStore>, TestDb)> {
    let db = TestDb::new().await?;
    db.client()
        .await?
        .batch_execute(
            "DO $$ DECLARE r record; BEGIN \
               FOR r IN SELECT table_schema FROM information_schema.tables \
               WHERE table_name = 'users' AND table_type = 'BASE TABLE' LOOP \
                 EXECUTE format('DROP TABLE IF EXISTS %I.users CASCADE', r.table_schema); \
               END LOOP; END $$",
        )
        .await
        .map_err(|e| sc_error::Error::database(e.to_string()))?;

    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let catalog = Arc::new(Catalog::init(driver as Arc<dyn DatabaseDriver>).await?);
    sc_auth::bootstrap(&catalog).await?;
    // `listFileStores` reads the stored definitions as well as the connected
    // registry (§1.4), so the table has to exist — the real boot path bootstraps
    // it in `connect_catalog` alongside `users` and `_sc_applications`.
    sc_catalog::bootstrap_file_stores(&catalog).await?;

    // Connect a local file store to the catalog, by name.
    let store = Arc::new(LocalFileStore::new("docs", temp_dir())?) as Arc<dyn FileStore>;
    catalog.connect_file_store(store.clone())?;

    let sessions = Arc::new(SessionStore::default());
    let apps = Arc::new(AppMounts::new(catalog.clone()));
    let router = build_router(
        &sc_api::admin_endpoints(),
        admin_handlers(catalog, apps),
        sessions,
        &ServerConfig::default(),
    )?;

    let mut client = Client::new(router);
    // Prime the CSRF cookie (a safe GET mints it) before the first mutation.
    client.send("GET", "/api/auth/status", None).await;
    // Bootstrap + log in an admin so the (admin-only) file endpoints are reachable.
    let (status, _) = client
        .send(
            "POST",
            "/api/first-user",
            Some(json!({ "email": "admin@example.com", "password": "hunter2pass" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    Ok((client, store, db))
}

#[tokio::test]
async fn file_manager_browse_read_write() -> sc_error::Result<()> {
    let (mut client, store, _db) = setup().await?;

    // --- the store is listed as connected --------------------------------
    let (status, body) = client.send("GET", "/api/file-stores", None).await;
    assert_eq!(status, StatusCode::OK);
    let stores = body.as_array().unwrap();
    assert_eq!(stores.len(), 1);
    assert_eq!(stores[0]["name"], json!("docs"));
    assert_eq!(stores[0]["is_git_repo"], json!(false));

    // --- write a text file (edit-a-text-file / upload) -------------------
    let (status, entry) = client
        .send(
            "POST",
            "/api/file-stores/docs/write",
            Some(json!({ "path": "notes/readme.md", "text": "# Hello\n" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(entry["name"], json!("readme.md"));
    assert_eq!(entry["path"], json!("notes/readme.md"));
    assert_eq!(entry["is_dir"], json!(false));
    assert_eq!(entry["size"], json!(8));

    // The bytes really landed in the store.
    assert_eq!(&store.read("notes/readme.md").await?[..], b"# Hello\n");

    // --- browse the root, then the sub-directory -------------------------
    let (_, body) = client
        .send(
            "POST",
            "/api/file-stores/docs/browse",
            Some(json!({ "dir": "" })),
        )
        .await;
    let names: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert_eq!(names, ["notes"]);
    assert_eq!(body[0]["is_dir"], json!(true));

    let (_, body) = client
        .send(
            "POST",
            "/api/file-stores/docs/browse",
            Some(json!({ "dir": "notes" })),
        )
        .await;
    assert_eq!(body[0]["name"], json!("readme.md"));
    assert_eq!(body[0]["path"], json!("notes/readme.md"));
    assert_eq!(body[0]["size"], json!(8));

    // --- read it back: text shortcut + base64 (download) -----------------
    let (status, body) = client
        .send(
            "POST",
            "/api/file-stores/docs/read",
            Some(json!({ "path": "notes/readme.md" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["text"], json!("# Hello\n"));
    assert_eq!(body["size"], json!(8));
    let decoded = BASE64.decode(body["base64"].as_str().unwrap()).unwrap();
    assert_eq!(decoded, b"# Hello\n");

    // --- upload arbitrary bytes via base64, read them back ---------------
    let raw: &[u8] = &[0u8, 159, 146, 150]; // not valid UTF-8
    let (status, _) = client
        .send(
            "POST",
            "/api/file-stores/docs/write",
            Some(json!({ "path": "blob.bin", "base64": BASE64.encode(raw) })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, body) = client
        .send(
            "POST",
            "/api/file-stores/docs/read",
            Some(json!({ "path": "blob.bin" })),
        )
        .await;
    // Non-UTF-8 bytes → no `text`, but base64 round-trips exactly.
    assert_eq!(body["text"], Value::Null);
    let decoded = BASE64.decode(body["base64"].as_str().unwrap()).unwrap();
    assert_eq!(decoded, raw);

    // --- error cases -----------------------------------------------------
    // Unknown store → 404.
    let (status, _) = client
        .send(
            "POST",
            "/api/file-stores/nope/browse",
            Some(json!({ "dir": "" })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Both `text` and `base64` → 400.
    let (status, _) = client
        .send(
            "POST",
            "/api/file-stores/docs/write",
            Some(json!({ "path": "x", "text": "a", "base64": "YQ==" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    Ok(())
}

#[tokio::test]
async fn xattr_metadata_round_trips_through_the_connected_store() -> sc_error::Result<()> {
    let (mut client, store, _db) = setup().await?;

    // Create a file through the API, then round-trip per-file metadata (stored in
    // xattrs, no DB row — design §9) through the same store handle the catalog
    // resolved by name.
    let (status, _) = client
        .send(
            "POST",
            "/api/file-stores/docs/write",
            Some(json!({ "path": "doc.md", "text": "# hi" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // No metadata set yet → defaults.
    assert_eq!(store.get_meta("doc.md").await?, FileMeta::default());

    let mut meta = FileMeta {
        min_role: Some(30),
        ..Default::default()
    };
    meta.attributes
        .insert("mime".into(), "text/markdown".into());
    store.set_meta("doc.md", &meta).await?;
    assert_eq!(store.get_meta("doc.md").await?, meta);

    // The metadata lives beside the bytes, so it never appears as a file in a
    // browse listing.
    let (_, body) = client
        .send(
            "POST",
            "/api/file-stores/docs/browse",
            Some(json!({ "dir": "" })),
        )
        .await;
    let names: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert_eq!(names, ["doc.md"]);

    // And the file's bytes are unchanged by the metadata write.
    assert_eq!(&store.read("doc.md").await?[..], b"# hi");

    Ok(())
}
