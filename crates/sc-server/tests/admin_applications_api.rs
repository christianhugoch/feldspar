//! Phase 10 "admin API" integration test: the application-management endpoints
//! driven end-to-end over HTTP against a **real Postgres**, through a router whose
//! live [`AppMounts`] the build endpoint mounts into (design §13.1/§13.2/§16).
//!
//! This is the configuration path GOALS asks for — "applications are created in
//! the admin UI" — exercised as the SPA would drive it: list frameworks, create
//! an application, build (+mount) it and watch it serve on its subdomain with no
//! restart, edit + rebuild, then delete and watch the subdomain stop resolving. A
//! failing build comes back as an Application error (a `422`, not a `500`)
//! carrying the bundler's diagnostics, with the previous bundle still serving.
//! Non-admins are rejected throughout.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sc_auth::{ROLE_ADMIN, ROLE_PUBLIC, SessionStore, create_user};
use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_files::LocalFileStore;
use sc_server::{
    AppMounts, CSRF_COOKIE, CSRF_HEADER, ServerConfig, admin_handlers, build_router_with_apps,
};
use sc_test_harness::TestDb;
use serde_json::{Value, json};
use tower::ServiceExt;

const BASE_DOMAIN: &str = "example.com";
const APP_HOST: &str = "blog.example.com";

/// A scratch directory removed when the guard drops.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "sc-server-adminapp-{}-{tag}-{:?}",
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

/// A cookie-carrying client addressing one host, echoing CSRF on mutations.
struct Client {
    router: Router,
    host: String,
    cookies: HashMap<String, String>,
}

impl Client {
    fn new(router: Router, host: &str) -> Client {
        Client {
            router,
            host: host.to_owned(),
            cookies: HashMap::new(),
        }
    }

    async fn raw(
        &mut self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Vec<u8>) {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, &self.host);
        if !self.cookies.is_empty() {
            let jar = self
                .cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            builder = builder.header(header::COOKIE, jar);
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
        (status, bytes.to_vec())
    }

    async fn send(&mut self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let (status, bytes) = self.raw(method, path, body).await;
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, value)
    }

    async fn login(&mut self, email: &str, password: &str) {
        // Load a page first so the CSRF cookie is minted before any mutation.
        self.raw("GET", "/api/auth/status", None).await;
        let (status, _) = self
            .send(
                "POST",
                "/api/login",
                Some(json!({ "email": email, "password": password })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "login should succeed");
    }
}

