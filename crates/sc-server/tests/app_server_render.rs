//! **Server-rendered applications** (`sc_app::server_render`, the Astro
//! module's framework): a build whose output holds a code body the server runs
//! per request — through the real router, with a real dispatcher and database
//! (SQLite, a file per test).
//!
//! The body here is written by hand, in the shape `plugins/astro`'s build
//! writes, so the test needs no npm; `plugins/astro/test/` covers the compiler
//! half.
//!
//! - a file the build wrote is served as a file, with an ETag and a 304;
//! - anything else runs the body, which reads the table with `db` — the code
//!   action's surface — and sees the request it is answering;
//! - `_server/` is never served, and an unknown path is the body's 404, not an
//!   SPA fallback;
//! - a form posts back with its CSRF field and gets the body's redirect;
//! - prerendering writes the listed pages into the output and the bundle.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sc_action::TriggerDispatcher;
use sc_app::{Application, AssetBundle, BuildReport, BuildSpec, FrameworkRef};
use sc_auth::SessionStore;
use sc_catalog::{Catalog, DataField};
use sc_db::DatabaseDriver;
use sc_db_sqlite::SqliteDriver;
use sc_server::{
    AppMounts, CSRF_COOKIE, MountedApp, ServerConfig, admin_handlers, build_router_with_apps,
    default_js_evaluator, install_triggers,
};
use sc_types::{Attrs, BasicType, TypeRef};
use tower::ServiceExt;

const BASE_DOMAIN: &str = "example.com";
const HOST: &str = "site.example.com";

/// The code body, as `plugins/astro`'s build writes it: one function body that
/// routes on `request` and answers `{ status, headers, body }`.
const ENTRY: &str = r#"
if (request.mode === "prerender-list") return { paths: ["/about"] };
const html = (body, status = 200) =>
  ({ status, headers: [["content-type", "text/html; charset=utf-8"]], body });
if (request.path === "/posts") {
  const posts = await db.posts.orderBy("id").rows();
  return html("<ul>" + posts.map((p) => "<li>" + p.title + "</li>").join("") + "</ul>");
}
if (request.path === "/about") {
  return html("<h1>about " + (await db.posts.count()) + " posts, " + request.mode + "</h1>");
}
if (request.path === "/contact" && request.method === "POST") {
  const email = request.form.find(([k]) => k === "email")[1];
  return { status: 303, headers: [["location", "/contact?from=" + email]], body: "" };
}
if (request.path === "/contact") {
  return html('<form method="post"><input name="_csrf" value="' + request.csrfToken + '"></form>');
}
return html("<h1>not found</h1>", 404);
"#;

/// A scratch directory removed when the guard drops.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "sc-server-render-{}-{tag}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn spec() -> BuildSpec {
    BuildSpec {
        command: "npm".to_owned(),
        args: vec!["run".to_owned(), "build".to_owned()],
        source_dir: "site".to_owned(),
        output_dir: "site/dist".to_owned(),
        install: None,
        server_entry: Some("_server/entry.js".to_owned()),
    }
}

fn bundle() -> AssetBundle {
    AssetBundle::new()
        .with("index.html", "<!doctype html><h1>home</h1>")
        .with("_astro/site.abc123.css", "body{margin:0}")
        .with("_server/entry.js", ENTRY)
}

struct Server {
    catalog: Arc<Catalog>,
    router: Router,
    dispatcher: Arc<TriggerDispatcher>,
    app: Application,
}

