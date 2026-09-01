//! What agent builds an application — declared by its **framework** (§13.3,
//! §11.3).
//!
//! Creating an application creates the agent that will build it. Which agent that
//! is cannot be a property of "applications in general": a code framework's app is
//! a source tree in a file store, so the agent that builds it is a coding agent
//! pointed at that tree, while a framework that renders from the catalog would
//! want something else entirely and one that is served from elsewhere wants
//! nothing at all. So the declaration sits beside the framework's other
//! declarations — its settings ([`framework_config_spec`](crate::framework_config_spec)),
//! its default CSP ([`framework_default_csp`](crate::framework_default_csp)) —
//! and is resolved the same way: **by name, from the registry**, because an
//! application is created before there is any built instance to ask.
//!
//! ## Why the traits are named as strings
//!
//! The traits themselves (`coding`, `build_application`) live in `sc-core-traits`,
//! which is layer 9 and sits *above* this crate — as it must, since a trait's
//! writes go through the row layer. A framework here can therefore only *name* the
//! trait it wants and the settings to give it, exactly as an application names its
//! API providers rather than holding them. That the names and the configuration
//! keys still match the real traits is asserted from above, in `sc-core-traits`'
//! own tests, where both sides are visible at once.
//!
//! Assembling a spec into an [`Agent`](sc_agent::Agent) and storing it is the
//! server's, for the same layering reason: this crate does not know agents exist.

use sc_types::Attrs;
use serde_json::Value as Json;

use crate::application::{Application, FrameworkRef};
use crate::build::app_source_from_config;
use crate::framework::CODE_FRAMEWORK;
use crate::react::REACT_FRAMEWORK;

/// The trait that reads, searches and edits a file store's contents.
pub const TRAIT_CODING: &str = "coding";
/// The trait that builds one application and reports its diagnostics.
pub const TRAIT_BUILD_APPLICATION: &str = "build_application";

/// `coding`'s file store setting.
pub const TRAIT_CFG_STORE: &str = "store";
/// `coding`'s sub-directory setting.
pub const TRAIT_CFG_ROOT: &str = "root";
/// `coding`'s "may create and change files" grant.
pub const TRAIT_CFG_MAY_EDIT: &str = "may_edit";
/// `coding`'s "may run the project's scripts" grant.
pub const TRAIT_CFG_MAY_RUN_SCRIPTS: &str = "may_run_scripts";
/// `build_application`'s application setting: a subdomain (§13.2).
pub const TRAIT_CFG_APPLICATION: &str = "application";

/// One trait an application's builder agent is created with: which trait, and how
/// it is configured.
///
/// The shape of `sc-agent`'s `EnabledTrait`, restated here because that type is a
/// layer above; the server maps one to the other where it can see both.
#[derive(Debug, Clone, PartialEq)]
pub struct BuilderTrait {
    /// The registered name of the trait.
    pub trait_: String,
    /// Its configuration, keyed by the trait's own settings.
    pub config: Attrs,
}

impl BuilderTrait {
    /// The named trait, with no configuration set.
    pub fn new(trait_: impl Into<String>) -> BuilderTrait {
        BuilderTrait {
            trait_: trait_.into(),
            config: Attrs::new(),
        }
    }

    /// Set one configuration value, returning `self` for chaining.
    pub fn with(mut self, key: impl Into<String>, value: impl Into<Json>) -> BuilderTrait {
        self.config.insert(key.into(), value.into());
        self
    }
}

/// The agent an application is created with: its name, what it is for, what it is
/// told it is, and the traits it is granted.
///
/// Deliberately *not* a provider or a model: which LLM a deployment has connected
/// is not something a framework can know, and picking one is the job of whoever
/// creates the record.
#[derive(Debug, Clone, PartialEq)]
pub struct BuilderAgentSpec {
    /// The agent's name — unique, and derived from the application's subdomain so
    /// it is stable and recognisably that application's.
    pub name: String,
    /// One line: what this agent is for.
    pub description: String,
    /// What the agent is told about the application before the conversation
    /// starts.
    pub system_prompt: String,
    /// The traits it is created with, in the order they are offered to the model.
    pub traits: Vec<BuilderTrait>,
}

/// The name the builder agent of `app` is created under.
///
/// Derived from the **subdomain** rather than the display name: the subdomain is
/// unique (§13.2) and an agent's name is unique too, so the derivation cannot
/// collide for two applications; and it is stable under a rename of the app's
/// human-facing name, which agents referenced from triggers and chats should be.
pub fn builder_agent_name(app: &Application) -> String {
    format!("build-{}", app.subdomain.trim())
}

