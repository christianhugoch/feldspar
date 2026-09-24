//! Build brings an application's generated code up to date **before** it runs
//! the bundler — all of it, the way "Update client" did, including the case
//! where the project directory is empty and has to be scaffolded.
//!
//! That is what lets the admin sidebar offer one button instead of two: an
//! admin who emptied a project (or whose store was unreachable when the app was
//! created) presses Build and gets a project and a build, rather than a bundler
//! failing in a directory with nothing in it.
//!
//! The framework is a module-declared one, stubbed as in `sc-app`'s
//! `declared_framework.rs`, because a `react` build would need a real
//! `npm install`: its scaffold writes a `build.sh` that the build then runs, so
//! the build succeeding is itself the proof the scaffold came first. The
//! registry is process-wide, which is why this is its own test binary.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sc_api::admin_endpoints;
use sc_app::{
    ApiConfig, Application, BuildTemplate, DeclaredFile, FilePhase, FrameworkDecl, FrameworkHost,
    FrameworkRef, FrameworkSet, bootstrap, install_frameworks, save_application,
};
use sc_auth::SessionStore;
use sc_catalog::{Catalog, FileStoreId, TableId};
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_expr::Template;
use sc_files::LocalFileStore;
use sc_server::{
    AppMounts, CSRF_COOKIE, CSRF_HEADER, ServerConfig, admin_handlers, build_router_with_apps,
};
use sc_test_harness::TestDb;
use sc_types::{BasicType, FormField};
use serde_json::{Value, json};
use tower::ServiceExt;

const BASE_DOMAIN: &str = "example.com";

fn template(source: &str) -> Template {
    Template::parse(source).expect("the test's own template parses")
}

/// A framework whose project is a shell script: scaffolded by the module, built
/// by `sh build.sh`, with a generated runtime directory like React's.
fn shell_framework() -> FrameworkDecl {
    FrameworkDecl {
        name: "shell".to_owned(),
        module: "@feldspar/shell".to_owned(),
        label: "Shell".to_owned(),
        description: "A project built by a shell script.".to_owned(),
        config_spec: vec![
            FormField::new("store", BasicType::Text).required(),
            FormField::new("project", BasicType::Text).default_value(""),
        ],
        build: BuildTemplate {
            store: template("{{ store }}"),
            source: template("{{ project }}"),
            output: template("{{ project }}/dist"),
            command: "sh build.sh".to_owned(),
            install: None,
            runtime: Some(template("{{ project }}/src/feldspar")),
            client_file: "client.ts".to_owned(),
        },
        csp: None,
        builder_prompt: None,
        checks: Vec::new(),
        scaffolds: true,
    }
}

struct StubHost;

#[async_trait]
impl FrameworkHost for StubHost {
    fn frameworks(&self) -> Vec<FrameworkDecl> {
        vec![shell_framework()]
    }

    async fn framework_files(
        &self,
        _name: &str,
        phase: FilePhase,
        context: Value,
    ) -> sc_error::Result<Vec<DeclaredFile>> {
        let runtime = context["runtime"].as_str().unwrap_or_default().to_owned();
        let mut files = vec![DeclaredFile {
            path: format!("{runtime}/composables.ts"),
            contents: "export {};\n".to_owned(),
        }];
        if phase == FilePhase::Scaffold {
            files.push(DeclaredFile {
                path: "build.sh".to_owned(),
                contents: "mkdir -p dist && echo '<!doctype html>built' > dist/index.html\n"
                    .to_owned(),
            });
        }
        Ok(files)
    }
}

struct Client {
    router: Router,
    cookies: HashMap<String, String>,
}

impl Client {
    async fn send(&mut self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, BASE_DOMAIN);
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
        for value in response.headers().get_all(header::SET_COOKIE) {
            let raw = value.to_str().unwrap();
            let pair = raw.split(';').next().unwrap();
            if let Some((name, val)) = pair.split_once('=') {
                self.cookies.insert(name.to_owned(), val.to_owned());
            }
        }
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[tokio::test]
async fn build_scaffolds_an_empty_project_and_regenerates_its_code_before_building()
-> sc_error::Result<()> {
    install_frameworks(FrameworkSet::new(Arc::new(StubHost)))?;

    let db = TestDb::new().await?;
    let dir = TempDir(std::env::temp_dir().join(format!(
        "sc-server-build-updates-client-{}",
        std::process::id()
    )));
    std::fs::remove_dir_all(&dir.0).ok();
    std::fs::create_dir_all(&dir.0)?;
    db.client()
        .await?
        .batch_execute(
            "CREATE TABLE tasks (\
               id bigint generated by default as identity primary key, \
               title text not null)",
        )
        .await
        .map_err(|e| sc_error::Error::database(e.to_string()))?;

    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let catalog = Arc::new(Catalog::init(driver as Arc<dyn DatabaseDriver>).await?);
    sc_auth::bootstrap(&catalog).await?;
    bootstrap(&catalog).await?;
    sc_catalog::bootstrap_table_meta(&catalog).await?;
    sc_catalog::bootstrap_field_meta(&catalog).await?;
    catalog.connect_file_store(Arc::new(LocalFileStore::new("apps", &dir.0)?))?;

    let apps = Arc::new(AppMounts::new(catalog.clone()));
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
    client
        .send(
            "POST",
            "/api/first-user",
            Some(json!({ "email": "admin@example.com", "password": "hunter2pass" })),
        )
        .await;

    // Saved, but never scaffolded: the project directory does not exist.
    let app = Application::new(
        "Site",
        "site",
        FrameworkRef::new("shell")
            .with("store", "apps")
            .with("project", "site"),
    )
    .with_table(TableId("tasks".to_owned()))
    .with_file_store(FileStoreId("apps".to_owned()))
    .with_api(ApiConfig::new("rest", "/api"));
    save_application(&catalog, &app).await?;
    let project = dir.0.join("site");
    assert!(!project.join("build.sh").exists());

    let (status, body) = client
        .send("POST", &format!("/api/applications/{}/build", app.id), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["built"], json!(true), "{body}");
    // The scaffold is news, so the log leads with it.
    assert!(
        body["log"].as_str().unwrap_or_default().contains("scaffolded"),
        "the log should say the project was scaffolded: {body}"
    );
    // The scaffold's own build script ran, so it was written first…
    assert!(project.join("dist/index.html").is_file());
    // …along with the generated client and runtime.
    assert!(project.join("src/feldspar/client.ts").is_file());
    assert!(project.join("src/feldspar/composables.ts").is_file());

    // A second build finds a project there: the generated code is rewritten,
    // nothing is scaffolded over the admin's work, and the log does not say
    // otherwise.
    std::fs::write(project.join("mine.txt"), "mine")?;
    std::fs::remove_file(project.join("src/feldspar/client.ts"))?;
    let (status, body) = client
        .send("POST", &format!("/api/applications/{}/build", app.id), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body["log"].as_str().unwrap_or_default().contains("scaffolded"),
        "{body}"
    );
    assert!(project.join("src/feldspar/client.ts").is_file());
    assert_eq!(std::fs::read_to_string(project.join("mine.txt"))?, "mine");
    Ok(())
}
