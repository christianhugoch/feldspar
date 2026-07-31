//! Phase 1 integration test: the LLM-provider **configuration** API, driven
//! through the assembled router as a browser-based admin would.
//!
//! Most of it is the file-store configuration API's story with a different noun,
//! and is asserted here rather than trusted because "the same shape" is a claim
//! about code that was written twice. The part that is genuinely new is the
//! **secret**: a provider's API key must reach the browser as a sentinel and
//! never as itself, and a save that hands the sentinel back must leave the
//! stored key alone. That claim spans the declaration, the handler and the row,
//! so the only place it can be pinned is here — at the HTTP boundary, which is
//! the boundary the key must not cross.
//!
//! `testLlmProvider` is exercised against a **stub HTTP server**, never a
//! vendor: no test may require an API key or spend a token (TODO Phase 1,
//! decision 7).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sc_auth::SessionStore;
use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_llm::{CFG_API_KEY, CFG_BASE_URL, CFG_MODEL, list_llm_providers};
use sc_server::{AppMounts, CSRF_COOKIE, CSRF_HEADER, ServerConfig, admin_handlers, build_router};
use sc_test_harness::TestDb;
use sc_types::SECRET_SENTINEL;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

/// A stub Anthropic endpoint replaying `body` with `status`, so *Test
/// connection* has something to talk to that is not a vendor.
async fn stub_provider(status: u16, content_type: &'static str, body: String) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let head = head.clone();
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let _ = socket.read(&mut buf).await;
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(body.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });
    format!("http://{addr}")
}

/// An Anthropic SSE body whose whole answer is `text`.
fn anthropic_answer(text: &str) -> String {
    let events = [
        json!({
            "type": "message_start",
            "message": {
                "id": "msg_1", "role": "assistant", "content": [],
                "model": "claude-sonnet-4-5", "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": 8, "output_tokens": 0,
                          "cache_read_input_tokens": null, "cache_creation_input_tokens": null},
            }
        }),
        json!({"type": "content_block_start", "index": 0,
               "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
               "delta": {"type": "text_delta", "text": text}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta",
               "delta": {"stop_reason": "end_turn", "stop_sequence": null},
               "usage": {"output_tokens": 2}}),
    ];
    events
        .iter()
        .map(|e| format!("data: {e}\n\n"))
        .collect::<Vec<_>>()
        .join("")
}

