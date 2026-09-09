//! The [`Application`] model (design §13.2).
//!
//! Multiple applications share one data layer; each sees only the subset of
//! tables and file stores it declares, is served on its own subdomain by one
//! primary [`Framework`](crate::Framework), exposes any number of API providers
//! (each on a sub-path), serves any number of [`StaticDir`]s, and carries a
//! [`CspPolicy`] that is **strict by default**. This module owns the pure-data
//! model; the behaviour of serving an app lives behind the
//! `Framework`/`ApiProvider` traits, and the row it is stored as lives in
//! [`store`](crate::store).
//!
//! An application has nothing to introspect — unlike a table, it exists only as
//! stored configuration — so its `_fd_applications` row is its only definition
//! (§13.2). That is why [`AppId`] is a UUID (the §9 rule for stored metadata)
//! while `TableId`/`FileStoreId` remain names: an app's *identity* is its row,
//! and the human-facing key that must be unique is its
//! [`subdomain`](Application::subdomain), which is what routing dispatches on.

use std::collections::BTreeMap;

use sc_catalog::{Attrs, FileStoreId, TableId};
use serde_json::Value as Json;
use uuid::Uuid;

/// Identifies an application within the server: the UUID primary key of its
/// `_fd_applications` row (design §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AppId(pub Uuid);

impl AppId {
    /// Mint an id for a new application.
    pub fn new() -> AppId {
        AppId(Uuid::new_v4())
    }
}

impl Default for AppId {
    fn default() -> Self {
        AppId::new()
    }
}

impl std::fmt::Display for AppId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A reference to an application's primary (or extra) UI framework, resolved to a
/// concrete [`Framework`](crate::Framework) at mount time.
///
/// The reference is pure data — a framework `name` (`"code"`, `"saltcorn-v1"`, …)
/// plus its `config` — so an [`Application`] stays serialisable and comparable
/// while the runtime owns the trait object. Mirrors how an
/// [`Endpoint`](sc_api::Endpoint) names its handler rather than holding it.
///
/// `config` is [`Attrs`] (a JSON object) rather than a string map because it must
/// hold whatever that framework's `config_spec()` describes (§13.3) — booleans,
/// numbers and lists as well as strings — and because the admin UI renders its
/// form from that spec and posts the result back as JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameworkRef {
    /// The framework's registered name.
    pub name: String,
    /// Framework-specific configuration (e.g. the source/output sub-paths of a
    /// code framework's file store), validated against its `config_spec`.
    pub config: Attrs,
}

impl FrameworkRef {
    /// A reference to the framework registered under `name`, with no config.
    pub fn new(name: impl Into<String>) -> FrameworkRef {
        FrameworkRef {
            name: name.into(),
            config: Attrs::new(),
        }
    }

    /// Set a config key, returning `self` for chaining.
    pub fn with(mut self, key: impl Into<String>, value: impl Into<Json>) -> FrameworkRef {
        self.config.insert(key.into(), value.into());
        self
    }

    /// A config setting as a string, if present and textual.
    pub fn config_str(&self, key: &str) -> Option<&str> {
        self.config.get(key)?.as_str()
    }
}

/// A reference to a trigger the application exposes through its API (design
/// §10.2), by the trigger's unique **name**.
///
/// A name rather than the trigger's id, and deliberately: the name is what the
/// endpoint is built from (`POST {mount}/actions/{name}`) and what the generated
/// client's method is called (`runFoo`), so it is already the app's contract with
/// its own code. Referring by id would let a rename silently repoint a published
/// endpoint at a differently-named action; referring by name breaks the reference
/// visibly, which is what a rename *is*.
///
/// A newtype for the same reason [`TableId`] is one: an app's declaration is a
/// list of names, and a `Vec<String>` beside `Vec<TableId>` invites passing one
/// where the other belongs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TriggerRef(pub String);

impl TriggerRef {
    /// A reference to the trigger named `name`.
    pub fn new(name: impl Into<String>) -> TriggerRef {
        TriggerRef(name.into())
    }
}

