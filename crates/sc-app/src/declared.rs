//! **Declared frameworks**: the frameworks a module supplies, as data.
//!
//! `react` and `code` are frameworks written in Rust: each is a handful of
//! functions the registry in [`crate::framework`] switches a name onto. A
//! framework supplied by a JavaScript module (§15.1's rule that every extension
//! point but a database driver may be implemented in a guest language) cannot be
//! that, because there is no Rust to call — so what a module supplies instead is
//! this: a [`FrameworkDecl`], which is the same seven answers written down.
//!
//! # Why the declaration is data and not a call
//!
//! Every question the admin UI asks a framework is asked **synchronously and
//! often**: rendering the application form asks for its settings, saving an
//! application validates against them, serving one asks for its default CSP, and
//! building one resolves its source directory. A module lives on a worker and is
//! reached by an awaited round trip, so a framework whose settings were a *call*
//! would make `framework_config_spec` async and take a worker with it — into
//! `app_source_from_config`, into `framework_builder_agent`, and from there into
//! most of the server.
//!
//! It does not need to be. Those seven answers do not depend on anything the
//! module learns at run time: they are what the plugin author wrote. So they
//! cross **once, when the module loads**, land in its manifest, and are installed
//! here as values. The one answer that genuinely is a computation — the project
//! files a scaffold writes — stays a call, on the path that was already async and
//! already had a worker ([`FrameworkHost`]).
//!
//! # Paths are templates
//!
//! A framework's source, output, runtime directory and builder prompt are
//! [`Template`]s over its own settings — `{{ project }}/dist` — parsed by
//! `sc-expr`'s one template parser, the same one an email subject and a trigger's
//! `only_if` go through. Rendered in a flat scope ([`Template::render_static`]),
//! because the names in scope are settings rather than a row's columns.
//!
//! **A blank setting collapses its path segment** ([`clean_path`]). `react`'s
//! rule that "blank means the store root" is not a `react` rule but a property of
//! deriving paths from a project name, so a declared framework gets it too, in
//! the one place it lives here.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use sc_error::{Error, Result};
use sc_expr::Template;
use sc_types::{Attrs, FormField};
use serde_json::Value as Json;

use crate::application::CspPolicy;
use crate::framework::{BuildSpec, FrameworkInfo, InstallSpec};

/// The `store` setting of a declared framework's [`BuildTemplate`] — a template
/// naming which of the framework's own settings holds the file store.
///
/// It is a template rather than a settings *name* so that the whole
/// [`BuildTemplate`] reads in one vocabulary: every other field of it is a
/// template, and a lone bare name in the middle would be a second rule to
/// remember. `"{{ store }}"` is what a framework writes.
pub type PathTemplate = Template;

/// Where a declared framework's source lives and how it builds — the
/// [`BuildSpec`] and the client path, before its settings are known.
#[derive(Debug, Clone, PartialEq)]
pub struct BuildTemplate {
    /// Which setting names the file store the project lives in.
    pub store: PathTemplate,
    /// The source directory, relative to the store.
    pub source: PathTemplate,
    /// The directory the bundler emits into, relative to the store.
    pub output: PathTemplate,
    /// The bundler command line, split on whitespace as the `code` framework's
    /// `command` setting is.
    pub command: String,
    /// The dependency install step, when the framework manages dependencies.
    pub install: Option<InstallSpec>,
    /// Where the generated runtime is written, relative to the store —
    /// `{{ project }}/src/feldspar`. `None` for a framework that consumes no
    /// generated code, which is also a framework that gets no typed client.
    pub runtime: Option<PathTemplate>,
    /// The generated client's file name **within** the runtime directory —
    /// `client.ts`. Only meaningful with a `runtime`.
    pub client_file: String,
}

