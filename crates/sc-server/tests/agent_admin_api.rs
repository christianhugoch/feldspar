//! Phase 4 integration test: the **agent** configuration API and the run
//! history, driven through the assembled router as the admin SPA drives them.
//!
//! Three things are asserted here and nowhere else, because each one spans the
//! declaration, the handler and the row:
//!
//! 1. **A save is validated against the same trait registry the chat runs
//!    with.** An agent naming a provider that does not exist, a trait that is not
//!    registered, or two traits whose tools would collide is refused while the
//!    admin is still looking at the form.
//! 2. **An agent that stopped being usable stays listed, with its reason.** Drop
//!    the table a trait was configured against and the agent is still there, with
//!    `error` saying what to fix — because editing it is the repair.
//! 3. **A run outlives the agent it was of.** `subject` is the agent's *name*
//!    (§11.4), so deleting the definition leaves the transcript readable.
//!
//! No provider is contacted: nothing here runs a turn. That is the chat socket's
//! test (`agent_chat.rs`), which drives one against a scripted provider.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sc_agent::{Agent, AgentLoop, EnabledTrait, Run, RunCaller, save_run};
use sc_auth::SessionStore;
use sc_catalog::{Catalog, DataField};
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_llm::LlmProviderDef;
use sc_server::{AppMounts, CSRF_COOKIE, CSRF_HEADER, ServerConfig, admin_handlers, build_router};
use sc_test_harness::TestDb;
use sc_types::{BasicType, TypeRef};
use serde_json::{Value, json};
use tower::ServiceExt;

/// A cookie-jar-carrying client over the router (CSRF + session), as the other
/// admin-API tests use.
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
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
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

/// A router over a real database with the platform tables bootstrapped, agents
/// installed, one LLM provider stored and an admin logged in.
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
    let agents = sc_server::install_agents(&catalog).await?;

    // One provider to point agents at. Nothing calls it — the endpoints under
    // test resolve it by name and never open a connection.
    sc_llm::save_llm_provider(
        &catalog,
        &LlmProviderDef::anthropic("house", "sk-ant-test", "claude-sonnet-4-5"),
    )
    .await?;

    let sessions = Arc::new(SessionStore::default());
    let apps = Arc::new(AppMounts::new(catalog.clone()).with_agents(agents));
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

/// A table with a primary key, so a `query_table` trait configured against it
/// validates.
async fn create_books(catalog: &Catalog) -> sc_error::Result<()> {
    catalog
        .create_table(
            "books",
            &[
                DataField::plain("id", TypeRef::Basic(BasicType::Int))
                    .required()
                    .primary_key(),
                DataField::plain("title", TypeRef::Basic(BasicType::Text)),
            ],
        )
        .await?;
    Ok(())
}

/// The body a form posts for an agent with no traits.
fn plain_agent(name: &str) -> Value {
    json!({
        "name": name,
        "description": "answers questions",
        "provider": "house",
        "model": null,
        "system_prompt": "You are helpful.",
        "traits": [],
        "min_role": null,
        "attributes": {},
    })
}

