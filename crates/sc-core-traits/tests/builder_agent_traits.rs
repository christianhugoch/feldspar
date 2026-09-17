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
    CODE_FRAMEWORK, EDIT_FORMAT_AUTO, FrameworkRef, REACT_FRAMEWORK, TRAIT_CFG_APPLICATION,
    TRAIT_CFG_CHECKS, TRAIT_CFG_EDIT_FORMAT, TRAIT_CFG_MAY_CHECK, TRAIT_CFG_MAY_EDIT,
    TRAIT_CFG_MAY_RUN_SCRIPTS, TRAIT_CFG_MAY_USE_SHELL, TRAIT_CFG_MAY_VIEW_APP, TRAIT_CFG_ROOT,
    TRAIT_CFG_STORE, TRAIT_CFG_WORKFLOW, TRAIT_CODING, WORKFLOW_PLANNED, framework_builder_agent,
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
fn the_grants_and_the_build_target_are_the_settings_the_coding_trait_means() {
    // The names are strings on the `sc-app` side, so assert they are the *same*
    // strings the trait uses — a settings rename that only touched the trait
    // would otherwise leave a builder agent that reads its source and cannot
    // change it, which is a silent loss of the capability, not an error.
    let registry = sc_core_traits::builtin_traits().expect("the built-in traits assemble");
    assert!(registry.get(TRAIT_CODING).is_some());
    for (declared, real) in [
        (TRAIT_CFG_STORE, sc_core_traits::CFG_STORE),
        (TRAIT_CFG_ROOT, sc_core_traits::CFG_ROOT),
        (TRAIT_CFG_MAY_EDIT, sc_core_traits::CFG_MAY_EDIT),
        (
            TRAIT_CFG_MAY_RUN_SCRIPTS,
            sc_core_traits::CFG_MAY_RUN_SCRIPTS,
        ),
        (TRAIT_CFG_MAY_CHECK, sc_core_traits::CFG_MAY_CHECK),
        (TRAIT_CFG_MAY_VIEW_APP, sc_core_traits::CFG_MAY_VIEW_APP),
        (TRAIT_CFG_MAY_USE_SHELL, sc_core_traits::CFG_MAY_USE_SHELL),
        (TRAIT_CFG_APPLICATION, sc_core_traits::CFG_APPLICATION),
        (TRAIT_CFG_CHECKS, sc_core_traits::CFG_CHECKS),
        (TRAIT_CFG_WORKFLOW, sc_core_traits::CFG_WORKFLOW),
        (TRAIT_CFG_EDIT_FORMAT, sc_core_traits::CFG_EDIT_FORMAT),
        (WORKFLOW_PLANNED, sc_core_traits::WORKFLOW_PLANNED),
        (EDIT_FORMAT_AUTO, sc_core_traits::EDIT_FORMAT_AUTO),
    ] {
        assert_eq!(declared, real);
    }

    // And the values a created application carries (§12): `coding` alone,
    // which may edit, check and look at the application it was created to
    // build, starts by planning, and may not run scripts or a shell.
    let app = react_app();
    let spec = spec_for(&app);
    assert_eq!(spec.traits.len(), 1, "{:?}", spec.traits);
    let coding = &spec.traits[0];
    assert_eq!(coding.trait_, TRAIT_CODING);
    for (key, value) in [
        (TRAIT_CFG_MAY_EDIT, json!(true)),
        (TRAIT_CFG_MAY_CHECK, json!(true)),
        (TRAIT_CFG_MAY_VIEW_APP, json!(true)),
        (TRAIT_CFG_MAY_RUN_SCRIPTS, json!(false)),
        (TRAIT_CFG_MAY_USE_SHELL, json!(false)),
        (TRAIT_CFG_APPLICATION, json!(app.subdomain)),
        (TRAIT_CFG_CHECKS, json!(["typecheck"])),
        (TRAIT_CFG_WORKFLOW, json!("planned")),
        (TRAIT_CFG_EDIT_FORMAT, json!("auto")),
    ] {
        assert_eq!(coding.config[key], value, "`{key}`");
    }
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
