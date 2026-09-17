//! A module's **application frameworks**, as `sc-app`'s [`FrameworkHost`]
//! (design §13.3, §15.1).
//!
//! A v1 plugin has no such key — this is *this* system's, like `modelproviders`
//! — and it is what makes "an admin picks a framework" reach past the two
//! frameworks written in Rust. A module exports:
//!
//! ```js
//! frameworks: {
//!   vue: {
//!     label: "Vue",
//!     config_fields: [{ name: "store", type: "String", required: true }],
//!     build: { store: "{{ store }}", source: "{{ project }}",
//!              output: "{{ project }}/dist", command: "npm run build",
//!              runtime: "{{ project }}/src/feldspar", client: "client.ts" },
//!     scaffold: async (ctx) => [{ path: "package.json", contents: "…" }],
//!     runtime: async (ctx) => [{ path: `${ctx.runtime}/composables.ts`, contents: "…" }],
//!   },
//! }
//! ```
//!
//! and an application whose `_fd_applications` row says `framework = "vue"` is
//! served, built, scaffolded and configured through it — by the same code that
//! serves, builds, scaffolds and configures a `react` app, because what a
//! framework *is* to `sc-app` is a settings spec, a build spec, a CSP and a file
//! generator, and all four are things this translates.
//!
//! # Two halves, and why they are different shapes
//!
//! The **declaration** is read once, when the module loads, and installed as
//! values ([`FrameworkDecl`]). It has to be: `framework_config_spec` renders a
//! form, `app_source_from_config` resolves a build, and both are synchronous
//! calls on paths that have no worker and no `await` to spare. Nothing in the
//! declaration depends on run-time state, so nothing is lost by reading it early.
//!
//! The **generator** stays in the worker and is reached through
//! [`framework_files`](FrameworkHost::framework_files), because what it produces
//! depends on the application — its tables, its API surface, its roles — which
//! the plugin author did not know.
//!
//! # A broken declaration costs the framework, not the module
//!
//! A template that will not parse, a build with no command, a name a built-in
//! already has: each drops **that framework** and is reported on the module's
//! card ([`issues`](ModuleFrameworks::issues)). The module still loads and its
//! actions still run — the rule every other facility key here follows, for the
//! reason [`crate::spec`] gives: a module with one odd declaration is a module
//! with one odd declaration.

use std::sync::Arc;

use async_trait::async_trait;
use sc_app::{
    BuildTemplate, CspPolicy, DeclaredFile, FilePhase, FrameworkDecl, FrameworkHost, InstallSpec,
};
use sc_error::{Error, Result};
use sc_expr::Template;
use serde_json::Value as Json;

use crate::host::{FrameworkManifest, ModuleHost};
use crate::modules::ModuleSet;
use crate::spec::config_fields_to_form_fields;

/// The frameworks this server's JavaScript modules supply, over the pool they
/// run on.
pub struct ModuleFrameworks {
    host: Arc<ModuleHost>,
    frameworks: Vec<(String, FrameworkDecl)>,
    issues: Vec<String>,
}

impl ModuleFrameworks {
    /// Every framework of every loaded module in `set`, over `host`.
    ///
    /// A module that would not load supplies none: it has no manifest, and a
    /// framework nobody can build with is not one to offer in the application
    /// form.
    ///
    /// **First declaration of a name wins**, and the loser is reported naming
    /// both modules. Framework names share one namespace because an application
    /// stores one — the same rule, and the same reason, as two modules claiming
    /// one action name.
    pub fn new(host: &Arc<ModuleHost>, set: &ModuleSet) -> ModuleFrameworks {
        let mut frameworks: Vec<(String, FrameworkDecl)> = Vec::new();
        let mut issues = Vec::new();
        for loaded in set.modules() {
            let Some(manifest) = &loaded.manifest else {
                continue;
            };
            let module = loaded.module.name.clone();
            for framework in &manifest.frameworks {
                if let Some((other, _)) = frameworks.iter().find(|(_, f)| f.name == framework.name)
                {
                    issues.push(format!(
                        "the framework `{}` declared by {module} is not available: {other} \
                         already declares a framework of that name, and an application \
                         stores a framework by name",
                        framework.name
                    ));
                    continue;
                }
                match declaration(framework, &module) {
                    Ok(decl) => frameworks.push((module.clone(), decl)),
                    Err(e) => issues.push(format!(
                        "the framework `{}` declared by {module} is not available: {e}",
                        framework.name
                    )),
                }
            }
        }
        ModuleFrameworks {
            host: Arc::clone(host),
            frameworks,
            issues,
        }
    }

