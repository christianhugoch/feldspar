//! Creating a React application's project **on the server** (TODO §2.3).
//!
//! This is the step that removes the SSH requirement, and the reason the `react`
//! framework is more than a settings preset: the MVP's tutorial asked an admin to
//! log into the host, run `npm create vite`, `npm install` and `git init`, and
//! only then fill in five paths. An admin with no shell — which is most of them —
//! could not use the product at all. Here the server writes the project.
//!
//! Three rules the implementation is built around:
//!
//! - **Never overwrite.** Scaffolding into a directory that has anything in it is
//!   refused with an error naming the directory. A generator that clobbers is
//!   worse than no generator, because the work it destroys was the admin's.
//! - **Against the app's real tables.** The project comes up showing real rows in
//!   real columns (see [`files`]), not a placeholder counter. A scaffold whose
//!   first job is to be deleted has not saved anyone any work.
//! - **Failures carry the tool's own output** (§16). A scaffold or install error
//!   is an *Application* error — the admin's configuration or environment — and
//!   the useful part of it is what `npm` or `git` said, not that something
//!   "failed".

mod files;

use bytes::Bytes;
use sc_api::EndpointSet;
use sc_catalog::Catalog;
use sc_error::{Context, Error, Result};
use tokio::process::Command;

use crate::api::{app_endpoints_with, app_graphql, app_schema_sql, app_tables};
use crate::application::Application;
use crate::build::{AppSource, app_source_from_config};
use crate::react::{REACT_FRAMEWORK, project_description, project_path};

pub use files::GeneratedFile;

/// What a scaffold did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldReport {
    /// The project directory within the store, e.g. `todo`; empty for the store
    /// root.
    pub project: String,
    /// The files written, store-relative, in the order they were written.
    pub files: Vec<String>,
    /// Whether `git init` ran in the project directory. `false` when the store is
    /// itself a git repository (the project is already inside one — a nested repo
    /// would be worse than none) or when git is not available.
    pub git_initialized: bool,
}

impl ScaffoldReport {
    /// A one-line summary for the admin's build/scaffold log.
    pub fn summary(&self) -> String {
        format!(
            "scaffolded {} file{} into {}{}",
            self.files.len(),
            if self.files.len() == 1 { "" } else { "s" },
            project_description(&self.project),
            if self.git_initialized {
                " (git repository initialised)"
            } else {
                ""
            }
        )
    }
}

/// Generate a complete React project for `app` into its file store.
///
/// Refuses if the app is not a `react` app, if its store is unreachable, or if
/// the project directory already has anything in it.
pub async fn scaffold_app(
    cat: &Catalog,
    app: &Application,
    dispatcher: Option<&std::sync::Arc<sc_action::TriggerDispatcher>>,
) -> Result<ScaffoldReport> {
    require_scaffoldable(app)?;
    require_api_provider(app)?;
    let source = app_source_from_config(&app.framework)?;
    let project = source.build.source_dir.clone();

    let store = cat.require_file_store(&source.store.0)?;
    if !is_empty_dir(store.as_ref(), &project).await? {
        return Err(Error::invalid(format!(
            "cannot scaffold into {} of file store `{}`: it is not empty. \
             Scaffolding never overwrites existing files; choose another project \
             name, or point the application at the project already there",
            project_description(&project),
            source.store.0
        )));
    }

    let tables = app_tables(app, cat)?;
    let endpoints = app_endpoints_with(app, cat, dispatcher)?;
    let graphql = app_graphql(app, cat)?;
    let schema_sql = app_schema_sql(app, cat)?;
    // This installation's roles, for the documentation that says how to get a
    // session: `feldspar auth token --role NAME` takes a name, and nothing inside
    // a project directory knows what this server calls its roles.
    let roles = documented_roles(cat).await?;
    let skill = crate::generate_skill(cat, app, crate::REACT_CLIENT_FILE);
    let generated = files::project_files(&files::ProjectContext {
        project: &project,
        app,
        tables: &tables,
        endpoints: &endpoints,
        graphql: graphql.as_ref(),
        schema_sql: &schema_sql,
        // Where this deployment serves its apps, so the generated documentation
        // names the URL to open. `None` when nobody told this process.
        origin: cat.public_origin(),
        roles: &roles,
        skill: &skill,
    });

    let mut written = Vec::with_capacity(generated.len());
    for file in &generated {
        let path = project_path(&project, &file.path);
        store
            .write(&path, Bytes::from(file.contents.clone().into_bytes()))
            .await
            .with_context(|| format!("scaffolding {path}"))?;
        written.push(path);
    }

    let git_initialized = if store.is_git_repo() {
        // The project is already inside a repository; `git init` here would
        // create a nested one, which is a worse state than the one we are in.
        false
    } else {
        init_git_repo(&source, store.local_path(&project)?.as_deref()).await?
    };

    Ok(ScaffoldReport {
        project,
        files: written,
        git_initialized,
    })
}