/// Install a bundler that stamps `marker` into the served `index.html`; when
/// `succeed` is false it exits non-zero with a bundler-style diagnostic instead.
fn write_bundler(root: &Path, marker: &str, succeed: bool) {
    let web = root.join("web");
    std::fs::create_dir_all(web.join("src")).unwrap();
    let script = if succeed {
        format!(
            "#!/bin/sh\n\
             set -e\n\
             mkdir -p dist\n\
             printf '<!doctype html><div id=root>{marker}</div>' > dist/index.html\n"
        )
    } else {
        "#!/bin/sh\necho 'TS2304: Cannot find name Widget' >&2\nexit 2\n".to_owned()
    };
    let path = web.join("build.sh");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// The create/update body for the blog app, built from the code framework config.
fn blog_body() -> Value {
    json!({
        "name": "Blog",
        "description": "The company blog",
        "subdomain": "blog",
        "framework": {
            "name": "code",
            "config": {
                "store": "apps",
                "source": "web",
                "output": "web/dist",
                "command": "sh build.sh"
            }
        },
        "extra_frameworks": [],
        "tables": ["posts"],
        "file_stores": ["apps"],
        "apis": [{ "provider": "rest", "mount": "/api" }],
        "static_dirs": [],
        "attributes": {}
    })
}

/// Bring up a catalog (posts table + `apps` store) and the router over a shared,
/// empty live [`AppMounts`] the admin build endpoint mounts into.
async fn setup(tmp: &TempDir) -> sc_error::Result<(Router, Arc<Catalog>, TestDb)> {
    let db = TestDb::new().await?;
    db.client()
        .await?
        .batch_execute(
            "DO $$ DECLARE r record; BEGIN \
               FOR r IN SELECT table_schema FROM information_schema.tables \
               WHERE table_name = 'users' AND table_type = 'BASE TABLE' LOOP \
                 EXECUTE format('DROP TABLE IF EXISTS %I.users CASCADE', r.table_schema); \
               END LOOP; END $$; \
             CREATE TABLE posts (\
               id bigint generated by default as identity primary key, \
               title text not null)",
        )
        .await
        .map_err(|e| sc_error::Error::database(e.to_string()))?;

    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let catalog = Arc::new(Catalog::init(driver as Arc<dyn DatabaseDriver>).await?);
    sc_auth::bootstrap(&catalog).await?;
    sc_app::bootstrap(&catalog).await?;
    catalog.connect_file_store(Arc::new(LocalFileStore::new("apps", tmp.path())?))?;

    let apps = Arc::new(AppMounts::new(catalog.clone()));
    let config = ServerConfig {
        base_domain: Some(BASE_DOMAIN.to_owned()),
        ..ServerConfig::default()
    };
    let router = build_router_with_apps(
        &sc_api::admin_endpoints(),
        admin_handlers(catalog.clone(), apps.clone()),
        Arc::new(SessionStore::default()),
        &config,
        apps,
    )?;
    Ok((router, catalog, db))
}

#[tokio::test]
async fn applications_are_managed_over_http_and_serve_without_a_restart() -> sc_error::Result<()> {
    let tmp = TempDir::new("story");
    let (router, catalog, _db) = setup(&tmp).await?;
    create_user(&catalog, "admin@example.com", "correct-horse", ROLE_ADMIN).await?;

    let mut admin = Client::new(router.clone(), BASE_DOMAIN);
    admin.login("admin@example.com", "correct-horse").await;

    // --- list frameworks: the UI can render a form it knows nothing about -----
    let (status, frameworks) = admin.send("GET", "/api/frameworks", None).await;
    assert_eq!(status, StatusCode::OK);
    let code = frameworks
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == json!("code"))
        .expect("the code framework is listed");
    let spec = code["config_spec"].as_array().unwrap();
    let store = spec
        .iter()
        .find(|f| f["name"] == json!("store"))
        .expect("the `store` setting is described");
    assert_eq!(store["type"], json!("text"));
    assert_eq!(store["required"], json!(true));
    assert_eq!(store["label"], json!("File store"));

    // --- create the application (a saved-but-unbuilt row) ---------------------
    let (status, created) = admin
        .send("POST", "/api/applications", Some(blog_body()))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["subdomain"], json!("blog"));
    assert_eq!(created["tables"], json!(["posts"]));
    let id = created["id"].as_str().expect("a minted id").to_owned();

    // It is listed, and CSP defaults to strict on the round-trip.
    let (_, list) = admin.send("GET", "/api/applications", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["csp"]["default-src"], json!(["'self'"]));

    // Saved but unbuilt: the subdomain does not serve the app yet.
    let mut app = Client::new(router.clone(), APP_HOST);
    let (_, body) = app.raw("GET", "/", None).await;
    assert_eq!(body, sc_server::BOOTSTRAP_HTML.as_bytes());

    // --- build (+ mount): it serves on its subdomain, no restart --------------
    write_bundler(tmp.path(), "v1", true);
    let (status, report) = admin
        .send("POST", &format!("/api/applications/{id}/build"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["built"], json!(true));

    let (status, body) = app.raw("GET", "/", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"<!doctype html><div id=root>v1</div>");

    // --- edit + rebuild: the new bundle is what serves ------------------------
    let mut edited = blog_body();
    edited["description"] = json!("Updated blog");
    let (status, _) = admin
        .send("PUT", &format!("/api/applications/{id}"), Some(edited))
        .await;
    assert_eq!(status, StatusCode::OK);

    write_bundler(tmp.path(), "v2", true);
    let (status, _) = admin
        .send("POST", &format!("/api/applications/{id}/build"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = app.raw("GET", "/", None).await;
    assert_eq!(body, b"<!doctype html><div id=root>v2</div>");

    // --- a failing build is an Application error (422), old bundle stays -------
    write_bundler(tmp.path(), "v3", false);
    let (status, err) = admin
        .send("POST", &format!("/api/applications/{id}/build"), None)
        .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a build failure is a client-fixable Application error, not a 500"
    );
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("TS2304: Cannot find name Widget"),
        "the bundler's diagnostics reach the admin: {err}"
    );
    // The previously built v2 is still serving.
    let (_, body) = app.raw("GET", "/", None).await;
    assert_eq!(body, b"<!doctype html><div id=root>v2</div>");

    // --- delete: the row goes and the subdomain stops resolving ---------------
    let (status, deleted) = admin
        .send("DELETE", &format!("/api/applications/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["deleted"], json!(true));

    let (_, list) = admin.send("GET", "/api/applications", None).await;
    assert_eq!(list.as_array().unwrap().len(), 0);
    let (_, body) = app.raw("GET", "/", None).await;
    assert_eq!(
        body,
        sc_server::BOOTSTRAP_HTML.as_bytes(),
        "a deleted app's subdomain falls back to the admin"
    );

    // Deleting again is a 404.
    let (status, _) = admin
        .send("DELETE", &format!("/api/applications/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn non_admins_are_rejected_from_every_application_endpoint() -> sc_error::Result<()> {
    let tmp = TempDir::new("authz");
    let (router, catalog, _db) = setup(&tmp).await?;
    // A real, non-admin user.
    create_user(&catalog, "reader@example.com", "correct-horse", ROLE_PUBLIC).await?;

    // Anonymous: every endpoint requires authentication.
    let mut anon = Client::new(router.clone(), BASE_DOMAIN);
    for (method, path) in [
        ("GET", "/api/applications"),
        ("POST", "/api/applications"),
        (
            "PUT",
            "/api/applications/00000000-0000-0000-0000-000000000000",
        ),
        (
            "DELETE",
            "/api/applications/00000000-0000-0000-0000-000000000000",
        ),
        (
            "POST",
            "/api/applications/00000000-0000-0000-0000-000000000000/build",
        ),
        ("GET", "/api/frameworks"),
    ] {
        // Prime CSRF for mutations.
        anon.raw("GET", "/api/auth/status", None).await;
        let (status, _) = anon.send(method, path, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} must require auth"
        );
    }

    // Logged in but not an admin: the role gate forbids the admin API. (A public
    // user cannot even log into the admin UI, so create it, then confirm the
    // endpoints stay closed to the anonymous session it never got.)
    let mut reader = Client::new(router, BASE_DOMAIN);
    reader.raw("GET", "/api/auth/status", None).await;
    let (status, _) = reader
        .send(
            "POST",
            "/api/login",
            Some(json!({ "email": "reader@example.com", "password": "correct-horse" })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a non-admin cannot log into the admin UI"
    );
    let (status, _) = reader.send("GET", "/api/applications", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    Ok(())
}
