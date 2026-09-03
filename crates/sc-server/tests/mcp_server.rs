//! Phase 4 integration test: the administration MCP server (design §13.6).
//!
//! Driven through the assembled router against a real Postgres database, the
//! way an external coding agent drives it — a bearer token, a JSON-RPC body, no
//! cookie anywhere.
//!
//! What is asserted here and cannot be asserted anywhere else, because each one
//! spans the route, the credential and the tool set:
//!
//! 1. **The four gates come in the right order.** A disabled server is a `404`
//!    that never reads the token table; an `Origin` header is a refusal; a
//!    non-local peer is refused while `mcp_loopback_only` is on; and a session
//!    cookie with a valid CSRF token — and no bearer — gets nowhere.
//! 2. **A token's six flags are the tool list.** `allow_triggers` off takes the
//!    trigger tools out of `tools/list` altogether, both the hand-written four
//!    and the tagged endpoints in that half.
//! 3. **A refusal is a result, not a transport error.** An ungranted operation
//!    comes back as `isError: true` with the sentence the model reads; an unknown
//!    *tool* is a JSON-RPC error, because there is nothing there for a model to
//!    correct.
//! 4. **A tier-2 tool really dispatches through the handler registry**, as the
//!    token's user, and comes back with what the endpoint returns.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use sc_auth::SessionStore;
use sc_catalog::Catalog;
use sc_config::{MCP_ENABLED, MCP_LOOPBACK_ONLY, set_config};
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_server::{
    AppMounts, CSRF_COOKIE, CSRF_HEADER, MCP_PROTOCOL_VERSION, MCP_ROUTE, ServerConfig,
    admin_handlers, build_router_with_apps,
};
use sc_test_harness::TestDb;
use serde_json::{Value, json};
use tower::ServiceExt;

/// The cookie-jar admin client the other server suites use, for the half of this
/// test that mints the token through the ordinary admin API.
struct Client {
    router: Router,
    cookies: HashMap<String, String>,
}

