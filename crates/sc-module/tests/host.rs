//! The module host through its own façade: the manifest, the stubs, the
//! configuration, and what a dead worker does to the calls that were in flight.
//!
//! `npm` is still needed to *install* a module and these tests skip without it.
//! `node` is not, and that is the milestone's whole claim: since the "Modules
//! in-process" milestone's phase 2 every assertion here runs on a
//! `deno_runtime` worker inside the test binary. The only edit these suites
//! needed was that skip condition.

#![cfg(feature = "deno-host")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
use crate::common;

use common::{closed, fixture, have_npm, installed, temp_root};
use sc_module::{CallHosts, Installer, ModuleHost, ModuleSource};
use serde_json::json;

#[tokio::test]
async fn a_module_loads_and_reports_what_it_supplies() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (installer, host, names) = installed("host-load", &["echo-module"]).await;
    let name = &names[0];

    let manifest = host
        .load(name, &installer.package_dir(name), &json!({}), &closed())
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
    assert!(census.contains(&("types", Some(2))), "{census:?}");
    // `viewtemplates` left it with Saltcorn UI (TODO "Saltcorn UI" 11.1): a
    // module's view patterns are loaded now.
    assert!(
        !census.iter().any(|(key, _)| *key == "viewtemplates" || *key == "headers"),
        "{census:?}"
    );
    // `table_providers` left the census when it started being loaded (§8.3), so
    // its absence here is the assertion that it is no longer "what you are not
    // getting".
    assert!(
        !census.iter().any(|(key, _)| *key == "table_providers"),
        "{census:?}"
    );
    let providers: Vec<&str> = manifest
        .table_providers
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(
        providers,
        ["echo_rows", "echo_writable", "echo_calls"],
        "{providers:?}"
    );
    assert!(manifest.issues.is_empty(), "{:?}", manifest.issues);

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

#[tokio::test]
async fn an_action_runs_and_gets_v1s_argument_object() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (installer, host, names) = installed("host-run", &["echo-module"]).await;
    let name = &names[0];
    host.load(
        name,
        &installer.package_dir(name),
        &json!({ "endpoint": "https://example.com" }),
        &closed(),
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
            CallHosts::default(),
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
    skip_without!(have_npm(), "npm is not on the PATH");
    let (installer, host, names) = installed("host-stubs", &["echo-module"]).await;
    let name = &names[0];
    // Loading at all is the first half of the assertion: the fixture requires
    // `@saltcorn/data/models/table` and `@saltcorn/data/db/state` at its top.
    host.load(name, &installer.package_dir(name), &json!({}), &closed())
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
            CallHosts::default(),
        )
        .await
        .unwrap();
    assert_eq!(interpolated, json!("vm1-7"));

    // Calling one that is not implemented fails by name, rather than answering
    // `undefined` and computing the wrong thing.
    let err = host
        .run(name, "echo_missing_api", json!({}), CallHosts::default())
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("findOne"), "{msg}");
    assert!(msg.contains("not available"), "{msg}");
    // The module's fault to the admin, not Saltcorn's (§16's split).
    assert_eq!(err.kind(), sc_error::ErrorKind::Application);

    let err = host
        .run(name, "echo_state", json!({}), CallHosts::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("getState"), "{err}");

    // `Table` is **not** on that tier any more — it is the real v1 class — but a
    // call given no surfaces has no authority to lend it, and that is what it
    // says. Named rather than answered with nothing, for the same reason.
    let err = host
        .run(
            name,
            "echo_table_no_caller",
            json!({}),
            CallHosts::default(),
        )
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("Table.findOne"), "{msg}");
    assert!(msg.contains("authority of the call"), "{msg}");

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