/// The roles the generated documentation names, or none when this database has
/// no roles table.
///
/// A running server always has one — [`sc_auth::bootstrap`] creates it before the
/// first user can exist — so the empty answer is for a catalog that was brought
/// up without it, which is a test's database rather than an installation's. It
/// degrades instead of failing because of what the list is *for*: one sentence of
/// documentation, and failing a build over a sentence would be the wrong trade.
/// The generator says "the roles are listed in the admin UI" and the project is
/// still written.
async fn documented_roles(cat: &Catalog) -> Result<Vec<sc_auth::Role>> {
    if cat.get(sc_auth::ROLES_TABLE)?.is_none() {
        return Ok(Vec::new());
    }
    sc_auth::list_roles(cat).await
}

/// Whether `app` is one this module may generate a project for.
///
/// Only `react` is: a `code` app's project is whatever the admin put in the store
/// — generating one over it is precisely the overwrite this module exists not to
/// do — and a build-less framework has no project at all.
pub fn require_scaffoldable(app: &Application) -> Result<()> {
    if app.framework.name == REACT_FRAMEWORK {
        return Ok(());
    }
    Err(Error::config(format!(
        "application `{}` uses framework `{}`, which brings its own project; \
         only `{REACT_FRAMEWORK}` is scaffolded",
        app.name, app.framework.name
    )))
}

/// A React app must enable at least one API provider.
///
/// Its whole data layer is generated from the app's endpoint set: no provider
/// means an empty `ApiClient`, and the login screen and every hook call methods
/// that do not exist. Left to run, that surfaces as a wall of TypeScript errors
/// in generated files — the real report that prompted this check had eleven, none
/// of which named the actual mistake, and all of which pointed at code the admin
/// never wrote.
///
/// So it is refused **before** anything is generated or installed, in the
/// admin's own vocabulary: the thing to fix is an empty APIs list on the
/// application, not a type error in `hooks.ts`.
pub fn require_api_provider(app: &Application) -> Result<()> {
    if !app.apis.is_empty() {
        return Ok(());
    }
    Err(Error::config(format!(
        "application `{}` enables no API provider, so it has no endpoints for its \
         React client to call — its pages could not read or write anything. Add an \
         API to the application (the `{}` provider mounted at `/api` is the usual \
         choice) and try again",
        app.name,
        sc_api::REST_PROVIDER
    )))
}

/// The generated file whose calls this check is about.
const AUTH_SOURCE: &str = "src/auth.tsx";

/// A build must not proceed when the project's auth layer calls endpoints the
/// application no longer exposes.
///
/// The scaffold's auth layer signs in through `login` / `logout` / `whoami`, and
/// only the REST provider projects them (§13.4). An app scaffolded with REST and
/// later left without it therefore has an `src/auth.tsx` calling three methods
/// that are no longer on the generated `ApiClient` — and since `tsc --noEmit`
/// type-checks every file in the project, that fails the build with four errors in
/// a file the admin never wrote and no mention of the actual cause.
///
/// So it is refused first, in the admin's own vocabulary — the same service
/// [`require_api_provider`] performs one provider along. The question asked is
/// about the **project's source**, not the app's providers, because an app that
/// never had an auth layer is not broken by not having one: a public API with a
/// public React client is a legitimate app, and it is what
/// [`has_auth`](files::has_auth) scaffolds when the endpoints are absent.
/// A project whose `auth.tsx` the admin has replaced with their own is likewise
/// theirs to get right.
async fn require_auth_endpoints_for_source(
    app: &Application,
    endpoints: &EndpointSet,
    store: &dyn sc_files::FileStore,
    project: &str,
) -> Result<()> {
    let missing: Vec<&str> = sc_api::AUTH_ENDPOINTS
        .into_iter()
        .filter(|name| endpoints.find(name).is_none())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    // Absent is the ordinary case for an app scaffolded without auth; unreadable
    // is a store problem the write that follows will report far better than a
    // check could.
    let auth_path = project_path(project, AUTH_SOURCE);
    let Ok(source) = store.read(&auth_path).await else {
        return Ok(());
    };
    let calls_them = missing
        .iter()
        .any(|name| String::from_utf8_lossy(&source).contains(&format!("api.{name}(")));
    if !calls_them {
        return Ok(());
    }
    Err(Error::config(format!(
        "application `{}` no longer exposes {}, which `{auth_path}` signs in \
         through — the build would fail type-checking a file it generated. Those \
         endpoints come from the `{}` provider; add it back to the application \
         (mounted at `/api` is the usual choice), or delete the app's auth layer if \
         it is meant to be usable without signing in",
        app.name,
        missing.join(" / "),
        sc_api::REST_PROVIDER,
    )))
}

