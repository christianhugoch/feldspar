//! The [`Application`] model (design §13.2).
//!
//! Multiple applications share one data layer; each sees only the subset of
//! tables and file stores it declares, is served on its own subdomain by one
//! primary [`Framework`](crate::Framework), exposes any number of API providers
//! (each on a sub-path), and carries a [`CspPolicy`] that is **strict by
//! default**. This module owns the pure-data model; the behaviour of serving an
//! app lives behind the `Framework`/`ApiProvider` traits.

use std::collections::BTreeMap;

use sc_catalog::{FileStoreId, TableId};

/// Identifies an application within the server. Like the other MVP identifiers
/// (`TableId`, `FileStoreId`), a stable string — its subdomain-safe slug.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppId(pub String);

/// A reference to an application's primary (or extra) UI framework, resolved to a
/// concrete [`Framework`](crate::Framework) at mount time.
///
/// The reference is pure data — a framework `name` (`"code"`, `"saltcorn-v1"`, …)
/// plus free-form `config` — so an [`Application`] stays serialisable and
/// comparable while the runtime owns the trait object. Mirrors how an
/// [`Endpoint`](sc_api::Endpoint) names its handler rather than holding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameworkRef {
    /// The framework's registered name.
    pub name: String,
    /// Free-form framework configuration (e.g. the source/output sub-paths of a
    /// code framework's file store).
    pub config: BTreeMap<String, String>,
}

impl FrameworkRef {
    /// A reference to the framework registered under `name`, with no config.
    pub fn new(name: impl Into<String>) -> FrameworkRef {
        FrameworkRef {
            name: name.into(),
            config: BTreeMap::new(),
        }
    }

    /// Set a config key, returning `self` for chaining.
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> FrameworkRef {
        self.config.insert(key.into(), value.into());
        self
    }
}

/// One API provider enabled for an application, mounted on a sub-path (design
/// §13.4). The MVP ships a REST provider; the model carries the provider `name`
/// so GraphQL/gRPC/tRPC/MCP slot in later without a shape change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiConfig {
    /// The provider name (`"rest"`, `"graphql"`, …).
    pub provider: String,
    /// The sub-path the provider is mounted at within the application, e.g.
    /// `/api`. Normalised to a leading slash and no trailing slash.
    pub mount: String,
}

impl ApiConfig {
    /// A provider `provider` mounted at `mount` (the mount is normalised to a
    /// single leading slash with no trailing slash).
    pub fn new(provider: impl Into<String>, mount: impl Into<String>) -> ApiConfig {
        ApiConfig {
            provider: provider.into(),
            mount: normalize_mount(&mount.into()),
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
        self.directives.insert(
            name.into(),
            sources.into_iter().map(Into::into).collect(),
        );
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
    /// Stable identity.
    pub id: AppId,
    /// Human-readable name.
    pub name: String,
    /// The subdomain the app is served on (each app gets its own).
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
    /// Enabled API providers, each on a sub-path.
    pub apis: Vec<ApiConfig>,
    /// The content-security policy (strict by default).
    pub csp: CspPolicy,
}

impl Application {
    /// Start an application with the given id, name, subdomain and primary
    /// framework. Everything else defaults to empty; the CSP defaults to
    /// [`CspPolicy::strict`]. Use the builder setters to fill in the rest.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        subdomain: impl Into<String>,
        framework: FrameworkRef,
    ) -> Application {
        Application {
            id: AppId(id.into()),
            name: name.into(),
            subdomain: subdomain.into(),
            framework,
            extra_frameworks: Vec::new(),
            tables: Vec::new(),
            file_stores: Vec::new(),
            apis: Vec::new(),
            csp: CspPolicy::strict(),
        }
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

    /// Enable an API provider on a sub-path.
    pub fn with_api(mut self, api: ApiConfig) -> Application {
        self.apis.push(api);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_an_application_with_access_subset() {
        let app = Application::new(
            "blog",
            "My Blog",
            "blog",
            FrameworkRef::new("code").with("source", "web").with("output", "web/dist"),
        )
        .with_table(TableId("posts".to_owned()))
        .with_file_store(FileStoreId("uploads".to_owned()))
        .with_api(ApiConfig::new("rest", "api"));

        assert_eq!(app.id, AppId("blog".to_owned()));
        assert_eq!(app.subdomain, "blog");
        assert_eq!(app.framework.name, "code");
        assert_eq!(app.framework.config.get("output").unwrap(), "web/dist");

        // Access is limited to the declared subset.
        assert!(app.can_access_table(&TableId("posts".to_owned())));
        assert!(!app.can_access_table(&TableId("users".to_owned())));
        assert!(app.can_access_file_store(&FileStoreId("uploads".to_owned())));
        assert!(!app.can_access_file_store(&FileStoreId("secrets".to_owned())));

        // One REST provider, mount normalised with a leading slash.
        assert_eq!(app.apis.len(), 1);
        assert_eq!(app.apis[0].provider, "rest");
        assert_eq!(app.apis[0].mount, "/api");
    }

    #[test]
    fn mount_normalisation() {
        assert_eq!(ApiConfig::new("rest", "").mount, "/");
        assert_eq!(ApiConfig::new("rest", "/").mount, "/");
        assert_eq!(ApiConfig::new("rest", "api/").mount, "/api");
        assert_eq!(ApiConfig::new("rest", "/api/v1/").mount, "/api/v1");
    }

    #[test]
    fn csp_defaults_to_strict_and_renders() {
        let app = Application::new("a", "A", "a", FrameworkRef::new("code"));
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