#[tokio::test]
async fn a_module_that_throws_is_an_ordinary_failed_call() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (installer, host, names) = installed("host-throw", &["echo-module"]).await;
    let name = &names[0];
    host.load(name, &installer.package_dir(name), &json!({}), &closed())
        .await
        .unwrap();

    let err = host
        .run(name, "echo_throw", json!({}), CallHosts::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("the module said no"), "{err}");

    // …and the host is still serving.
    assert_eq!(
        host.run(
            name,
            "echo_row",
            json!({ "configuration": { "greeting": "still here" } }),
            CallHosts::default()
        )
        .await
        .unwrap()["greeting"],
        json!("still here")
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

#[tokio::test]
async fn a_module_that_kills_its_worker_fails_its_call_and_the_next_one_works() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (installer, host, names) = installed("host-crash", &["echo-module"]).await;
    let name = &names[0];
    host.load(name, &installer.package_dir(name), &json!({}), &closed())
        .await
        .unwrap();

    let err = host
        .run(name, "echo_exit", json!({}), CallHosts::default())
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("process.exit"),
        "the caller should be told what happened: {err}"
    );

    // The next call starts a new worker and replays the load, so the module is
    // back without anybody asking for it.
    let result = host
        .run(
            name,
            "echo_row",
            json!({ "configuration": { "greeting": "restarted" } }),
            CallHosts::default(),
        )
        .await
        .unwrap();
    assert_eq!(result["greeting"], json!("restarted"));

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

