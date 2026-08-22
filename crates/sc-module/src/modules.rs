//! The loaded module set: every stored module, what it supplies, and what is
//! wrong with it.
//!
//! This is what a server holds between the store (rows) and the registry
//! (actions). Loading is **reported, never fatal**: a module whose package is
//! missing, whose code throws at load, or whose action name is already taken is
//! carried in the set with its reason so the Modules tab can show it, and every
//! other module still works. A server that refused to start because one module
//! was broken would be a server nobody could fix from the admin UI — which is
//! the one place the module was installed from.

use std::sync::Arc;

use sc_action::ActionRegistry;
use sc_catalog::Catalog;
use sc_error::Result;
use sc_types::{Attrs, FormField};
use serde_json::{Value as Json, json};

use crate::action::ModuleAction;
use crate::host::{ModuleHost, ModuleManifest};
use crate::install::Installer;
use crate::module::Module;
use crate::spec::config_fields_to_form_fields;
use crate::store::list_modules;

/// Something wrong with a module that did not stop the rest of the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleIssue {
    /// The module it is about.
    pub module: String,
    /// What is wrong, in a sentence an admin can act on.
    pub problem: String,
}

/// One module as it stands: its row, what its package says it supplies, and its
/// issues.
#[derive(Debug, Clone)]
pub struct LoadedModule {
    /// The stored row.
    pub module: Module,
    /// What the package turned out to supply, or `None` if it would not load.
    pub manifest: Option<ModuleManifest>,
    /// The module's own settings, translated from its `configuration_workflow`.
    pub config_spec: Vec<FormField>,
    /// Everything wrong with it.
    pub issues: Vec<String>,
}

impl LoadedModule {
    /// The action names this module contributed to the registry.
    pub fn action_names(&self) -> Vec<String> {
        self.manifest
            .as_ref()
            .map(|m| m.actions.iter().map(|a| a.name.clone()).collect())
            .unwrap_or_default()
    }

    /// Whether the module is loaded and contributing.
    pub fn is_loaded(&self) -> bool {
        self.manifest.is_some()
    }
}

/// Every installed module, loaded.
pub struct ModuleSet {
    modules: Vec<LoadedModule>,
}

impl ModuleSet {
    /// An empty set — a server with no modules installed, and the starting point
    /// for a test.
    pub fn empty() -> ModuleSet {
        ModuleSet {
            modules: Vec::new(),
        }
    }

    /// Load every stored module into `host`, and register the actions they
    /// supply into `registry`.
    ///
    /// The registry is the caller's, already carrying the built-ins, because
    /// **a module must not be able to displace a built-in**: registration
    /// refuses a duplicate name (that is `ActionRegistry`'s rule, for exactly
    /// this reason), and the refusal becomes the module's issue rather than an
    /// error here.
    pub async fn load(
        catalog: &Catalog,
        host: &Arc<ModuleHost>,
        installer: &Installer,
        registry: &mut ActionRegistry,
    ) -> Result<ModuleSet> {
        let stored = list_modules(catalog).await?;
        let mut modules = Vec::with_capacity(stored.len());
        for module in stored {
            modules.push(load_one(&module, host, installer, registry).await);
        }
        Ok(ModuleSet { modules })
    }

    /// The loaded modules, in name order.
    pub fn modules(&self) -> &[LoadedModule] {
        &self.modules
    }

    /// One module by package name.
    pub fn get(&self, name: &str) -> Option<&LoadedModule> {
        self.modules.iter().find(|m| m.module.name == name)
    }

    /// Every issue across every module — what a boot logs and what the Modules
    /// tab shows in red.
    pub fn issues(&self) -> Vec<ModuleIssue> {
        self.modules
            .iter()
            .flat_map(|loaded| {
                loaded.issues.iter().map(|problem| ModuleIssue {
                    module: loaded.module.name.clone(),
                    problem: problem.clone(),
                })
            })
            .collect()
    }
}