/// The builder agent the framework `fw` declares for `app`, or `None` for a
/// framework that has no application-building agent to declare.
///
/// The registry lookup, mirroring [`framework_config_spec`](crate::framework_config_spec):
/// resolved from the framework's *name*, because an application is created long
/// before it is built and there is no [`Framework`](crate::Framework) instance to
/// ask.
///
/// Both registered frameworks build from a file store, so both declare a coding
/// agent over that store's source directory — but they declare it separately and
/// say different things in the prompt, because what the agent is working on
/// differs: a `react` app is a project Saltcorn scaffolded and whose generated
/// client must not be hand-edited, while a `code` app is whatever the admin
/// brought.
pub fn framework_builder_agent(fw: &FrameworkRef, app: &Application) -> Option<BuilderAgentSpec> {
    match fw.name.as_str() {
        REACT_FRAMEWORK => coding_agent(fw, app, react_prompt),
        CODE_FRAMEWORK => coding_agent(fw, app, code_prompt),
        _ => None,
    }
}

/// A coding agent over the framework's source tree, plus the build of this one
/// application: read and edit the code, build it, read the diagnostics, fix them.
///
/// `None` when the framework's settings do not resolve to a source tree — which
/// on a saved application means the config was rejected on save, so there is
/// nothing to point an agent at and nothing to report either.
fn coding_agent(
    fw: &FrameworkRef,
    app: &Application,
    prompt: fn(&Application, &str, &str) -> String,
) -> Option<BuilderAgentSpec> {
    let source = app_source_from_config(fw).ok()?;
    let store = source.store.0;
    let root = source.build.source_dir;
    Some(BuilderAgentSpec {
        name: builder_agent_name(app),
        description: format!("Builds the `{}` application", app.name),
        system_prompt: prompt(app, &store, &root),
        traits: vec![
            BuilderTrait::new(TRAIT_CODING)
                .with(TRAIT_CFG_STORE, store)
                .with(TRAIT_CFG_ROOT, root)
                // The point of this agent: it exists to change the application's
                // source.
                .with(TRAIT_CFG_MAY_EDIT, true)
                // Off, like the trait's own default. Building is what this agent
                // needs and it has `build_application` for that; running the
                // project's other scripts executes code the agent did not write,
                // which is a grant the admin gives deliberately rather than one
                // that arrives with an application.
                .with(TRAIT_CFG_MAY_RUN_SCRIPTS, false),
            BuilderTrait::new(TRAIT_BUILD_APPLICATION)
                .with(TRAIT_CFG_APPLICATION, app.subdomain.trim()),
        ],
    })
}

/// The prompt for a scaffolded React project: the conventions it can rely on, and
/// the generated files it must not hand-edit.
fn react_prompt(app: &Application, store: &str, root: &str) -> String {
    format!(
        "You build the `{name}` application, a React + Vite project served at the \
         `{subdomain}` subdomain. Its source is in the `{store}` file store under \
         `{root}`, and your file tools are scoped to exactly that directory.\n\n\
         {SHARED_PROMPT}\n\n\
         Two conventions of this framework: `src/feldspar/` is generated from the \
         application's own API — its client and typed hooks are rewritten on every \
         build, so read it to learn what data is available but never edit it — and \
         the application's data is reached through that client, never by talking to \
         a database.",
        name = app.name,
        subdomain = app.subdomain.trim(),
    )
}

/// The prompt for a `code` application: the same job, without conventions this
/// framework has not got.
fn code_prompt(app: &Application, store: &str, root: &str) -> String {
    format!(
        "You build the `{name}` application, served at the `{subdomain}` subdomain. \
         Its source is in the `{store}` file store under `{root}`, and your file \
         tools are scoped to exactly that directory. The project is the admin's \
         own — read it before changing it rather than assuming a layout.\n\n\
         {SHARED_PROMPT}",
        name = app.name,
        subdomain = app.subdomain.trim(),
    )
}