/// A framework a module supplies: everything the registry in
/// [`crate::framework`] answers for `react`, written down instead of compiled.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameworkDecl {
    /// The registry key and what an application's `FrameworkRef` stores —
    /// `vue`. Unqualified and sharing one namespace with the built-ins and with
    /// every other module's, exactly as module actions and functions do.
    pub name: String,
    /// The package that declared it. Not part of the key — it is what an error
    /// message names when two modules want the same one.
    pub module: String,
    /// The human name the framework picker shows.
    pub label: String,
    /// One sentence: what this framework does for the admin, and what it asks of
    /// them in return.
    pub description: String,
    /// Its settings, in the one [`FormField`] vocabulary every configurable
    /// thing here speaks.
    pub config_spec: Vec<FormField>,
    /// Where its source is and how it builds.
    pub build: BuildTemplate,
    /// The widenings its output needs on top of [`CspPolicy::strict`], or
    /// `None` for a framework that did not say and therefore gets the baseline.
    pub csp: Option<CspPolicy>,
    /// The system prompt of the coding agent that builds an application of this
    /// framework, as a template. `None` for a framework that declares no builder
    /// agent.
    pub builder_prompt: Option<Template>,
    /// Whether the module answers [`FrameworkHost::framework_files`] for the
    /// scaffold phase — i.e. whether an application of this framework has a
    /// project Saltcorn writes, or one the admin brought.
    pub scaffolds: bool,
}

impl FrameworkDecl {
    /// How it presents itself in the framework picker.
    pub fn info(&self) -> FrameworkInfo {
        FrameworkInfo {
            name: self.name.clone(),
            label: if self.label.trim().is_empty() {
                self.name.clone()
            } else {
                self.label.clone()
            },
            description: self.description.clone(),
            // A declared framework serves a built bundle on the app's own paths;
            // there is no headless one to declare, and assuming there is a UI to
            // protect is the safe direction to be wrong in anyway (see
            // `framework_serves_ui`).
            serves_ui: true,
        }
    }

    /// The default CSP for an app of this framework: the strict baseline plus
    /// whatever the framework said its output needs.
    pub fn default_csp(&self) -> CspPolicy {
        match &self.csp {
            Some(csp) => {
                let mut policy = CspPolicy::strict();
                for (directive, sources) in &csp.directives {
                    policy = policy.directive(directive.clone(), sources.clone());
                }
                policy
            }
            None => CspPolicy::strict(),
        }
    }

    /// The values its templates are rendered against: every setting's resolved
    /// value, as text.
    ///
    /// Resolved through the spec, so a setting the admin left blank renders as
    /// the framework's own default rather than as an unbound name.
    fn bindings(&self, config: &Attrs) -> BTreeMap<String, String> {
        self.config_spec
            .iter()
            .map(|field| {
                let value = match field.resolve(config) {
                    None | Some(Json::Null) => String::new(),
                    Some(Json::String(s)) => s.trim().to_owned(),
                    Some(other) => other.to_string(),
                };
                (field.name().to_owned(), value)
            })
            .collect()
    }

    /// The file store an application of this framework keeps its source in.
    pub fn store(&self, config: &Attrs) -> Result<String> {
        let store = self
            .build
            .store
            .render_static(&self.bindings(config))
            .map_err(|e| self.blame(e))?;
        let store = store.trim();
        if store.is_empty() {
            return Err(Error::invalid(format!(
                "framework `{}` resolves its file store to nothing; the setting its \
                 `store` template names has no value",
                self.name
            )));
        }
        Ok(store.to_owned())
    }

    /// Its [`BuildSpec`] for an application configured with `config`.
    pub fn build_spec(&self, config: &Attrs) -> Result<BuildSpec> {
        let bindings = self.bindings(config);
        let render = |t: &PathTemplate| -> Result<String> {
            let rendered = t.render_static(&bindings).map_err(|e| self.blame(e))?;
            self.safe_path(&rendered)
        };
        let (command, args) = split_command(&self.build.command, &self.name)?;
        Ok(BuildSpec {
            command,
            args,
            source_dir: render(&self.build.source)?,
            output_dir: render(&self.build.output)?,
            install: self.build.install.clone(),
        })
    }

    /// Where its generated runtime goes, relative to the store — or `None` for a
    /// framework that consumes no generated code.
    pub fn runtime_dir(&self, config: &Attrs) -> Result<Option<String>> {
        let Some(runtime) = &self.build.runtime else {
            return Ok(None);
        };
        let rendered = runtime
            .render_static(&self.bindings(config))
            .map_err(|e| self.blame(e))?;
        Ok(Some(self.safe_path(&rendered)?))
    }

    /// Where its generated typed client goes, relative to the store.
    pub fn client_path(&self, config: &Attrs) -> Result<Option<String>> {
        match self.runtime_dir(config)? {
            Some(dir) => Ok(Some(
                self.safe_path(&format!("{dir}/{}", self.build.client_file))?,
            )),
            None => Ok(None),
        }
    }

