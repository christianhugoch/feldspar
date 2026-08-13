//! Backup and restore, driven through the assembled router as an admin's browser
//! drives it (design §16).
//!
//! The claim worth an integration test is the **round trip between two
//! installations**, because that is what a backup is for and it is the one thing
//! no unit test can reach: a table with rows, a file store with a file in it, a
//! role, a trigger and the SSL settings are set up on one server, backed up as a
//! zip through the HTTP route an admin's browser posts to, and restored into a
//! *second*, empty server through the two requests the Backup screen makes. What
//! arrives there is then read back through the ordinary admin API — not out of the
//! zip — because "the rows are in the file" is not the claim; "the second server
//! has them" is.
//!
//! Around that, four things the screen depends on and that are easy to get wrong:
//!
//! - the **choice is remembered** as what was left out, so a table added later is
//!   in the next backup, and the dialog reopens on the admin's tuned selection;
//! - **rows cannot be backed up without their table's metadata**, whatever a
//!   client sends;
//! - a zip that is **not a backup** is refused by name rather than restored as
//!   nothing;
//! - and every one of these is **admin-only**, like every other configuration
//!   endpoint.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sc_api::admin_endpoints;
use sc_auth::SessionStore;
use sc_catalog::{Catalog, DataField};
use sc_files::FileStoreDef;
use sc_config::{MODE_CUSTOM, SSL_MODE, SSL_PRIVATE_KEY, stored_config};
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_server::{
    AppMounts, CSRF_COOKIE, CSRF_HEADER, ServerConfig, admin_handlers, build_router_with_apps,
    default_js_evaluator, install_agents, install_triggers,
};
use sc_test_harness::TestDb;
use sc_types::{BasicType, TypeRef};
use serde_json::{Value, json};
use tower::ServiceExt;

const ADMIN: &str = "admin@example.com";
const PASSWORD: &str = "hunter2pass";

/// A cookie-jar-carrying client over the router (CSRF + session), as the other
/// admin-API tests use — plus the two calls whose bodies are not JSON.
struct Client {
    router: Router,
    cookies: HashMap<String, String>,
}

impl Client {
    async fn send(&mut self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let (status, bytes, _) = self
            .raw(
                method,
                path,
                body.map(|b| (serde_json::to_vec(&b).unwrap(), "application/json")),
            )
            .await;
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, value)
    }

    /// The bytes of a response, and its headers — what a download has to be read
    /// through.
    async fn raw(
        &mut self,
        method: &str,
        path: &str,
        body: Option<(Vec<u8>, &str)>,
    ) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
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
            Some((bytes, content_type)) => builder
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(bytes))
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
        let headers = response.headers().clone();
        // Generous: a backup of a handful of rows and one small file, plus the
        // zip's own overhead.
        let bytes = axum::body::to_bytes(response.into_body(), 32 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec();
        (status, bytes, headers)
    }
}

/// One server: a real database with every platform table bootstrapped, agents and
/// triggers installed, and an admin logged in.
struct Server {
    client: Client,
    catalog: Arc<Catalog>,
    /// Where this server's file store keeps its files.
    files: std::path::PathBuf,
    _db: TestDb,
}