/// Rewrite the generated runtime (`src/feldspar/`) for `app` — the typed client,
/// the hooks, and the GraphQL client and schema of an app that enables that
/// provider — leaving every other file alone.
///
/// Run on **every build**, which is what keeps the hooks and the client honest
/// when the app's tables change: adding a table in the admin UI makes
/// `useNewTable()` exist without anyone regenerating anything by hand. It is also
/// why the runtime is generated rather than shipped as a package (§2.1) — it is
/// shaped by this app's endpoints.
pub async fn emit_react_runtime(
    cat: &Catalog,
    app: &Application,
    source: &AppSource,
    dispatcher: Option<&std::sync::Arc<sc_action::TriggerDispatcher>>,
) -> Result<Vec<String>> {
    // Checked here as well as at scaffold time, because this is the path an
    // application saved before the check existed arrives on — and a build that
    // fails with the reason beats one that fails with its consequences.
    require_api_provider(app)?;
    let project = &source.build.source_dir;
    let store = cat.require_file_store(&source.store.0)?;
    let tables = app_tables(app, cat)?;
    let endpoints = app_endpoints_with(app, cat, dispatcher)?;
    require_auth_endpoints_for_source(app, &endpoints, store.as_ref(), project).await?;
    // Regenerated on every build for the same reason the hooks are: an admin who
    // enables the GraphQL provider, or adds a table to the app, gets a schema
    // describing what is actually mounted at the next build — with nobody
    // exporting an SDL by hand.
    let graphql = app_graphql(app, cat)?;
    // Rewritten on the same schedule and for the same reason as the client: the
    // tables it describes are the ones this app declares, and a column added in
    // the admin UI has to reach the file a coding agent writes SQL against.
    let schema_sql = app_schema_sql(app, cat)?;

    // Re-read on every build rather than cached, for the same reason the tables
    // are: an admin who adds a role gets a README that names it at the next
    // build, with nobody re-scaffolding anything.
    let roles = documented_roles(cat).await?;
    // The map of the half of the application this project cannot see (§13.6),
    // rewritten with the rest of the generated directory because the tool
    // surface it names is the one this server is currently offering.
    let skill = crate::generate_skill(cat, app, crate::REACT_CLIENT_FILE);

    let mut written = Vec::new();
    for file in files::runtime_files(&files::ProjectContext {
        project,
        app,
        tables: &tables,
        endpoints: &endpoints,
        graphql: graphql.as_ref(),
        schema_sql: &schema_sql,
        origin: cat.public_origin(),
        roles: &roles,
        skill: &skill,
    }) {
        let path = project_path(project, &file.path);
        store
            .write(&path, Bytes::from(file.contents.into_bytes()))
            .await
            .with_context(|| format!("generating {path}"))?;
        written.push(path);
    }
    Ok(written)
}

/// What [`update_app_client`] did — and it matters which, because the two
/// outcomes are very different news about the same button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientUpdate {
    /// The project directory was empty, so a whole project was written into it.
    /// An admin who pressed "update the generated client" and got a project back
    /// has to be told, not congratulated on a regeneration that did not happen.
    Scaffolded(ScaffoldReport),
    /// The generated files were rewritten, leaving everything else alone. The
    /// paths written, store-relative.
    Regenerated(Vec<String>),
}

impl ClientUpdate {
    /// A one-line summary for the admin's log, saying **which** of the two
    /// happened.
    pub fn summary(&self) -> String {
        match self {
            ClientUpdate::Scaffolded(report) => format!(
                "the project directory was empty, so it was scaffolded: {}",
                report.summary()
            ),
            ClientUpdate::Regenerated(files) if files.is_empty() => {
                "this application generates no client, so nothing was rewritten".to_owned()
            }
            ClientUpdate::Regenerated(files) => {
                format!("regenerated {}", files.join(", "))
            }
        }
    }

    /// The files written, whichever path was taken.
    pub fn files(&self) -> &[String] {
        match self {
            ClientUpdate::Scaffolded(report) => &report.files,
            ClientUpdate::Regenerated(files) => files,
        }
    }
}

/// Bring an application's generated code up to date on demand — the admin
/// screen's button, and the CLI's re-emit with one extra case handled.
///
/// Ordinarily this rewrites `src/feldspar/**` and nothing else
/// ([`emit_app_client`](crate::emit_app_client)). But a project directory that
/// is **empty** has nothing to rewrite: the app was created before its store was
/// reachable, or somebody deleted the tree. Re-emitting into it would leave a
/// `src/feldspar/` with no project around it — a directory of generated files
/// that cannot build — so an empty directory is scaffolded instead, through the
/// scaffold's own emptiness check rather than a second opinion about what
/// "empty" means.
///
/// Which of the two happened is in the return value rather than folded into a
/// single "done": scaffolding writes an entire project, and an admin must not
/// have to discover that by looking.
pub async fn update_app_client(
    cat: &Catalog,
    app: &Application,
    dispatcher: Option<&std::sync::Arc<sc_action::TriggerDispatcher>>,
) -> Result<ClientUpdate> {
    if require_scaffoldable(app).is_ok() {
        let source = app_source_from_config(&app.framework)?;
        let store = cat.require_file_store(&source.store.0)?;
        if is_empty_dir(store.as_ref(), &source.build.source_dir).await? {
            return scaffold_app(cat, app, dispatcher)
                .await
                .map(ClientUpdate::Scaffolded);
        }
    }
    crate::build::emit_app_client(cat, app, dispatcher)
        .await
        .map(ClientUpdate::Regenerated)
}

