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
use std::sync::{Arc, RwLock};

use sc_api::ApiProvider;
use sc_app::{
    Application, CodeFramework, Framework, app_source_from_config, build_application,
    list_applications,
};
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
    pub fn new(
        app: Application,
        framework: Arc<dyn Framework>,
        cat: &Catalog,
    ) -> Result<MountedApp> {
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

/// The applications this server serves, resolved by subdomain — **live shared
/// state**, not a value frozen at router-build time (design §13.2 "the mount
/// registry is live; a full restart should never be required").
///
/// The registry is mutated in place through a shared handle: the boot path
/// [`mount_all`]s every stored app, and a later create/edit/delete
/// [`build_and_mount`]s or [`unmount`](AppMounts::unmount)s one app while the
/// server keeps serving the rest. The mounts live behind an `RwLock` so requests
/// read them concurrently and a mount/unmount briefly takes the write lock; each
/// [`MountedApp`] is an [`Arc`] so a reader clones its handle and drops the lock
/// rather than holding it across the request.
///
/// Empty by default: a server with no apps mounted serves only the admin API and
/// its SPA, which is exactly the MVP's default deployment.
#[derive(Default)]
pub struct AppMounts {
    /// The catalog the providers run against — and what the apps build against.
    /// `None` only for [`none`](AppMounts::none), the admin-only server that can
    /// never mount an app.
    catalog: Option<Arc<Catalog>>,
    /// Subdomain → the app served there. Behind an `RwLock` for live mutation.
    by_subdomain: RwLock<HashMap<String, Arc<MountedApp>>>,
}

impl AppMounts {
    /// No applications — the admin-only server.
    pub fn none() -> AppMounts {
        AppMounts::default()
    }

    /// A registry whose apps build and run against `catalog`.
    pub fn new(catalog: Arc<Catalog>) -> AppMounts {
        AppMounts {
            catalog: Some(catalog),
            by_subdomain: RwLock::new(HashMap::new()),
        }
    }

    /// Mount an app on its declared subdomain, refusing a collision.
    ///
    /// Two apps may not claim the same subdomain — it is how a request is routed
    /// to one of them, so a collision is a configuration error rather than a
    /// last-one-wins surprise. Use [`remount`](AppMounts::remount) to replace the
    /// app already on a subdomain (an edit or a rebuild).
    pub fn mount(&self, app: MountedApp) -> Result<()> {
        let subdomain = app.app.subdomain.clone();
        let mut mounts = self.write();
        if mounts.contains_key(&subdomain) {
            return Err(Error::config(format!(
                "two applications claim the subdomain `{subdomain}`"
            )));
        }
        mounts.insert(subdomain, Arc::new(app));
        Ok(())
    }

    /// Mount an app, **replacing** whatever was on its subdomain — the runtime
    /// re-mount an edit or a rebuild does.
    ///
    /// A request in flight against the previous mount keeps serving from the
    /// [`Arc`] it already cloned; the next request resolves the new one.
    pub fn remount(&self, app: MountedApp) {
        let subdomain = app.app.subdomain.clone();
        self.write().insert(subdomain, Arc::new(app));
    }

    /// Unmount the app on `subdomain`, so it stops resolving. Returns whether one
    /// was there to remove.
    pub fn unmount(&self, subdomain: &str) -> bool {
        self.write().remove(subdomain).is_some()
    }

    /// The app served on `subdomain`, as an [`Arc`] the caller holds after the
    /// read lock is released.
    pub fn get(&self, subdomain: &str) -> Option<Arc<MountedApp>> {
        self.read().get(subdomain).cloned()
    }

    /// The catalog the apps' providers run against.
    pub fn catalog(&self) -> Option<&Arc<Catalog>> {
        self.catalog.as_ref()
    }

    /// Whether any app is mounted.
    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    /// The mounted subdomains, sorted.
    pub fn subdomains(&self) -> Vec<String> {
        let mut names: Vec<String> = self.read().keys().cloned().collect();
        names.sort_unstable();
        names
    }

    /// Read the mounts, recovering from a poisoned lock: a panic while mounting
    /// must not take the whole registry down — the worst a torn write leaves is a
    /// stale entry, which the next mount overwrites.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, Arc<MountedApp>>> {
        self.by_subdomain.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Write the mounts, recovering from a poisoned lock (see [`read`](Self::read)).
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Arc<MountedApp>>> {
        self.by_subdomain.write().unwrap_or_else(|e| e.into_inner())
    }
}

/// Build an application from its stored configuration and mount it live on its
/// subdomain, with **no process restart** (design §13.2).
///
/// This is the runtime create/edit path: resolve the app's build step from its
/// framework config, run the bundler, and [`remount`](AppMounts::remount) the
/// resulting framework — replacing any earlier version on the same subdomain.
///
/// A **failed build leaves the previously mounted version serving**: the build
/// runs to completion before anything is mounted, so an `Err` here — carrying the
/// bundler's own diagnostics (§16) — never disturbs what is already up.
pub async fn build_and_mount(apps: &AppMounts, app: Application) -> Result<sc_app::BuildReport> {
    let catalog = apps.catalog().ok_or_else(|| {
        Error::config("this server was built with no catalog, so it cannot mount applications")
    })?;
    let source = app_source_from_config(&app.framework)?;
    let report = build_application(catalog, &app, &source).await?;
    let framework = Arc::new(CodeFramework::new(
        app.framework.name.clone(),
        report.bundle.clone(),
    ));
    let mounted = MountedApp::new(app, framework, catalog)?;
    apps.remount(mounted);
    Ok(report)
}

/// Load every stored application and build + mount each — what the server does at
/// boot (design §13.2).
///
/// **A single app that fails to build must not stop the server or the other
/// apps**, so a per-app failure is logged and skipped rather than propagated: the
/// operator gets a running server with the apps that built and a clear line about
/// the one that did not, which they can fix and rebuild without a restart.
pub async fn mount_all(apps: &AppMounts) {
    let catalog = match apps.catalog() {
        Some(catalog) => catalog,
        // An admin-only server has nothing to mount.
        None => return,
    };
    let stored = match list_applications(catalog).await {
        Ok(apps) => apps,
        Err(e) => {
            eprintln!("saltcorn: could not load applications to mount: {e}");
            return;
        }
    };
    for app in stored {
        let subdomain = app.subdomain.clone();
        match build_and_mount(apps, app).await {
            Ok(_) => eprintln!("saltcorn: mounted application `{subdomain}`"),
            Err(e) => {
                eprintln!("saltcorn: application `{subdomain}` failed to build, skipping: {e}")
            }
        }
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