impl Client {
    async fn send(&mut self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if !self.cookies.is_empty() {
            let jar = self
                .cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            builder = builder.header(header::COOKIE, jar);
        }
        if method != "GET"
            && method != "HEAD"
            && let Some(csrf) = self.cookies.get(CSRF_COOKIE)
        {
            builder = builder.header(CSRF_HEADER, csrf);
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
            if let Ok(text) = raw.to_str()
                && let Some((name, value)) = text.split(';').next().unwrap_or("").split_once('=')
            {
                if value.is_empty() {
                    self.cookies.remove(name);
                } else {
                    self.cookies.insert(name.to_owned(), value.to_owned());
                }
            }
        }
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
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

/// One MCP request, built the way a client builds it: a bearer credential, a
/// JSON-RPC body, a peer address and **no cookie**.
struct McpRequest {
    token: Option<String>,
    origin: Option<&'static str>,
    peer: Option<&'static str>,
    cookies: Option<String>,
    csrf: Option<String>,
    body: Value,
}

impl McpRequest {
    /// The ordinary case: this token, this JSON-RPC message, from localhost.
    fn new(token: &str, body: Value) -> McpRequest {
        McpRequest {
            token: Some(token.to_owned()),
            origin: None,
            peer: Some("127.0.0.1:51234"),
            cookies: None,
            csrf: None,
            body,
        }
    }

    async fn send(self, router: &Router) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(MCP_ROUTE)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = &self.token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(origin) = self.origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        if let Some(cookies) = &self.cookies {
            builder = builder.header(header::COOKIE, cookies);
        }
        if let Some(csrf) = &self.csrf {
            builder = builder.header(CSRF_HEADER, csrf);
        }
        let mut request = builder
            .body(Body::from(serde_json::to_vec(&self.body).unwrap()))
            .unwrap();
        // The peer address arrives as a request extension in production because
        // the listener is bound with connect info; `oneshot` has no listener, so
        // the test puts it there itself. Leaving it out is the "unknown peer"
        // case, which the loopback check treats as remote.
        if let Some(peer) = self.peer {
            let addr: SocketAddr = peer.parse().unwrap();
            request.extensions_mut().insert(ConnectInfo(addr));
        }
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
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

/// A router over a real database with the platform tables bootstrapped, an admin
/// signed in and the MCP server switched **on**.
async fn setup() -> sc_error::Result<(Client, Router, Arc<Catalog>, TestDb)> {
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
    sc_app::bootstrap(&catalog).await?;
    sc_config::bootstrap(&catalog).await?;
    sc_catalog::bootstrap_table_meta(&catalog).await?;
    sc_catalog::bootstrap_field_meta(&catalog).await?;
    sc_catalog::bootstrap_file_stores(&catalog).await?;
    sc_llm::bootstrap_llm_providers(&catalog).await?;
    let agents = sc_server::install_agents(&catalog).await?;
    // One provider for an agent to name. Nothing calls it: no turn is run here.
    sc_llm::save_llm_provider(
        &catalog,
        &sc_llm::LlmProviderDef::anthropic("house", "sk-ant-test", "claude-sonnet-4-5"),
    )
    .await?;
    let triggers =
        sc_server::install_triggers(&catalog, sc_server::default_js_evaluator(), &agents).await?;

    let sessions = Arc::new(SessionStore::default());
    let apps = Arc::new(
        AppMounts::new(catalog.clone())
            .with_agents(agents)
            .with_triggers(triggers),
    );
    // `build_router_with_apps` rather than `build_router`, because the MCP route
    // reaches the catalog through the same `AppMounts` the app subdomains do —
    // and that is the call the real server makes (see `serve`). A router built
    // with no mounts has no catalog and therefore no MCP surface, which is the
    // same answer as the switch being off.
    let router = build_router_with_apps(
        &sc_api::admin_endpoints(),
        admin_handlers(catalog.clone(), apps.clone()),
        sessions,
        &ServerConfig::default(),
        apps,
    )?;

    let mut client = Client {
        router: router.clone(),
        cookies: HashMap::new(),
    };
    client.send("GET", "/api/auth/status", None).await;
    let (status, _) = client
        .send(
            "POST",
            "/api/first-user",
            Some(json!({ "email": "admin@example.com", "password": "hunter2pass" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    Ok((client, router, catalog, db))
}

/// Turn the server on, with the loopback switch as given.
async fn enable_mcp(catalog: &Catalog, loopback_only: bool) -> sc_error::Result<()> {
    set_config(catalog, MCP_ENABLED, json!(true)).await?;
    set_config(catalog, MCP_LOOPBACK_ONLY, json!(loopback_only)).await
}

/// Mint a token through the admin API, as an administrator does.
async fn mint(client: &mut Client, label: &str, grants: Value) -> String {
    let (status, minted) = client
        .send(
            "POST",
            "/api/api-tokens",
            Some(json!({ "label": label, "grants": grants })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{minted}");
    minted["secret"]
        .as_str()
        .expect("the one plaintext")
        .to_owned()
}

/// The tool names a `tools/list` answer carries.
fn tool_names(listed: &Value) -> Vec<String> {
    listed["result"]["tools"]
        .as_array()
        .expect("a tools array")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

// --- the gates --------------------------------------------------------------

/// Off by default, and the difference between off and absent is deliberately
/// invisible. The token is minted first so the assertion is about the *switch*
/// and not about the credential.
#[tokio::test]
async fn a_disabled_server_is_a_404_and_a_valid_token_does_not_change_that() -> sc_error::Result<()>
{
    let (mut client, router, catalog, _db) = setup().await?;
    let token = mint(&mut client, "claude-code on my laptop", json!({})).await;

    let (status, body) = McpRequest::new(
        &token,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    )
    .send(&router)
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    // Not a word about tokens: a disabled feature should not be distinguishable
    // from one that was never built.
    assert!(!body.to_string().contains("token"), "{body}");

    // And the same body reaches the same route once it is switched on, with no
    // restart in between.
    enable_mcp(&catalog, true).await?;
    let (status, body) = McpRequest::new(
        &token,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    )
    .send(&router)
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(())
}

/// The confused-deputy story, asserted: a browser that is *logged in as this
/// very admin*, with a CSRF token the SPA would send, reaches nothing.
#[tokio::test]
async fn a_session_cookie_and_a_csrf_token_are_not_a_credential_here() -> sc_error::Result<()> {
    let (client, router, catalog, _db) = setup().await?;
    enable_mcp(&catalog, true).await?;
    // The admin's live session and CSRF token, exactly as the SPA holds them.
    let session = client
        .cookies
        .get("sc_session")
        .cloned()
        .expect("a session");
    let csrf = client
        .cookies
        .get(CSRF_COOKIE)
        .cloned()
        .expect("a csrf token");

    let request = McpRequest {
        token: None,
        origin: None,
        peer: Some("127.0.0.1:51234"),
        cookies: Some(format!("sc_session={session}; {CSRF_COOKIE}={csrf}")),
        csrf: Some(csrf.clone()),
        body: json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    };
    let (status, body) = request.send(&router).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("Bearer"), "{body}");
    // The CSRF middleware did not refuse it — the route did. If the middleware
    // had, the status would be 403 and the message would be about CSRF, and the
    // exemption would be silently doing nothing.
    assert!(!message.contains("CSRF"), "{body}");
    Ok(())
}

/// A bearer request carries neither the CSRF cookie nor the header, so without
/// the exemption of §13.6 every POST here would be a `403` before the route ran.
#[tokio::test]
async fn a_bearer_request_is_exempt_from_the_csrf_check() -> sc_error::Result<()> {
    let (mut client, router, catalog, _db) = setup().await?;
    enable_mcp(&catalog, true).await?;
    let token = mint(&mut client, "laptop", json!({})).await;

    let (status, body) = McpRequest::new(
        &token,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
    )
    .send(&router)
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], json!({}));
    Ok(())
}

/// Per the specification's DNS-rebinding guidance: refused outright, not
/// validated against a list.
#[tokio::test]
async fn a_request_carrying_an_origin_is_refused() -> sc_error::Result<()> {
    let (mut client, router, catalog, _db) = setup().await?;
    enable_mcp(&catalog, true).await?;
    let token = mint(&mut client, "laptop", json!({})).await;

    let mut request = McpRequest::new(
        &token,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
    );
    request.origin = Some("https://evil.example.com");
    let (status, body) = request.send(&router).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["error"].as_str().unwrap().contains("Origin"), "{body}");
    Ok(())
}

/// The loopback switch, and what an unknown peer counts as.
#[tokio::test]
async fn a_remote_peer_is_refused_while_the_loopback_switch_is_on() -> sc_error::Result<()> {
    let (mut client, router, catalog, _db) = setup().await?;
    enable_mcp(&catalog, true).await?;
    let token = mint(&mut client, "laptop", json!({})).await;
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });

    let mut request = McpRequest::new(&token, ping.clone());
    request.peer = Some("203.0.113.7:40000");
    let (status, body) = request.send(&router).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["error"].as_str().unwrap().contains("local"), "{body}");

    // A peer nobody can name is remote, because the safer answer is the one to
    // fall back to.
    let mut request = McpRequest::new(&token, ping.clone());
    request.peer = None;
    let (status, _) = request.send(&router).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // …and with the switch off, the same remote peer is served.
    enable_mcp(&catalog, false).await?;
    let mut request = McpRequest::new(&token, ping);
    request.peer = Some("203.0.113.7:40000");
    let (status, body) = request.send(&router).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(())
}

/// Revocation is what taking a token back means, and it takes effect on the very
/// next call because the lookup reads the row rather than a cached copy.
#[tokio::test]
async fn a_revoked_token_stops_working_on_the_next_call() -> sc_error::Result<()> {
    let (mut client, router, catalog, _db) = setup().await?;
    enable_mcp(&catalog, true).await?;
    let token = mint(&mut client, "the laptop I lost", json!({})).await;
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });

    let (status, _) = McpRequest::new(&token, ping.clone()).send(&router).await;
    assert_eq!(status, StatusCode::OK);

    let (_, list) = client.send("GET", "/api/api-tokens", None).await;
    let id = list[0]["id"].as_str().unwrap().to_owned();
    let (status, _) = client
        .send("POST", &format!("/api/api-tokens/{id}/revoke"), None)
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = McpRequest::new(&token, ping).send(&router).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    // Which of revoked, expired, unknown and demoted this was — the four are
    // different things to tell somebody.
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("revoked"),
        "{body}"
    );