/// The repository's own directory, which is not project content — see
/// [`is_empty_dir`].
const GIT_DIR: &str = ".git";

/// Whether `dir` in `store` is absent or empty.
///
/// A missing directory and an empty one are the same answer to the only question
/// being asked — "is there anything here to lose?" — so a listing error for a
/// path that does not exist is not propagated. Any other listing failure is,
/// because it means the store could not answer.
///
/// `.git` does not count as content. The project directory *is* the store root
/// for an app whose store is its own repository (see `CFG_PROJECT`), and a clone
/// with no files in it still has a `.git` — treating that as "something to lose"
/// would refuse to scaffold into exactly the empty repository the feature is for,
/// while losing nothing the admin wrote.
async fn is_empty_dir(store: &dyn sc_files::FileStore, dir: &str) -> Result<bool> {
    match store.list(dir).await {
        Ok(entries) => Ok(entries.iter().all(|e| e.name == GIT_DIR)),
        // The store cannot distinguish "no such directory" in its error type; a
        // path that cannot be listed holds nothing this can destroy.
        Err(_) => Ok(true),
    }
}

/// `git init` in the project directory, so the app's source is a git repository
/// as §13.3 expects.
///
/// Only reached when the store is *not* itself a repo. A missing `git`, or a
/// `git` that fails, is **not** fatal: the project is written and buildable, and
/// failing the scaffold over version control would throw away the work that
/// succeeded. It is reported instead.
async fn init_git_repo(source: &AppSource, project_path: Option<&std::path::Path>) -> Result<bool> {
    let Some(path) = project_path else {
        // An object-store-backed app has no working tree to init; it also cannot
        // build (§13.3), which the build step says better than this could.
        return Ok(false);
    };
    let output = Command::new("git")
        .arg("init")
        .arg("--quiet")
        .current_dir(path)
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => Ok(true),
        Ok(out) => {
            eprintln!(
                "feldspar: `git init` in {} failed ({}): {}",
                path.display(),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            Ok(false)
        }
        Err(e) => {
            eprintln!(
                "feldspar: could not run `git init` in {} ({e}); \
                 the project was scaffolded without version control",
                path.display()
            );
            let _ = source;
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::{ApiConfig, FrameworkRef};
    use crate::framework::CFG_STORE;
    use crate::react::CFG_PROJECT;

    /// An app configured for `react`, project `todo`, in store `apps`.
    fn react_app() -> Application {
        Application::new(
            "Todo",
            "todo",
            FrameworkRef::new(REACT_FRAMEWORK)
                .with(CFG_STORE, "apps")
                .with(CFG_PROJECT, "todo"),
        )
        .with_api(ApiConfig::new("rest", "/api"))
    }

    #[test]
    fn a_report_summarises_what_an_admin_needs_to_see() {
        let report = ScaffoldReport {
            project: "todo".to_owned(),
            files: vec!["todo/package.json".to_owned()],
            git_initialized: true,
        };
        assert_eq!(
            report.summary(),
            "scaffolded 1 file into `todo` (git repository initialised)"
        );
        let report = ScaffoldReport {
            files: vec!["a".to_owned(), "b".to_owned()],
            git_initialized: false,
            ..report
        };
        assert_eq!(report.summary(), "scaffolded 2 files into `todo`");

        // A project at the store root has no directory to name, so the summary
        // says where it went rather than printing an empty pair of backticks.
        let root = ScaffoldReport {
            project: String::new(),
            ..report
        };
        assert_eq!(root.summary(), "scaffolded 2 files into the store root");
    }

    #[test]
    fn only_a_react_app_is_scaffolded() {
        assert!(require_scaffoldable(&react_app()).is_ok());

        let mut app = react_app();
        app.framework = FrameworkRef::new(crate::framework::CODE_FRAMEWORK);
        let err = require_scaffoldable(&app)
            .expect_err("only react scaffolds")
            .to_string();
        assert!(err.contains(REACT_FRAMEWORK), "{err}");
        assert!(err.contains("code"), "{err}");
        assert!(err.contains("Todo"), "should name the app: {err}");
    }
}