#[tokio::test]
async fn an_agent_is_created_listed_edited_and_deleted() -> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;
    create_books(&catalog).await?;

    let (status, body) = client.send("GET", "/api/agents", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_array().unwrap().is_empty());

    let mut create = plain_agent("librarian");
    create["traits"] = json!([{ "trait": "query_table", "config": { "table": "books" } }]);
    create["attributes"] = json!({ "max_steps": 5 });
    let (status, created) = client.send("POST", "/api/agents", Some(create)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["traits"][0]["trait"], json!("query_table"));
    assert_eq!(created["error"], Value::Null);

    // The stored row is the definition: what comes back on a list is what was
    // saved, traits and sparse attributes included.
    let (_, listed) = client.send("GET", "/api/agents", None).await;
    let agent = &listed.as_array().unwrap()[0];
    assert_eq!(agent["name"], json!("librarian"));
    assert_eq!(agent["provider"], json!("house"));
    assert_eq!(agent["system_prompt"], json!("You are helpful."));
    assert_eq!(agent["traits"][0]["config"]["table"], json!("books"));
    assert_eq!(agent["attributes"]["max_steps"], json!(5));
    assert_eq!(agent["min_role"], Value::Null);

    // An edit that adds a second trait over the same table: two grants, two
    // tools, one agent.
    let mut edit = plain_agent("librarian");
    edit["traits"] = json!([
        { "trait": "query_table", "config": { "table": "books" } },
        { "trait": "insert_row", "config": { "table": "books" } },
    ]);
    edit["min_role"] = json!(1);
    let (status, updated) = client
        .send("PUT", &format!("/api/agents/{id}"), Some(edit))
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["traits"].as_array().unwrap().len(), 2);
    assert_eq!(updated["min_role"], json!(1));

    let (status, deleted) = client
        .send("DELETE", &format!("/api/agents/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["deleted"], json!(true));
    let (_, listed) = client.send("GET", "/api/agents", None).await;
    assert!(listed.as_array().unwrap().is_empty());
    Ok(())
}

/// Everything the *save* refuses, refused where it is fixable (§11.2): while the
/// admin is still looking at the form, not inside a conversation.
#[tokio::test]
async fn a_save_is_validated_against_the_registry_the_chat_runs_with() -> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;
    create_books(&catalog).await?;

    let mut missing_provider = plain_agent("a");
    missing_provider["provider"] = json!("nowhere");
    let (status, body) = client
        .send("POST", "/api/agents", Some(missing_provider))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("nowhere"),
        "the refusal must name the provider: {body}"
    );

    let mut unknown_trait = plain_agent("b");
    unknown_trait["traits"] = json!([{ "trait": "read_minds", "config": {} }]);
    let (status, body) = client
        .send("POST", "/api/agents", Some(unknown_trait))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("read_minds"), "{body}");

    let mut missing_table = plain_agent("c");
    missing_table["traits"] = json!([{ "trait": "query_table", "config": { "table": "shelves" } }]);
    let (status, body) = client
        .send("POST", "/api/agents", Some(missing_table))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("shelves"), "{body}");

    // Two instances of one trait over one table derive the same tool name, and
    // the model could not tell them apart.
    let mut colliding = plain_agent("d");
    colliding["traits"] = json!([
        { "trait": "query_table", "config": { "table": "books" } },
        { "trait": "query_table", "config": { "table": "books" } },
    ]);
    let (status, body) = client.send("POST", "/api/agents", Some(colliding)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("query_books"), "{body}");

    // And a name is a key: two agents cannot share one.
    let (status, _) = client
        .send("POST", "/api/agents", Some(plain_agent("twin")))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = client
        .send("POST", "/api/agents", Some(plain_agent("twin")))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    Ok(())
}

/// An agent whose world changed underneath it is **not** hidden: it is listed
/// with the reason, because the list is the screen the repair happens on.
#[tokio::test]
async fn an_agent_whose_table_was_dropped_stays_listed_with_its_reason() -> sc_error::Result<()> {
    let (mut client, catalog, db) = setup().await?;
    create_books(&catalog).await?;

    let mut create = plain_agent("librarian");
    create["traits"] = json!([{ "trait": "query_table", "config": { "table": "books" } }]);
    let (status, _) = client.send("POST", "/api/agents", Some(create)).await;
    assert_eq!(status, StatusCode::CREATED);

    db.client()
        .await?
        .batch_execute("DROP TABLE books")
        .await
        .map_err(|e| sc_error::Error::database(e.to_string()))?;
    catalog.reload().await?;

    let (status, listed) = client.send("GET", "/api/agents", None).await;
    assert_eq!(status, StatusCode::OK);
    let agent = &listed.as_array().unwrap()[0];
    assert_eq!(agent["name"], json!("librarian"));
    let reason = agent["error"].as_str().unwrap_or("");
    assert!(
        reason.contains("books"),
        "the reason must name what to fix: {reason}"
    );
    Ok(())
}