/// A router over a real database with the platform tables bootstrapped, and an
/// admin logged in.
async fn setup() -> sc_error::Result<(Client, Arc<Catalog>, TestDb)> {
    let db = TestDb::new().await?;
    // Neutralise any `users` table inherited from the template database before
    // bootstrap introspects, exactly as the other server tests do.
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
    sc_app::bootstrap(&catalog).await?;
    sc_catalog::bootstrap_file_stores(&catalog).await?;
    sc_llm::bootstrap_llm_providers(&catalog).await?;

    let sessions = Arc::new(SessionStore::default());
    let apps = Arc::new(AppMounts::new(catalog.clone()));
    let router = build_router(
        &sc_api::admin_endpoints(),
        admin_handlers(catalog.clone(), apps),
        sessions,
        &ServerConfig::default(),
    )?;

    let mut client = Client::new(router);
    client.send("GET", "/api/auth/status", None).await;
    let (status, _) = client
        .send(
            "POST",
            "/api/first-user",
            Some(json!({ "email": "admin@example.com", "password": "hunter2pass" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    Ok((client, catalog, db))
}

#[tokio::test]
async fn a_provider_is_created_listed_edited_and_deleted() -> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;

    let (status, body) = client.send("GET", "/api/llm-providers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_array().unwrap().is_empty());

    let (status, created) = client
        .send(
            "POST",
            "/api/llm-providers",
            Some(json!({
                "name": "house",
                "description": "The house Anthropic key",
                "backend": "anthropic",
                "config": { "api_key": "sk-ant-real", "model": "claude-sonnet-4-5" },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["name"], json!("house"));
    assert!(created["id"].is_string());
    let id = created["id"].as_str().unwrap().to_owned();

    // Editing the description leaves everything else alone.
    let (status, updated) = client
        .send(
            "PUT",
            &format!("/api/llm-providers/{id}"),
            Some(json!({
                "name": "house",
                "description": "Renamed",
                "backend": "anthropic",
                "config": { "api_key": SECRET_SENTINEL, "model": "claude-opus-4-1" },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["description"], json!("Renamed"));
    assert_eq!(updated["config"][CFG_MODEL], json!("claude-opus-4-1"));

    let (_, listed) = client.send("GET", "/api/llm-providers", None).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);

    let (status, deleted) = client
        .send("DELETE", &format!("/api/llm-providers/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["deleted"], json!(true));
    assert!(list_llm_providers(&catalog).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn the_api_key_never_crosses_the_wire_and_a_save_does_not_destroy_it() -> sc_error::Result<()>
{
    let (mut client, catalog, _db) = setup().await?;

    let (status, created) = client
        .send(
            "POST",
            "/api/llm-providers",
            Some(json!({
                "name": "house",
                "description": "",
                "backend": "anthropic",
                "config": { "api_key": "sk-ant-real-secret", "model": "claude-sonnet-4-5" },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();

    // Even the response to the *create* — the request that carried the key —
    // reads it back redacted, because redaction happens where the record is
    // serialised rather than in a screen.
    assert_eq!(created["config"][CFG_API_KEY], json!(SECRET_SENTINEL));
    assert!(
        !created.to_string().contains("sk-ant"),
        "not even a prefix may leave: {created}"
    );

    // And so does the listing, which is the endpoint a second reader would use.
    let (_, listed) = client.send("GET", "/api/llm-providers", None).await;
    assert!(
        !listed.to_string().contains("sk-ant"),
        "the listing leaked the key: {listed}"
    );
    assert_eq!(listed[0]["config"][CFG_API_KEY], json!(SECRET_SENTINEL));

    // The stored key, meanwhile, is the real one — redaction is on the way out,
    // not on the way in.
    let stored = list_llm_providers(&catalog).await?;
    assert_eq!(stored[0].setting(CFG_API_KEY), Some("sk-ant-real-secret"));

    // The admin edits the name and saves, handing back exactly what they were
    // shown. This is the failure the sentinel contract exists to prevent: a
    // masked field written over the key.
    let (status, updated) = client
        .send(
            "PUT",
            &format!("/api/llm-providers/{id}"),
            Some(json!({
                "name": "house-anthropic",
                "description": "",
                "backend": "anthropic",
                "config": { "api_key": SECRET_SENTINEL, "model": "claude-sonnet-4-5" },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");

    let stored = list_llm_providers(&catalog).await?;
    assert_eq!(stored[0].name, "house-anthropic");
    assert_eq!(
        stored[0].setting(CFG_API_KEY),
        Some("sk-ant-real-secret"),
        "the sentinel must not be saved over the key"
    );

    // Replacing the key deliberately still works.
    let (status, _) = client
        .send(
            "PUT",
            &format!("/api/llm-providers/{id}"),
            Some(json!({
                "name": "house-anthropic",
                "description": "",
                "backend": "anthropic",
                "config": { "api_key": "sk-ant-rotated", "model": "claude-sonnet-4-5" },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let stored = list_llm_providers(&catalog).await?;
    assert_eq!(stored[0].setting(CFG_API_KEY), Some("sk-ant-rotated"));
    Ok(())
}

#[tokio::test]
async fn the_backends_endpoint_declares_the_key_as_a_secret() -> sc_error::Result<()> {
    let (mut client, _catalog, _db) = setup().await?;

    let (status, body) = client.send("GET", "/api/llm-provider-backends", None).await;
    assert_eq!(status, StatusCode::OK);
    let backends = body.as_array().unwrap();
    assert_eq!(backends.len(), 2, "{body}");

    for backend in backends {
        let spec = backend["config_spec"].as_array().unwrap();
        let key = spec
            .iter()
            .find(|f| f["name"] == json!(CFG_API_KEY))
            .unwrap_or_else(|| panic!("{} declares no api_key", backend["name"]));
        // This flag is the whole of what tells the form to render a password
        // input; without it the key would be typed in the clear.
        assert_eq!(key["secret"], json!(true), "{backend}");
        assert_eq!(key["required"], json!(true), "{backend}");
        // And the base URL is defaulted rather than required, which is what
        // makes an OpenAI-compatible endpoint a value the admin types.
        let base = spec
            .iter()
            .find(|f| f["name"] == json!(CFG_BASE_URL))
            .unwrap();
        assert_eq!(base["required"], json!(false));
        assert!(base["default"].is_string());
    }
    Ok(())
}

#[tokio::test]
async fn test_connection_reports_what_the_provider_said() -> sc_error::Result<()> {
    let (mut client, _catalog, _db) = setup().await?;
    let base = stub_provider(200, "text/event-stream", anthropic_answer("ok")).await;

    let (status, body) = client
        .send(
            "POST",
            "/api/llm-provider-test",
            Some(json!({
                "backend": "anthropic",
                "config": { "api_key": "sk-ant-x", "model": "claude-sonnet-4-5",
                            "base_url": base },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], json!(true), "{body}");
    // The model's own reply, so an admin pointed at the wrong endpoint sees a
    // wrong answer rather than a green tick.
    assert_eq!(body["message"], json!("ok"));
    assert_eq!(body["model"], json!("claude-sonnet-4-5"));
    Ok(())
}

#[tokio::test]
async fn a_rejected_key_is_a_reported_failure_not_a_broken_request() -> sc_error::Result<()> {
    let (mut client, _catalog, _db) = setup().await?;
    let base = stub_provider(
        401,
        "application/json",
        r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#
            .to_owned(),
    )
    .await;

    let (status, body) = client
        .send(
            "POST",
            "/api/llm-provider-test",
            Some(json!({
                "backend": "anthropic",
                "config": { "api_key": "sk-ant-wrong", "model": "claude-sonnet-4-5",
                            "base_url": base },
            })),
        )
        .await;

    // 200, because the provider refusing *is* the answer to "does this work?".
    // An error status would make the UI render it as a broken request rather
    // than as the diagnostic the admin asked for.
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], json!(false), "{body}");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.contains("invalid x-api-key") || message.contains("401"),
        "the provider's own words are the actionable part: {message}"
    );
    Ok(())
}

#[tokio::test]
async fn testing_a_saved_provider_does_not_require_retyping_its_key() -> sc_error::Result<()> {
    let (mut client, _catalog, _db) = setup().await?;
    let base = stub_provider(200, "text/event-stream", anthropic_answer("ok")).await;

    let (_, created) = client
        .send(
            "POST",
            "/api/llm-providers",
            Some(json!({
                "name": "house",
                "description": "",
                "backend": "anthropic",
                "config": { "api_key": "sk-ant-real", "model": "claude-sonnet-4-5",
                            "base_url": base },
            })),
        )
        .await;
    let id = created["id"].as_str().unwrap().to_owned();

    // The form sends back exactly what it was shown — the sentinel — plus the
    // provider's id. Without the id there is nothing to resolve it against,
    // which is why the test-connection body carries one.
    let (status, body) = client
        .send(
            "POST",
            "/api/llm-provider-test",
            Some(json!({
                "id": id,
                "backend": "anthropic",
                "config": created["config"],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], json!(true), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_structurally_wrong_provider_is_refused_where_the_admin_can_fix_it()
-> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;

    // No API key.
    let (status, body) = client
        .send(
            "POST",
            "/api/llm-providers",
            Some(json!({
                "name": "house", "description": "", "backend": "anthropic",
                "config": { "model": "claude-sonnet-4-5" },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains(CFG_API_KEY), "{body}");

    // A backend nothing implements. A *configuration* error rather than an
    // invalid one — 422, as an unknown file-store backend is — because the
    // request was well-formed and the system cannot serve it, and the error
    // names what there is.
    let (status, body) = client
        .send(
            "POST",
            "/api/llm-providers",
            Some(json!({
                "name": "house", "description": "", "backend": "bedrock",
                "config": { "api_key": "x" },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.to_string().contains("anthropic"), "{body}");

    // A setting the backend never declared, which is a typo rather than
    // something to accept and ignore.
    let (status, body) = client
        .send(
            "POST",
            "/api/llm-providers",
            Some(json!({
                "name": "house", "description": "", "backend": "anthropic",
                "config": { "api_key": "x", "model": "m", "temperture": 0.5 },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("temperture"), "{body}");

    // Nothing was written by any of them.
    assert!(list_llm_providers(&catalog).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn two_providers_cannot_share_a_name() -> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;

    let body = json!({
        "name": "house", "description": "", "backend": "anthropic",
        "config": { "api_key": "sk-1", "model": "claude-sonnet-4-5" },
    });
    let (status, _) = client
        .send("POST", "/api/llm-providers", Some(body.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, err) = client.send("POST", "/api/llm-providers", Some(body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    assert!(err.to_string().contains("house"), "{err}");
    assert_eq!(list_llm_providers(&catalog).await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn every_provider_endpoint_requires_a_session() -> sc_error::Result<()> {
    let (admin, _catalog, _db) = setup().await?;

    // A client with no session. The admin API's `login` authenticates *admins*
    // only, so "a logged-in non-admin" is not a state this surface can be in —
    // anonymous is the whole of what an unauthorised caller looks like here, and
    // it is what these endpoints have to refuse.
    let mut user = Client::new(admin.router.clone());
    user.send("GET", "/api/auth/status", None).await;

    // A provider holds an API key; nothing below admin may read, write or test
    // one. Asserted per endpoint rather than for one, because each carries its
    // own `AuthRequirement` and a missing one would not show up anywhere else.
    for (method, path, body) in [
        ("GET", "/api/llm-providers".to_owned(), None),
        (
            "POST",
            "/api/llm-providers".to_owned(),
            Some(
                json!({"name": "x", "description": "", "backend": "anthropic",
                        "config": {"api_key": "k", "model": "m"}}),
            ),
        ),
        ("GET", "/api/llm-provider-backends".to_owned(), None),
        (
            "POST",
            "/api/llm-provider-test".to_owned(),
            Some(json!({"backend": "anthropic", "config": {"api_key": "k", "model": "m"}})),
        ),
    ] {
        let (status, _) = user.send(method, &path, body).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} was reachable without a session"
        );
    }
    Ok(())
}
