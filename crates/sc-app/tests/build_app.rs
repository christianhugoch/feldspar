//! Phase 9 integration test: an app's source in a **git-repo file store** is
//! built by invoking its bundler, and the output is served by the code framework
//! (design §13.3).
//!
//! The catalog-free half of the build step is unit-tested in `sc-app`'s `build`
//! module; what needs a real database is the path through the [`Catalog`] — the
//! app names a file store, the catalog resolves it, and the build runs against
//! that store's tree.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sc_app::{
    AppRequest, AppSource, Application, BuildSpec, CodeFramework, FrameworkRef, build_app,
    build_code_framework,
};
use sc_catalog::{Catalog, FileStoreId};
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_files::LocalFileStore;
use sc_test_harness::TestDb;

/// A scratch directory removed when the guard drops.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> sc_error::Result<TempDir> {
        let dir = std::env::temp_dir().join(format!(
            "sc-app-build-it-{}-{tag}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir)?;
        Ok(TempDir(dir))
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

/// Lay out an app source tree in `root`: a git repo whose `web/` holds a bundler
/// that emits an SPA into `web/dist`.
///
/// The stand-in bundler is a shell script rather than a real `npm run build`: to
/// the build step a bundler is just a process that emits files into the output
/// directory, and depending on a Node toolchain would make this test need one.
fn write_app_source(root: &Path) -> sc_error::Result<()> {
    // The design (§13.3) has the app's source live in a git repository; a bare
    // `.git` directory is enough to make the store report itself as one.
    std::fs::create_dir_all(root.join(".git"))?;

    let web = root.join("web");
    std::fs::create_dir_all(&web)?;
    let script = web.join("build.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         set -e\n\
         mkdir -p dist/assets\n\
         printf '<!doctype html><div id=root></div>' > dist/index.html\n\
         printf 'console.log(\"app\")' > dist/assets/app.js\n\
         printf 'body{margin:0}' > dist/assets/app.css\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn build_spec() -> BuildSpec {
    BuildSpec {
        command: "sh".to_owned(),
        args: vec!["build.sh".to_owned()],
        source_dir: "web".to_owned(),
        output_dir: "web/dist".to_owned(),
    }
}

/// A catalog over a real (empty) Postgres. The build step never touches table
/// data — it needs the catalog only as the registry that resolves a file store
/// by name — but the catalog cannot be constructed without a live database.
async fn catalog(db: &TestDb) -> sc_error::Result<Catalog> {
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    Catalog::init(driver as Arc<dyn DatabaseDriver>).await
}

#[tokio::test]
async fn builds_an_app_from_a_git_repo_file_store_and_serves_the_bundle()
-> sc_error::Result<()> {
    let db = TestDb::new().await?;
    let cat = catalog(&db).await?;

    let tmp = TempDir::new("ok")?;
    write_app_source(tmp.path())?;
    cat.connect_file_store(Arc::new(LocalFileStore::new("apps", tmp.path())?))?;

    // An application whose primary framework is a code framework sourced from
    // that store. The `FrameworkRef` carries the same sub-paths as the spec: it
    // is the serialisable form the runtime resolves to the framework below.
    let app = Application::new(
        "blog",
        "My Blog",
        "blog",
        FrameworkRef::new("code")
            .with("store", "apps")
            .with("source", "web")
            .with("output", "web/dist"),
    )
    .with_file_store(FileStoreId("apps".to_owned()));
    assert!(app.can_access_file_store(&FileStoreId("apps".to_owned())));

    let source = AppSource::new(FileStoreId("apps".to_owned()), build_spec());
    let report = build_app(&cat, &source).await?;

    // The bundler ran and its three emitted files were picked up.
    assert_eq!(report.bundle.len(), 3);
    assert_eq!(report.output_dir, tmp.path().join("web").join("dist"));
    // The store is a git repo, as the design expects an app's source to be.
    assert!(report.git_repo);

    // The built output is what the framework serves.
    let fw = CodeFramework::new("code", report.bundle);
    let index = fw.serve(&AppRequest::get("/"));
    assert_eq!(index.status, 200);
    assert_eq!(index.content_type, "text/html; charset=utf-8");
    assert_eq!(&index.body[..], b"<!doctype html><div id=root></div>");

    let js = fw.serve(&AppRequest::get("/assets/app.js"));
    assert_eq!(js.status, 200);
    assert_eq!(js.content_type, "text/javascript; charset=utf-8");
    assert_eq!(&js.body[..], b"console.log(\"app\")");

    let css = fw.serve(&AppRequest::get("/assets/app.css"));
    assert_eq!(css.status, 200);
    assert_eq!(css.content_type, "text/css; charset=utf-8");

    // A client-routed deep link resolves to the SPA entry point.
    assert_eq!(fw.serve(&AppRequest::get("/posts/42")).status, 200);

    // The catalog was never asked for a table: an app's UI reaches data only
    // through the API providers, never through the framework.
    assert!(cat.get("posts")?.is_none());
    Ok(())
}

#[tokio::test]
async fn build_code_framework_returns_a_rebuildable_framework() -> sc_error::Result<()> {
    let db = TestDb::new().await?;
    let cat = catalog(&db).await?;

    let tmp = TempDir::new("fw")?;
    write_app_source(tmp.path())?;
    cat.connect_file_store(Arc::new(LocalFileStore::new("apps", tmp.path())?))?;

    let source = AppSource::new(FileStoreId("apps".to_owned()), build_spec());
    let fw = build_code_framework(&cat, "code", &source).await?;

    assert_eq!(sc_app::Framework::name(&fw), "code");
    // The spec rides along on the framework, so the app can be rebuilt without
    // the caller having kept the source around.
    assert_eq!(sc_app::Framework::build(&fw), Some(build_spec()));
    assert_eq!(fw.serve(&AppRequest::get("/")).status, 200);

    // Rebuilding from the framework's own spec reproduces the bundle.
    let rebuilt = build_app(&cat, &AppSource::new(source.store.clone(), build_spec())).await?;
    assert_eq!(rebuilt.bundle.len(), 3);
    Ok(())
}

#[tokio::test]
async fn a_failing_bundler_fails_the_build_with_its_diagnostics() -> sc_error::Result<()> {
    let db = TestDb::new().await?;
    let cat = catalog(&db).await?;

    let tmp = TempDir::new("fail")?;
    write_app_source(tmp.path())?;
    // Replace the bundler with one that fails the way a real one does: a
    // diagnostic on stderr and a non-zero exit.
    std::fs::write(
        tmp.path().join("web").join("build.sh"),
        "#!/bin/sh\necho 'error TS2304: Cannot find name Foo' >&2\nexit 1\n",
    )?;
    cat.connect_file_store(Arc::new(LocalFileStore::new("apps", tmp.path())?))?;

    let source = AppSource::new(FileStoreId("apps".to_owned()), build_spec());
    let err = build_app(&cat, &source)
        .await
        .expect_err("a failing bundler must fail the build");

    // The failure names the bundler's own diagnostic, not just an exit code.
    let msg = err.to_string();
    assert!(msg.contains("error TS2304: Cannot find name Foo"), "{msg}");
    Ok(())
}

#[tokio::test]
async fn a_store_not_connected_to_the_catalog_is_rejected() -> sc_error::Result<()> {
    let db = TestDb::new().await?;
    let cat = catalog(&db).await?;

    // Nothing connected under this name.
    let source = AppSource::new(FileStoreId("apps".to_owned()), build_spec());
    let err = build_app(&cat, &source)
        .await
        .expect_err("an unconnected store must fail the build");
    assert!(err.to_string().contains("apps"), "{err}");
    Ok(())
}