/// What is true of building any application, whichever framework it is on.
const SHARED_PROMPT: &str = "\
Work in small steps: search and read before you edit, make the change, then build \
the application and read the result. A failed build's diagnostics name the file, \
the line and the problem — fix those and build again, and do not report a change \
as done until it builds clean. If a request is ambiguous, ask rather than guess.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framework::{CFG_COMMAND, CFG_OUTPUT, CFG_SOURCE, CFG_STORE};
    use crate::react::CFG_PROJECT;

    fn react_app() -> Application {
        Application::new(
            "Todo",
            "todo",
            FrameworkRef::new(REACT_FRAMEWORK)
                .with(CFG_STORE, "apps")
                .with(CFG_PROJECT, "todo"),
        )
    }

    fn code_app() -> Application {
        Application::new(
            "Blog",
            "blog",
            FrameworkRef::new(CODE_FRAMEWORK)
                .with(CFG_STORE, "apps")
                .with(CFG_SOURCE, "web")
                .with(CFG_OUTPUT, "web/dist")
                .with(CFG_COMMAND, "npm run build"),
        )
    }

    #[test]
    fn a_react_app_gets_a_coding_agent_over_its_project_directory() {
        let app = react_app();
        let spec = framework_builder_agent(&app.framework, &app).expect("react declares one");

        assert_eq!(spec.name, "build-todo");
        assert!(spec.description.contains("Todo"), "{}", spec.description);

        // The coding trait is scoped to the *derived* project directory, not the
        // store root: an agent that could edit every project in the store would
        // be one grant for every application that shares it.
        let coding = &spec.traits[0];
        assert_eq!(coding.trait_, TRAIT_CODING);
        assert_eq!(coding.config[TRAIT_CFG_STORE], Json::from("apps"));
        assert_eq!(coding.config[TRAIT_CFG_ROOT], Json::from("todo"));
        // It exists to change the source, so editing is on; running the project's
        // other scripts is not part of building and stays the admin's to grant.
        assert_eq!(coding.config[TRAIT_CFG_MAY_EDIT], Json::from(true));
        assert_eq!(
            coding.config[TRAIT_CFG_MAY_RUN_SCRIPTS],
            Json::from(false),
            "running arbitrary scripts is not something an application creation grants"
        );

        // And it can build the one application it was created for, by subdomain.
        let build = &spec.traits[1];
        assert_eq!(build.trait_, TRAIT_BUILD_APPLICATION);
        assert_eq!(build.config[TRAIT_CFG_APPLICATION], Json::from("todo"));

        // The prompt says which application, where its source is, and the
        // convention a model would otherwise break on its first edit.
        let prompt = &spec.system_prompt;
        assert!(prompt.contains("Todo"), "{prompt}");
        assert!(prompt.contains("apps"), "{prompt}");
        assert!(prompt.contains("src/feldspar/"), "{prompt}");
        assert!(prompt.contains("build"), "{prompt}");
    }

    #[test]
    fn a_code_app_gets_one_over_its_stated_source_directory() {
        let app = code_app();
        let spec = framework_builder_agent(&app.framework, &app).expect("code declares one");

        assert_eq!(spec.name, "build-blog");
        let coding = &spec.traits[0];
        assert_eq!(coding.config[TRAIT_CFG_STORE], Json::from("apps"));
        // The `code` framework states its source directory rather than deriving
        // it, and that is the directory the agent gets.
        assert_eq!(coding.config[TRAIT_CFG_ROOT], Json::from("web"));

        // No React conventions are claimed for a project this framework knows
        // nothing about.
        assert!(!spec.system_prompt.contains("src/feldspar/"));
        assert!(spec.system_prompt.contains("Blog"));
    }

    #[test]
    fn the_name_is_the_subdomain_so_two_applications_cannot_collide() {
        // The subdomain is unique (§13.2) and an agent's name is unique, so the
        // derivation has to be from the one to the other.
        assert_eq!(builder_agent_name(&react_app()), "build-todo");
        assert_eq!(builder_agent_name(&code_app()), "build-blog");
        // And it is stable under a rename of the display name, which is what an
        // agent referenced from a chat or a trigger needs.
        let mut renamed = react_app();
        renamed.name = "To-do list".to_owned();
        assert_eq!(builder_agent_name(&renamed), "build-todo");
    }

    #[test]
    fn a_framework_with_no_source_tree_declares_no_agent() {
        // An unregistered framework — one from a guest language, say — is not
        // assumed to want a coding agent, and nothing here fails over it.
        let app = Application::new("Legacy", "legacy", FrameworkRef::new("saltcorn-v1"));
        assert_eq!(framework_builder_agent(&app.framework, &app), None);

        // Nor does a registered one whose settings do not resolve to a source
        // tree: there is nothing to point the agent at.
        let unset = Application::new("Broken", "broken", FrameworkRef::new(CODE_FRAMEWORK));
        assert_eq!(framework_builder_agent(&unset.framework, &unset), None);
    }
}