    /// Tidy a rendered path and refuse one that climbs out of the file store.
    ///
    /// This is `check_project_name`'s job, asked the other way round. `react`
    /// constrains the *setting* to a plain identifier, which it can do because it
    /// knows which setting the paths are built from; a declared framework's
    /// templates may be built from any of its settings, in any arrangement, so
    /// what is checked is the **result**. A `..` reaching a build would be caught
    /// by the store's own `resolve_under` — but as a path escaping a store, at
    /// build time, rather than as the setting the admin typed, on save, and §1.6
    /// is that the admin should hear it while looking at the form.
    fn safe_path(&self, rendered: &str) -> Result<String> {
        let path = clean_path(rendered);
        if path.split('/').any(|segment| segment == "..") {
            return Err(Error::invalid(format!(
                "framework `{}` (from module {}) resolves a path to `{path}`, which \
                 climbs out of the file store; check the settings it was built from",
                self.name, self.module
            )));
        }
        Ok(path)
    }

    /// Its builder agent's system prompt, with `extra` in scope beside its own
    /// settings — the application's name, its subdomain, the store and the
    /// project root, none of which is a setting.
    pub fn prompt(
        &self,
        config: &Attrs,
        extra: &BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        let Some(prompt) = &self.builder_prompt else {
            return Ok(None);
        };
        let mut bindings = self.bindings(config);
        bindings.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
        prompt
            .render_static(&bindings)
            .map(Some)
            .map_err(|e| self.blame(e))
    }

    /// Say which framework a template failed in. A module may declare several,
    /// and "there is no `projekt` here" alone does not say whose.
    fn blame(&self, e: Error) -> Error {
        Error::invalid(format!(
            "framework `{}` (from module {}): {e}",
            self.name, self.module
        ))
    }
}

/// Tidy a rendered path: collapse the empty segments a blank setting leaves, and
/// drop a leading or trailing separator.
///
/// This is `react::project_path`'s "blank means the store root" rule, generalised
/// and put in one place. A framework writes `{{ project }}/dist` once; whether
/// the admin named a project decides whether that is `todo/dist` or `dist`, and
/// no framework should have to write the conditional — nor be able to get it
/// wrong and produce a store-relative path with a leading `/` that every store
/// would then have to forgive.
pub fn clean_path(path: &str) -> String {
    path.split('/')
        .filter(|segment| !segment.trim().is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// Split a declared command line into the executable and its arguments —
/// whitespace-separated, no quoting, exactly as the `code` framework's `command`
/// setting is split.
fn split_command(line: &str, framework: &str) -> Result<(String, Vec<String>)> {
    let mut parts = line.split_whitespace().map(str::to_owned);
    let command = parts.next().ok_or_else(|| {
        Error::invalid(format!(
            "framework `{framework}` declares an empty build command; it should be \
             something like `npm run build`"
        ))
    })?;
    Ok((command, parts.collect()))
}

/// One file a declared framework's generator produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredFile {
    /// Where it goes, relative to the **project directory**.
    pub path: String,
    /// What is in it.
    pub contents: String,
}

/// Which set of files is being asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePhase {
    /// The whole project, written once when the application is created.
    Scaffold,
    /// The framework's own generated code, rewritten on every build and whenever
    /// the application's definition changes.
    Runtime,
}

impl FilePhase {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            FilePhase::Scaffold => "scaffold",
            FilePhase::Runtime => "runtime",
        }
    }
}

/// The seam a module's frameworks reach this crate through — [`TableProviderHost`]
/// inverted the same way.
///
/// `sc-app` cannot call a module: `sc-module` is above it, and the whole point of
/// §15.1 is that a framework need not be Rust at all. So this crate declares what
/// it needs and something above implements it, which is exactly the arrangement
/// `sc-catalog` has for provided tables.
///
/// [`TableProviderHost`]: sc_catalog::TableProviderHost
#[async_trait]
pub trait FrameworkHost: Send + Sync {
    /// Every framework every loaded module declares, in load order.
    fn frameworks(&self) -> Vec<FrameworkDecl>;

    /// The files framework `name` produces for `context` in `phase`.
    async fn framework_files(
        &self,
        name: &str,
        phase: FilePhase,
        context: Json,
    ) -> Result<Vec<DeclaredFile>>;
}