/// Load one module and register its actions, collecting everything that went
/// wrong instead of returning it.
async fn load_one(
    module: &Module,
    host: &Arc<ModuleHost>,
    installer: &Installer,
    registry: &mut ActionRegistry,
) -> LoadedModule {
    let mut issues = Vec::new();
    let dir = installer.package_dir(&module.name);
    if !installer.is_installed(&module.name) {
        issues.push(format!(
            "its package is not installed at {} — reinstall it from Settings → Modules",
            dir.display()
        ));
        return LoadedModule {
            module: module.clone(),
            manifest: None,
            config_spec: Vec::new(),
            issues,
        };
    }

    let configuration = Json::Object(module.configuration.clone());
    let manifest = match host.load(&module.name, &dir, &configuration).await {
        Ok(manifest) => manifest,
        Err(e) => {
            issues.push(format!("it did not load: {}", sc_error::format_chain(&e)));
            return LoadedModule {
                module: module.clone(),
                manifest: None,
                config_spec: Vec::new(),
                issues,
            };
        }
    };
    issues.extend(manifest.issues.iter().cloned());

    let (config_spec, spec_issues) =
        config_fields_to_form_fields(&manifest.config_fields, "this module");
    issues.extend(spec_issues);

    for action in &manifest.actions {
        let (spec, action_issues) = config_fields_to_form_fields(
            &action.config_fields,
            &format!("the action `{}`", action.name),
        );
        issues.extend(action_issues);
        let registered = ModuleAction::new(
            &module.name,
            &action.name,
            &action.description,
            spec,
            Arc::clone(host),
        );
        if let Err(e) = registry.register(Arc::new(registered)) {
            // The built-in — or the module that got there first — keeps the
            // name. Which implementation answers to `insert_row` must not
            // depend on the order modules were installed in.
            issues.push(format!(
                "its action `{}` is not available: {e}",
                action.name
            ));
        }
    }

    LoadedModule {
        module: module.clone(),
        manifest: Some(manifest),
        config_spec,
        issues,
    }
}

/// A module's configuration, redacted for the wire: every `secret` field
/// replaced by the sentinel (§11.1).
pub fn redacted_configuration(loaded: &LoadedModule) -> Attrs {
    sc_types::redact_attrs(&loaded.config_spec, &loaded.module.configuration)
}

/// A module's unsupported-entity census, as the API reports it.
pub fn unsupported_json(loaded: &LoadedModule) -> Vec<Json> {
    loaded
        .manifest
        .as_ref()
        .map(|manifest| {
            manifest
                .unsupported
                .iter()
                .map(|entity| json!({ "key": entity.key, "count": entity.count }))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{ActionManifest, UnsupportedEntity};
    use crate::module::ModuleSource;

    fn loaded_with(manifest: Option<ModuleManifest>, issues: Vec<String>) -> LoadedModule {
        LoadedModule {
            module: Module::new("@saltcorn/mqtt", ModuleSource::Npm, "@saltcorn/mqtt"),
            manifest,
            config_spec: Vec::new(),
            issues,
        }
    }

    fn manifest() -> ModuleManifest {
        ModuleManifest {
            name: "@saltcorn/mqtt".into(),
            api_version: Some(1),
            plugin_name: None,
            actions: vec![ActionManifest {
                name: "mqtt_publish".into(),
                description: String::new(),
                require_row: false,
                config_fields: Vec::new(),
            }],
            config_fields: Vec::new(),
            unsupported: vec![UnsupportedEntity {
                key: "eventTypes".into(),
                count: Some(1),
            }],
            issues: Vec::new(),
        }
    }

    #[test]
    fn a_loaded_module_reports_the_actions_it_supplies() {
        let loaded = loaded_with(Some(manifest()), Vec::new());
        assert!(loaded.is_loaded());
        assert_eq!(loaded.action_names(), vec!["mqtt_publish".to_owned()]);
    }

    #[test]
    fn a_module_that_did_not_load_supplies_nothing_and_says_why() {
        let loaded = loaded_with(None, vec!["its package is not installed".into()]);
        assert!(!loaded.is_loaded());
        assert!(loaded.action_names().is_empty());
        let set = ModuleSet {
            modules: vec![loaded],
        };
        let issues = set.issues();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].module, "@saltcorn/mqtt");
        assert!(issues[0].problem.contains("not installed"));
    }

    #[test]
    fn the_unsupported_census_reaches_the_wire() {
        let loaded = loaded_with(Some(manifest()), Vec::new());
        let census = unsupported_json(&loaded);
        assert_eq!(census.len(), 1);
        assert_eq!(census[0]["key"], json!("eventTypes"));
        assert_eq!(census[0]["count"], json!(1));
    }

    #[test]
    fn an_empty_set_has_nothing_to_say() {
        let set = ModuleSet::empty();
        assert!(set.modules().is_empty());
        assert!(set.issues().is_empty());
        assert!(set.get("@saltcorn/mqtt").is_none());
    }
}
