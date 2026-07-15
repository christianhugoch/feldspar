//! Serving applications from the Saltcorn process (design §13.2/§13.3).
//!
//! Each application is served on **its own subdomain** by its one primary
//! [`Framework`], with its API providers mounted on sub-paths beneath it. A
//! [`MountedApp`] is that triple — the [`Application`] record, the framework that
//! serves its UI, and the providers that serve its data — and [`AppMounts`] is
//! the registry the router resolves a request's `Host` against.
//!
//! **The app never touches the database.** Its framework serves static bundled
//! assets and nothing else; every byte of data it shows arrives through an
//! [`ApiProvider`], which enforces the app's declared table subset (§13.2) and
//! the §7 authorization layer. That is a structural guarantee, not a convention:
//! a [`Framework`] is handed a [`Catalog`] but [`CodeFramework`](sc_app::CodeFramework)
//! ignores it, and the app's own code is JavaScript in a browser with no route
//! to the database at all.

use std::collections::HashMap;
use std::sync::Arc;

use sc_api::ApiProvider;
use sc_app::{Application, Framework};
use sc_catalog::Catalog;
use sc_error::{Error, Result};

/// One application served by this process: its record, its UI framework, and its
/// API providers.
pub struct MountedApp {
    /// The application record — subdomain, table subset, CSP.
    pub app: Application,
    /// The framework serving its UI (a [`CodeFramework`](sc_app::CodeFramework)
    /// over a built bundle, for the MVP).
    pub framework: Arc<dyn Framework>,
    /// The API providers it enables, each on its own sub-path.
    pub providers: Vec<Box<dyn ApiProvider>>,
}

impl MountedApp {
    /// Mount `app`, served by `framework`, with the API providers it declares.
    ///
    /// The providers come from [`sc_app::app_providers`], so a mounted app's API
    /// is exactly the one its record declares and its generated client is typed
    /// against — there is no way to mount an API the app did not ask for.
    pub fn new(app: Application, framework: Arc<dyn Framework>, cat: &Catalog) -> Result<MountedApp> {
        let providers = sc_app::app_providers(&app, cat)?;
        Ok(MountedApp {
            app,
            framework,
            providers,
        })
    }

    /// The provider whose mount claims `path`, if any.
    ///
    /// The longest mount wins, so a provider at `/api/v2` takes precedence over
    /// one at `/api` for `/api/v2/posts` regardless of registration order.
    pub fn provider_for(&self, path: &str) -> Option<&dyn ApiProvider> {
        self.providers
            .iter()
            .filter(|p| path_under_mount(&p.mount(), path))
            .max_by_key(|p| p.mount().len())
            .map(AsRef::as_ref)
    }
}

/// The applications this server serves, resolved by subdomain.
///
/// Empty by default: a server with no apps mounted serves only the admin API and
/// its SPA, which is exactly the MVP's default deployment.
#[derive(Default)]
pub struct AppMounts {
    /// The catalog the providers run against. `None` only when no app is
    /// mounted; an app's API cannot run without it.
    catalog: Option<Arc<Catalog>>,
    /// Subdomain → the app served there.
    by_subdomain: HashMap<String, MountedApp>,
}

impl AppMounts {
    /// No applications — the admin-only server.
    pub fn none() -> AppMounts {
        AppMounts::default()
    }

    /// A registry whose apps run against `catalog`.
    pub fn new(catalog: Arc<Catalog>) -> AppMounts {
        AppMounts {
            catalog: Some(catalog),
            by_subdomain: HashMap::new(),
        }
    }

    /// Mount an app on its declared subdomain.
    ///
    /// Two apps may not claim the same subdomain — it is how a request is routed
    /// to one of them, so a collision is a configuration error rather than a
    /// last-one-wins surprise.
    pub fn mount(mut self, app: MountedApp) -> Result<AppMounts> {
        let subdomain = app.app.subdomain.clone();
        if self.by_subdomain.contains_key(&subdomain) {
            return Err(Error::config(format!(
                "two applications claim the subdomain `{subdomain}`"
            )));
        }
        self.by_subdomain.insert(subdomain, app);
        Ok(self)
    }

    /// The app served on `subdomain`.
    pub fn get(&self, subdomain: &str) -> Option<&MountedApp> {
        self.by_subdomain.get(subdomain)
    }

    /// The catalog the apps' providers run against.
    pub fn catalog(&self) -> Option<&Arc<Catalog>> {
        self.catalog.as_ref()
    }

    /// Whether any app is mounted.
    pub fn is_empty(&self) -> bool {
        self.by_subdomain.is_empty()
    }

    /// The mounted subdomains, sorted.
    pub fn subdomains(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.by_subdomain.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }
}

/// The application subdomain a `Host` names, given the server's base domain.
///
/// `blog.example.com` under base domain `example.com` is the app `blog`; the
/// base domain itself, a host under a different domain, or a deeper label
/// (`a.b.example.com`) is not an app — those fall through to the admin. The port
/// is ignored, so `blog.example.com:3000` resolves in local development.
///
/// Returns `None` when no base domain is configured: app routing is opt-in, and
/// guessing an app from an arbitrary `Host` header would let a request pick its
/// own app.
pub fn subdomain_of<'h>(host: &'h str, base_domain: Option<&str>) -> Option<&'h str> {
    let base = base_domain?;
    // Strip the port. An IPv6 literal host has no subdomain to find anyway, and
    // its colons would confuse this — but `[::1]` never matches a base domain.
    let host = host.split(':').next()?;
    let host = host.strip_suffix('.').unwrap_or(host); // tolerate a fully-qualified trailing dot
    let label = host.strip_suffix(base)?.strip_suffix('.')?;
    // Exactly one label: `blog` yes, `a.b` no.
    (!label.is_empty() && !label.contains('.')).then_some(label)
}

/// Whether `path` falls under a provider's `mount`.
///
/// `/api` claims `/api` and `/api/posts` but not `/apiary`; a mount of `/`
/// claims everything.
fn path_under_mount(mount: &str, path: &str) -> bool {
    if mount == "/" {
        return true;
    }
    path == mount || path.starts_with(&format!("{mount}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdomain_resolution() {
        let base = Some("example.com");
        assert_eq!(subdomain_of("blog.example.com", base), Some("blog"));
        // The port is ignored, so local development resolves.
        assert_eq!(subdomain_of("blog.example.com:3000", base), Some("blog"));
        // A fully-qualified name with a trailing dot is the same host.
        assert_eq!(subdomain_of("blog.example.com.", base), Some("blog"));

        // The base domain itself is not an app.
        assert_eq!(subdomain_of("example.com", base), None);
        // A deeper name is not a single app subdomain.
        assert_eq!(subdomain_of("a.b.example.com", base), None);
        // A different domain is not ours — notably one that merely *ends* with
        // the base domain's text.
        assert_eq!(subdomain_of("blog.notexample.com", base), None);
        assert_eq!(subdomain_of("evil.com", base), None);

        // Without a configured base domain, no host names an app: a request must
        // not be able to choose its own app via the Host header.
        assert_eq!(subdomain_of("blog.example.com", None), None);
    }

    #[test]
    fn mount_prefix_matching() {
        assert!(path_under_mount("/api", "/api"));
        assert!(path_under_mount("/api", "/api/posts"));
        // A prefix that is not a path boundary does not match.
        assert!(!path_under_mount("/api", "/apiary"));
        assert!(!path_under_mount("/api", "/other"));
        // A root mount claims everything.
        assert!(path_under_mount("/", "/anything/at/all"));
    }
}