/// The installed set: the declarations, and the host that generates their files.
#[derive(Clone)]
pub struct FrameworkSet {
    declared: Arc<Vec<FrameworkDecl>>,
    host: Option<Arc<dyn FrameworkHost>>,
}

impl Default for FrameworkSet {
    fn default() -> FrameworkSet {
        FrameworkSet {
            declared: Arc::new(Vec::new()),
            host: None,
        }
    }
}

impl FrameworkSet {
    /// The set `host` supplies, asked for its declarations once.
    pub fn new(host: Arc<dyn FrameworkHost>) -> FrameworkSet {
        FrameworkSet {
            declared: Arc::new(host.frameworks()),
            host: Some(host),
        }
    }

    /// A set of declarations with no host — every question but "generate the
    /// files" answerable, which is what a test of the registry needs.
    pub fn from_declarations(declared: Vec<FrameworkDecl>) -> FrameworkSet {
        FrameworkSet {
            declared: Arc::new(declared),
            host: None,
        }
    }

    /// The declarations, in the order the modules were loaded.
    pub fn declarations(&self) -> &[FrameworkDecl] {
        &self.declared
    }

    /// The framework declared under `name`, or `None`.
    pub fn find(&self, name: &str) -> Option<&FrameworkDecl> {
        self.declared.iter().find(|f| f.name == name)
    }

    /// Generate framework `name`'s files for `context`.
    ///
    /// An error rather than an empty list when nothing hosts them: a scaffold
    /// that quietly wrote no files would leave the admin an empty project
    /// directory and no reason for it.
    pub async fn files(
        &self,
        name: &str,
        phase: FilePhase,
        context: Json,
    ) -> Result<Vec<DeclaredFile>> {
        let host = self.host.as_ref().ok_or_else(|| {
            Error::config(format!(
                "framework `{name}` is declared by a module, but no module host is \
                 running to generate its files"
            ))
        })?;
        host.framework_files(name, phase, context).await
    }
}

/// The process-wide installed set.
///
/// A global for the reason `sc-types`' rich-type registry is one: the frameworks
/// this server has are a property of the *server*, every consumer asks by name,
/// and threading a set through `framework_config_spec`, `app_source_from_config`
/// and `framework_builder_agent` would put it in the signature of most of the
/// admin API for the benefit of a lookup. Unlike that registry it is swappable,
/// because a module can be installed while the server is running: the server
/// rebuilds and installs the whole set on every module change, as it already does
/// for the action registry and the table providers.
///
/// Every lookup below is a pure function of an explicit set, so the registry's
/// behaviour is tested without touching this.
static INSTALLED: RwLock<Option<FrameworkSet>> = RwLock::new(None);

/// Install the frameworks the modules declare, replacing whatever was installed.
///
/// Called on every module reload. A poisoned lock is reported rather than
/// panicked on, and leaves the previous set in place — a server whose framework
/// list is one reload stale still serves every application it has.
pub fn install_frameworks(set: FrameworkSet) -> Result<()> {
    let mut guard = INSTALLED
        .write()
        .map_err(|_| Error::msg("the framework registry lock is poisoned"))?;
    *guard = Some(set);
    Ok(())
}

/// The installed set — empty on a server with no modules, and on every test that
/// never installed one.
pub fn installed_frameworks() -> FrameworkSet {
    match INSTALLED.read() {
        Ok(guard) => guard.clone().unwrap_or_default(),
        Err(poisoned) => poisoned.into_inner().clone().unwrap_or_default(),
    }
}