    /// An empty set — a server with no modules, and the starting point for a
    /// test that wants the seam without a worker.
    pub fn empty(host: &Arc<ModuleHost>) -> ModuleFrameworks {
        ModuleFrameworks {
            host: Arc::clone(host),
            frameworks: Vec::new(),
            issues: Vec::new(),
        }
    }

    /// What could not be translated, one sentence each.
    pub fn issues(&self) -> &[String] {
        &self.issues
    }

    /// Which module declared `name`, refusing a name this set does not have
    /// before a worker is reached.
    ///
    /// Re-checked here even though `sc-app` resolved the framework through this
    /// same list: a module can be deleted between an application form being
    /// rendered and its scaffold button being pressed, and the honest answer then
    /// is a sentence naming what went away rather than a worker's "not loaded in
    /// this host".
    fn require(&self, name: &str) -> Result<&str> {
        self.frameworks
            .iter()
            .find(|(_, f)| f.name == name)
            .map(|(module, _)| module.as_str())
            .ok_or_else(|| {
                Error::not_found(format!(
                    "no installed module supplies the framework `{name}`; it may have been \
                     uninstalled, or failed to load"
                ))
            })
    }
}

#[async_trait]
impl FrameworkHost for ModuleFrameworks {
    fn frameworks(&self) -> Vec<FrameworkDecl> {
        self.frameworks.iter().map(|(_, f)| f.clone()).collect()
    }

    async fn framework_files(
        &self,
        name: &str,
        phase: FilePhase,
        context: Json,
    ) -> Result<Vec<DeclaredFile>> {
        let module = self.require(name)?.to_owned();
        let answer = self
            .host
            .framework_files(&module, name, phase.as_str(), &context)
            .await?;
        let Json::Array(files) = answer else {
            return Err(Error::config(format!(
                "the {} of framework `{name}` of module {module} answered {answer}, which \
                 is not a list of files",
                phase.as_str()
            )));
        };
        files
            .iter()
            .map(|file| declared_file(file, name, &module, phase))
            .collect()
    }
}

/// One `{ path, contents }` answer.
///
/// Checked here as well as in the host script, because the two checks answer to
/// different owners: the script's protects the module author from their own
/// generator, and this one protects the file store from a host that is not this
/// server's script — a module worker is a process, and what comes back over a
/// pipe is data until it has been read.
fn declared_file(
    file: &Json,
    framework: &str,
    module: &str,
    phase: FilePhase,
) -> Result<DeclaredFile> {
    let bad = |what: &str| {
        Error::config(format!(
            "the {} of framework `{framework}` of module {module} answered a file {what}",
            phase.as_str()
        ))
    };
    let path = file
        .get("path")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| bad("with no path"))?;
    let contents = file
        .get("contents")
        .and_then(Json::as_str)
        .ok_or_else(|| bad(&format!("at `{path}` whose contents are not text")))?;
    Ok(DeclaredFile {
        path: path.to_owned(),
        contents: contents.to_owned(),
    })
}

/// Translate one manifest entry into the declaration `sc-app` installs.
fn declaration(manifest: &FrameworkManifest, module: &str) -> Result<FrameworkDecl> {
    let name = manifest.name.trim();
    if name.is_empty() {
        return Err(Error::invalid(
            "it has no name, and an application stores a framework by name".to_owned(),
        ));
    }
    // The issues from translating the fields are dropped here rather than
    // collected, for the reason the table providers drop theirs: they are already
    // the module's, the loader records them on its card, and reporting them again
    // on every application form would put a module's problem in front of an admin
    // creating an unrelated app.
    let (config_spec, _) =
        config_fields_to_form_fields(&manifest.config_fields, &format!("the framework `{name}`"));
    Ok(FrameworkDecl {
        name: name.to_owned(),
        module: module.to_owned(),
        label: manifest.label.trim().to_owned(),
        description: manifest.description.trim().to_owned(),
        build: build_template(&manifest.build, &config_spec)?,
        config_spec,
        csp: csp(&manifest.csp),
        builder_prompt: match manifest.builder_prompt.trim() {
            "" => None,
            prompt => Some(Template::parse(prompt)?),
        },
        checks: checks(&manifest.checks)?,
        scaffolds: manifest.scaffolds,
    })
}

/// The declared checks: script names, each once. Refused when the module loads,
/// for the reason a bad template is — the agent's save would refuse them later,
/// on an application form that is not the module author's.
fn checks(declared: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for name in declared.iter().map(|n| n.trim()) {
        if name.is_empty() {
            return Err(Error::invalid(
                "its `checks` lists an empty script name".to_owned(),
            ));
        }
        if out.iter().any(|n| n == name) {
            return Err(Error::invalid(format!("its `checks` lists `{name}` twice")));
        }
        out.push(name.to_owned());
    }
    Ok(out)
}

