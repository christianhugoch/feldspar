//! The **git file-store backend**, driven through the assembled router as an
//! admin would drive it: generate a deploy key, create the store (which clones
//! it), edit a file through the file manager, commit, push, pull.
//!
//! The remote is a bare repository in a temp directory. `git clone`, `pull` and
//! `push` behave identically over a local path, so the whole round trip is
//! exercised with no network and no credentials; `sc-files`' own
//! `tests/git_store.rs` covers the driver in isolation, and this covers the part
//! only the server can get right — that saving a store clones it, that the live
//! registry ends up holding a usable store, and that a clone that fails leaves a
//! saved, editable definition rather than an error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

/// A cookie-jar-carrying client over the router (CSRF + session), mirroring the
/// helper in `file_store_admin_api.rs`.
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

/// The data directory every store in this binary clones into.
///
/// Set once for the whole process, because the environment is process-wide and
/// these tests run in parallel: a per-test `set_var` would have tests reading
/// each other's value. One directory is enough because a clone lands under the
/// store's *name*, and every test below names its store differently.
fn data_dir() -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("sc-git-api-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Never the developer's real data directory: a test that cloned there
        // would leave repositories behind and collide with the next run.
        unsafe { std::env::set_var(sc_files::DATA_DIR_ENV, &dir) };
        dir
    })
}

/// Run git in `dir`, asserting it succeeded.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap_or_else(|e| panic!("running `git {}`: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "`git {}` failed: {}{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A bare repository holding one commit, standing in for the remote whose URL an
/// admin would paste into the form.
fn origin_with_a_commit(tag: &str) -> PathBuf {
    let base = data_dir().join(format!("remotes-{tag}"));
    let bare = base.join("origin.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "--bare", "--initial-branch=main", "."]);

    let seed = base.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "--initial-branch=main", "."]);
    git(&seed, &["config", "user.email", "seed@example.com"]);
    git(&seed, &["config", "user.name", "Seed"]);
    std::fs::write(seed.join("README.md"), "# from the remote\n").unwrap();
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-m", "initial"]);
    git(&seed, &["remote", "add", "origin", &bare.to_string_lossy()]);
    git(&seed, &["push", "origin", "main"]);
    bare
}