impl std::fmt::Display for TriggerRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One API provider enabled for an application, mounted on a sub-path (design
/// §13.4). The MVP ships a REST provider; the model carries the provider `name`
/// so GraphQL/gRPC/tRPC/MCP slot in later without a shape change.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiConfig {
    /// The provider name (`"rest"`, `"graphql"`, …).
    pub provider: String,
    /// The sub-path the provider is mounted at within the application, e.g.
    /// `/api`. Normalised to a leading slash and no trailing slash.
    pub mount: String,
    /// Provider-specific configuration, validated on save against the spec the
    /// provider declares
    /// ([`api_provider_config_spec`](crate::api_provider_config_spec)).
    ///
    /// [`Attrs`] rather than a string map, and for the same reasons
    /// [`FrameworkRef::config`] is: GraphQL's aggregation switch is a boolean
    /// and its four bounds are numbers, and the admin form renders the
    /// provider's declared spec and posts the result back as JSON — with no
    /// provider-specific code in the form.
    pub config: Attrs,
}

impl ApiConfig {
    /// A provider `provider` mounted at `mount` (the mount is normalised to a
    /// single leading slash with no trailing slash), with no settings.
    pub fn new(provider: impl Into<String>, mount: impl Into<String>) -> ApiConfig {
        ApiConfig {
            provider: provider.into(),
            mount: normalize_mount(&mount.into()),
            config: Attrs::new(),
        }
    }

    /// Set a config key, returning `self` for chaining.
    pub fn with(mut self, key: impl Into<String>, value: impl Into<Json>) -> ApiConfig {
        self.config.insert(key.into(), value.into());
        self
    }

    /// Replace the whole config bag, returning `self` for chaining — how a
    /// caller that already has one (a provider's limits rendered back to
    /// settings) states it in one move.
    pub fn with_config(mut self, config: Attrs) -> ApiConfig {
        self.config = config;
        self
    }
}

/// A subdirectory of one of the app's file stores, served as static assets under
/// a sub-path of the app (design §13.2).
///
/// Deliberately separate from the framework's own bundle: the framework serves
/// the app's UI, a `StaticDir` serves files that are simply *there* — docs,
/// media, a downloads folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticDir {
    /// The sub-path within the app the directory is served at, e.g. `/docs`.
    /// Normalised like an [`ApiConfig`] mount.
    pub mount: String,
    /// The file store the directory lives in — which should be one the app
    /// declares access to.
    pub store: FileStoreId,
    /// The subdirectory within that store (`""` = the store's root).
    pub path: String,
}

impl StaticDir {
    /// Serve `path` within `store` at the app sub-path `mount`.
    pub fn new(mount: impl Into<String>, store: FileStoreId, path: impl Into<String>) -> StaticDir {
        StaticDir {
            mount: normalize_mount(&mount.into()),
            store,
            path: path.into(),
        }
    }
}

/// Normalise a mount/sub-path to a single leading slash and no trailing slash;
/// `""` and `"/"` both become `"/"`.
fn normalize_mount(raw: &str) -> String {
    let trimmed = raw.trim_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else {
        format!("/{trimmed}")
    }
}

/// A Content-Security-Policy for an application (design §12/§13.2). Strict by
/// default: because the admin/app UI is a bundled React SPA with no inline
/// scripts, CSP is satisfied structurally, so the default policy locks every
/// resource to the app's own origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CspPolicy {
    /// Directive name (`default-src`, `script-src`, …) → its source list. Order
    /// is stable so the rendered header is deterministic.
    pub directives: BTreeMap<String, Vec<String>>,
}

impl CspPolicy {
    /// The strict default: `default-src 'self'` — everything must come from the
    /// application's own origin.
    pub fn strict() -> CspPolicy {
        let mut directives = BTreeMap::new();
        directives.insert("default-src".to_owned(), vec!["'self'".to_owned()]);
        CspPolicy { directives }
    }

    /// Set (replacing) a directive's source list, returning `self` for chaining.
    pub fn directive(
        mut self,
        name: impl Into<String>,
        sources: impl IntoIterator<Item = impl Into<String>>,
    ) -> CspPolicy {
        self.directives
            .insert(name.into(), sources.into_iter().map(Into::into).collect());
        self
    }