/// The traits endpoint is what makes the agent form generic: it declares each
/// trait's configuration in the same `FormField` vocabulary every other settings
/// form in the admin UI renders.
#[tokio::test]
async fn the_traits_endpoint_declares_each_traits_configuration() -> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;
    create_books(&catalog).await?;

    let (status, traits) = client.send("GET", "/api/agent-traits", None).await;
    assert_eq!(status, StatusCode::OK);
    let traits = traits.as_array().unwrap();
    let names: Vec<&str> = traits.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"query_table"), "{names:?}");
    assert!(names.contains(&"run_trigger"), "{names:?}");

    let query = traits
        .iter()
        .find(|t| t["name"] == json!("query_table"))
        .unwrap();
    assert!(!query["description"].as_str().unwrap_or("").is_empty());
    let table = query["config_spec"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == json!("table"))
        .expect("the table setting is declared");
    assert_eq!(table["required"], json!(true));
    // Every trait that names a *target* declares it as required, which is what
    // stops a blank trait form being savable — the same property the trait
    // registry's own test asserts, seen here through the wire shape the form
    // renders from.
    //
    // `admin_copilot` is the exception, and the reason is §11.3's: it names
    // no table, because the tables it makes do not exist when it is configured.
    // Its form is four grants, each with a default, and a blank one is a
    // meaningful (read-only) configuration rather than an incomplete one — which
    // the wire shape has to be able to express, or the admin UI would refuse to
    // save a valid agent.
    for trait_ in traits {
        let spec = trait_["config_spec"].as_array().unwrap();
        assert!(!spec.is_empty(), "{trait_}");
        if trait_["name"] == json!("admin_copilot") {
            assert!(
                spec.iter().all(|f| f["type"] == json!("bool")),
                "the trait with no target is configured entirely by grants: {trait_}"
            );
            continue;
        }
        assert!(
            spec.iter().any(|f| f["required"] == json!(true)),
            "{trait_}"
        );
    }
    Ok(())
}