async fn setup(tmp: &TempDir) -> sc_error::Result<Server> {
    let driver = Arc::new(SqliteDriver::open(tmp.path().join("site.db"))?);
    let catalog = Arc::new(Catalog::init(driver as Arc<dyn DatabaseDriver>).await?);
    sc_auth::bootstrap(&catalog).await?;
    sc_app::bootstrap(&catalog).await?;
    sc_catalog::bootstrap_table_meta(&catalog).await?;
    sc_catalog::bootstrap_field_meta(&catalog).await?;
    sc_catalog::bootstrap_code_libraries(&catalog).await?;
    sc_config::bootstrap(&catalog).await?;
    catalog
        .create_table(
            "posts",
            &[
                DataField::plain("id", TypeRef::Basic(BasicType::Int))
                    .required()
                    .primary_key(),
                DataField::plain("title", TypeRef::Basic(BasicType::Text)),
            ],
        )
        .await?;

    let evaluator = default_js_evaluator();
    let agents = sc_server::install_agents(&catalog).await?;
    let models = sc_server::install_models(&catalog, sc_model::DEFAULT_MAX_ROWS).await?;
    let dispatcher = install_triggers(&catalog, evaluator.clone(), &agents, &models).await?;
    // The rows, written the way a page would write them.
    dispatcher
        .run_code(
            &catalog,
            "run_js_code",
            "seed",
            r#"await db.posts.insert({ id: 1, title: "Hello" });
               await db.posts.insert({ id: 2, title: "World" });"#,
            Attrs::new(),
            None,
            None,
            None,
        )
        .await?;

    // Not saved: saving checks the framework against the installed modules, and
    // this test mounts the framework directly rather than installing the module.
    let app = Application::new("Site", "site", FrameworkRef::new("astro"));
    let apps = Arc::new(
        AppMounts::new(catalog.clone())
            .with_evaluator(evaluator)
            .with_triggers(dispatcher.clone()),
    );
    let framework = sc_app::mount_code_framework(&app, bundle(), spec(), Some(dispatcher.clone()))?;
    apps.mount(MountedApp::new_with(
        app.clone(),
        framework,
        &catalog,
        apps.evaluator(),
        apps.triggers(),
    )?)?;
    let config = ServerConfig {
        base_domain: Some(BASE_DOMAIN.to_owned()),
        ..ServerConfig::default()
    };
    let router = build_router_with_apps(
        &sc_api::admin_endpoints(),
        admin_handlers(catalog.clone(), apps.clone()),
        Arc::new(SessionStore::default()),
        &config,
        apps.clone(),
    )?;
    Ok(Server {
        catalog,
        router,
        dispatcher,
        app,
    })
}