    /// Render the `Content-Security-Policy` header value, e.g.
    /// `default-src 'self'; script-src 'self'`.
    pub fn header_value(&self) -> String {
        self.directives
            .iter()
            .map(|(name, sources)| {
                if sources.is_empty() {
                    name.clone()
                } else {
                    format!("{name} {}", sources.join(" "))
                }
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

impl Default for CspPolicy {
    fn default() -> Self {
        CspPolicy::strict()
    }
}

/// An application: a UI framework plus an access subset of the shared data layer,
/// served on its own subdomain (design §13.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Application {
    /// Stable identity: the UUID of its `_fd_applications` row (§9).
    pub id: AppId,
    /// Human-readable name.
    pub name: String,
    /// Human-readable description (§9 requires one on every metadata row; the
    /// empty string means "none given").
    pub description: String,
    /// The subdomain the app is served on — the unique routing key.
    pub subdomain: String,
    /// The one primary UI framework.
    pub framework: FrameworkRef,
    /// Additional frameworks the app may bring in (default stance: one primary
    /// framework per app; design Open Questions §18).
    pub extra_frameworks: Vec<FrameworkRef>,
    /// The subset of tables the app can access.
    pub tables: Vec<TableId>,
    /// The subset of file stores the app can access.
    pub file_stores: Vec<FileStoreId>,
    /// The triggers the app exposes as API endpoints (§10.2).
    ///
    /// An **opt-in subset**, exactly like [`tables`](Application::tables): a
    /// trigger is server-side configuration, and a server-side action becomes
    /// callable from outside only because an app said so. A trigger the app does
    /// not name has no endpoint, and a request for it is a 404 rather than a
    /// 403 — there is nothing there.
    pub triggers: Vec<TriggerRef>,
    /// Enabled API providers, each on a sub-path.
    pub apis: Vec<ApiConfig>,
    /// Statically-served store subdirectories, each on a sub-path.
    pub static_dirs: Vec<StaticDir>,
    /// The content-security policy (strict by default).
    pub csp: CspPolicy,
    /// Sparse per-app values that do not warrant a column of their own (the §9
    /// column-vs-attributes rule). A framework's settings do **not** live here —
    /// they belong to the framework, in [`FrameworkRef::config`].
    pub attributes: Attrs,
}

impl Application {
    /// Start a **new** application with the given name, subdomain and primary
    /// framework, minting a fresh [`AppId`].
    ///
    /// Everything else defaults to empty; the CSP defaults to
    /// [`CspPolicy::strict`]. Use the builder setters to fill in the rest. To
    /// reconstruct an *existing* application (which already has an id), use
    /// [`with_id`](Application::with_id).
    pub fn new(
        name: impl Into<String>,
        subdomain: impl Into<String>,
        framework: FrameworkRef,
    ) -> Application {
        Application {
            id: AppId::new(),
            name: name.into(),
            description: String::new(),
            subdomain: subdomain.into(),
            framework,
            extra_frameworks: Vec::new(),
            tables: Vec::new(),
            file_stores: Vec::new(),
            triggers: Vec::new(),
            apis: Vec::new(),
            static_dirs: Vec::new(),
            csp: CspPolicy::strict(),
            attributes: Attrs::new(),
        }
    }

    /// Set the id — for an application being reconstructed from its stored row
    /// rather than created.
    pub fn with_id(mut self, id: AppId) -> Application {
        self.id = id;
        self
    }

    /// Set the description.
    pub fn description(mut self, description: impl Into<String>) -> Application {
        self.description = description.into();
        self
    }

    /// Grant the app access to a table.
    pub fn with_table(mut self, table: TableId) -> Application {
        self.tables.push(table);
        self
    }

    /// Grant the app access to a file store.
    pub fn with_file_store(mut self, store: FileStoreId) -> Application {
        self.file_stores.push(store);
        self
    }

    /// Expose a trigger through the app's API.
    pub fn with_trigger(mut self, trigger: TriggerRef) -> Application {
        self.triggers.push(trigger);
        self
    }

    /// Enable an API provider on a sub-path.
    pub fn with_api(mut self, api: ApiConfig) -> Application {
        self.apis.push(api);
        self
    }

    /// Serve a store subdirectory on a sub-path.
    pub fn with_static_dir(mut self, dir: StaticDir) -> Application {
        self.static_dirs.push(dir);
        self
    }

    /// Set a sparse per-app attribute, returning `self` for chaining.
    pub fn attribute(mut self, key: impl Into<String>, value: impl Into<Json>) -> Application {
        self.attributes.insert(key.into(), value.into());
        self
    }

    /// Replace the CSP policy.
    pub fn csp(mut self, csp: CspPolicy) -> Application {
        self.csp = csp;
        self
    }

    /// Whether the app is permitted to access `table`.
    pub fn can_access_table(&self, table: &TableId) -> bool {
        self.tables.contains(table)
    }

    /// Whether the app is permitted to access `store`.
    pub fn can_access_file_store(&self, store: &FileStoreId) -> bool {
        self.file_stores.contains(store)
    }

    /// Whether the app exposes the trigger named `name`.
    pub fn exposes_trigger(&self, name: &str) -> bool {
        self.triggers.iter().any(|t| t.0 == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_an_application_with_access_subset() {
        let app = Application::new(
            "My Blog",
            "blog",
            FrameworkRef::new("code")
                .with("source", "web")
                .with("output", "web/dist"),
        )
        .description("The company blog")
        .with_table(TableId("posts".to_owned()))
        .with_file_store(FileStoreId("uploads".to_owned()))
        .with_trigger(TriggerRef::new("send_digest"))
        .with_api(ApiConfig::new("rest", "api"))
        .with_static_dir(StaticDir::new(
            "docs",
            FileStoreId("uploads".to_owned()),
            "handbook",
        ));

        assert_eq!(app.subdomain, "blog");
        assert_eq!(app.description, "The company blog");
        assert_eq!(app.framework.name, "code");
        assert_eq!(app.framework.config_str("output"), Some("web/dist"));

        // Access is limited to the declared subset.
        assert!(app.can_access_table(&TableId("posts".to_owned())));
        assert!(!app.can_access_table(&TableId("users".to_owned())));
        assert!(app.can_access_file_store(&FileStoreId("uploads".to_owned())));
        assert!(!app.can_access_file_store(&FileStoreId("secrets".to_owned())));
        // Triggers are the same opt-in subset: one is exposed, everything else
        // the server has configured is not.
        assert!(app.exposes_trigger("send_digest"));
        assert!(!app.exposes_trigger("purge_users"));

        // One REST provider, mount normalised with a leading slash.
        assert_eq!(app.apis.len(), 1);
        assert_eq!(app.apis[0].provider, "rest");
        assert_eq!(app.apis[0].mount, "/api");

        // One static dir, mount normalised the same way.
        assert_eq!(app.static_dirs.len(), 1);
        assert_eq!(app.static_dirs[0].mount, "/docs");
        assert_eq!(app.static_dirs[0].path, "handbook");
    }

    #[test]
    fn each_new_application_gets_its_own_id() {
        let a = Application::new("A", "a", FrameworkRef::new("code"));
        let b = Application::new("B", "b", FrameworkRef::new("code"));
        assert_ne!(a.id, b.id);

        // An app reconstructed from a row keeps the stored id.
        let restored = Application::new("A", "a", FrameworkRef::new("code")).with_id(a.id);
        assert_eq!(restored.id, a.id);
    }

    #[test]
    fn framework_config_holds_json_not_just_strings() {
        // The point of `Attrs` over a string map: a `config_spec` may describe a
        // bool or a number, and the admin UI posts it back as one (§13.3).
        let fw = FrameworkRef::new("code")
            .with("source", "web")
            .with("minify", true)
            .with("workers", 4);
        assert_eq!(fw.config_str("source"), Some("web"));
        assert_eq!(fw.config["minify"], Json::Bool(true));
        assert_eq!(fw.config["workers"], Json::from(4));
        // A non-string setting is not silently stringified.
        assert_eq!(fw.config_str("minify"), None);
    }

    #[test]
    fn mount_normalisation() {
        assert_eq!(ApiConfig::new("rest", "").mount, "/");
        assert_eq!(ApiConfig::new("rest", "/").mount, "/");
        assert_eq!(ApiConfig::new("rest", "api/").mount, "/api");
        assert_eq!(ApiConfig::new("rest", "/api/v1/").mount, "/api/v1");
        assert_eq!(
            StaticDir::new("docs/", FileStoreId("s".to_owned()), "d").mount,
            "/docs"
        );
    }

    #[test]
    fn csp_defaults_to_strict_and_renders() {
        let app = Application::new("A", "a", FrameworkRef::new("code"));
        assert_eq!(app.csp, CspPolicy::strict());
        assert_eq!(app.csp.header_value(), "default-src 'self'");

        let policy = CspPolicy::strict()
            .directive("script-src", ["'self'"])
            .directive("img-src", ["'self'", "data:"]);
        // Rendered deterministically in directive-name order.
        assert_eq!(
            policy.header_value(),
            "default-src 'self'; img-src 'self' data:; script-src 'self'"
        );
    }
}
