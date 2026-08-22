//! The Node sidecar: the protocol, the manifest, the stubs, and what a dead
//! host does to the calls that were in flight.

#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;

use common::{fixture, have_node, have_npm, installed, temp_root};
use sc_module::{Installer, ModuleHost, ModuleSource};
use serde_json::json;

#[tokio::test]
async fn a_module_loads_and_reports_what_it_supplies() {
    skip_without!(
        have_node() && have_npm(),
        "node and npm are not both on the PATH"
    );
    let (installer, host, names) = installed("host-load", &["echo-module"]).await;
    let name = &names[0];

    let manifest = host
        .load(name, &installer.package_dir(name), &json!({}))
        .await
        .unwrap();

    assert_eq!(manifest.name, *name);
    assert_eq!(manifest.api_version, Some(1));
    assert_eq!(manifest.plugin_name.as_deref(), Some("echo"));

    let actions: Vec<&str> = manifest.actions.iter().map(|a| a.name.as_str()).collect();
    assert!(actions.contains(&"echo_row"), "{actions:?}");
    assert!(actions.contains(&"echo_interpolate"), "{actions:?}");

    // The action's `configFields` arrive evaluated — including the one that is
    // an async function of a context rather than an array.
    let echo_row = manifest
        .actions
        .iter()
        .find(|a| a.name == "echo_row")
        .unwrap();
    assert_eq!(echo_row.description, "Echo what the action was given");
    assert_eq!(echo_row.config_fields.len(), 3);
    assert_eq!(echo_row.config_fields[0]["name"], json!("greeting"));
    let interpolate = manifest
        .actions
        .iter()
        .find(|a| a.name == "echo_interpolate")
        .unwrap();
    assert_eq!(interpolate.config_fields.len(), 1);
    assert_eq!(interpolate.config_fields[0]["name"], json!("template"));

    // The module's own configuration form, read out of its
    // `configuration_workflow` (§5).
    assert_eq!(manifest.config_fields.len(), 2);
    assert_eq!(manifest.config_fields[0]["name"], json!("endpoint"));
    assert_eq!(manifest.config_fields[1]["fieldview"], json!("password"));

    // What it also supplies and this version does not load (§6) — counted, so
    // the admin knows what they are not getting.
    let census: Vec<(&str, Option<u64>)> = manifest
        .unsupported
        .iter()
        .map(|e| (e.key.as_str(), e.count))
        .collect();
    assert!(census.contains(&("viewtemplates", Some(2))), "{census:?}");
    assert!(census.contains(&("table_providers", Some(1))), "{census:?}");
    assert!(manifest.issues.is_empty(), "{:?}", manifest.issues);

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

#[tokio::test]
async fn an_action_runs_and_gets_v1s_argument_object() {
    skip_without!(
        have_node() && have_npm(),
        "node and npm are not both on the PATH"
    );
    let (installer, host, names) = installed("host-run", &["echo-module"]).await;
    let name = &names[0];
    host.load(
        name,
        &installer.package_dir(name),
        &json!({ "endpoint": "https://example.com" }),
    )
    .await
    .unwrap();

    let result = host
        .run(
            name,
            "echo_row",
            json!({
                "row": { "id": 1, "title": "Dune" },
                "table": { "name": "books" },
                "configuration": { "greeting": "hello" },
                "user": { "email": "a@b.c" },
                "mode": "insert",
            }),
        )
        .await
        .unwrap();

    assert_eq!(result["greeting"], json!("hello"));
    assert_eq!(result["row"]["title"], json!("Dune"));
    assert_eq!(result["table"]["name"], json!("books"));
    assert_eq!(result["user"]["email"], json!("a@b.c"));
    // The module's own configuration reached `actions(cfg)`.
    assert_eq!(
        result["module_config"]["endpoint"],
        json!("https://example.com")
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

#[tokio::test]
async fn the_saltcorn_stubs_are_free_to_require_and_named_when_called() {
    skip_without!(
        have_node() && have_npm(),
        "node and npm are not both on the PATH"
    );
    let (installer, host, names) = installed("host-stubs", &["echo-module"]).await;
    let name = &names[0];
    // Loading at all is the first half of the assertion: the fixture requires
    // `@saltcorn/data/models/table` and `@saltcorn/data/db/state` at its top.
    host.load(name, &installer.package_dir(name), &json!({}))
        .await
        .unwrap();

    // `interpolate` is real, because a module that names its snapshots with one
    // needs the real thing (§3).
    let interpolated = host
        .run(
            name,
            "echo_interpolate",
            json!({
                "row": { "name": "vm1", "id": 7 },
                "configuration": { "template": "{{ name }}-{{ id }}" },
            }),
        )
        .await
        .unwrap();
    assert_eq!(interpolated, json!("vm1-7"));

    // Calling one that is not implemented fails by name, rather than answering
    // `undefined` and computing the wrong thing.
    let err = host
        .run(name, "echo_missing_api", json!({}))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("findOne"), "{msg}");
    assert!(msg.contains("not available"), "{msg}");
    // The module's fault to the admin, not Saltcorn's (§16's split).
    assert_eq!(err.kind(), sc_error::ErrorKind::Application);

    let err = host.run(name, "echo_state", json!({})).await.unwrap_err();
    assert!(err.to_string().contains("getState"), "{err}");

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

#[tokio::test]
async fn a_module_that_throws_is_an_ordinary_failed_call() {
    skip_without!(
        have_node() && have_npm(),
        "node and npm are not both on the PATH"
    );
    let (installer, host, names) = installed("host-throw", &["echo-module"]).await;
    let name = &names[0];
    host.load(name, &installer.package_dir(name), &json!({}))
        .await
        .unwrap();

    let err = host.run(name, "echo_throw", json!({})).await.unwrap_err();
    assert!(err.to_string().contains("the module said no"), "{err}");

    // …and the host is still serving.
    assert_eq!(
        host.run(
            name,
            "echo_row",
            json!({ "configuration": { "greeting": "still here" } })
        )
        .await
        .unwrap()["greeting"],
        json!("still here")
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

#[tokio::test]
async fn a_module_that_kills_the_host_fails_its_call_and_the_next_one_works() {
    skip_without!(
        have_node() && have_npm(),
        "node and npm are not both on the PATH"
    );
    let (installer, host, names) = installed("host-crash", &["echo-module"]).await;
    let name = &names[0];
    host.load(name, &installer.package_dir(name), &json!({}))
        .await
        .unwrap();

    let err = host.run(name, "echo_exit", json!({})).await.unwrap_err();
    assert!(
        err.to_string().contains("exited"),
        "the caller should be told the host died: {err}"
    );

    // The next call starts a new process and replays the load, so the module is
    // back without anybody asking for it.
    let result = host
        .run(
            name,
            "echo_row",
            json!({ "configuration": { "greeting": "restarted" } }),
        )
        .await
        .unwrap();
    assert_eq!(result["greeting"], json!("restarted"));

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

#[tokio::test]
async fn a_module_that_cannot_be_required_is_reported_and_the_host_stays_up() {
    skip_without!(
        have_node() && have_npm(),
        "node and npm are not both on the PATH"
    );
    let (installer, host, names) =
        installed("host-broken", &["broken-module", "echo-module"]).await;
    let broken = &names[0];
    let echo = &names[1];

    let err = host
        .load(broken, &installer.package_dir(broken), &json!({}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("cannot be loaded"), "{err}");

    // The good module still loads in the same process.
    let manifest = host
        .load(echo, &installer.package_dir(echo), &json!({}))
        .await
        .unwrap();
    assert!(!manifest.actions.is_empty());

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

#[tokio::test]
async fn a_host_over_an_empty_root_still_answers_a_ping() {
    skip_without!(have_node(), "node is not on the PATH");
    // No npm, no install, no modules: the host script is written from the binary
    // and `node` starts it. This is the check a diagnostics screen makes.
    let root = temp_root("host-ping");
    let host = ModuleHost::new(&root);
    let pong = host.ping().await.unwrap();
    assert_eq!(pong["pong"], json!(true));
    assert!(pong["node"].as_str().unwrap_or_default().starts_with('v'));
    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_checkouts_own_dependencies_are_installed_and_its_v1_ones_are_not() {
    skip_without!(
        have_node() && have_npm(),
        "node and npm are not both on the PATH"
    );
    // Two failures every real module hits, and both are about the *tree* rather
    // than about the module:
    //
    // - npm installs a local directory by **symlinking** it, and does not
    //   install a symlinked package's dependencies — so `require("async-mqtt")`
    //   at the top of `@saltcorn/mqtt` throws and the module does not load at
    //   all. `--install-links` copies it in instead, and the dependencies come
    //   with it.
    // - a v1 plugin depends on `@saltcorn/data`, which is v1's whole server —
    //   270 MB of the program this one replaces, which the host stubs before a
    //   line of it is read. The project's npm `overrides` redirect it to a stub,
    //   and npm honours those for a copied package where it ignores them for a
    //   linked one.
    let root = temp_root("host-linked-deps");
    let installer = Installer::new(&root);

    // A checkout that depends on another package, written here rather than
    // checked in because the dependency has to be named by an **absolute** path
    // for the install to work offline.
    let checkout = root.join("src/needs-dep");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(
        checkout.join("package.json"),
        format!(
            r#"{{"name":"@saltcorn-test/needs-dep","version":"1.0.0","main":"index.js",
                 "dependencies":{{"@saltcorn-test/dep":"file:{}",
                                  "@saltcorn/data":"^1.0.0"}}}}"#,
            fixture("dep-module").display()
        ),
    )
    .unwrap();
    std::fs::write(
        checkout.join("index.js"),
        r#"const { shout } = require("@saltcorn-test/dep");
           const Table = require("@saltcorn/data/models/table");
           module.exports = {
             sc_plugin_api_version: 1,
             actions: () => ({ shout: { run: async ({ row }) => shout(row.what) } }),
           };"#,
    )
    .unwrap();

    let package = installer
        .install(ModuleSource::Local, &checkout.display().to_string())
        .await
        .unwrap();
    assert_eq!(package.name, "@saltcorn-test/needs-dep");
    // The dependency is in the modules root…
    assert!(installer.is_installed("@saltcorn-test/dep"));
    // …and what is installed for `@saltcorn/data` is the **stub**, not v1's
    // server: present, so the dependency resolves, and empty, because nothing
    // ever loads it.
    let v1 = std::fs::read_to_string(installer.package_dir("@saltcorn/data").join("package.json"))
        .expect("the v1 API dependency resolves to something");
    assert!(
        v1.contains("999.0.0"),
        "the real @saltcorn/data was installed: {v1}"
    );

    let host = ModuleHost::new(&root);
    let dir = installer.package_dir(&package.name);
    host.load(&package.name, &dir, &json!({})).await.unwrap();
    let shouted = host
        .run(
            &package.name,
            "shout",
            json!({ "row": { "what": "hello" } }),
        )
        .await
        .unwrap();
    assert_eq!(shouted, json!("HELLO!"));

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_module_whose_own_dependency_is_missing_says_which_one() {
    skip_without!(
        have_node() && have_npm(),
        "node and npm are not both on the PATH"
    );
    let root = temp_root("host-missing-dep");
    let installer = Installer::new(&root);

    // A checkout declaring nothing and requiring something — what a module with
    // an **undeclared** dependency looks like (v1 plugins get away with it
    // because v1's own server hoists the package; here nothing does).
    let checkout = root.join("src/undeclared");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(
        checkout.join("package.json"),
        r#"{"name":"@saltcorn-test/undeclared","version":"1.0.0","main":"index.js"}"#,
    )
    .unwrap();
    std::fs::write(
        checkout.join("index.js"),
        r#"require("some-package-nobody-installed");
           module.exports = { actions: () => ({}) };"#,
    )
    .unwrap();

    let package = installer
        .install(ModuleSource::Local, &checkout.display().to_string())
        .await
        .unwrap();
    let host = ModuleHost::new(&root);
    let err = host
        .load(
            &package.name,
            &installer.package_dir(&package.name),
            &json!({}),
        )
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("some-package-nobody-installed"), "{msg}");
    // And what to do about it, which node's own message does not say.
    assert!(msg.contains("npm install"), "{msg}");

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn reloading_a_module_picks_up_a_new_configuration() {
    skip_without!(
        have_node() && have_npm(),
        "node and npm are not both on the PATH"
    );
    let root = temp_root("host-reload");
    let installer = Installer::new(&root);
    let package = installer
        .install(
            ModuleSource::Local,
            &fixture("echo-module").display().to_string(),
        )
        .await
        .unwrap();
    let host = ModuleHost::new(&root);
    let dir = installer.package_dir(&package.name);

    host.load(&package.name, &dir, &json!({ "endpoint": "first" }))
        .await
        .unwrap();
    host.load(&package.name, &dir, &json!({ "endpoint": "second" }))
        .await
        .unwrap();

    let result = host
        .run(
            &package.name,
            "echo_row",
            json!({ "configuration": { "greeting": "x" } }),
        )
        .await
        .unwrap();
    assert_eq!(result["module_config"]["endpoint"], json!("second"));

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}

/// The milestone's definition of done, against the **real** `@saltcorn/mqtt`
/// from the npm registry.
///
/// `#[ignore]` because it reaches the network, which no test in this workspace
/// does by default: run it with `cargo test -p sc-module --test host -- --ignored
/// --nocapture` when changing the installer. What it pins is the thing the
/// fixtures cannot — that a v1 plugin written years ago, with `@saltcorn/data`
/// in its dependencies and `async-mqtt` underneath it, installs and loads here.
#[tokio::test]
#[ignore = "reaches the npm registry"]
async fn the_real_mqtt_module_installs_from_npm_and_supplies_its_action() {
    skip_without!(
        have_node() && have_npm(),
        "node and npm are not both on the PATH"
    );
    let root = temp_root("host-real-mqtt");
    let installer = Installer::new(&root);

    let package = installer
        .install(ModuleSource::Npm, "@saltcorn/mqtt")
        .await
        .unwrap();
    assert_eq!(package.name, "@saltcorn/mqtt");

    let host = ModuleHost::new(&root);
    let manifest = host
        .load(
            &package.name,
            &installer.package_dir(&package.name),
            &json!({ "broker_url": "mqtt://localhost" }),
        )
        .await
        .unwrap();

    // One action, with the Channel setting the trigger form renders.
    assert_eq!(manifest.actions.len(), 1);
    assert_eq!(manifest.actions[0].name, "mqtt_publish");
    assert_eq!(
        manifest.actions[0].config_fields[0]["name"],
        json!("channel")
    );
    // Its own configuration form, eleven fields of it, read out of the v1
    // `configuration_workflow`.
    assert!(
        manifest.config_fields.len() >= 10,
        "{:?}",
        manifest.config_fields
    );
    assert_eq!(manifest.config_fields[0]["name"], json!("broker_url"));
    // And the event type it also supplies, which this version does not load.
    assert!(
        manifest.unsupported.iter().any(|e| e.key == "eventTypes"),
        "{:?}",
        manifest.unsupported
    );
    // v1's server was **not** downloaded: the dependency resolved to the stub.
    let v1 = std::fs::read_to_string(installer.package_dir("@saltcorn/data").join("package.json"))
        .expect("the v1 API dependency resolves to something");
    assert!(
        v1.contains("999.0.0"),
        "the real @saltcorn/data was installed"
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}