/// The `build` object: five templates, a command and an optional install step.
///
/// Every template is checked **here** against the framework's own settings, so a
/// declaration naming a setting it does not have is refused when the module loads
/// — on the module's card, where its author's name is — rather than at the moment
/// an admin presses Build.
fn build_template(build: &Json, spec: &[sc_types::FormField]) -> Result<BuildTemplate> {
    let optional = |key: &str| -> Result<Option<Template>> {
        match build.get(key).and_then(Json::as_str) {
            Some(source) => {
                let template = Template::parse(source)
                    .map_err(|e| Error::invalid(format!("its `{key}`: {e}")))?;
                for name in template.identifiers()? {
                    if !spec.iter().any(|f| f.name() == name) {
                        return Err(Error::invalid(format!(
                            "its `{key}` interpolates `{name}`, which is not one of its \
                             settings ({})",
                            settings_list(spec)
                        )));
                    }
                }
                Ok(Some(template))
            }
            None => Ok(None),
        }
    };
    // A path the framework must have: absent is a declaration that cannot resolve
    // to a directory, which is the same failure as one that will not parse.
    let required = |key: &str| -> Result<Template> {
        optional(key)?.ok_or_else(|| Error::invalid(format!("it declares no `{key}`")))
    };
    let command = build
        .get("command")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| Error::invalid("it declares no build `command`".to_owned()))?;
    Ok(BuildTemplate {
        store: required("store")?,
        source: required("source")?,
        output: required("output")?,
        command: command.to_owned(),
        install: install(build.get("install")),
        runtime: optional("runtime")?,
        client_file: build
            .get("client")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .unwrap_or(sc_app::REACT_CLIENT_FILE)
            .to_owned(),
    })
}