    // An invented token is refused too, and by a different sentence.
    let (status, body) = McpRequest::new(
        "fspk_notarealtoken",
        json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
    )
    .send(&router)
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not recognised"),
        "{body}"
    );
    Ok(())
}

// --- the protocol -----------------------------------------------------------

/// The handshake and the pinned revision.
#[tokio::test]
async fn initialize_answers_with_the_one_revision_this_server_speaks() -> sc_error::Result<()> {
    let (mut client, router, catalog, _db) = setup().await?;
    enable_mcp(&catalog, true).await?;
    let token = mint(&mut client, "laptop", json!({})).await;

    let (status, body) = McpRequest::new(
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "claude-code", "version": "1.0.0" },
            },
        }),
    )
    .send(&router)
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["result"]["protocolVersion"],
        json!(MCP_PROTOCOL_VERSION)
    );
    assert!(
        body["result"]["capabilities"]["tools"].is_object(),
        "{body}"
    );
    assert!(body["result"]["serverInfo"]["name"].is_string(), "{body}");

    // A different revision is refused rather than negotiated by accident, and
    // the refusal names both.
    let (status, body) = McpRequest::new(
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "initialize",
            "params": { "protocolVersion": "1999-01-01" },
        }),
    )
    .send(&router)
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains(MCP_PROTOCOL_VERSION), "{body}");
    assert!(message.contains("1999-01-01"), "{body}");

    // The notification that follows takes no answer at all.
    let (status, _) = McpRequest::new(
        &token,
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .send(&router)
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    Ok(())
}

