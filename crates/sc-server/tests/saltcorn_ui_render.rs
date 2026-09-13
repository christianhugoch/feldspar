//! Saltcorn UI, rendering (TODO "Saltcorn UI" 5.8): v1's own view patterns
//! serving a restored Saltcorn 1 backup's views over HTTP, through the assembled
//! router, on the application's subdomain.
//!
//! The data is the real `saltcorn-v1-BooksDB.zip`, restored through the two
//! requests the Backup screen makes; the views are that backup's own, with their
//! v1 configurations unchanged (the import itself is Phase 8). Two claims:
//!
//! - **Live**: *List Books* renders as a document — the layout, the menu, the
//!   assets, a row per book with its author joined in — under Saltcorn UI's
//!   CSP; the page embedding the Filter renders; `/` names what there is; a
//!   viewer below a view's role is refused; the static assets are served.
//! - **Golden**: each of the six patterns (List, Show, Edit, Feed, Filter,
//!   ListShowList) renders, as an ajax request gets it, exactly the committed
//!   HTML. This is what catches a shim returning something plausible and wrong,
//!   and what lets the vendored copy be refreshed with confidence.
//!   `SC_UPDATE_GOLDEN=1` rewrites the expected files.
//!
//! Both need the built Saltcorn UI bundle, and skip without it.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sc_api::admin_endpoints;
use sc_app::{Application, FrameworkRef, save_application};
use sc_auth::SessionStore;
use sc_catalog::{Catalog, FileStoreId, TableId};
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_files::FileStoreDef;
use sc_server::{
    AppMounts, CSRF_COOKIE, CSRF_HEADER, ModuleServices, ServerConfig, admin_handlers,
    build_and_mount, build_router_with_apps, default_js_evaluator, install_agents,
    install_triggers,
};
use sc_test_harness::TestDb;
use sc_viewpattern::{Page, SALTCORN_UI_FRAMEWORK, View};
use serde_json::{Value, json};
use tower::ServiceExt;

const BASE_DOMAIN: &str = "example.com";
const APP_HOST: &str = "booksdb.example.com";
const ADMIN: &str = "admin@example.com";
const PASSWORD: &str = "hunter2pass";
const V1_BACKUP: &[u8] = include_bytes!("fixtures/saltcorn-v1-BooksDB.zip");

/// The built bundle's directory, if there is one.
fn bundle_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(sc_viewpattern::BUNDLE_DIR_IN_CHECKOUT);
    dir.join(sc_viewpattern::VIEW_RUNTIME_FILE)
        .is_file()
        .then_some(dir)
}

/// A cookie-carrying client over the router, which can also speak to an
/// application's subdomain.
struct Client {
    router: Router,
    cookies: HashMap<String, String>,
}

struct Answer {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

impl Client {
    async fn send(&mut self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let answer = self
            .request(
                method,
                None,
                path,
                &[],
                body.map(|b| (serde_json::to_vec(&b).unwrap(), "application/json")),
            )
            .await;
        (
            answer.status,
            serde_json::from_str(&answer.body).unwrap_or(Value::Null),
        )
    }

    /// A GET of `path` on the application's subdomain.
    async fn app_get(&mut self, path: &str, headers: &[(&str, &str)]) -> Answer {
        self.request("GET", Some(APP_HOST), path, headers, None)
            .await
    }