struct Answer {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

async fn send(router: &Router, request: Request<Body>) -> Answer {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    Answer {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn get(router: &Router, path: &str) -> Answer {
    let request = Request::builder()
        .uri(path)
        .header(header::HOST, HOST)
        .body(Body::empty())
        .unwrap();
    send(router, request).await
}

#[tokio::test]
async fn files_are_files_and_everything_else_runs_the_page_body() -> sc_error::Result<()> {
    let tmp = TempDir::new("serve");
    let server = setup(&tmp).await?;

    // A file the build wrote: no body is run for it.
    let home = get(&server.router, "/").await;
    assert_eq!(home.status, StatusCode::OK);
    assert_eq!(home.body, "<!doctype html><h1>home</h1>");
    let tag = home.headers.get(header::ETAG).expect("an etag").clone();
    let again = send(
        &server.router,
        Request::builder()
            .uri("/")
            .header(header::HOST, HOST)
            .header(header::IF_NONE_MATCH, tag)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);
    let css = get(&server.router, "/_astro/site.abc123.css").await;
    assert!(
        css.headers[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("immutable")
    );

    // A rendered page reads the table with the code action's `db`.
    let posts = get(&server.router, "/posts").await;
    assert_eq!(posts.status, StatusCode::OK, "{}", posts.body);
    assert_eq!(posts.body, "<ul><li>Hello</li><li>World</li></ul>");
    assert_eq!(
        posts.headers[header::CONTENT_TYPE],
        "text/html; charset=utf-8"
    );
    assert_eq!(posts.headers[header::CACHE_CONTROL], "private, no-cache");

    // The site's source is never a file; an unknown path is the body's 404.
    let source = get(&server.router, "/_server/entry.js").await;
    assert_eq!(source.status, StatusCode::NOT_FOUND);
    assert!(!source.body.contains("db.posts"), "{}", source.body);
    let missing = get(&server.router, "/no/such/page").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.body, "<h1>not found</h1>");
    Ok(())
}

#[tokio::test]
async fn a_form_posts_back_with_its_csrf_field_and_is_redirected() -> sc_error::Result<()> {
    let tmp = TempDir::new("form");
    let server = setup(&tmp).await?;

    // The page carries this browser's token, which its cookie also holds.
    let page = get(&server.router, "/contact").await;
    assert_eq!(page.status, StatusCode::OK);
    let cookie = page
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with(&format!("{CSRF_COOKIE}=")))
        .expect("a CSRF cookie on first contact")
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let token = cookie.split_once('=').unwrap().1.to_owned();
    assert!(
        page.body.contains(&format!("value=\"{token}\"")),
        "{}",
        page.body
    );

    let post = |body: String| {
        Request::builder()
            .method("POST")
            .uri("/contact")
            .header(header::HOST, HOST)
            .header(header::COOKIE, cookie.clone())
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap()
    };
    let sent = send(&server.router, post(format!("_csrf={token}&email=a%40b.c"))).await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(sent.headers[header::LOCATION], "/contact?from=a@b.c");

    // Without the field, the CSRF check refuses it before any page runs.
    let forged = send(&server.router, post("email=a%40b.c".to_owned())).await;
    assert_eq!(forged.status, StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn prerendering_writes_the_listed_pages_through_the_same_body() -> sc_error::Result<()> {
    let tmp = TempDir::new("prerender");
    let server = setup(&tmp).await?;
    let output_dir = tmp.path().join("dist");
    std::fs::create_dir_all(&output_dir).unwrap();
    let mut report = BuildReport {
        bundle: bundle(),
        output_dir: output_dir.clone(),
        git_repo: false,
        stdout: String::new(),
        stderr: String::new(),
        client_path: None,
        installed: false,
        install_log: None,
        reused: false,
    };
    let written = sc_app::prerender_site(
        &server.catalog,
        &server.app,
        &spec(),
        &mut report,
        Some(&server.dispatcher),
    )
    .await?;
    assert_eq!(written, 1);
    let page = "<h1>about 2 posts, prerender</h1>";
    assert_eq!(
        std::fs::read_to_string(output_dir.join("about/index.html")).unwrap(),
        page
    );
    assert_eq!(
        &report.bundle.get("about/index.html").unwrap().bytes[..],
        page.as_bytes()
    );
    assert!(report.stdout.contains("prerendered 1 page(s)"));

    // Mounted from that bundle, the page is a file: it says `prerender`, not
    // `render`, because no body ran for this request.
    let framework = sc_app::mount_code_framework(
        &server.app,
        report.bundle.clone(),
        spec(),
        Some(server.dispatcher.clone()),
    )?;
    let served = framework
        .handle(sc_app::AppRequest::get("/about"), &server.catalog)
        .await?;
    assert_eq!(&served.body[..], page.as_bytes());

    // A build with no server to run the pages on skips the step and says so.
    let mut bare = report.clone();
    bare.stdout.clear();
    let none =
        sc_app::prerender_site(&server.catalog, &server.app, &spec(), &mut bare, None).await?;
    assert_eq!(none, 0);
    assert!(bare.stdout.contains("prerendering skipped"));
    Ok(())
}

/// The whole path with the real module: `plugins/astro`'s own generators write
/// a project, npm builds it with the real compiler, and the code body it wrote
/// runs in the real isolate against the real table — the check that the
/// compiled output asks nothing of the isolate that it does not have (no
/// `URL`, the sandbox's own `Headers`). Opt-in, as the Vue build is: it needs
/// Node, npm and the registry.
#[tokio::test]
async fn the_bundled_astro_module_builds_a_site_this_server_renders() -> sc_error::Result<()> {
    if std::env::var("SC_TEST_NPM").as_deref() != Ok("1") {
        eprintln!("skipping: set SC_TEST_NPM=1 to run the real npm build");
        return Ok(());
    }
    let tmp = TempDir::new("astro-e2e");
    let server = setup(&tmp).await?;
    let project = tmp.path().join("site");
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/astro/index.js");
    // What the module worker does on Scaffold, in Node: the same functions.
    let script = r#"
const fs = require("fs"), path = require("path");
const [plugin, root] = process.argv.slice(1);
const ctx = {
  project: "site", name: "site", runtime: "src/feldspar", client: "src/feldspar/client.ts",
  auth: false, app: { name: "Acme", subdomain: "site", description: "Rockets, fast.", url: "" },
  settings: { site: "" },
  tables: [{ name: "posts", pascal: "Posts", title: "Posts", client: "posts", pk: "id",
             fields: [{ name: "id", primary_key: true }, { name: "title", primary_key: false }] }],
};
for (const f of require(plugin).frameworks.astro.scaffold(ctx)) {
  fs.mkdirSync(path.dirname(path.join(root, f.path)), { recursive: true });
  fs.writeFileSync(path.join(root, f.path), f.contents);
}"#;
    let run = |program: &str, args: &[&str]| {
        let out = std::process::Command::new(program)
            .args(args)
            .current_dir(&project)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{program} {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    std::fs::create_dir_all(&project).unwrap();
    run(
        "node",
        &[
            "-e",
            script,
            plugin.to_str().unwrap(),
            project.to_str().unwrap(),
        ],
    );
    run("npm", &["install", "--no-audit", "--no-fund"]);
    run("npm", &["run", "build"]);

    let mut report = BuildReport {
        bundle: AssetBundle::from_dir(project.join("dist"))?,
        output_dir: project.join("dist"),
        git_repo: false,
        stdout: String::new(),
        stderr: String::new(),
        client_path: None,
        installed: false,
        install_log: None,
        reused: false,
    };
    let written = sc_app::prerender_site(
        &server.catalog,
        &server.app,
        &spec(),
        &mut report,
        Some(&server.dispatcher),
    )
    .await?;
    assert_eq!(written, 2, "the landing page and the 404 page");
    let framework = sc_app::mount_code_framework(
        &server.app,
        report.bundle.clone(),
        spec(),
        Some(server.dispatcher.clone()),
    )?;
    let page = |path: &str| {
        let framework = framework.clone();
        let catalog = server.catalog.clone();
        let mut req = sc_app::AppRequest::get(path);
        req.base_url = "http://site.example.com".to_owned();
        req.csrf_token = "tok".to_owned();
        async move { framework.handle(req, &catalog).await }
    };

    let home = page("/").await?;
    let home = String::from_utf8_lossy(&home.body).into_owned();
    assert!(home.contains("Rockets, fast."), "{home}");
    assert!(
        home.contains("<link rel=\"stylesheet\" href=\"/_astro/site."),
        "{home}"
    );

    let posts = page("/posts").await?;
    assert_eq!(
        posts.status,
        200,
        "{}",
        String::from_utf8_lossy(&posts.body)
    );
    let posts = String::from_utf8_lossy(&posts.body).into_owned();
    assert!(
        // Scoped styles put the component's class on each element.
        posts.contains(">Hello</span>") && posts.contains(">World</span>"),
        "{posts}"
    );
    assert!(
        posts.contains("<link rel=\"canonical\" href=\"http://site.example.com/posts\""),
        "{posts}"
    );

    let contact = page("/contact").await?;
    assert!(
        String::from_utf8_lossy(&contact.body)
            .contains("<input type=\"hidden\" name=\"_csrf\" value=\"tok\">")
    );
    let missing = page("/nope").await?;
    assert_eq!(missing.status, 404);
    assert!(String::from_utf8_lossy(&missing.body).contains("Page not found"));
    let sitemap = page("/sitemap.xml").await?;
    assert!(
        String::from_utf8_lossy(&sitemap.body).contains("<loc>http://site.example.com/posts</loc>")
    );
    Ok(())
}