/// The six flags **are** the tool list: an area that is off takes its tools out
/// of the listing rather than leaving them to be refused, and it does so for
/// both tiers, because a checkbox that meant two things would not be one
/// vocabulary.
#[tokio::test]
async fn a_token_without_the_trigger_area_is_offered_no_trigger_tools() -> sc_error::Result<()> {
    let (mut client, router, catalog, _db) = setup().await?;
    enable_mcp(&catalog, true).await?;

    let full = mint(&mut client, "everything", json!({})).await;
    let narrow = mint(
        &mut client,
        "schema only",
        json!({ "allow_triggers": false, "allow_applications": false }),
    )
    .await;
    let list = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });

    let (_, body) = McpRequest::new(&full, list.clone()).send(&router).await;
    let offered = tool_names(&body);
    // Tier 1: the nine composite tools.
    for name in [
        "describe_schema",
        "edit_schema",
        "describe_triggers",
        "describe_action",
        "save_trigger",
        "delete_trigger",
        "describe_applications",
        "save_api_query",
        "delete_api_query",
    ] {
        assert!(offered.contains(&name.to_owned()), "{offered:?}");
    }
    // Tier 2: the tagged endpoints, under the endpoint's own name.
    for name in [
        "listFieldTypes",
        "listAgents",
        "createAgent",
        "listRuns",
        "getRun",
        "listTriggers",
        "listActions",
        "saveWorkflow",
        "listApplications",
        "buildApplication",
    ] {
        assert!(offered.contains(&name.to_owned()), "{offered:?}");
    }
    // Tier 3 is absent — the number of tools is the design constraint.
    for absent in ["listRows", "writeFile", "listUsers", "createApiToken"] {
        assert!(!offered.contains(&absent.to_owned()), "{offered:?}");
    }

    let (_, body) = McpRequest::new(&narrow, list).send(&router).await;
    let offered = tool_names(&body);
    for gone in [
        // The hand-written four…
        "describe_triggers",
        "save_trigger",
        "delete_trigger",
        "describe_action",
        // …and the tagged endpoints in the same half.
        "listTriggers",
        "listActions",
        "getWorkflow",
        "saveWorkflow",
        "listWorkflowRuns",
        // …and the applications half, likewise.
        "describe_applications",
        "listApplications",
        "buildApplication",
    ] {
        assert!(!offered.contains(&gone.to_owned()), "{offered:?}");
    }
    // The schema half has no area, so it is always there.
    assert!(
        offered.contains(&"describe_schema".to_owned()),
        "{offered:?}"
    );
    assert!(
        offered.contains(&"listFieldTypes".to_owned()),
        "{offered:?}"
    );

    // Every listed tool declares an object `inputSchema`, which is what an MCP
    // client requires.
    for tool in body["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["type"], json!("object"), "{tool}");
        assert!(
            !tool["description"].as_str().unwrap_or_default().is_empty(),
            "{tool}"
        );
    }
    Ok(())
}