async fn setup() -> sc_error::Result<Server> {
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
    sc_config::bootstrap(&catalog).await?;
    sc_catalog::bootstrap_table_meta(&catalog).await?;
    sc_catalog::bootstrap_field_meta(&catalog).await?;
    sc_catalog::bootstrap_file_stores(&catalog).await?;
    sc_llm::bootstrap_llm_providers(&catalog).await?;
    let agents = install_agents(&catalog).await?;
    let dispatcher = install_triggers(&catalog, default_js_evaluator(), &agents).await?;

    let apps = Arc::new(
        AppMounts::new(catalog.clone())
            .with_agents(agents)
            .with_triggers(dispatcher),
    );
    let router = build_router_with_apps(
        &admin_endpoints(),
        admin_handlers(catalog.clone(), apps.clone()),
        Arc::new(SessionStore::default()),
        &ServerConfig::default(),
        apps,
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

    Ok(Server {
        client,
        catalog,
        files: temp_dir(),
        _db: db,
    })
}

/// A table with rows, a file store with a file in it, an extra role, a trigger on
/// the table, and a stored SSL setting — one of everything the backup carries.
async fn furnish(server: &mut Server) -> sc_error::Result<()> {
    let (status, body) = server
        .client
        .send("POST", "/api/tables", Some(json!({ "name": "books" })))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = server
        .client
        .send(
            "POST",
            "/api/tables/books/fields",
            Some(json!({ "name": "title", "type": "text", "label": "The title" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // A configured table, so the overlay has something to restore.
    let (status, body) = server
        .client
        .send(
            "PUT",
            "/api/tables/books",
            Some(json!({
                "label": "Books",
                "description": "Everything on the shelf",
                "min_role_read": 100,
                "min_role_write": 1,
                "ownership_formula": "",
                "rls_enabled": false,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    for title in ["Dune", "Solaris"] {
        let (status, body) = server
            .client
            .send("POST", "/api/tables/books/rows", Some(json!({ "title": title })))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    // A file store on disk, with one file written through the API that puts it
    // there.
    let (status, body) = server
        .client
        .send(
            "POST",
            "/api/file-stores",
            Some(json!({
                "name": "assets",
                "description": "Uploaded files",
                "backend": "local",
                "config": { "path": server.files.to_string_lossy() },
                "min_role": null,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = server
        .client
        .send(
            "POST",
            "/api/file-stores/assets/write",
            Some(json!({ "path": "notes/readme.txt", "text": "hello from the backup" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // An application whose source tree lives in that store, with a bundler that is
    // a shell script — enough for a real build, without npm.
    write_project(&server.files);
    let (status, body) = server
        .client
        .send(
            "POST",
            "/api/applications",
            Some(json!({
                "name": "Blog",
                "description": "",
                "subdomain": "blog",
                "framework": { "name": "code", "config": {
                    "store": "assets",
                    "source": "web",
                    "output": "web/dist",
                    "command": "sh build.sh",
                    "client": "web/src/client.ts",
                }},
                "extra_frameworks": [],
                "tables": ["books"],
                "file_stores": ["assets"],
                "triggers": [],
                "apis": [{ "provider": "rest", "mount": "/api", "config": {} }],
                "static_dirs": [],
                "attributes": {},
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = server
        .client
        .send(
            "POST",
            "/api/roles",
            Some(json!({ "role": 40, "name": "Librarian", "description": "Keeps the shelf" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = server
        .client
        .send(
            "POST",
            "/api/triggers",
            Some(json!({
                "name": "note_new_book",
                "description": "",
                "when": "insert",
                "channel": "books",
                "action": "run_js_code",
                "configuration": { "code": "row.title" },
                "min_role": null,
                "enabled": true,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (certificate, private_key) = self_signed();
    let (status, body) = server
        .client
        .send(
            "POST",
            "/api/settings",
            Some(json!({ "values": {
                SSL_MODE: MODE_CUSTOM,
                "ssl_certificate": certificate,
                SSL_PRIVATE_KEY: private_key,
            }})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(())
}

/// A fresh, unique temp directory (created on disk) for one server's file store.
fn temp_dir() -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!(
        "sc-backup-test-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    base
}

/// An application's source tree inside a file store's directory: a bundler that is
/// a shell script, and the one file it produces.
///
/// A real build without a real bundler — what is under test is that a restore
/// *runs* the build over the tree it just restored, not what a bundler does with
/// it. `sh build.sh` rather than `./build.sh` because a zip entry carries no
/// executable bit for the restore to put back.
fn write_project(store: &std::path::Path) {
    let web = store.join("web");
    std::fs::create_dir_all(web.join("src")).unwrap();
    std::fs::write(
        web.join("build.sh"),
        "#!/bin/sh\n\
         set -e\n\
         mkdir -p dist/assets\n\
         printf '<!doctype html><div id=root>restored</div>' > dist/index.html\n\
         printf 'console.log(1)' > dist/assets/app.js\n",
    )
    .unwrap();
}

/// A self-signed certificate for `localhost`, and its key.
fn self_signed() -> (String, String) {
    let key = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    (key.cert.pem(), key.key_pair.serialize_pem())
}

/// Take a backup of everything the server offers, returning the zip.
async fn backup_everything(server: &mut Server) -> Vec<u8> {
    let (status, options) = server.client.send("GET", "/api/backup", None).await;
    assert_eq!(status, StatusCode::OK, "{options}");
    let (status, bytes, headers) = server
        .client
        .raw(
            "POST",
            "/backup/create",
            Some((
                serde_json::to_vec(&json!({ "include": options["include"] })).unwrap(),
                "application/json",
            )),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/zip"
    );
    // Named, so a browser's save dialog offers something an admin can find again.
    let disposition = headers
        .get(header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(disposition.starts_with("attachment; filename=\"saltcorn-backup-"), "{disposition}");
    bytes
}

/// One entry of a zip, as text.
fn entry(archive: &[u8], path: &str) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive)).expect("a zip");
    let mut file = zip.by_name(path).unwrap_or_else(|_| panic!("no `{path}` in the backup"));
    let mut text = String::new();
    file.read_to_string(&mut text).expect("readable text");
    text
}

/// Whether a zip holds an entry at all.
fn has_entry(archive: &[u8], path: &str) -> bool {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive)).expect("a zip");
    zip.by_name(path).is_ok()
}

#[tokio::test]
async fn a_backup_restores_into_a_second_installation() -> sc_error::Result<()> {
    let mut source = setup().await?;
    furnish(&mut source).await?;
    let archive = backup_everything(&mut source).await;

    // The file is a zip an unarchiver can open, and it says what it holds.
    let manifest: Value = serde_json::from_str(&entry(&archive, "manifest.json")).unwrap();
    assert_eq!(manifest["format"], json!("saltcorn-backup"));
    assert_eq!(manifest["contents"]["users"], json!(1));
    assert_eq!(
        entry(&archive, "file-stores/assets/files/notes/readme.txt"),
        "hello from the backup"
    );

    // --- and now the other server -------------------------------------------
    let mut target = setup().await?;
    // Its own store, at its own path: the backup's definition points at the source
    // server's directory, and the restore must not repoint a working store at it.
    // This is the arrangement an admin restoring onto a new machine has.
    sc_catalog::save_file_store(
        &target.catalog,
        &FileStoreDef::local("assets", target.files.to_string_lossy()),
    )
    .await?;
    sc_catalog::connect_file_store_def(
        &target.catalog,
        &sc_catalog::load_file_store_by_name(&target.catalog, "assets")
            .await?
            .expect("the store just saved"),
    )?;

    let (status, uploaded, _) = {
        let (status, bytes, _) = target
            .client
            .raw("POST", "/backup/upload", Some((archive.clone(), "application/zip")))
            .await;
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value, ())
    };
    assert_eq!(status, StatusCode::OK, "{uploaded}");
    // What the dialog is built from: everything the file holds, ticked.
    assert_eq!(uploaded["available"]["tables"][0]["name"], json!("books"));
    assert_eq!(uploaded["available"]["tables"][0]["count"], json!(2));
    assert_eq!(uploaded["include"]["table_data"], json!(["books"]));
    assert_eq!(uploaded["include"]["ssl"], json!(true));

    let (status, report) = target
        .client
        .send(
            "POST",
            "/api/backup/restore",
            Some(json!({ "id": uploaded["id"], "include": uploaded["include"] })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");

    // --- read it back through the ordinary admin API -------------------------
    let (status, tables) = target.client.send("GET", "/api/tables", None).await;
    assert_eq!(status, StatusCode::OK, "{tables}");
    let books = tables
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == json!("books"))
        .expect("the restored table");
    // The settings came with it, not just the columns.
    assert_eq!(books["label"], json!("Books"));
    assert_eq!(books["description"], json!("Everything on the shelf"));
    assert_eq!(books["min_role_read"], json!(100));

    let (status, fields) = target.client.send("GET", "/api/tables/books/fields", None).await;
    assert_eq!(status, StatusCode::OK, "{fields}");
    let title = fields
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == json!("title"))
        .expect("the restored column");
    assert_eq!(title["label"], json!("The title"));

    let (status, rows) = target.client.send("GET", "/api/tables/books/rows", None).await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    let titles: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["title"].as_str())
        .collect();
    assert_eq!(titles, vec!["Dune", "Solaris"]);

    // The file's bytes went into *this* server's store.
    let (status, file) = target
        .client
        .send(
            "POST",
            "/api/file-stores/assets/read",
            Some(json!({ "path": "notes/readme.txt" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{file}");
    assert_eq!(file["text"], json!("hello from the backup"));
    assert!(
        target.files.join("notes/readme.txt").exists(),
        "the bytes are in the target server's own directory"
    );
    // …and the store it already had was left pointing where it pointed.
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("already defined")),
        "the kept definition is reported: {report}"
    );

    // The application came back **built**: the bundler ran over the source tree the
    // file store restored a moment earlier, and its output is in this server's own
    // directory. "Restore and go" is the claim, so the build is part of the restore
    // rather than a button somebody has to know to press.
    let (status, applications) = target.client.send("GET", "/api/applications", None).await;
    assert_eq!(status, StatusCode::OK, "{applications}");
    assert!(
        applications
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["subdomain"] == json!("blog")),
        "{applications}"
    );
    assert!(
        report["restored"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap_or_default().contains("`blog` built and serving")),
        "the build is reported: {report}"
    );
    assert_eq!(
        std::fs::read_to_string(target.files.join("web/dist/index.html")).unwrap(),
        "<!doctype html><div id=root>restored</div>"
    );
    // …and the client the build generates is typed against *this* server's schema.
    assert!(
        target.files.join("web/src/client.ts").exists(),
        "the generated client was written into the restored source tree"
    );

    let (status, roles) = target.client.send("GET", "/api/roles", None).await;
    assert_eq!(status, StatusCode::OK, "{roles}");
    assert!(
        roles
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["name"] == json!("Librarian")),
        "{roles}"
    );

    let (status, triggers) = target.client.send("GET", "/api/triggers", None).await;
    assert_eq!(status, StatusCode::OK, "{triggers}");
    let trigger = triggers
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == json!("note_new_book"))
        .expect("the restored trigger");
    // Live, not merely stored: a restored trigger with an error would have one
    // here, and it fires on the table that was restored beside it.
    assert_eq!(trigger["error"], Value::Null);
    assert_eq!(trigger["channel"], json!("books"));

    // The private key travelled — a backup that redacted it would restore a server
    // that cannot serve HTTPS — and it is *stored*, not merely echoed.
    assert_eq!(
        stored_config(&target.catalog, SSL_MODE).await?,
        Some(json!(MODE_CUSTOM))
    );
    let restored_key = stored_config(&target.catalog, SSL_PRIVATE_KEY)
        .await?
        .expect("the restored private key");
    assert!(
        restored_key.as_str().unwrap().contains("PRIVATE KEY"),
        "the key itself, not the sentinel: {restored_key}"
    );

    // The admin who ran the restore is still the admin who ran it: the backup's
    // own account has the same address, and a restore that overwrote it would have
    // replaced their password with the source server's hash.
    let (status, users) = target.client.send("GET", "/api/users", None).await;
    assert_eq!(status, StatusCode::OK, "{users}");
    assert_eq!(users.as_array().unwrap().len(), 1, "{users}");
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("already on this server")),
        "the kept account is reported: {report}"
    );
    Ok(())
}

#[tokio::test]
async fn what_a_backup_includes_is_remembered_as_what_was_left_out() -> sc_error::Result<()> {
    let mut server = setup().await?;
    furnish(&mut server).await?;

    // Everything, to begin with.
    let (status, options) = server.client.send("GET", "/api/backup", None).await;
    assert_eq!(status, StatusCode::OK, "{options}");
    assert_eq!(options["include"]["tables"], json!(["books"]));
    assert_eq!(options["include"]["table_data"], json!(["books"]));
    assert_eq!(options["include"]["users"], json!(true));
    assert_eq!(options["available"]["tables"][0]["count"], json!(2));

    // A narrowed backup: the definitions but not the rows, and no users.
    let (status, bytes, _) = server
        .client
        .raw(
            "POST",
            "/backup/create",
            Some((
                serde_json::to_vec(&json!({ "include": {
                    "tables": ["books"],
                    "table_data": [],
                    "applications": [],
                    "file_stores": ["assets"],
                    "users": false,
                    "agents": false,
                    "triggers": true,
                    "ssl": true,
                }}))
                .unwrap(),
                "application/json",
            )),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    // The rows really were left out of the file, not merely absent from the dialog.
    assert!(has_entry(&bytes, "tables/books/table.json"));
    assert!(!has_entry(&bytes, "tables/books/rows.json"));
    assert!(!has_entry(&bytes, "users.json"));
    assert!(has_entry(&bytes, "triggers.json"));

    // Reopened, the dialog shows the tuned selection…
    let (status, options) = server.client.send("GET", "/api/backup", None).await;
    assert_eq!(status, StatusCode::OK, "{options}");
    assert_eq!(options["include"]["table_data"], json!([]));
    assert_eq!(options["include"]["users"], json!(false));
    assert_eq!(options["include"]["triggers"], json!(true));

    // …and it is stored as *exclusions*, which is what makes a table created later
    // part of the next backup without anybody going back to the dialog.
    let stored = stored_config(&server.catalog, sc_config::BACKUP_INCLUDE)
        .await?
        .expect("the remembered selection");
    assert_eq!(stored["exclude_table_data"], json!(["books"]));
    assert_eq!(stored["exclude_tables"], json!([]));
    assert_eq!(stored["users"], json!(false));

    server
        .catalog
        .create_table(
            "reviews",
            &[
                DataField::plain("id", TypeRef::Basic(BasicType::Int))
                    .required()
                    .primary_key(),
            ],
        )
        .await?;
    let (status, options) = server.client.send("GET", "/api/backup", None).await;
    assert_eq!(status, StatusCode::OK, "{options}");
    assert_eq!(options["include"]["tables"], json!(["books", "reviews"]));
    // The new table's rows are in; the one that was unticked stays unticked.
    assert_eq!(options["include"]["table_data"], json!(["reviews"]));
    Ok(())
}

/// Rows without their table's metadata would be rows nothing could read, so the
/// server narrows the payload rather than trusting it.
#[tokio::test]
async fn rows_are_never_backed_up_without_their_table() -> sc_error::Result<()> {
    let mut server = setup().await?;
    furnish(&mut server).await?;

    let (status, bytes, _) = server
        .client
        .raw(
            "POST",
            "/backup/create",
            Some((
                serde_json::to_vec(&json!({ "include": {
                    "tables": [],
                    "table_data": ["books"],
                    "applications": [],
                    "file_stores": [],
                    "users": true,
                    "agents": false,
                    "triggers": false,
                    "ssl": false,
                }}))
                .unwrap(),
                "application/json",
            )),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert!(!has_entry(&bytes, "tables/books/rows.json"));
    assert!(!has_entry(&bytes, "tables/books/table.json"));
    assert!(has_entry(&bytes, "users.json"));
    Ok(())
}

/// A zip that is not a backup is refused where the admin can see it, rather than
/// restored as nothing.
#[tokio::test]
async fn a_file_that_is_not_a_backup_is_refused_by_name() -> sc_error::Result<()> {
    let mut server = setup().await?;

    let (status, body, _) = server
        .client
        .raw(
            "POST",
            "/backup/upload",
            Some((b"this is not a zip at all".to_vec(), "application/zip")),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = String::from_utf8_lossy(&body);
    assert!(message.contains("not a zip"), "{message}");

    // A real zip with nothing of ours in it: a different mistake, and told apart.
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("holiday.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    std::io::Write::write_all(&mut zip, b"photographs").unwrap();
    let not_a_backup = zip.finish().unwrap().into_inner();
    let (status, body, _) = server
        .client
        .raw(
            "POST",
            "/backup/upload",
            Some((not_a_backup, "application/zip")),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = String::from_utf8_lossy(&body);
    assert!(message.contains("manifest.json"), "{message}");

    // And a restore naming an upload nobody made.
    let (status, body) = server
        .client
        .send(
            "POST",
            "/api/backup/restore",
            Some(json!({
                "id": uuid::Uuid::new_v4().to_string(),
                "include": { "tables": [], "table_data": [], "applications": [],
                             "file_stores": [], "users": false, "agents": false,
                             "triggers": false, "ssl": false },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    Ok(())
}

/// Backing up is reading the whole installation and restoring is writing it, so
/// both are admin-only — and so is asking what there is to back up.
#[tokio::test]
async fn every_backup_route_is_admin_only() -> sc_error::Result<()> {
    let mut server = setup().await?;
    let (status, _) = server.client.send("POST", "/api/logout", None).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = server.client.send("GET", "/api/backup", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = server
        .client
        .raw(
            "POST",
            "/backup/create",
            Some((b"{}".to_vec(), "application/json")),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = server
        .client
        .raw("POST", "/backup/upload", Some((b"zip".to_vec(), "application/zip")))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = server
        .client
        .send("POST", "/api/backup/restore", Some(json!({ "id": "x" })))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    Ok(())
}
