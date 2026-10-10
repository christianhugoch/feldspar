//! **Framework factories**: a framework written in Rust *outside* this crate,
//! mounted without this crate depending on it (TODO "Saltcorn UI" 5.2).
//!
//! `react` and `code` are compiled into the registry in [`crate::framework`]; a
//! module's framework is a [`FrameworkDecl`](crate::FrameworkDecl), data that
//! builds into a static bundle. Saltcorn UI is neither: it is Rust, it lives in
//! `sc-viewpattern` — a crate *above* this one, because it renders through the
//! module worker — and it has no build step at all, so there is no bundle for
//! [`CodeFramework`](crate::CodeFramework) to serve. What it needs from the
//! registry is a **named constructor**: the answers the admin UI asks by name
//! (label, settings, default CSP), plus "mount this application".
//!
//! One factory is compiled in rather than installed: [`none`](crate::NONE_FRAMEWORK),
//! the framework with no UI and no build, which is constructed for the same
//! reason Saltcorn UI is — there is nothing to build — and so goes through the
//! same seam.
//!
//! The registry is shaped like [`installed_frameworks`](crate::installed_frameworks):
//! a process-wide set installed at boot, asked by name. It is consulted after the
//! two built-ins and before the modules' declarations, so a module cannot declare
//! a framework over a compiled one.

use std::path::Path;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_types::{Attrs, FormField};

use crate::application::{Application, CspPolicy};
use crate::framework::{Framework, FrameworkInfo};

/// What a factory is handed to mount one application: the server's shared
/// services, which a mounted framework keeps for the life of the mount.
pub struct MountContext<'a> {
    /// The catalog the application's data is read through.
    pub catalog: &'a Arc<Catalog>,
    /// The formula engine, for ownership rules the translator cannot lower.
    pub evaluator: Option<Arc<dyn sc_expr::JsEvaluator>>,
    /// The trigger dispatcher, so what the framework runs can run triggers.
    pub triggers: Option<Arc<sc_action::TriggerDispatcher>>,
    /// Where this binary's bundle for the framework is, if it was built with
    /// one. `None` is a build without it, and a factory that needs one says so.
    pub bundle_dir: Option<&'a Path>,
}

/// A framework that is constructed rather than built.
#[async_trait]
pub trait FrameworkFactory: Send + Sync {
    /// How it presents itself in the picker; `info().name` is the registry key.
    fn info(&self) -> FrameworkInfo;

    /// Its settings, in the one [`FormField`] vocabulary.
    fn config_spec(&self) -> Vec<FormField>;

    /// The policy an application of it gets when the admin states none.
    fn default_csp(&self) -> CspPolicy;

    /// A check the spec vocabulary cannot state, run on both validation paths.
    fn check_config(&self, _config: &Attrs) -> Result<()> {
        Ok(())
    }

    /// A check that needs the database as it is now — that a setting names
    /// something that exists — run on save only
    /// ([`validate_framework_config`](crate::validate_framework_config)), the
    /// way a server-query setting's options are: whether it exists is settled
    /// where the admin can fix it, and a build or a mount does not re-ask.
    async fn check_config_against(&self, _catalog: &Catalog, _config: &Attrs) -> Result<()> {
        Ok(())
    }

    /// Mount `app`: construct the framework that serves it.
    async fn mount(&self, app: &Application, ctx: MountContext<'_>) -> Result<Arc<dyn Framework>>;
}

/// The installed factories, in installation order.
static INSTALLED: RwLock<Vec<Arc<dyn FrameworkFactory>>> = RwLock::new(Vec::new());

/// Install `factory`, replacing an installed one of the same name. Installing
/// the same factory twice is therefore harmless, which is what lets every boot
/// path (and every test server) install it without coordinating.
pub fn install_framework_factory(factory: Arc<dyn FrameworkFactory>) -> Result<()> {
    let name = factory.info().name;
    let mut guard = INSTALLED
        .write()
        .map_err(|_| Error::msg("the framework factory registry lock is poisoned"))?;
    match guard.iter_mut().find(|f| f.info().name == name) {
        Some(slot) => *slot = factory,
        None => guard.push(factory),
    }
    Ok(())
}

/// Every installed factory, in installation order, then the ones compiled into
/// this crate ([`NONE_FRAMEWORK`](crate::NONE_FRAMEWORK)).
///
/// The compiled ones come last because they are the least-common choice in the
/// picker, and an installed factory of the same name would be a mistake the
/// compiled one should not be displaced by — so it is dropped instead.
pub fn framework_factories() -> Vec<Arc<dyn FrameworkFactory>> {
    let mut factories = match INSTALLED.read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    factories.retain(|f| f.info().name != crate::none::NONE_FRAMEWORK);
    factories.push(Arc::new(crate::none::NoneFactory));
    factories
}

/// The factory installed under `name`, if any.
pub fn framework_factory(name: &str) -> Option<Arc<dyn FrameworkFactory>> {
    framework_factories()
        .into_iter()
        .find(|f| f.info().name == name)
}
