//! Phase 3.3 integration test: creating and editing fields through the admin API,
//! against a real Postgres database (design §6, §3.3).
//!
//! What is asserted, end to end through the router: `createField` takes a `type`
//! (basic or rich) and derives the column's SQL type from it; a rich field and a
//! `File` field come back from `listFields` with their overlay merged on;
//! `updateField` edits the overlay in place; `listFieldTypes` offers basic types,
//! rich types and the Key/File kinds each with the attribute spec the editor
//! renders; and an unknown type name is refused.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sc_auth::SessionStore;
use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_server::{AppMounts, CSRF_COOKIE, CSRF_HEADER, ServerConfig, admin_handlers, build_router};
use sc_test_harness::TestDb;
use serde_json::{Value, json};
use tower::ServiceExt;

/// A cookie-jar client over the router (session + CSRF), as a browser SPA would.
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

/// A router over the admin endpoints with a logged-in admin and a `book` table.
async fn setup() -> sc_error::Result<(Client, TestDb)> {
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
    sc_catalog::bootstrap_field_meta(&catalog).await?;

    let sessions = Arc::new(SessionStore::default());
    let apps = Arc::new(AppMounts::new(catalog.clone()));
    let router = build_router(
        &sc_api::admin_endpoints(),
        admin_handlers(catalog, apps),
        sessions,
        &ServerConfig::default(),
    )?;
    let mut client = Client::new(router);

    // Seed the CSRF cookie before the first mutation (double-submit).
    client.send("GET", "/api/auth/status", None).await;
    client
        .send(
            "POST",
            "/api/first-user",
            Some(json!({ "email": "admin@example.com", "password": "hunter2pass" })),
        )
        .await;
    client
        .send("POST", "/api/tables", Some(json!({ "name": "book" })))
        .await;
    Ok((client, db))
}

/// The field named `name` from a `listFields` array.
fn field<'a>(fields: &'a Value, name: &str) -> &'a Value {
    fields
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == json!(name))
        .unwrap_or_else(|| panic!("field `{name}` not listed"))
}