    async fn request(
        &mut self,
        method: &str,
        host: Option<&str>,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<(Vec<u8>, &str)>,
    ) -> Answer {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(host) = host {
            builder = builder.header(header::HOST, host);
        }
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        if !self.cookies.is_empty() {
            let cookie = self
                .cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            builder = builder.header(header::COOKIE, cookie);
        }
        if method != "GET"
            && let Some(csrf) = self.cookies.get(CSRF_COOKIE)
        {
            builder = builder.header(CSRF_HEADER, csrf);
        }
        let request = match body {
            Some((bytes, content_type)) => builder
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(bytes))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        for raw in response.headers().get_all(header::SET_COOKIE) {
            if let Some((name, value)) = raw
                .to_str()
                .ok()
                .and_then(|t| t.split(';').next())
                .and_then(|p| p.split_once('='))
            {
                if value.is_empty() {
                    self.cookies.remove(name);
                } else {
                    self.cookies.insert(name.to_owned(), value.to_owned());
                }
            }
        }
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 32 * 1024 * 1024)
            .await
            .unwrap();
        Answer {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }
}

/// A server with the modules (and so the view runtime) running, BooksDB
/// restored, the admin signed in, and the BooksDB Saltcorn UI application
/// mounted with the backup's views, one Feed, one ListShowList and its page.
struct Server {
    client: Client,
    _catalog: Arc<Catalog>,
    _modules: Arc<ModuleServices>,
    _db: TestDb,
    _files: TempDir,
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn setup(tag: &str, bundle: PathBuf) -> sc_error::Result<Server> {
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
    sc_viewpattern::bootstrap(&catalog).await?;
    let agents = install_agents(&catalog).await?;
    let models = sc_server::install_models(&catalog, sc_model::DEFAULT_MAX_ROWS).await?;
    let dispatcher = install_triggers(&catalog, default_js_evaluator(), &agents, &models).await?;
    let root = std::env::temp_dir().join(format!(
        "sc-saltcorn-ui-render-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let modules = ModuleServices::install(
        &catalog,
        &dispatcher,
        &agents,
        &models,
        Some(root.join("modules")),
        None,
        1,
        sc_server::default_python_adapter(),
        Some(bundle.clone()),
    )
    .await?;

    let apps = Arc::new(
        AppMounts::new(catalog.clone())
            .with_agents(agents)
            .with_triggers(dispatcher)
            .with_saltcorn_ui_dir(Some(bundle)),
    );
    let config = ServerConfig {
        base_domain: Some(BASE_DOMAIN.to_owned()),
        ..ServerConfig::default()
    };
    let router = build_router_with_apps(
        &admin_endpoints(),
        admin_handlers(catalog.clone(), apps.clone()),
        Arc::new(SessionStore::default()),
        &config,
        apps.clone(),
    )?;
    let mut client = Client {
        router,
        cookies: HashMap::new(),
    };
    client.send("GET", "/api/auth/status", None).await;
    let (status, body) = client
        .send(
            "POST",
            "/api/first-user",
            Some(json!({ "email": ADMIN, "password": PASSWORD })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // --- BooksDB's tables and rows, restored as the Backup screen restores them.
    let files = root.join("files");
    std::fs::create_dir_all(&files)?;
    sc_catalog::save_file_store(
        &catalog,
        &FileStoreDef::local("BooksDB", files.to_string_lossy()),
    )
    .await?;
    sc_catalog::connect_file_store_def(
        &catalog,
        &sc_catalog::load_file_store_by_name(&catalog, "BooksDB")
            .await?
            .expect("the store just saved"),
    )?;
    let uploaded = client
        .request(
            "POST",
            None,
            "/backup/upload",
            &[],
            Some((V1_BACKUP.to_vec(), "application/zip")),
        )
        .await;
    assert_eq!(uploaded.status, StatusCode::OK, "{}", uploaded.body);
    let uploaded: Value = serde_json::from_str(&uploaded.body).unwrap();
    let (status, report) = client
        .send(
            "POST",
            "/api/backup/restore",
            Some(json!({ "id": uploaded["id"], "include": uploaded["include"] })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");

    // --- The application, and the backup's own views and page.
    let mut app = Application::new(
        "BooksDB",
        "booksdb",
        FrameworkRef::new(SALTCORN_UI_FRAMEWORK)
            .with("site_name", "BooksDB")
            .with(
                "menu_items",
                json!([
                    { "type": "Page", "label": "Overview", "pagename": "BooksOverview", "min_role": 1 },
                    { "type": "View", "label": "Books", "viewname": "List Books", "min_role": 1 },
                    { "type": "Admin Page", "label": "Tables", "admin_page": "Tables", "min_role": 1 },
                ]),
            ),
    )
    .with_file_store(FileStoreId("BooksDB".to_owned()));
    for table in ["Books", "Authors", "Publishers"] {
        app = app.with_table(TableId(table.to_owned()));
    }
    app.csp = sc_app::framework_default_csp(SALTCORN_UI_FRAMEWORK);
    let app = save_application(&catalog, &app).await?;

    let pack: Value = serde_json::from_str(&zip_entry(V1_BACKUP, "pack.json")).unwrap();
    for v in pack["views"].as_array().unwrap() {
        let mut view = View::new(
            app.id,
            v["name"].as_str().unwrap(),
            v["viewtemplate"].as_str().unwrap(),
            v["table"].as_str().unwrap(),
        )
        .min_role(u8::try_from(v["min_role"].as_u64().unwrap()).unwrap())
        .configuration(v["configuration"].as_object().cloned().unwrap_or_default());
        view.slug = Some(v["slug"].clone()).filter(|s| !s.is_null());
        view.attributes = v["attributes"].as_object().cloned().unwrap_or_default();
        sc_viewpattern::save_view(&catalog, &view).await?;
    }
    // The two patterns BooksDB has no view of.
    let feed = View::new(app.id, "Feed Books", "Feed", "Books")
        .min_role(1)
        .configuration(object(json!({
            "show_view": "Show Books", "order_field": "title", "descending": false,
            "cols_sm": 1, "cols_md": 1, "cols_lg": 1, "cols_xl": 1,
        })));
    sc_viewpattern::save_view(&catalog, &feed).await?;
    let lsl = View::new(app.id, "Books and details", "ListShowList", "Books")
        .min_role(1)
        .configuration(object(json!({
            "list_view": "List Books", "show_view": "Show Books", "list_width": 4, "subtables": {},
        })));
    sc_viewpattern::save_view(&catalog, &lsl).await?;
    for p in pack["pages"].as_array().unwrap() {
        let mut page = Page::new(app.id, p["name"].as_str().unwrap())
            .title(p["title"].as_str().unwrap_or_default())
            .layout(p["layout"].clone())
            .min_role(u8::try_from(p["min_role"].as_u64().unwrap()).unwrap());
        page.attributes.insert(
            "root_page_for_roles".to_owned(),
            p["root_page_for_roles"].clone(),
        );
        sc_viewpattern::save_page(&catalog, &page).await?;
    }

    // Saving the rows is not deploying them; the mount re-reads them.
    build_and_mount(&apps, app).await?;

    Ok(Server {
        client,
        _catalog: catalog,
        _modules: modules,
        _db: db,
        _files: TempDir(root),
    })
}

fn object(value: Value) -> serde_json::Map<String, Value> {
    value.as_object().cloned().unwrap()
}

fn zip_entry(archive: &[u8], path: &str) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive)).expect("a zip");
    let mut file = zip.by_name(path).expect("the entry");
    let mut text = String::new();
    file.read_to_string(&mut text).expect("text");
    text
}

/// 5.5–5.8: *List Books* over HTTP, and the routes around it.
#[tokio::test]
async fn list_books_renders_over_http_on_the_applications_subdomain() -> sc_error::Result<()> {
    let Some(bundle) = bundle_dir() else {
        eprintln!(
            "skipping: the Saltcorn UI bundle is not built (npm ci && npm run build in ui/saltcorn-ui)"
        );
        return Ok(());
    };
    let mut server = setup("live", bundle).await?;
    let client = &mut server.client;

    let list = client.app_get("/view/List%20Books", &[]).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    assert_eq!(
        list.headers[header::CONTENT_TYPE],
        "text/html; charset=utf-8"
    );
    // §10: the framework's own policy, and nothing but `script-src` relaxed.
    let csp = list.headers[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap();
    assert!(csp.contains("script-src 'self' 'unsafe-inline'"), "{csp}");
    assert!(csp.contains("default-src 'self'"), "{csp}");
    assert!(!csp.contains("unsafe-eval"), "{csp}");

    let html = &list.body;
    // The document (5.6): the head, the assets, the title.
    assert!(html.starts_with("<!doctype html>"), "{html}");
    assert!(html.contains("<title>List Books</title>"), "{html}");
    for asset in [
        "bootstrap.min.css",
        "jquery-3.6.0.min.js",
        "saltcorn-common.js",
        "saltcorn.js",
    ] {
        assert!(
            html.contains(&format!(
                "/static_assets/{}/{asset}",
                sc_viewpattern::ASSET_VERSION_TAG
            )),
            "{asset}: {html}"
        );
    }
    // The layout: the navbar with the brand and the menu, minus the v1 admin page.
    assert!(html.contains("id=\"mainNav\""), "{html}");
    assert!(html.contains(">BooksDB<"), "{html}");
    assert!(html.contains("href=\"/page/BooksOverview\""), "{html}");
    assert!(html.contains("href=\"/view/List%20Books\""), "{html}");
    assert!(!html.contains(">Tables<"), "{html}");
    // The list: v1's own table, a row per book, the authors' last names joined in.
    let books = row_count(client, "Books").await;
    assert!(books > 0);
    let rows = html.matches("<tr").count();
    assert_eq!(rows, 1 + books, "a header and {books} books: {html}");
    for author in authors(client).await {
        assert!(html.contains(&format!(">{author}<")), "{author}: {html}");
    }
    assert!(html.contains("/view/Show%20Books?id="), "{html}");

    // The same view to an ajax reload: the HTML alone, no document.
    let fragment = client
        .app_get(
            "/view/List%20Books",
            &[("X-Requested-With", "XMLHttpRequest")],
        )
        .await;
    assert_eq!(fragment.status, StatusCode::OK);
    assert!(
        !fragment.body.contains("<!doctype html>"),
        "{}",
        fragment.body
    );
    assert_eq!(fragment.body.matches("<tr").count(), 1 + books);

    // The page: the Filter, and the list it embeds.
    let page = client.app_get("/page/BooksOverview", &[]).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("data-sc-view-source"), "{}", page.body);
    assert_eq!(page.body.matches("<tr").count(), 1 + books, "{}", page.body);

    // `/`: no root page for any role, so the document names what there is.
    let index = client.app_get("/", &[]).await;
    assert_eq!(index.status, StatusCode::OK);
    assert!(
        index.body.contains("href=\"/page/BooksOverview\""),
        "{}",
        index.body
    );
    assert!(
        index.body.contains("href=\"/view/List%20Books\""),
        "{}",
        index.body
    );

    // A static asset, from the bundle.
    let css = client
        .app_get(
            &format!(
                "/static_assets/{}/saltcorn.css",
                sc_viewpattern::ASSET_VERSION_TAG
            ),
            &[],
        )
        .await;
    assert_eq!(css.status, StatusCode::OK);
    assert_eq!(css.headers[header::CONTENT_TYPE], "text/css; charset=utf-8");
    let escape = client
        .app_get("/static_assets/x/..%2F..%2Fview-runtime.js", &[])
        .await;
    assert_eq!(escape.status, StatusCode::NOT_FOUND);

    // Nothing by that name, and a viewer below the view's role.
    let missing = client.app_get("/view/No%20such%20view", &[]).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert!(missing.body.contains("No such view"), "{}", missing.body);
    let mut anonymous = Client {
        router: client.router.clone(),
        cookies: HashMap::new(),
    };
    let refused = anonymous.app_get("/view/List%20Books", &[]).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert!(!refused.body.contains("<tr"), "{}", refused.body);
    Ok(())
}

/// A table's rows, read back through the admin API.
async fn rows_of(client: &mut Client, table: &str) -> Vec<Value> {
    let (status, rows) = client
        .send("GET", &format!("/api/tables/{table}/rows"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    let rows = rows.get("rows").cloned().unwrap_or(rows);
    rows.as_array().expect("rows").clone()
}

async fn row_count(client: &mut Client, table: &str) -> usize {
    rows_of(client, table).await.len()
}

/// The authors' last names, read back through the admin API.
async fn authors(client: &mut Client) -> Vec<String> {
    let names: Vec<String> = rows_of(client, "Authors")
        .await
        .iter()
        .filter_map(|r| r["last_name"].as_str().map(str::to_owned))
        .collect();
    assert!(!names.is_empty());
    names
}

/// 5.8: each of the six patterns renders the committed HTML.
#[tokio::test]
async fn the_six_patterns_render_their_golden_html() -> sc_error::Result<()> {
    let Some(bundle) = bundle_dir() else {
        eprintln!(
            "skipping: the Saltcorn UI bundle is not built (npm ci && npm run build in ui/saltcorn-ui)"
        );
        return Ok(());
    };
    let mut server = setup("golden", bundle).await?;
    let golden =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/saltcorn-ui-golden");
    let update = std::env::var_os("SC_UPDATE_GOLDEN").is_some();
    let cases = [
        ("list", "/view/List%20Books"),
        ("show", "/view/Show%20Books?id=1"),
        ("edit", "/view/Edit%20Books?id=1"),
        ("feed", "/view/Feed%20Books"),
        ("filter", "/view/Filter%20books"),
        ("listshowlist", "/view/Books%20and%20details?id=1"),
    ];
    let mut wrong = Vec::new();
    for (name, path) in cases {
        let answer = server
            .client
            .app_get(path, &[("X-Requested-With", "XMLHttpRequest")])
            .await;
        assert_eq!(answer.status, StatusCode::OK, "{name}: {}", answer.body);
        let rendered = normalise(&answer.body);
        let file = golden.join(format!("{name}.html"));
        if update {
            std::fs::create_dir_all(&golden)?;
            std::fs::write(&file, &rendered)?;
            continue;
        }
        let expected = std::fs::read_to_string(&file).unwrap_or_default();
        if rendered != expected {
            wrong.push(format!(
                "{name} ({path}) differs from {}:\n{rendered}",
                file.display()
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{}\n\n(SC_UPDATE_GOLDEN=1 rewrites the expected files)",
        wrong.join("\n\n")
    );
    Ok(())
}

/// What legitimately differs between two renders of one view, replaced by a
/// placeholder so the rest is compared exactly:
///
/// - a UUID — the signed-in admin's id, new in every test database;
/// - v1's random form ids (`form3fa9c1`) — an action column's `rndid` is
///   *stored* in the view's configuration and stays;
/// - the text of a `<time>`, which v1 renders in the server's locale and
///   timezone and `saltcorn-common.js` re-renders in the browser's — its
///   `datetime` attribute is kept, and is the value.
fn normalise(html: &str) -> String {
    let uuid =
        regex_lite::Regex::new("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
            .unwrap();
    // A form's id, wherever it is used: its `id`, and the script that finds it.
    let form_id = regex_lite::Regex::new(r#"\bform[0-9a-f]{6}\b"#).unwrap();
    let time = regex_lite::Regex::new(r"(<time [^>]*>)[^<]*(</time>)").unwrap();
    let out = uuid.replace_all(html, "UUID");
    let out = form_id.replace_all(&out, "formRNDID");
    let out = time.replace_all(&out, "${1}TIME${2}");
    let mut out = out.trim_end().to_owned();
    out.push('\n');
    out
}
