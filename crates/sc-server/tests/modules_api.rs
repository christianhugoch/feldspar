//! Modules through the admin API, and a trigger that runs one — the whole
//! milestone from the outside.
//!
//! What is asserted here and nowhere else is the **live** part: installing a
//! module through the API makes its action available to a trigger *on the server
//! that is already running*, with no restart, and deleting the module takes it
//! away again. That claim spans the installer, the Node host, the action
//! registry, the dispatcher and the row layer, so the HTTP boundary is the only
//! place it can be pinned.
//!
//! Everything here needs `node` and `npm`, and skips without them — a Rust-only
//! checkout stays green, as it does for the `tsc` tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sc_action::{Action, EventKind, Trigger};
use sc_auth::SessionStore;
use sc_catalog::{Catalog, DataField};
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_server::{
    AppMounts, CSRF_COOKIE, CSRF_HEADER, ModuleServices, ServerConfig, admin_handlers,
    build_router_with_apps, default_js_evaluator, install_agents, install_triggers,
};
use sc_test_harness::TestDb;
use sc_types::{BasicType, SECRET_SENTINEL, TypeRef};
use serde_json::{Value, json};
use tower::ServiceExt;

/// One of `sc-module`'s fixture packages — the same v1-shaped module its own
/// tests use, rather than a second one to keep in step.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../sc-module/tests/fixtures")
        .join(name)
}