#[tokio::test]
async fn create_read_and_edit_a_rich_field_and_a_file_field() -> sc_error::Result<()> {
    let (mut client, _db) = setup().await?;

    // A rich `String` field: `type` is the rich type's name, and its attributes
    // ride along. The SQL type is derived (text), never sent.
    let (status, body) = client
        .send(
            "POST",
            "/api/tables/book/fields",
            Some(json!({
                "name": "title",
                "type": "string",
                "attributes": { "max_length": 200, "regex": "^[A-Za-z ]+$" }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["type"], json!("string"));
    assert_eq!(
        body["sql_type"],
        json!("text"),
        "derived from the rich type"
    );
    assert_eq!(body["attributes"]["max_length"], json!(200));

    // A `File` field: a plain text column plus a File kind pointing at a store,
    // folder and MIME allow-list.
    let (status, body) = client
        .send(
            "POST",
            "/api/tables/book/fields",
            Some(json!({
                "name": "cover",
                "type": "text",
                "kind": { "type": "file", "store": "uploads", "folder": "covers",
                          "mime_allow": ["image/png"] }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["kind"]["type"], json!("file"));
    assert_eq!(body["kind"]["store"], json!("uploads"));

    // Both read back merged through `listFields`.
    let (_, fields) = client.send("GET", "/api/tables/book/fields", None).await;
    assert_eq!(field(&fields, "title")["type"], json!("string"));
    assert_eq!(
        field(&fields, "title")["attributes"]["max_length"],
        json!(200)
    );
    assert_eq!(field(&fields, "cover")["kind"]["type"], json!("file"));
    assert_eq!(field(&fields, "cover")["kind"]["folder"], json!("covers"));

    // `updateField` edits the overlay in place. The whole overlay is stated (as
    // for a table's settings), so the type is re-sent to keep the field rich.
    let (status, body) = client
        .send(
            "PUT",
            "/api/tables/book/fields/title",
            Some(json!({
                "type": "string",
                "label": "Book title",
                "attributes": { "max_length": 100 }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["label"], json!("Book title"));
    assert_eq!(body["attributes"]["max_length"], json!(100));
    assert_eq!(body["type"], json!("string"), "still rich after the edit");

    Ok(())
}

#[tokio::test]
async fn list_field_types_offers_basic_rich_and_kinds_with_specs() -> sc_error::Result<()> {
    let (mut client, _db) = setup().await?;

    let (status, body) = client.send("GET", "/api/field-types", None).await;
    assert_eq!(status, StatusCode::OK);
    let types = body.as_array().unwrap();

    let by_name = |name: &str| types.iter().find(|t| t["name"] == json!(name));

    // A basic type, with no attribute spec.
    let text = by_name("text").expect("text listed");
    assert_eq!(text["category"], json!("basic"));
    assert!(text["config_spec"].as_array().unwrap().is_empty());

    // A rich type, carrying its attribute spec (so the editor can render it).
    let string = by_name("string").expect("string listed");
    assert_eq!(string["category"], json!("rich"));
    let spec_names: Vec<&str> = string["config_spec"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect();
    assert!(spec_names.contains(&"max_length"), "{spec_names:?}");

    // The File kind, with its parameter spec.
    let file = by_name("file").expect("file kind listed");
    assert_eq!(file["category"], json!("kind"));
    let file_params: Vec<&str> = file["config_spec"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect();
    assert!(file_params.contains(&"store"), "{file_params:?}");

    // And the Key kind exists too.
    assert!(by_name("key").is_some(), "key kind listed");
    Ok(())
}

#[tokio::test]
async fn an_unknown_type_is_refused_by_name() -> sc_error::Result<()> {
    let (mut client, _db) = setup().await?;

    let (status, body) = client
        .send(
            "POST",
            "/api/tables/book/fields",
            Some(json!({ "name": "mystery", "type": "wibble" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("wibble"),
        "the error names the unknown type: {body}"
    );

    // And no half-made column was left behind.
    let (_, fields) = client.send("GET", "/api/tables/book/fields", None).await;
    assert!(
        fields
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["name"] != json!("mystery")),
        "no `mystery` column exists"
    );
    Ok(())
}

/// A **calculated field**: created with no column (a virtual overlay field), an
/// `sc-expr` expression under a `calc` kind, computed on read, not writable, and
/// with a broken expression refused up front.
#[tokio::test]
async fn create_and_read_a_calculated_field() -> sc_error::Result<()> {
    let (mut client, _db) = setup().await?;

    // A real column the calc field reads.
    let (status, body) = client
        .send(
            "POST",
            "/api/tables/book/fields",
            Some(json!({ "name": "pages", "type": "int8" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // The calc field: no storage type of its own matters — `pages * 2` computed
    // on read. `type` names only how the value is displayed.
    let (status, body) = client
        .send(
            "POST",
            "/api/tables/book/fields",
            Some(json!({
                "name": "double_pages",
                "type": "int8",
                "kind": { "type": "calc", "expression": "pages * 2" }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["kind"]["type"], json!("calc"));
    assert_eq!(body["kind"]["expression"], json!("pages * 2"));

    // It reads back merged, and is *not* a real column: `information_schema` has
    // `pages` but no `double_pages`.
    let (_, fields) = client.send("GET", "/api/tables/book/fields", None).await;
    assert_eq!(
        field(&fields, "double_pages")["kind"]["type"],
        json!("calc")
    );
    let cols = _db
        .client()
        .await?
        .query(
            "SELECT column_name FROM information_schema.columns WHERE table_name = 'book'",
            &[],
        )
        .await
        .map_err(|e| sc_error::Error::database(e.to_string()))?;
    let names: Vec<String> = cols.iter().map(|r| r.get::<_, String>(0)).collect();
    assert!(
        names.iter().any(|n| n == "pages"),
        "pages is a column: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "double_pages"),
        "the calc field is virtual, not a column: {names:?}"
    );

    // Computed on read: insert a row with pages = 100, and the calc field is 200.
    let (status, row) = client
        .send(
            "POST",
            "/api/tables/book/rows",
            Some(json!({ "pages": 100 })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let (_, rows) = client.send("GET", "/api/tables/book/rows", None).await;
    let first = &rows.as_array().unwrap()[0];
    assert_eq!(
        first["double_pages"],
        json!(200),
        "computed on read: {first}"
    );

    // A calc field is not writable: naming it on a write is refused.
    let (status, body) = client
        .send(
            "POST",
            "/api/tables/book/rows",
            Some(json!({ "pages": 5, "double_pages": 999 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // A broken expression is a 400 naming the field, and writes no overlay.
    let (status, body) = client
        .send(
            "POST",
            "/api/tables/book/fields",
            Some(json!({
                "name": "bad_calc",
                "type": "int8",
                "kind": { "type": "calc", "expression": "nonexistent_col + 1" }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("bad_calc"),
        "the error names the field: {body}"
    );

    // Using `user` in a calc field is refused (no caller in the calc scope).
    let (status, body) = client
        .send(
            "POST",
            "/api/tables/book/fields",
            Some(json!({
                "name": "mine",
                "type": "int8",
                "kind": { "type": "calc", "expression": "pages === user.id" }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("user"),
        "the error mentions `user`: {body}"
    );

    // Neither invalid field was created.
    let (_, fields) = client.send("GET", "/api/tables/book/fields", None).await;
    for missing in ["bad_calc", "mine"] {
        assert!(
            fields
                .as_array()
                .unwrap()
                .iter()
                .all(|f| f["name"] != json!(missing)),
            "`{missing}` must not exist"
        );
    }

    Ok(())
}