/// The settings a framework has, for an error message that must show the author
/// what they could have written.
fn settings_list(spec: &[sc_types::FormField]) -> String {
    if spec.is_empty() {
        return "it declares none".to_owned();
    }
    spec.iter()
        .map(|f| format!("`{}`", f.name()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `install: { command: "npm install", marker: "node_modules" }`.
///
/// Absent, or missing either half, means the framework does not manage the
/// project's dependencies — which is a legitimate declaration (`code`'s), and the
/// safe way to read a half-written one: a build that does not install is slower
/// to diagnose than one that does, but an install with no marker would run on
/// every single build.
fn install(declared: Option<&Json>) -> Option<InstallSpec> {
    let declared = declared?;
    let line = declared.get("command")?.as_str()?.trim();
    let marker = declared.get("marker")?.as_str()?.trim();
    if line.is_empty() || marker.is_empty() {
        return None;
    }
    let mut parts = line.split_whitespace().map(str::to_owned);
    Some(InstallSpec {
        command: parts.next()?,
        args: parts.collect(),
        marker: marker.to_owned(),
    })
}

/// `csp: { "img-src": ["'self'", "data:"] }` — the widenings, on top of the
/// strict baseline that `sc-app` applies.
///
/// A directive whose value is not a list of strings is dropped rather than
/// refused: the alternative is losing a whole framework over one malformed
/// directive, and what is lost instead is a widening — which fails *closed*, in
/// the direction a content-security policy should fail.
fn csp(declared: &Json) -> Option<CspPolicy> {
    let Json::Object(directives) = declared else {
        return None;
    };
    let mut policy = CspPolicy {
        directives: Default::default(),
    };
    for (directive, sources) in directives {
        let Json::Array(sources) = sources else {
            continue;
        };
        let sources: Vec<String> = sources
            .iter()
            .filter_map(Json::as_str)
            .map(str::to_owned)
            .collect();
        if !sources.is_empty() {
            policy.directives.insert(directive.clone(), sources);
        }
    }
    (!policy.directives.is_empty()).then_some(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(value: Json) -> FrameworkManifest {
        serde_json::from_value(value).expect("the test's own manifest deserialises")
    }

    fn vue() -> Json {
        json!({
            "name": "vue",
            "label": "Vue",
            "description": "A Vue 3 project.",
            "config_fields": [
                { "name": "store", "type": "String", "required": true },
                { "name": "project", "type": "String", "default": "" }
            ],
            "build": {
                "store": "{{ store }}",
                "source": "{{ project }}",
                "output": "{{ project }}/dist",
                "command": "npm run build",
                "install": { "command": "npm install", "marker": "node_modules" },
                "runtime": "{{ project }}/src/feldspar",
                "client": "client.ts"
            },
            "csp": { "img-src": ["'self'", "data:"] },
            "builder_prompt": "You maintain {{ app }}.",
            "checks": ["typecheck"],
            "scaffolds": true
        })
    }

    #[test]
    fn declared_checks_are_script_names_each_once() {
        let mut twice = vue();
        twice["checks"] = json!(["typecheck", " typecheck "]);
        let err = declaration(&manifest(twice), "@feldspar/vue").unwrap_err();
        assert!(err.to_string().contains("`typecheck` twice"), "{err}");
        let mut blank = vue();
        blank["checks"] = json!([""]);
        assert!(declaration(&manifest(blank), "@feldspar/vue").is_err());
        // And a framework that names none has none.
        let mut none = vue();
        none.as_object_mut().unwrap().remove("checks");
        assert!(
            declaration(&manifest(none), "@feldspar/vue")
                .unwrap()
                .checks
                .is_empty()
        );
    }

    #[test]
    fn a_declaration_becomes_the_registry_entry_sc_app_installs() {
        let decl = declaration(&manifest(vue()), "@feldspar/vue").unwrap();
        assert_eq!(decl.name, "vue");
        assert_eq!(decl.module, "@feldspar/vue");
        assert_eq!(decl.label, "Vue");
        assert!(decl.scaffolds);
        assert_eq!(decl.checks, ["typecheck"]);
        // The settings arrive through the same translation an action's do.
        let names: Vec<&str> = decl.config_spec.iter().map(|f| f.name()).collect();
        assert_eq!(names, ["store", "project"]);
        assert!(decl.config_spec[0].required);
        // And the paths are the templates, resolvable against those settings.
        let config = [
            ("store".to_owned(), json!("apps")),
            ("project".to_owned(), json!("todo")),
        ]
        .into_iter()
        .collect();
        assert_eq!(decl.store(&config).unwrap(), "apps");
        let spec = decl.build_spec(&config).unwrap();
        assert_eq!(spec.command, "npm");
        assert_eq!(spec.output_dir, "todo/dist");
        assert_eq!(
            spec.install.unwrap().marker,
            "node_modules",
            "the install step is read whole or not at all"
        );
        assert_eq!(
            decl.client_path(&config).unwrap().unwrap(),
            "todo/src/feldspar/client.ts"
        );
        assert_eq!(
            decl.default_csp().directives["img-src"],
            ["'self'", "data:"]
        );
    }

    #[test]
    fn a_template_naming_a_setting_the_framework_has_not_got_is_refused_on_load() {
        // The point of checking here: the author hears about it on the module's
        // card, not the admin at the moment they press Build.
        let mut broken = vue();
        broken["build"]["output"] = json!("{{ projekt }}/dist");
        let msg = declaration(&manifest(broken), "@feldspar/vue")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("projekt"), "{msg}");
        assert!(
            msg.contains("`store`") && msg.contains("`project`"),
            "{msg}"
        );
    }

    #[test]
    fn a_build_that_cannot_say_how_to_build_is_refused() {
        let mut broken = vue();
        broken["build"] = json!({ "store": "{{ store }}", "source": "{{ project }}" });
        let msg = declaration(&manifest(broken), "@feldspar/vue")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("command"), "{msg}");

        let mut broken = vue();
        broken["build"]["output"] = Json::Null;
        let msg = declaration(&manifest(broken), "@feldspar/vue")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("output"), "{msg}");
    }

    #[test]
    fn a_framework_that_declares_no_runtime_gets_no_generated_client() {
        let mut plain = vue();
        plain["build"]["runtime"] = Json::Null;
        let decl = declaration(&manifest(plain), "@feldspar/vue").unwrap();
        let config = [("store".to_owned(), json!("apps"))].into_iter().collect();
        assert_eq!(decl.runtime_dir(&config).unwrap(), None);
        assert_eq!(decl.client_path(&config).unwrap(), None);
    }

    #[test]
    fn a_malformed_csp_directive_is_dropped_rather_than_widening_anything() {
        let mut odd = vue();
        odd["csp"] = json!({ "img-src": "not a list", "connect-src": ["'self'"] });
        let decl = declaration(&manifest(odd), "@feldspar/vue").unwrap();
        let policy = decl.default_csp();
        assert!(!policy.directives.contains_key("img-src"));
        assert_eq!(policy.directives["connect-src"], ["'self'"]);
        assert_eq!(policy.directives["default-src"], ["'self'"]);
    }
}