fn have(program: &str) -> bool {
    std::process::Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

macro_rules! skip_without_node {
    () => {
        if !have("node") || !have("npm") {
            eprintln!("skipping: node and npm are not both on the PATH");
            return Ok(());
        }
    };
}

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

/// A server with the module machinery installed over a throwaway modules root,
/// and an admin logged in.
struct Server {
    client: Client,
    catalog: Arc<Catalog>,
    modules: Arc<ModuleServices>,
    dispatcher: Arc<sc_action::TriggerDispatcher>,
    root: PathBuf,
    _db: TestDb,
}

async fn setup(tag: &str) -> sc_error::Result<Server> {
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
    sc_catalog::bootstrap_file_stores(&catalog).await?;

    let agents = install_agents(&catalog).await?;
    let dispatcher = install_triggers(&catalog, default_js_evaluator(), &agents).await?;
    let root = std::env::temp_dir().join(format!("sc-modules-api-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let modules =
        ModuleServices::install(&catalog, &dispatcher, &agents, Some(root.clone())).await?;

    let sessions = Arc::new(SessionStore::default());
    let apps = Arc::new(
        AppMounts::new(catalog.clone())
            .with_evaluator(default_js_evaluator())
            .with_triggers(dispatcher.clone())
            .with_agents(agents)
            .with_modules(modules.clone()),
    );
    let router = build_router_with_apps(
        &sc_api::admin_endpoints(),
        admin_handlers(catalog.clone(), apps.clone()),
        sessions,
        &ServerConfig::default(),
        apps,
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

    Ok(Server {
        client,
        catalog,
        modules,
        dispatcher,
        root,
        _db: db,
    })
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Install the echo fixture through the API.
async fn install_echo(client: &mut Client) -> Value {
    let (status, body) = client
        .send(
            "POST",
            "/api/modules",
            Some(json!({
                "source": "local",
                "location": fixture("echo-module").display().to_string(),
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

#[tokio::test]
async fn a_module_is_installed_listed_configured_and_deleted() -> sc_error::Result<()> {
    skip_without_node!();
    let mut server = setup("crud").await?;
    let client = &mut server.client;

    // Nothing installed, but the tab still knows where packages go and whether
    // the toolchain that installs them is there.
    let (status, body) = client.send("GET", "/api/modules", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["modules"].as_array().unwrap().is_empty());
    assert_eq!(body["npm"], json!(true));
    assert_eq!(body["node"], json!(true));
    assert!(body["root"].as_str().unwrap().contains("sc-modules-api"));

    let installed = install_echo(client).await;
    assert_eq!(installed["name"], json!("@saltcorn-test/echo"));
    assert_eq!(installed["version"], json!("0.1.0"));
    assert_eq!(installed["source"], json!("local"));
    assert_eq!(installed["loaded"], json!(true));
    assert_eq!(installed["api_version"], json!(1));
    let id = installed["id"].as_str().unwrap().to_owned();

    // The actions it supplies, with their settings translated from v1's
    // `configFields` — this is what the trigger form renders.
    let actions = installed["actions"].as_array().unwrap();
    let echo_row = actions
        .iter()
        .find(|a| a["name"] == json!("echo_row"))
        .expect("echo_row is listed");
    assert_eq!(
        echo_row["description"],
        json!("Echo what the action was given")
    );
    let spec = echo_row["config_spec"].as_array().unwrap();
    assert_eq!(spec[0]["name"], json!("greeting"));
    assert_eq!(spec[0]["required"], json!(true));
    assert_eq!(spec[1]["type"], json!("int"));

    // What it also supplies and this version does not load, so the admin knows
    // what they are not getting.
    let census = installed["unsupported"].as_array().unwrap();
    assert!(
        census.iter().any(|e| e["key"] == json!("viewtemplates")),
        "{census:?}"
    );

    // The module's own settings, from its `configuration_workflow`.
    let config_spec = installed["config_spec"].as_array().unwrap();
    assert_eq!(config_spec[0]["name"], json!("endpoint"));
    assert_eq!(config_spec[1]["secret"], json!(true));

    // Configure it: the endpoint and a password.
    let (status, saved) = client
        .send(
            "PUT",
            &format!("/api/modules/{id}"),
            Some(json!({
                "configuration": { "endpoint": "https://example.test", "token": "s3cret" },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["configuration"]["endpoint"],
        json!("https://example.test")
    );
    // The secret never comes back.
    assert_eq!(saved["configuration"]["token"], json!(SECRET_SENTINEL));

    // …and a save that hands the sentinel back keeps the stored value.
    let (status, saved) = client
        .send(
            "PUT",
            &format!("/api/modules/{id}"),
            Some(json!({
                "configuration": { "endpoint": "https://elsewhere.test", "token": SECRET_SENTINEL },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let stored = sc_module::load_module_by_name(&server.catalog, "@saltcorn-test/echo")
        .await?
        .unwrap();
    assert_eq!(stored.configuration.get("token"), Some(&json!("s3cret")));
    assert_eq!(
        stored.configuration.get("endpoint"),
        Some(&json!("https://elsewhere.test"))
    );

    // Reload is idempotent and reports what it loaded.
    let (status, reloaded) = client.send("POST", "/api/modules-reload", None).await;
    assert_eq!(status, StatusCode::OK, "{reloaded}");
    assert_eq!(reloaded["modules"], json!(1));

    let (status, deleted) = client
        .send("DELETE", &format!("/api/modules/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert_eq!(deleted["deleted"], json!(true));
    let (_, body) = client.send("GET", "/api/modules", None).await;
    assert!(body["modules"].as_array().unwrap().is_empty());

    Ok(())
}

#[tokio::test]
async fn a_modules_action_becomes_available_to_a_trigger_with_no_restart() -> sc_error::Result<()> {
    skip_without_node!();
    let mut server = setup("trigger").await?;

    // Before the module is installed, its action is not one a trigger may name.
    let (status, body) = server.client.send("GET", "/api/actions", None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<String> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(!names.contains(&"echo_row".to_owned()), "{names:?}");

    let installed = install_echo(&mut server.client).await;
    let id = installed["id"].as_str().unwrap().to_owned();

    // Afterwards it is — on the same running server, through the same registry
    // the admin UI's action picker reads.
    let (_, body) = server.client.send("GET", "/api/actions", None).await;
    let echo = body
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == json!("echo_row"))
        .expect("the module's action is in the picker");
    assert_eq!(echo["config_spec"][0]["name"], json!("greeting"));

    // A table, and a trigger on it whose action is the module's.
    let books = server
        .catalog
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
    let _ = books;

    let mut trigger = Trigger::new("echo a book", EventKind::Insert, "echo_row");
    trigger.channel = Some("books".into());
    trigger
        .configuration
        .insert("greeting".into(), json!("hello"));
    sc_action::save_trigger(&server.catalog, &server.dispatcher.registry(), &trigger).await?;
    server.dispatcher.reload(&server.catalog).await?;

    // Running it reaches the module, with the trigger's configuration.
    let result = server
        .dispatcher
        .run_trigger(&server.catalog, "echo a book", json!({ "id": 1 }), None)
        .await?;
    assert_eq!(result["greeting"], json!("hello"));

    // Deleting the module takes the action away again, and the trigger that
    // named it says so rather than silently doing nothing.
    let (status, _) = server
        .client
        .send("DELETE", &format!("/api/modules/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(server.modules.modules().modules().is_empty());
    let err = server
        .dispatcher
        .run_trigger(&server.catalog, "echo a book", json!({ "id": 2 }), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("echo_row"), "{err}");

    Ok(())
}

#[tokio::test]
async fn a_module_that_claims_a_built_ins_name_is_reported_and_does_not_take_it()
-> sc_error::Result<()> {
    skip_without_node!();
    let mut server = setup("clash").await?;

    let (status, installed) = server
        .client
        .send(
            "POST",
            "/api/modules",
            Some(json!({
                "source": "local",
                "location": fixture("clash-module").display().to_string(),
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{installed}");

    // It is installed and loaded, its own action is available, and the clash is
    // an issue the tab shows rather than a silent substitution.
    assert_eq!(installed["loaded"], json!(true));
    let issues = installed["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(
        issues[0].as_str().unwrap().contains("insert_row"),
        "{issues:?}"
    );

    let registry = server.dispatcher.registry();
    assert!(registry.get("clash_ok").is_some());
    // The built-in still answers to `insert_row`: which implementation runs must
    // not depend on what somebody installed.
    assert_eq!(
        registry.require("insert_row").unwrap().description(),
        sc_core_actions::InsertRow.description()
    );

    Ok(())
}

#[tokio::test]
async fn installing_a_module_is_admin_only() -> sc_error::Result<()> {
    skip_without_node!();
    let server = setup("auth").await?;
    // A fresh client with no session: every module endpoint is configuration,
    // and configuration is admin-only.
    let mut anonymous = Client::new(server.client.router.clone());
    for (method, path) in [
        ("GET", "/api/modules"),
        ("POST", "/api/modules"),
        ("POST", "/api/modules-reload"),
    ] {
        let (status, _) = anonymous.send(method, path, Some(json!({}))).await;
        assert!(
            status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN,
            "{method} {path} answered {status}"
        );
    }
    Ok(())
}
