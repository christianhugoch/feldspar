//! Bringing **modules** up at boot, and keeping them up while the server runs
//! (TODO "Modules", phase 3).
//!
//! A module's actions are ordinary [`Action`](sc_action::Action)s in the
//! registry the dispatcher runs from, so installing one has to *change that
//! registry* — on a server that is already serving, without a restart, because
//! the Modules tab is where the module was installed from and a form that
//! appears to do nothing is the worst outcome available.
//!
//! [`ModuleServices`] is what makes that a single act:
//!
//! 1. rebuild the **base** registry (the built-ins plus the agent action) from
//!    scratch — never mutate the live one, which a firing trigger may be
//!    reading;
//! 2. load every stored module into it, collecting each one's issues;
//! 3. swap it into the dispatcher; and
//! 4. reload the trigger set against it, which is what turns a trigger that was
//!    broken ("unknown action `mqtt_publish`") back into a working one.
//!
//! Every step reports rather than fails. A module that will not install, will
//! not load or claims a name that is taken is carried in the set with its
//! reason; the server starts, and the admin can see and fix it.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use sc_action::TriggerDispatcher;
use sc_catalog::Catalog;
use sc_error::{Context, Error, Result};
use sc_module::{Installer, ModuleFunctions, ModuleHost, ModuleSet, bootstrap_modules};

use crate::agents::AgentServices;

/// The module machinery a running server holds: the npm project, the worker
/// pool modules run on, and the loaded set.
pub struct ModuleServices {
    catalog: Arc<Catalog>,
    dispatcher: Arc<TriggerDispatcher>,
    agents: AgentServices,
    installer: Installer,
    host: Arc<ModuleHost>,
    /// The loaded set, replaced whole by [`reload`](ModuleServices::reload).
    loaded: RwLock<Arc<ModuleSet>>,
}

impl ModuleServices {
    /// Bring the modules up: ensure the table, load every stored module, and
    /// swap the resulting action set into `dispatcher`.
    ///
    /// `root` is where packages are installed — `--modules-dir`, or the
    /// platform's data directory. Resolving it is **not** fatal when it fails
    /// and no module is installed: a server with no modules should not refuse to
    /// start because it could not work out where it would have put them.
    /// `workers` is how many module workers the pool runs (`--module-workers`):
    /// a module is pinned to one for its lifetime, so the reason to run a second
    /// is blast radius rather than throughput.
    pub async fn install(
        catalog: &Arc<Catalog>,
        dispatcher: &Arc<TriggerDispatcher>,
        agents: &AgentServices,
        root: Option<PathBuf>,
        workers: usize,
    ) -> Result<Arc<ModuleServices>> {
        bootstrap_modules(catalog)
            .await
            .context("ensuring the modules table exists")?;

        let root = match root {
            Some(root) => root,
            None => match sc_module::default_modules_root() {
                Ok(root) => root,
                Err(e) => {
                    // Report and carry on with a root nothing will be installed
                    // into: an install through the API will fail with the same
                    // message, in front of the admin who can act on it.
                    eprintln!("saltcorn: {}", sc_error::format_chain(&e));
                    PathBuf::from("modules")
                }
            },
        };

        let services = Arc::new(ModuleServices {
            catalog: Arc::clone(catalog),
            dispatcher: Arc::clone(dispatcher),
            agents: agents.clone(),
            installer: Installer::new(&root),
            host: Arc::new(ModuleHost::with_workers(&root, workers)),
            loaded: RwLock::new(Arc::new(ModuleSet::empty())),
        });
        services.reload().await?;
        for issue in services.modules().issues() {
            eprintln!(
                "saltcorn: module `{}` is installed but not fully usable: {}",
                issue.module, issue.problem
            );
        }
        Ok(services)
    }

    /// Rebuild the action set from the built-ins plus every stored module, swap
    /// it into the dispatcher, and reload the triggers against it.
    ///
    /// The one operation every module change goes through — install, configure,
    /// delete, and the Reload button — so there is one answer to "what happens
    /// to the live server", and no caller has to remember the four steps.
    pub async fn reload(&self) -> Result<()> {
        let mut registry = crate::triggers::base_action_registry(&self.agents)?;
        let set =
            ModuleSet::load(&self.catalog, &self.host, &self.installer, &mut registry).await?;
        self.dispatcher.set_registry(Arc::new(registry))?;
        // The functions the modules supply, on the catalog (§4a). Installed here
        // rather than beside the registry because they are not actions and their
        // callers are not the dispatcher: a formula hoists one through
        // `prefetch_bindings` and a code body calls one through `modfn`, and
        // what both of those hold is a `Catalog`.
        self.catalog
            .set_module_functions(Arc::new(ModuleFunctions::new(&self.host, &set)))?;
        // The trigger set is validated against the action registry, so a
        // trigger naming a module action was invalid until this moment.
        self.dispatcher
            .reload(&self.catalog)
            .await
            .context("reloading the triggers after a module change")?;
        match self.loaded.write() {
            Ok(mut guard) => *guard = Arc::new(set),
            Err(_) => return Err(Error::msg("module set lock poisoned")),
        }
        Ok(())
    }

    /// The loaded set as it stands — what the Modules tab renders.
    pub fn modules(&self) -> Arc<ModuleSet> {
        match self.loaded.read() {
            Ok(guard) => Arc::clone(&guard),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    /// The npm project modules are installed into.
    pub fn installer(&self) -> &Installer {
        &self.installer
    }

    /// The worker pool modules run on.
    pub fn host(&self) -> &Arc<ModuleHost> {
        &self.host
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_services_are_shareable_and_name_their_root() {
        // Nothing is spawned or read here — the host is lazy and the installer
        // is a path — so this is the one thing worth asserting without a
        // database: that the two handles point where the caller said.
        let installer = Installer::new("/srv/modules");
        assert_eq!(installer.root(), std::path::Path::new("/srv/modules"));
        let host = ModuleHost::new("/srv/modules");
        assert_eq!(host.root(), std::path::Path::new("/srv/modules"));
    }
}