/// The framework declared under `name`, from the installed set.
pub fn declared_framework(name: &str) -> Option<FrameworkDecl> {
    installed_frameworks().find(name).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sc_types::BasicType;
    use serde_json::json;

    fn template(s: &str) -> Template {
        Template::parse(s).expect("the test's own template parses")
    }

    pub(crate) fn vue_decl() -> FrameworkDecl {
        FrameworkDecl {
            name: "vue".to_owned(),
            module: "@feldspar/vue".to_owned(),
            label: "Vue".to_owned(),
            description: "A Vue 3 project.".to_owned(),
            config_spec: vec![
                FormField::new("store", BasicType::Text).required(),
                FormField::new("project", BasicType::Text).default_value(""),
            ],
            build: BuildTemplate {
                store: template("{{ store }}"),
                source: template("{{ project }}"),
                output: template("{{ project }}/dist"),
                command: "npm run build".to_owned(),
                install: Some(InstallSpec {
                    command: "npm".to_owned(),
                    args: vec!["install".to_owned()],
                    marker: "node_modules".to_owned(),
                }),
                runtime: Some(template("{{ project }}/src/feldspar")),
                client_file: "client.ts".to_owned(),
            },
            csp: Some(CspPolicy::strict().directive("img-src", ["'self'", "data:"])),
            builder_prompt: Some(template(
                "You maintain {{ app }} in {{ project }} of store {{ store }}.",
            )),
            scaffolds: true,
        }
    }

    fn config(pairs: &[(&str, &str)]) -> Attrs {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), json!(v)))
            .collect()
    }

    #[test]
    fn the_paths_are_templates_over_the_frameworks_own_settings() {
        let decl = vue_decl();
        let cfg = config(&[("store", "apps"), ("project", "todo")]);
        assert_eq!(decl.store(&cfg).unwrap(), "apps");
        let spec = decl.build_spec(&cfg).unwrap();
        assert_eq!(spec.command, "npm");
        assert_eq!(spec.args, ["run", "build"]);
        assert_eq!(spec.source_dir, "todo");
        assert_eq!(spec.output_dir, "todo/dist");
        assert_eq!(
            decl.client_path(&cfg).unwrap().unwrap(),
            "todo/src/feldspar/client.ts"
        );
    }

    #[test]
    fn a_blank_setting_collapses_the_segment_it_would_have_filled() {
        // `react`'s "blank means the store root", falling out of the templates
        // rather than being written again per framework.
        let decl = vue_decl();
        let cfg = config(&[("store", "apps"), ("project", "")]);
        let spec = decl.build_spec(&cfg).unwrap();
        assert_eq!(spec.source_dir, "");
        assert_eq!(spec.output_dir, "dist");
        assert_eq!(
            decl.client_path(&cfg).unwrap().unwrap(),
            "src/feldspar/client.ts"
        );
        // And a setting the admin never filled in resolves to the spec's own
        // default, not to an unbound name.
        let spec = decl.build_spec(&config(&[("store", "apps")])).unwrap();
        assert_eq!(spec.output_dir, "dist");
    }

    #[test]
    fn the_csp_is_the_strict_baseline_plus_what_the_framework_asked_for() {
        let csp = vue_decl().default_csp();
        assert_eq!(csp.directives["default-src"], ["'self'"]);
        assert_eq!(csp.directives["img-src"], ["'self'", "data:"]);
        // And a framework that says nothing gets the baseline, unwidened.
        let mut plain = vue_decl();
        plain.csp = None;
        assert_eq!(plain.default_csp(), CspPolicy::strict());
    }

    #[test]
    fn the_builder_prompt_sees_the_settings_and_the_application() {
        let decl = vue_decl();
        let extra = [("app".to_owned(), "My Todo".to_owned())]
            .into_iter()
            .collect();
        let prompt = decl
            .prompt(&config(&[("store", "apps"), ("project", "todo")]), &extra)
            .unwrap()
            .unwrap();
        assert_eq!(prompt, "You maintain My Todo in todo of store apps.");
    }

    #[test]
    fn a_template_naming_something_that_is_not_a_setting_says_whose_it_is() {
        let mut decl = vue_decl();
        decl.build.output = template("{{ projekt }}/dist");
        let msg = decl
            .build_spec(&config(&[("store", "apps"), ("project", "todo")]))
            .unwrap_err()
            .to_string();
        assert!(
            msg.contains("vue") && msg.contains("@feldspar/vue"),
            "{msg}"
        );
        assert!(msg.contains("projekt"), "{msg}");
    }

    #[test]
    fn a_path_that_climbs_out_of_the_store_is_refused_where_the_setting_was_typed() {
        let decl = vue_decl();
        let msg = decl
            .build_spec(&config(&[("store", "apps"), ("project", "../../etc")]))
            .unwrap_err()
            .to_string();
        assert!(msg.contains("climbs out"), "{msg}");
        assert!(msg.contains("vue"), "{msg}");
    }

    #[tokio::test]
    async fn a_set_with_no_host_answers_every_question_but_generate() {
        let set = FrameworkSet::from_declarations(vec![vue_decl()]);
        assert!(set.find("vue").is_some());
        assert!(set.find("react").is_none());
        let err = set
            .files("vue", FilePhase::Scaffold, json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no module host"), "{err}");
    }
}