/// A tier-2 tool is a real request through the real registry: the answer is what
/// the endpoint's handler returns, for the token's user.
#[tokio::test]
async fn a_tagged_endpoint_is_dispatched_through_the_handler_registry() -> sc_error::Result<()> {
    let (mut client, router, catalog, _db) = setup().await?;
    enable_mcp(&catalog, true).await?;
    let token = mint(&mut client, "laptop", json!({})).await;

    // A read with no arguments at all.
    let (status, body) = McpRequest::new(
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "listFieldTypes", "arguments": {} },
        }),
    )
    .send(&router)
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"]["isError"], json!(false), "{body}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    // The type vocabulary `edit_schema` writes a field in.
    assert!(
        text.contains("\"text\"") && text.contains("\"int\""),
        "{text}"
    );

    // A write, whose effect is visible through the ordinary admin API — which is
    // the point of one authorization model: the row an agent made is the row the
    // screen shows.
    let (status, body) = McpRequest::new(
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "createAgent",
                "arguments": {
                    "name": "helper",
                    "description": "made over MCP",
                    "provider": "house",
                    "model": null,
                    "system_prompt": "Be helpful.",
                    "min_role": 1,
                    "traits": [],
                    "attributes": {},
                },
            },
        }),
    )
    .send(&router)
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"]["isError"], json!(false), "{body}");

    let (_, agents) = client.send("GET", "/api/agents", None).await;
    assert_eq!(agents.as_array().unwrap().len(), 1, "{agents}");
    assert_eq!(agents[0]["name"], json!("helper"));
    Ok(())
}

/// The two shapes a failure can take, and which is which.
#[tokio::test]
async fn an_unknown_tool_is_a_protocol_error_and_a_refused_grant_is_a_result()
-> sc_error::Result<()> {
    let (mut client, router, catalog, _db) = setup().await?;
    enable_mcp(&catalog, true).await?;
    // Create and edit, not drop — the milestone's own example.
    let token = mint(
        &mut client,
        "claude-code on my laptop",
        json!({ "allow_create": true, "allow_edit": true, "allow_drop": false }),
    )
    .await;

    // An unknown tool is wrong with the *call*: there is nothing inside a tool
    // that does not exist for a model to correct.
    let (status, body) = McpRequest::new(
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "drop_everything", "arguments": {} },
        }),
    )
    .send(&router)
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["error"]["code"], json!(-32601), "{body}");
    assert!(body["result"].is_null(), "{body}");

    // An ungranted operation is a **result**: the model reads it, and it names
    // the operation and the flag that would allow it.
    let (status, body) = McpRequest::new(
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "edit_schema",
                "arguments": { "operations": [{ "op": "drop_table", "table": "invoices" }] },
            },
        }),
    )
    .send(&router)
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"]["isError"], json!(true), "{body}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("allow_drop"), "{text}");

    // And the same flag governs the tier-2 tool that destroys something, so the
    // six flags mean one thing whichever tier a tool came from.
    let (_, body) = McpRequest::new(
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "deleteAgent",
                "arguments": { "id": "11111111-2222-3333-4444-555555555555" },
            },
        }),
    )
    .send(&router)
    .await;
    assert_eq!(body["result"]["isError"], json!(true), "{body}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("allow_drop"), "{text}");

    // A method this server does not implement is a protocol error too.
    let (_, body) = McpRequest::new(
        &token,
        json!({ "jsonrpc": "2.0", "id": 4, "method": "resources/list" }),
    )
    .send(&router)
    .await;
    assert_eq!(body["error"]["code"], json!(-32601), "{body}");
    Ok(())
}