#[tokio::test]
async fn a_module_that_cannot_be_required_is_reported_and_the_host_stays_up() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (installer, host, names) =
        installed("host-broken", &["broken-module", "echo-module"]).await;
    let broken = &names[0];
    let echo = &names[1];

    let err = host
        .load(
            broken,
            &installer.package_dir(broken),
            &json!({}),
            &closed(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("cannot be loaded"), "{err}");

    // The good module still loads on the same worker.
    let manifest = host
        .load(echo, &installer.package_dir(echo), &json!({}), &closed())
        .await
        .unwrap();
    assert!(!manifest.actions.is_empty());

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

#[tokio::test]
async fn a_host_over_an_empty_root_still_answers_a_ping() {
    // No npm, no install, no modules, and nothing on the PATH: the host script
    // is written from the binary and a worker in this process evaluates it. This
    // is the check a diagnostics screen makes.
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
    skip_without!(have_npm(), "npm is not on the PATH");
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
    host.load(&package.name, &dir, &json!({}), &closed())
        .await
        .unwrap();
    let shouted = host
        .run(
            &package.name,
            "shout",
            json!({ "row": { "what": "hello" } }),
            CallHosts::default(),
        )
        .await
        .unwrap();
    assert_eq!(shouted, json!("HELLO!"));

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_module_whose_own_dependency_is_missing_says_which_one() {
    skip_without!(have_npm(), "npm is not on the PATH");
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
            &closed(),
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
    skip_without!(have_npm(), "npm is not on the PATH");
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

    host.load(
        &package.name,
        &dir,
        &json!({ "endpoint": "first" }),
        &closed(),
    )
    .await
    .unwrap();
    host.load(
        &package.name,
        &dir,
        &json!({ "endpoint": "second" }),
        &closed(),
    )
    .await
    .unwrap();

    let result = host
        .run(
            &package.name,
            "echo_row",
            json!({ "configuration": { "greeting": "x" } }),
            CallHosts::default(),
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
    skip_without!(have_npm(), "npm is not on the PATH");
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
            &closed(),
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

/// The milestone's **definition of done**, against a real broker: a wired
/// `mqtt_publish` publishes, and a subscriber sees the message.
///
/// Skipped unless a broker is configured, because a test suite does not get to
/// require a message broker. Point `SC_TEST_MQTT_BROKER` at one — for example
/// `SC_TEST_MQTT_BROKER=mqtt://127.0.0.1:1883` — and this runs; it reaches the
/// npm registry too, which is the same opt-in.
///
/// What it pins that the fixtures cannot is the whole road at once: npm
/// installs a v1 plugin written years ago, `require("async-mqtt")` resolves out
/// of the modules root through byonm, `onLoad` opens a **real socket** from
/// inside a Deno worker that was granted exactly that one host and nothing
/// else, and the action publishes through the client `onLoad` left behind. With
/// no `node` running any of it.
///
/// The subscriber is thirty lines of MQTT 3.1.1 over a `TcpStream` rather than a
/// crate: the assertion is that a byte arrived at a broker, and a dependency
/// that only a skipped test would use is a dependency the workspace should not
/// carry.
#[tokio::test]
async fn the_real_mqtt_module_publishes_to_a_real_broker() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let Ok(broker) = std::env::var("SC_TEST_MQTT_BROKER") else {
        eprintln!("skipping: SC_TEST_MQTT_BROKER names no broker");
        return;
    };
    let (broker_host, broker_port) = split_broker(&broker);
    let topic = format!("saltcorn/test/{}", std::process::id());

    // Subscribed *before* the module publishes, because a QoS 0 message nobody
    // is listening for is a message that never existed.
    let mut subscriber = mqtt_subscriber(&broker_host, broker_port, &topic);

    let root = temp_root("host-mqtt-broker");
    let installer = Installer::new(&root);
    let package = installer
        .install(ModuleSource::Npm, "@saltcorn/mqtt")
        .await
        .unwrap();

    let host = ModuleHost::new(&root);
    let manifest = host
        .load(
            &package.name,
            &installer.package_dir(&package.name),
            &json!({
                "broker_url": broker_host.clone(),
                "protocol": "mqtt",
                "port": broker_port,
                // A channel nobody publishes on. `subscribe_channels` is
                // required by the module's own settings form, and pointing it
                // at the topic under test would have the module receive its own
                // message and hand it to the `Trigger.emitEvent` stub, which
                // throws by design (§15.1's third tier). What is under test is
                // the publish.
                "subscribe_channels": format!("{topic}/inbox"),
            }),
            // The one thing it may reach. Everything else — the filesystem, the
            // environment, a second broker — is closed, and this is the module
            // the sidecar could never have said that about.
            &sc_module::ModulePermissions {
                net: vec![format!("{broker_host}:{broker_port}")],
                ..sc_module::ModulePermissions::closed()
            },
        )
        .await
        .unwrap();
    assert!(
        manifest.issues.is_empty(),
        "onLoad connected without complaint: {:?}",
        manifest.issues
    );

    // The client connects asynchronously; mqtt.js queues a publish made before
    // it is up, but the broker cannot deliver one that has not been sent.
    tokio::time::sleep(std::time::Duration::from_millis(750)).await;
    host.run(
        &package.name,
        "mqtt_publish",
        json!({
            "row": { "id": 7, "title": "Dune" },
            "configuration": { "channel": topic.clone() },
        }),
        CallHosts::default(),
    )
    .await
    .unwrap();

    let (seen_topic, payload) = read_publish(&mut subscriber);
    assert_eq!(seen_topic, topic);
    let row: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(row["title"], json!("Dune"));

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}

/// `mqtt://host:port`, `host:port` or `host` — the host and the port a broker is
/// on, defaulting to MQTT's own 1883.
fn split_broker(url: &str) -> (String, u16) {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = rest.split('/').next().unwrap_or(rest);
    match rest.rsplit_once(':') {
        Some((host, port)) => (host.to_owned(), port.parse().unwrap_or(1883)),
        None => (rest.to_owned(), 1883),
    }
}

/// A connected MQTT 3.1.1 subscriber to one topic, at QoS 0.
fn mqtt_subscriber(host: &str, port: u16, topic: &str) -> std::net::TcpStream {
    use std::io::Write;

    let mut stream = std::net::TcpStream::connect((host, port))
        .unwrap_or_else(|e| panic!("connecting to the broker at {host}:{port}: {e}"));
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();

    // CONNECT: protocol name, level 4, clean session, a 60-second keepalive and
    // a client id nobody else on the broker will be using.
    let client_id = format!("saltcorn-test-{}", std::process::id());
    let mut body = vec![0x00, 0x04, b'M', b'Q', b'T', b'T', 0x04, 0x02, 0x00, 0x3C];
    put_string(&mut body, &client_id);
    stream.write_all(&packet(0x10, &body)).unwrap();
    let (kind, connack) = read_packet(&mut stream);
    assert_eq!(kind & 0xF0, 0x20, "the broker did not answer CONNACK");
    assert_eq!(
        connack.get(1),
        Some(&0),
        "the broker refused the connection"
    );

    // SUBSCRIBE, packet id 1, QoS 0.
    let mut body = vec![0x00, 0x01];
    put_string(&mut body, topic);
    body.push(0x00);
    stream.write_all(&packet(0x82, &body)).unwrap();
    let (kind, _) = read_packet(&mut stream);
    assert_eq!(kind & 0xF0, 0x90, "the broker did not answer SUBACK");
    stream
}

/// The next PUBLISH the broker sends: its topic and its payload as text.
///
/// Anything else the broker says on the way — a PINGRESP, a retained message on
/// another topic — is skipped rather than mistaken for the answer.
fn read_publish(stream: &mut std::net::TcpStream) -> (String, String) {
    for _ in 0..8 {
        let (kind, body) = read_packet(stream);
        if kind & 0xF0 != 0x30 {
            continue;
        }
        let length = usize::from(u16::from_be_bytes([body[0], body[1]]));
        let topic = String::from_utf8_lossy(&body[2..2 + length]).into_owned();
        // QoS 0 only, so there is no packet identifier between the two.
        let payload = String::from_utf8_lossy(&body[2 + length..]).into_owned();
        return (topic, payload);
    }
    panic!("the broker sent no PUBLISH for the module's message");
}

/// A length-prefixed UTF-8 string, as every MQTT string is.
fn put_string(into: &mut Vec<u8>, text: &str) {
    let bytes = text.as_bytes();
    let length = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
    into.extend_from_slice(&length.to_be_bytes());
    into.extend_from_slice(bytes);
}

/// A whole packet: the type byte, the remaining length as a varint, the body.
fn packet(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![kind];
    let mut remaining = body.len();
    loop {
        let mut byte = u8::try_from(remaining % 128).unwrap_or(0);
        remaining /= 128;
        if remaining > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if remaining == 0 {
            break;
        }
    }
    out.extend_from_slice(body);
    out
}

/// The next whole packet off the wire: its type byte and its body.
fn read_packet(stream: &mut std::net::TcpStream) -> (u8, Vec<u8>) {
    use std::io::Read;

    let mut one = [0u8; 1];
    stream.read_exact(&mut one).expect("the broker hung up");
    let kind = one[0];
    let mut remaining = 0usize;
    let mut shift = 1usize;
    loop {
        stream.read_exact(&mut one).expect("the broker hung up");
        remaining += usize::from(one[0] & 0x7F) * shift;
        if one[0] & 0x80 == 0 {
            break;
        }
        shift *= 128;
    }
    let mut body = vec![0u8; remaining];
    stream.read_exact(&mut body).expect("the broker hung up");
    (kind, body)
}

#[tokio::test]
async fn a_table_provider_that_cannot_serve_a_table_is_reported_and_skipped() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (installer, host, names) = installed("host-no-rows", &["no-rows-module"]).await;
    let name = &names[0];

    let manifest = host
        .load(name, &installer.package_dir(name), &json!({}), &closed())
        .await
        .unwrap();

    // `get_table` is the one method that produces rows, so a provider without it
    // is a table nobody could read. Reported and skipped — never offered, and
    // never fatal: the module loaded, and its action is there.
    assert!(manifest.table_providers.is_empty());
    assert_eq!(manifest.issues.len(), 1, "{:?}", manifest.issues);
    assert!(
        manifest.issues[0].contains("no_rows"),
        "{:?}",
        manifest.issues
    );
    assert!(
        manifest.issues[0].contains("get_table"),
        "{:?}",
        manifest.issues
    );
    assert_eq!(manifest.actions.len(), 1);

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}