/// The run history: listed newest first without their transcripts, read whole
/// one at a time, and outliving the agent they were of.
#[tokio::test]
async fn runs_are_listed_read_and_deleted_and_outlive_their_agent() -> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;

    let (status, created) = client
        .send("POST", "/api/agents", Some(plain_agent("librarian")))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let agent_id = created["id"].as_str().unwrap().to_owned();

    // Two runs, written the way the chat socket writes them.
    let mut older = AgentLoop::new(20);
    older.push_user("how many books?")?;
    let older = Run::new("librarian", &RunCaller::system(), &older).description("how many books?");
    save_run(&catalog, &older).await?;
    let mut newer = AgentLoop::new(20);
    newer.push_user("and who by?")?;
    let newer = Run::new("librarian", &RunCaller::system(), &newer).description("and who by?");
    save_run(&catalog, &newer).await?;

    let (status, runs) = client.send("GET", "/api/agent-runs/librarian", None).await;
    assert_eq!(status, StatusCode::OK);
    let runs = runs.as_array().unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0]["subject"], json!("librarian"));
    assert_eq!(runs[0]["state"], json!("running"));
    // The list carries no transcript: a sidebar of dozens of conversations must
    // not be dozens of conversations.
    assert!(runs[0].get("context").is_none(), "{}", runs[0]);
    assert!(!runs[0]["description"].as_str().unwrap_or("").is_empty());

    // A run of another agent is not in this agent's history.
    let (_, none) = client.send("GET", "/api/agent-runs/other", None).await;
    assert!(none.as_array().unwrap().is_empty());

    let (status, whole) = client
        .send("GET", &format!("/api/runs/{}", newer.id.0), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(whole["id"], json!(newer.id.0.to_string()));
    // The transcript is in `context`, as the loop stored it.
    let messages = whole["context"]["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], json!("user"));
    assert_eq!(messages[0]["content"], json!("and who by?"));

    // Deleting the agent leaves its runs: `subject` is the name, and a
    // transcript that vanished with the definition would take the record of
    // what happened with it (§11.4).
    let (status, _) = client
        .send("DELETE", &format!("/api/agents/{agent_id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, runs) = client.send("GET", "/api/agent-runs/librarian", None).await;
    assert_eq!(runs.as_array().unwrap().len(), 2);

    let (status, deleted) = client
        .send("DELETE", &format!("/api/runs/{}", older.id.0), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["deleted"], json!(true));
    let (_, runs) = client.send("GET", "/api/agent-runs/librarian", None).await;
    assert_eq!(runs.as_array().unwrap().len(), 1);

    // A run that is not there is a 404, not an empty answer.
    let (status, _) = client
        .send("GET", &format!("/api/runs/{}", uuid::Uuid::new_v4()), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

/// A provider an agent still calls through cannot be deleted out from under it.
/// `sc-llm` cannot see `_sc_agents` from a layer below, so the server collects
/// the references and passes them in — the check exists only if that happens.
#[tokio::test]
async fn a_provider_an_agent_uses_cannot_be_deleted() -> sc_error::Result<()> {
    let (mut client, _catalog, _db) = setup().await?;
    let (status, _) = client
        .send("POST", "/api/agents", Some(plain_agent("librarian")))
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, providers) = client.send("GET", "/api/llm-providers", None).await;
    let provider_id = providers.as_array().unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (status, body) = client
        .send("DELETE", &format!("/api/llm-providers/{provider_id}"), None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("librarian"), "{body}");

    // With the agent gone, the provider goes too.
    let (_, agents) = client.send("GET", "/api/agents", None).await;
    let agent_id = agents.as_array().unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    client
        .send("DELETE", &format!("/api/agents/{agent_id}"), None)
        .await;
    let (status, _) = client
        .send("DELETE", &format!("/api/llm-providers/{provider_id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    Ok(())
}

/// Every agent and run endpoint is an admin's, and says so before it does
/// anything.
#[tokio::test]
async fn the_agent_endpoints_require_an_admin_session() -> sc_error::Result<()> {
    let (mut client, _catalog, _db) = setup().await?;
    let (status, _) = client
        .send("POST", "/api/agents", Some(plain_agent("librarian")))
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // A fresh client with no session at all.
    let mut anonymous = Client::new(client.router.clone());
    for (method, path) in [
        ("GET", "/api/agents"),
        ("GET", "/api/agent-traits"),
        ("GET", "/api/agent-runs/librarian"),
    ] {
        let (status, _) = anonymous.send(method, path, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
    Ok(())
}

/// A stored agent read back through the library is the agent that was posted —
/// the row *is* the definition (§9), so this is the claim the API's shape rests
/// on.
#[tokio::test]
async fn the_posted_agent_is_the_stored_agent() -> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;
    create_books(&catalog).await?;

    let mut create = plain_agent("librarian");
    create["model"] = json!("claude-opus-4-1");
    create["traits"] = json!([{
        "trait": "query_table",
        "config": { "table": "books", "max_rows": 25 },
    }]);
    let (status, _) = client.send("POST", "/api/agents", Some(create)).await;
    assert_eq!(status, StatusCode::CREATED);

    let stored = sc_agent::load_agent_by_name(&catalog, "librarian")
        .await?
        .expect("the agent is stored");
    let expected = Agent::with_id(stored.id, "librarian", "house")
        .description("answers questions")
        .model("claude-opus-4-1")
        .system_prompt("You are helpful.")
        .with_trait(
            EnabledTrait::new("query_table")
                .config("table", "books")
                .config("max_rows", 25),
        );
    assert_eq!(stored, expected);
    Ok(())
}