/// A router over a real database with the platform tables bootstrapped, and an
/// admin logged in.
async fn setup() -> sc_error::Result<(Client, Arc<Catalog>, TestDb)> {
    data_dir();
    let db = TestDb::new().await?;
    // Neutralise any `users` table inherited from the template database before
    // bootstrap introspects (see `file_store_admin_api.rs`).
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

/// Run an instance-scope operation and return its output, asserting it worked.
async fn operation(
    client: &mut Client,
    id: &str,
    name: &str,
    input: Option<Value>,
) -> String {
    let (status, body) = client
        .send(
            "POST",
            &format!("/api/file-stores/{id}/operations/{name}"),
            Some(json!({ "input": input.unwrap_or_else(|| json!({})) })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{name}: {body}");
    body["output"].as_str().unwrap_or_default().to_owned()
}

/// Create a git store named `name` for `url`, as the form would.
async fn create_git_store(client: &mut Client, name: &str, url: &str) -> (StatusCode, Value) {
    client
        .send(
            "POST",
            "/api/file-stores",
            Some(json!({
                "name": name,
                "description": "",
                "backend": "git",
                "config": { "url": url },
                "min_role": Value::Null,
            })),
        )
        .await
}

#[tokio::test]
async fn a_git_store_is_cloned_on_save_and_serves_the_repository() -> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;
    let origin = origin_with_a_commit("serve");

    // The git backend is offered alongside `local`, with its settings declared —
    // the form renders it with no knowledge of what a repository is.
    let (_, backends) = client.send("GET", "/api/file-store-backends", None).await;
    let git_backend = backends
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == json!("git"))
        .expect("the git backend is registered");
    let settings: Vec<&str> = git_backend["config_spec"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(settings, ["url", "branch", "key_path", "public_key"]);

    // And its *operations* are declared the same way, so the form renders a
    // button per operation with no knowledge of what a repository is. This is
    // the contract a plugin-supplied backend gets too.
    let ops = git_backend["operations"].as_array().unwrap();
    let names: Vec<&str> = ops.iter().map(|o| o["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        ["generate_deploy_key", "status", "clone", "pull", "push", "commit"]
    );
    let by_name = |n: &str| ops.iter().find(|o| o["name"] == json!(n)).unwrap();
    // The key generator runs against unsaved settings — it has to, because
    // saving clones and cloning needs a key the remote already accepts.
    assert_eq!(by_name("generate_deploy_key")["scope"], json!("configure"));
    assert_eq!(by_name("pull")["scope"], json!("instance"));
    // Status reports rather than acts, so it runs when the screen opens.
    assert_eq!(by_name("status")["automatic"], json!(true));
    assert_eq!(by_name("pull")["automatic"], json!(false));
    // The clone *is* the creation of a git store, so it runs on the first save
    // and a failure means nothing is created. It stays a button too, for a
    // working copy that has been lost.
    assert_eq!(by_name("clone")["on_create"], json!(true));
    assert_eq!(by_name("pull")["on_create"], json!(false));
    // A commit's message is an ordinary `FormField`, rendered by the same code
    // that renders settings.
    let commit_input = by_name("commit")["input_spec"].as_array().unwrap();
    assert_eq!(commit_input.len(), 1);
    assert_eq!(commit_input[0]["name"], json!("message"));
    assert_eq!(commit_input[0]["required"], json!(true));
    // A public key is one long line to copy, so it declares itself multi-line
    // rather than the UI knowing which settings happen to hold keys.
    let public_key = git_backend["config_spec"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == json!("public_key"))
        .unwrap();
    assert_eq!(public_key["multiline"], json!(true));

    // The `local` backend declares none — most backends have nothing to do
    // beyond reading and writing files, which is why operations are declared
    // rather than assumed.
    let local = backends
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == json!("local"))
        .unwrap();
    assert!(local["operations"].as_array().unwrap().is_empty());

    // Creating the store clones it — that is the whole difference from a local
    // store, whose directory has to exist already.
    let (status, created) =
        create_git_store(&mut client, "site", &origin.to_string_lossy()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["connected"], json!(true));
    assert_eq!(created["error"], Value::Null);
    assert_eq!(created["is_git_repo"], json!(true));
    let id = created["id"].as_str().unwrap().to_owned();

    // It is in the live registry, so the file manager reaches it with no
    // restart, and it serves what the remote had.
    assert!(catalog.file_store("site")?.is_some());
    let (status, listing) = client
        .send(
            "POST",
            "/api/file-stores/site/browse",
            Some(json!({ "dir": "" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        listing
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["name"] == json!("README.md")),
        "{listing}"
    );

    // A clean clone, on the remote's branch, with its history. The report is
    // *text*, which is what keeps the admin UI free of git: a screen rendering
    // branches and ahead/behind counts would be a screen that knows what those
    // are, and could not render a plugin backend's status at all.
    let report = operation(&mut client, &id, "status", None).await;
    assert!(report.contains("On branch main."), "{report}");
    assert!(report.contains("No uncommitted changes."), "{report}");
    assert!(report.contains("Up to date with the remote."), "{report}");
    assert!(report.contains("initial"), "{report}");

    // Edit a file through the file manager — the ordinary write path, which
    // knows nothing about git — and the repository sees the change.
    let (status, _) = client
        .send(
            "POST",
            "/api/file-stores/site/write",
            Some(json!({ "path": "index.md", "text": "# hello\n" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let report = operation(&mut client, &id, "status", None).await;
    assert!(report.contains("1 uncommitted change(s):"), "{report}");
    assert!(report.contains("index.md"), "{report}");

    // Commit everything, with the admin's message — an argument the operation
    // declared, validated against that declaration like any setting.
    let out = operation(
        &mut client,
        &id,
        "commit",
        Some(json!({ "message": "add the index" })),
    )
    .await;
    assert!(!out.is_empty());

    // A commit with no message is refused by the declared `required` argument,
    // not by anything that knows what a commit is.
    let (status, refused) = client
        .send(
            "POST",
            &format!("/api/file-stores/{id}/operations/commit"),
            Some(json!({ "input": { } })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    assert!(
        serde_json::to_string(&refused).unwrap().contains("message"),
        "{refused}"
    );

    // Committing again with nothing changed is a no-op, not a failure: pressing
    // the button on an unchanged tree asks a question rather than making a
    // mistake.
    let out = operation(
        &mut client,
        &id,
        "commit",
        Some(json!({ "message": "nothing here" })),
    )
    .await;
    assert!(out.contains("Nothing to commit"), "{out}");

    // One commit ahead of the remote until it is pushed …
    let report = operation(&mut client, &id, "status", None).await;
    assert!(report.contains("No uncommitted changes."), "{report}");
    assert!(report.contains("1 commit(s) to push."), "{report}");

    // … and after the push the remote has the file.
    operation(&mut client, &id, "push", None).await;
    assert_eq!(git(&origin, &["show", "main:index.md"]), "# hello");

    // Someone else pushes; pulling brings it into the store.
    let elsewhere = data_dir().join("elsewhere-serve");
    std::fs::create_dir_all(&elsewhere).unwrap();
    git(&elsewhere, &["clone", &origin.to_string_lossy(), "."]);
    git(&elsewhere, &["config", "user.email", "other@example.com"]);
    git(&elsewhere, &["config", "user.name", "Other"]);
    std::fs::write(elsewhere.join("upstream.md"), "elsewhere\n").unwrap();
    git(&elsewhere, &["add", "-A"]);
    git(&elsewhere, &["commit", "-m", "upstream"]);
    git(&elsewhere, &["push", "origin", "main"]);

    operation(&mut client, &id, "pull", None).await;
    let (status, file) = client
        .send(
            "POST",
            "/api/file-stores/site/read",
            Some(json!({ "path": "upstream.md" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(file["text"], json!("elsewhere\n"));

    Ok(())
}

/// A deploy key is generated **before** the store exists, which is the ordering
/// the whole flow turns on: the admin must be able to install the public half at
/// the remote before anything tries to clone.
///
/// It runs through the generic `configure`-scope endpoint, addressed by
/// *backend*, and what it returns is a settings patch the form adopts. Nothing
/// in the path knows what a deploy key is.
#[tokio::test]
async fn a_deploy_key_is_generated_before_the_store_is_saved() -> sc_error::Result<()> {
    let (mut client, _catalog, _db) = setup().await?;

    let (status, res) = client
        .send(
            "POST",
            "/api/file-store-backends/git/operations/generate_deploy_key",
            Some(json!({
                // The settings as they stand in the form — the store does not
                // exist, so there is nothing else to run against.
                "name": "private site",
                "config": { "url": "git@example.com:me/site.git" },
                "input": {},
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{res}");

    // The operation filled in two settings, and left the one the admin typed
    // alone.
    let config = &res["config"];
    assert_eq!(config["url"], json!("git@example.com:me/site.git"));
    let public = config["public_key"].as_str().unwrap();
    assert!(public.starts_with("ssh-ed25519 "), "{public}");
    assert!(!public.contains('\n'));
    // The private half is a *path*: it must not leave the machine.
    let key_path = PathBuf::from(config["key_path"].as_str().unwrap());
    assert!(key_path.is_file());
    assert!(
        !serde_json::to_string(&res)
            .unwrap()
            .contains("PRIVATE KEY"),
        "the private key must not be returned: {res}"
    );
    // Its output is the public key, for the admin to copy.
    assert!(res["output"].as_str().unwrap().contains(public), "{res}");

    // The form saves the patched config unchanged, and the store works.
    let origin = origin_with_a_commit("deploykey");
    let (status, created) = client
        .send(
            "POST",
            "/api/file-stores",
            Some(json!({
                "name": "keyed",
                "description": "",
                "backend": "git",
                "config": {
                    "url": origin.to_string_lossy(),
                    "key_path": key_path.to_string_lossy(),
                    "public_key": public,
                },
                "min_role": Value::Null,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["config"]["public_key"], json!(public));
    assert_eq!(created["connected"], json!(true));

    Ok(())
}

/// An `instance`-scope operation cannot be reached through the backend endpoint
/// either — the scope check runs both ways.
#[tokio::test]
async fn a_backend_endpoint_refuses_an_instance_operation() -> sc_error::Result<()> {
    let (mut client, _catalog, _db) = setup().await?;
    let (status, body) = client
        .send(
            "POST",
            "/api/file-store-backends/git/operations/pull",
            Some(json!({ "name": "x", "config": {}, "input": {} })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    Ok(())
}

/// **A clone that fails creates nothing.** Creating a store is transactional:
/// the backend builds what it has to build *before* anything is written, so a
/// failed attempt leaves no row.
///
/// This is the bug the rule exists for. Saving first and reporting the failure
/// would leave the admin correcting the URL, pressing Create again, and being
/// told the name is already taken — by the row their own failed attempt left
/// behind, which they never asked for and cannot see a reason for.
#[tokio::test]
async fn a_failed_clone_creates_nothing_and_the_retry_succeeds() -> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;
    let missing = data_dir().join("no-such-repository");

    let (status, body) =
        create_git_store(&mut client, "retried", &missing.to_string_lossy()).await;
    // Refused, with git's own message — which is what tells the admin what to
    // change.
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let reason = serde_json::to_string(&body).unwrap();
    assert!(reason.contains("no-such-repository"), "{reason}");

    // Nothing was created: no row, nothing in the registry.
    let (_, stores) = client.send("GET", "/api/file-stores", None).await;
    assert!(
        stores.as_array().unwrap().is_empty(),
        "a failed create must leave no store: {stores}"
    );
    assert!(catalog.file_store("retried")?.is_none());

    // So correcting the URL and pressing Create again — with the same name —
    // works, instead of colliding with the failed attempt.
    let origin = origin_with_a_commit("retry");
    let (status, created) =
        create_git_store(&mut client, "retried", &origin.to_string_lossy()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["connected"], json!(true));
    assert!(catalog.file_store("retried")?.is_some());

    Ok(())
}

/// A clone that fails while **editing** an existing store is reported, not
/// fatal — the opposite of the create above, and §1.2's rule.
///
/// The asymmetry is the substance: there is nothing to preserve on a create, but
/// an existing store must stay saved and editable even when it cannot be brought
/// up, because editing it is the repair. Refusing the save would trap an admin
/// whose remote is briefly unreachable with a definition they can no longer
/// correct.
#[tokio::test]
async fn a_clone_that_fails_on_edit_keeps_the_store_and_reports_why() -> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;
    let origin = origin_with_a_commit("edit");

    let (status, created) =
        create_git_store(&mut client, "edited", &origin.to_string_lossy()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();

    // The working copy goes missing — a disk wiped, a restore onto a fresh
    // machine — and the remote is unreachable too.
    let report = operation(&mut client, &id, "status", None).await;
    let tree = working_copy_line(&report)
        .trim_start_matches("Working copy:")
        .trim()
        .to_owned();
    std::fs::remove_dir_all(&tree).unwrap();
    let (status, updated) = client
        .send(
            "PUT",
            &format!("/api/file-stores/{id}"),
            Some(json!({
                "name": "edited",
                "description": "",
                "backend": "git",
                "config": { "url": data_dir().join("gone-away").to_string_lossy() },
                "min_role": Value::Null,
            })),
        )
        .await;
    // Saved, not refused, with the reason recorded against it.
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["connected"], json!(false));
    assert!(
        updated["error"].as_str().unwrap().contains("gone-away"),
        "{updated}"
    );
    assert!(catalog.file_store("edited")?.is_none());

    // And pointing it back at a real repository is the repair, with no restart.
    let (status, repaired) = client
        .send(
            "PUT",
            &format!("/api/file-stores/{id}"),
            Some(json!({
                "name": "edited",
                "description": "",
                "backend": "git",
                "config": { "url": origin.to_string_lossy() },
                "min_role": Value::Null,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{repaired}");
    assert_eq!(repaired["connected"], json!(true));
    assert_eq!(repaired["error"], Value::Null);
    assert!(catalog.file_store("edited")?.is_some());

    Ok(())
}

/// The repository endpoints refuse a store that is not a git store, rather than
/// running git in a directory that is nobody's repository.
#[tokio::test]
async fn repository_operations_refuse_a_non_git_store() -> sc_error::Result<()> {
    let (mut client, _catalog, _db) = setup().await?;
    let dir = data_dir().join("plain-directory");
    std::fs::create_dir_all(&dir).unwrap();

    let (status, created) = client
        .send(
            "POST",
            "/api/file-stores",
            Some(json!({
                "name": "plain",
                "description": "",
                "backend": "local",
                "config": { "path": dir.to_string_lossy() },
                "min_role": Value::Null,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap().to_owned();

    // The `local` backend declares no operations, so every one of these is
    // refused by name — the generic check, not a git-specific one.
    for op in ["status", "pull", "push", "clone"] {
        let (status, body) = client
            .send(
                "POST",
                &format!("/api/file-stores/{id}/operations/{op}"),
                Some(json!({ "input": {} })),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{op}: {body}");
        let message = serde_json::to_string(&body).unwrap();
        assert!(message.contains("local"), "{op}: {message}");
        assert!(message.contains(op), "{op}: {message}");
    }

    // And a `configure`-scope operation cannot be run through the instance
    // endpoint: it is written against unsaved configuration, so running it here
    // would run it against a definition it was never meant for.
    let (status, body) = client
        .send(
            "POST",
            &format!("/api/file-stores/{id}/operations/generate_deploy_key"),
            Some(json!({ "input": {} })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    Ok(())
}

/// A git store's recorded clone directory survives a rename.
///
/// Without it, renaming would re-derive a directory from the new name, point the
/// store at an empty one, and abandon whatever was uncommitted in the old one.
#[tokio::test]
async fn renaming_a_git_store_keeps_its_working_tree() -> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;
    let origin = origin_with_a_commit("rename");

    let (_, created) = create_git_store(&mut client, "before", &origin.to_string_lossy()).await;
    let id = created["id"].as_str().unwrap().to_owned();

    // Uncommitted work in the tree — the thing a fresh clone would lose.
    client
        .send(
            "POST",
            "/api/file-stores/before/write",
            Some(json!({ "path": "draft.md", "text": "work in progress\n" })),
        )
        .await;
    let before = operation(&mut client, &id, "status", None).await;
    let tree = working_copy_line(&before);

    let (status, renamed) = client
        .send(
            "PUT",
            &format!("/api/file-stores/{id}"),
            Some(json!({
                "name": "after",
                "description": "",
                "backend": "git",
                "config": { "url": origin.to_string_lossy() },
                "min_role": Value::Null,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!(renamed["connected"], json!(true));
    // The old handle is gone from the registry; the new name serves the *same*
    // working tree.
    assert!(catalog.file_store("before")?.is_none());
    assert!(catalog.file_store("after")?.is_some());

    let after = operation(&mut client, &id, "status", None).await;
    assert_eq!(working_copy_line(&after), tree);
    let (_, file) = client
        .send(
            "POST",
            "/api/file-stores/after/read",
            Some(json!({ "path": "draft.md" })),
        )
        .await;
    assert_eq!(file["text"], json!("work in progress\n"));

    Ok(())
}

/// The "Working copy: …" line of a git status report — where the clone actually
/// is, which is what a rename must not change.
fn working_copy_line(report: &str) -> String {
    report
        .lines()
        .find(|l| l.starts_with("Working copy:"))
        .unwrap_or_else(|| panic!("no working-copy line in:\n{report}"))
        .to_owned()
}
