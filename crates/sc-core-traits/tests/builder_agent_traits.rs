//! The builder agent a framework declares (`sc_app::framework_builder_agent`)
//! names traits and settings that live **here**, one layer up — so this is the
//! only place both halves are visible at once, and therefore the only place the
//! two can be held to each other.
//!
//! `sc-app` is layer 8 and the traits are layer 9: a framework can name `coding`
//! and `may_edit` as strings and nothing more, so nothing in `sc-app` fails to
//! compile if a trait is renamed or a setting changes shape. What would happen
//! instead is that every application created afterwards would report an
//! `agent_error` the admin cannot act on. These tests are what turns that into a
//! build failure.
//!
//! No database: this is the structural half — the names resolve, and the
//! configuration validates against the specs the traits declare. Whether the
//! store exists and the agent saves is asserted end-to-end over HTTP in
//! `sc-server`'s `app_builder_agent` test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sc_app::{
    Application, BuilderAgentSpec, CFG_COMMAND, CFG_OUTPUT, CFG_PROJECT, CFG_SOURCE, CFG_STORE,
    CODE_FRAMEWORK, FrameworkRef, REACT_FRAMEWORK, TRAIT_BUILD_APPLICATION, TRAIT_CFG_APPLICATION,
    TRAIT_CFG_MAY_EDIT, TRAIT_CFG_MAY_RUN_SCRIPTS, TRAIT_CFG_ROOT, TRAIT_CFG_STORE, TRAIT_CODING,
    framework_builder_agent,
};
use sc_types::validate_attrs;
use serde_json::json;

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

fn spec_for(app: &Application) -> BuilderAgentSpec {
    framework_builder_agent(&app.framework, app).expect("a code framework declares a builder agent")
}

#[test]
fn every_trait_a_framework_declares_is_a_registered_one_configured_as_it_declares() {
    let registry = sc_core_traits::builtin_traits().expect("the built-in traits assemble");

    for app in [react_app(), code_app()] {
        let spec = spec_for(&app);
        assert!(
            !spec.traits.is_empty(),
            "a builder agent with no traits could not build anything"
        );
        for enabled in &spec.traits {
            // The name resolves — the check that a renamed trait breaks here
            // rather than on the next application an admin creates.
            let trait_ = registry.require(&enabled.trait_).unwrap_or_else(|e| {
                panic!(
                    "framework `{}` declares `{}`: {e}",
                    app.framework.name, enabled.trait_
                )
            });
            // ...and every setting it declares is one that trait has, of the
            // type it declares it with. This is `validate_agent`'s own check,
            // run without a database.
            validate_attrs(&trait_.config_spec(), &enabled.config).unwrap_or_else(|e| {
                panic!(
                    "framework `{}`'s `{}` configuration: {e}",
                    app.framework.name, enabled.trait_
                )
            });
        }
    }
}

#[test]
fn the_coding_grant_and_the_build_target_are_the_settings_those_traits_mean() {
    // The names are strings on the `sc-app` side, so assert they are the *same*
    // strings the traits use — a settings rename that only touched the trait
    // would otherwise leave a builder agent that reads its source and cannot
    // change it, which is a silent loss of the capability, not an error.
    let registry = sc_core_traits::builtin_traits().expect("the built-in traits assemble");
    assert!(registry.get(TRAIT_CODING).is_some());
    assert!(registry.get(TRAIT_BUILD_APPLICATION).is_some());
    assert_eq!(TRAIT_CFG_STORE, sc_core_traits::CFG_STORE);
    assert_eq!(TRAIT_CFG_ROOT, sc_core_traits::CFG_ROOT);
    assert_eq!(TRAIT_CFG_MAY_EDIT, sc_core_traits::CFG_MAY_EDIT);
    assert_eq!(
        TRAIT_CFG_MAY_RUN_SCRIPTS,
        sc_core_traits::CFG_MAY_RUN_SCRIPTS
    );
    assert_eq!(TRAIT_CFG_APPLICATION, sc_core_traits::CFG_APPLICATION);

    // And the values a created application carries: the agent may edit the
    // source it was created to build, and builds *that* application.
    let app = react_app();
    let spec = spec_for(&app);
    let coding = spec
        .traits
        .iter()
        .find(|t| t.trait_ == TRAIT_CODING)
        .expect("the coding trait");
    assert_eq!(coding.config[TRAIT_CFG_MAY_EDIT], json!(true));
    let build = spec
        .traits
        .iter()
        .find(|t| t.trait_ == TRAIT_BUILD_APPLICATION)
        .expect("the build trait");
    assert_eq!(build.config[TRAIT_CFG_APPLICATION], json!(app.subdomain));
}

#[test]
fn the_scope_a_framework_declares_is_one_the_coding_trait_resolves() {
    // The store and the sub-directory are what every one of that trait's tool
    // names is derived from (`edit_file_apps_todo`), so a scope it cannot
    // resolve is an agent that cannot be saved.
    let app = react_app();
    let spec = spec_for(&app);
    let coding = spec
        .traits
        .iter()
        .find(|t| t.trait_ == TRAIT_CODING)
        .expect("the coding trait");
    let scope = sc_core_traits::configured_scope(&coding.config).expect("the scope resolves");
    assert_eq!(scope.store, "apps");
    assert_eq!(scope.root, "todo");

    // Every derived name is one a provider will accept — the length rule
    // `save_agent` applies, checked here where the derivation is declared.
    for name in sc_core_traits::tool_names::coding(&scope) {
        assert!(!name.is_empty());
        assert!(name.len() <= 64, "tool `{name}` is {} chars", name.len());
    }
}
