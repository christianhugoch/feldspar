//! The **in-process** module host: a v1 plugin loaded and run on a Deno worker
//! in this process, with no `node` anywhere near it (TODO "Modules in-process",
//! phases 1 and 2).
//!
//! `npm` is still needed to *install* a module and these tests skip without it.
//! `node` is not: every assertion below runs on a `deno_runtime` worker inside
//! the test binary, which is the milestone's whole claim.

#![cfg(feature = "deno-host")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;

use std::time::Duration;

use common::{have_npm, installed_on_deno};
use sc_module::PoolBounds;
use serde_json::json;

/// The default bounds, which is what a server runs.
fn default_bounds() -> PoolBounds {
    PoolBounds::default()
}

#[tokio::test]
async fn a_module_loads_and_runs_in_this_process() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (installer, host, names) =
        installed_on_deno("deno-load", &["echo-module"], 1, default_bounds()).await;
    let name = &names[0];

    let manifest = host
        .load(
            name,
            &installer.package_dir(name),
            &json!({ "endpoint": "e" }),
        )
        .await
        .unwrap();

    // The same manifest the sidecar answers — phase 0 diffed these byte for
    // byte against `node`'s, and this is the assertion that keeps them equal.
    assert_eq!(manifest.name, *name);
    assert_eq!(manifest.api_version, Some(1));
    assert_eq!(manifest.plugin_name.as_deref(), Some("echo"));
    let actions: Vec<&str> = manifest.actions.iter().map(|a| a.name.as_str()).collect();
    assert!(actions.contains(&"echo_row"), "{actions:?}");
    assert_eq!(manifest.config_fields.len(), 2);
    let census: Vec<&str> = manifest
        .unsupported
        .iter()
        .map(|e| e.key.as_str())
        .collect();
    assert!(census.contains(&"viewtemplates"), "{census:?}");
    assert!(manifest.issues.is_empty(), "{:?}", manifest.issues);

    // And the action runs, with v1's argument object and the module's own
    // configuration closed over by `actions(cfg)`.
    let value = host
        .run(
            name,
            "echo_row",
            json!({ "row": { "id": 7 }, "configuration": { "greeting": "hi" } }),
        )
        .await
        .unwrap();
    assert_eq!(value["greeting"], json!("hi"));
    assert_eq!(value["row"]["id"], json!(7));
    assert_eq!(value["module_config"]["endpoint"], json!("e"));

    // `require` reached the modules root's own `node_modules` through byonm,
    // and the `@saltcorn/*` stubs are the same three tiers: a call into one of
    // them throws, naming itself, rather than quietly answering `undefined`.
    let err = host
        .run(name, "echo_missing_api", json!({}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("models/table.findOne"), "{err}");

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

/// Phase 2: a module's `console.*` is the server's log, not a forwarded stderr.
///
/// What is asserted here is the **seam**, which is what a test can see from
/// outside the worker thread: `console.log` and its siblings resolve to the
/// native `__scLog` the host installed, and calling four of them formats and
/// logs without throwing. Were the global missing, or its signature wrong, the
/// action would fail with a `ReferenceError` instead of answering — which is
/// exactly what this asserts it does not do. (The lines themselves go to
/// `sc_log` from the worker's own thread, and `sc_log`'s capture is
/// thread-local, so the *text* is not reachable from here.)
#[tokio::test]
async fn a_modules_console_log_goes_to_the_log_and_not_to_a_pipe() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (installer, host, names) =
        installed_on_deno("deno-log", &["echo-module"], 1, default_bounds()).await;
    let name = &names[0];
    host.load(name, &installer.package_dir(name), &json!({}))
        .await
        .unwrap();

    let value = host
        .run(
            name,
            "echo_log",
            json!({ "configuration": { "greeting": "hello" } }),
        )
        .await
        .unwrap();
    assert_eq!(value, json!("logged"));

    // And the worker is still serving afterwards: a log line is not a call, and
    // nothing about it can be mistaken for one now that there is no stream for
    // it to corrupt.
    assert_eq!(host.ping().await.unwrap()["pong"], json!(true));

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

/// §4a's third limit, one milestone early because the seam is where it lands: a
/// result that will not serialise is a failure naming why, not a mangled value.
#[tokio::test]
async fn a_result_that_is_not_json_fails_with_a_sentence() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (installer, host, names) =
        installed_on_deno("deno-cycle", &["echo-module"], 1, default_bounds()).await;
    let name = &names[0];
    host.load(name, &installer.package_dir(name), &json!({}))
        .await
        .unwrap();

    let err = host.run(name, "echo_cycle", json!({})).await.unwrap_err();
    assert!(
        err.to_string().contains("not JSON"),
        "the failure should name what went wrong: {err}"
    );
    // The module's fault, and the worker is untouched by it.
    assert_eq!(err.kind(), sc_error::ErrorKind::Application);
    assert_eq!(
        host.run(name, "echo_row", json!({ "configuration": {} }))
            .await
            .unwrap()["row"],
        json!(null)
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

/// The pool is what runs, and nothing else is: no `node` child exists while a
/// module is loaded and answering.
#[tokio::test]
async fn no_node_process_is_involved() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (installer, host, names) =
        installed_on_deno("deno-nonode", &["echo-module"], 1, default_bounds()).await;
    let name = &names[0];
    host.load(name, &installer.package_dir(name), &json!({}))
        .await
        .unwrap();

    // The host answers, so a module is resident…
    let pong = host.ping().await.unwrap();
    assert_eq!(pong["pong"], json!(true));
    // …and nothing is running the sidecar's script. Not "no children at all":
    // the other tests in this binary are running `npm install` beside this one,
    // and npm is a node process nobody claimed to have removed — this milestone
    // takes `node` out of *running* a module, not out of installing one.
    let sidecars: Vec<String> = children_of_this_process()
        .into_iter()
        .filter(|child| child.contains(sc_module::host::HOST_SCRIPT_NAME))
        .collect();
    assert!(
        sidecars.is_empty(),
        "a module sidecar is running: {sidecars:?}"
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

/// §4: `process.exit()` costs the module's worker and nothing else.
#[tokio::test]
async fn a_module_that_exits_loses_its_call_and_leaves_the_server_up() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (installer, host, names) =
        installed_on_deno("deno-exit", &["echo-module"], 1, default_bounds()).await;
    let name = &names[0];
    host.load(
        name,
        &installer.package_dir(name),
        &json!({ "endpoint": "e" }),
    )
    .await
    .unwrap();

    // The exiting call is lost, and it is lost **by name** rather than by
    // waiting out the 120-second wall clock.
    let err = host.run(name, "echo_exit", json!({})).await.unwrap_err();
    assert!(
        err.to_string().contains("process.exit"),
        "the failure should name what happened: {err}"
    );

    // The next call gets a fresh worker with the load replayed into it — the
    // module is back without anybody having reinstalled or reconfigured it, and
    // it still has the configuration it was loaded with.
    let value = host
        .run(
            name,
            "echo_row",
            json!({ "row": {}, "configuration": { "greeting": "again" } }),
        )
        .await
        .unwrap();
    assert_eq!(value["greeting"], json!("again"));
    assert_eq!(value["module_config"]["endpoint"], json!("e"));

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

/// The JS slice, with the bound turned down so a test can wait for it: a module
/// that never yields is stopped, named, and costs its own worker.
#[tokio::test]
async fn a_runaway_module_is_stopped_by_the_js_slice() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let bounds = PoolBounds {
        // Long enough that `require`ing the fixture is not mistaken for a
        // runaway, short enough that a test may wait for it.
        js_slice: Duration::from_millis(1500),
        // And a wall clock well under the default two minutes, so that a
        // regression here is a failing test rather than a two-minute one: the
        // slice is what must stop this call, not the caller giving up.
        timeout: Duration::from_secs(20),
        ..PoolBounds::default()
    };
    let (installer, host, names) =
        installed_on_deno("deno-spin", &["echo-module"], 1, bounds).await;
    let name = &names[0];
    let manifest = host
        .load(name, &installer.package_dir(name), &json!({}))
        .await
        .unwrap();
    let actions: Vec<&str> = manifest.actions.iter().map(|a| a.name.as_str()).collect();
    assert!(actions.contains(&"echo_spin"), "{actions:?}");

    let err = host.run(name, "echo_spin", json!({})).await.unwrap_err();
    assert!(
        err.to_string().contains("without yielding"),
        "the failure should name the slice: {err}"
    );
    // The worker is replaced and the module replayed: a runaway costs its own
    // worker and no more than that.
    let pong = host.ping().await.unwrap();
    assert_eq!(pong["pong"], json!(true));
    let value = host
        .run(
            name,
            "echo_row",
            json!({ "row": {}, "configuration": { "greeting": "still here" } }),
        )
        .await
        .unwrap();
    assert_eq!(value["greeting"], json!("still here"));

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

/// Two modules, two workers: a module is pinned to one worker for its lifetime,
/// and an exit on one worker does not reach a module on the other.
#[tokio::test]
async fn a_module_on_another_worker_does_not_notice() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (installer, host, names) = installed_on_deno(
        "deno-pinned",
        &["echo-module", "clash-module"],
        2,
        default_bounds(),
    )
    .await;
    for name in &names {
        host.load(name, &installer.package_dir(name), &json!({}))
            .await
            .unwrap();
    }
    let echo = names
        .iter()
        .find(|n| n.contains("echo"))
        .expect("the echo fixture");
    let other = names
        .iter()
        .find(|n| *n != echo)
        .expect("the other fixture");
    assert_ne!(
        host.worker_of(echo).await,
        host.worker_of(other).await,
        "two modules and two workers should not share one"
    );

    let _ = host.run(echo, "echo_exit", json!({})).await.unwrap_err();
    // The other worker never saw it: its module answers straight away, with no
    // restart and no replay in between.
    assert_eq!(
        host.run(other, "clash_ok", json!({})).await.unwrap(),
        json!("fine")
    );
    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

/// An unloaded module is forgotten by the replay table too: it must not come
/// back when the worker it was on is restarted under it.
#[tokio::test]
async fn an_unloaded_module_is_not_replayed() {
    skip_without!(have_npm(), "npm is not on the PATH");
    // One worker on purpose: the point is that the *shared* worker's restart
    // does not resurrect what was uninstalled.
    let (installer, host, names) = installed_on_deno(
        "deno-unload",
        &["echo-module", "clash-module"],
        1,
        default_bounds(),
    )
    .await;
    for name in &names {
        host.load(name, &installer.package_dir(name), &json!({}))
            .await
            .unwrap();
    }
    let echo = names
        .iter()
        .find(|n| n.contains("echo"))
        .expect("the echo fixture");
    let gone = names
        .iter()
        .find(|n| *n != echo)
        .expect("the other fixture");

    host.unload(gone).await;
    assert_eq!(host.worker_of(gone).await, None);

    // Take the worker down and let the next call rebuild it. What comes back is
    // the module that is still installed, and only that one.
    let _ = host.run(echo, "echo_exit", json!({})).await.unwrap_err();
    assert_eq!(
        host.run(echo, "echo_row", json!({ "row": {}, "configuration": {} }))
            .await
            .unwrap()["row"],
        json!({})
    );
    let err = host.run(gone, "clash_ok", json!({})).await.unwrap_err();
    assert!(err.to_string().contains("not loaded"), "{err}");

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

/// This process's children, as their full command lines, read out of `/proc`.
#[cfg(target_os = "linux")]
fn children_of_this_process() -> Vec<String> {
    let pid = std::process::id();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut children = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(status) = std::fs::read_to_string(path.join("status")) else {
            continue;
        };
        let parent = status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:"))
            .and_then(|v| v.trim().parse::<u32>().ok());
        if parent != Some(pid) {
            continue;
        }
        // The command line rather than the name: `comm` is truncated to fifteen
        // characters, and what identifies a sidecar is its script's path.
        let command = std::fs::read_to_string(path.join("cmdline"))
            .unwrap_or_default()
            .replace('\0', " ");
        children.push(command);
    }
    children
}

#[cfg(not(target_os = "linux"))]
fn children_of_this_process() -> Vec<String> {
    // No portable process table; the Linux assertion is the one that matters,
    // and it is where CI runs.
    Vec::new()
}
