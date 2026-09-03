//! Phase 4.1 of the Python code adapter: what a body may import.
//!
//! The rules are §1's — the standard library minus the modules that reach the
//! process, the network and the disk, plus every package installed in this
//! server's environment, with `os` allowed for `os.path` and `os.environ` not
//! readable — and they are asserted here through the runtime, because the gate
//! is a property of a *run*: it is the `__builtins__` a run's globals carry, so
//! there is nothing to test that is not a body importing something.
//!
//! **The last two tests are about what the gate is not.** §1 and §10 both say
//! it is hygiene rather than privilege, and a test suite that only asserted the
//! refusals would read as though this were a sandbox. So `open` being there, and
//! a library's own imports going straight past the gate, are pinned as
//! deliberate behaviour rather than left as gaps somebody would later "fix".

use std::time::Duration;

use sc_expr::CodeCall;
use sc_python::{PythonEnv, PythonRuntime, PythonState};
use serde_json::{Value as Json, json};

/// Run one body with no hosts — the gate needs none of them.
async fn run(code: &str) -> Result<Json, String> {
    PythonRuntime::new()
        .run(CodeCall {
            code: code.to_owned(),
            timeout: Some(Duration::from_secs(5)),
            ..CodeCall::default()
        })
        .await
        .map_err(|e| e.to_string())
}

/// The body's failure, for the tests that are about a refusal.
async fn refusal(code: &str) -> String {
    run(code).await.expect_err("that should have been refused")
}

// ---------------------------------------------------------------------------
// The deny-list
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subprocess_is_refused_by_name_and_the_rule_is_in_the_sentence() {
    let said = refusal("import subprocess\nreturn 1\n").await;
    // Named — an author reading this should not have to guess which of their
    // imports it was.
    assert!(said.contains("subprocess"), "{said}");
    // Shaped like an `ImportError` on the line that made it (§1).
    assert!(said.contains("ImportError"), "{said}");
    assert!(said.contains("line 1"), "the author's own line: {said}");
    // And saying what the rule is, rather than only that there is one.
    assert!(said.contains("reach this process"), "{said}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_network_modules_are_refused_and_point_at_fetch() {
    for code in [
        "import socket\nreturn 1\n",
        "import ssl\nreturn 1\n",
        "import http.client\nreturn 1\n",
        "from urllib import request\nreturn 1\n",
        "import urllib.request\nreturn 1\n",
    ] {
        let said = refusal(code).await;
        assert!(said.contains("fetch(url"), "{code} => {said}");
    }
}

/// A dotted entry refuses that module and what is under it, and **nothing
/// else**: `urllib.parse` is string manipulation and is what a body building a
/// query wants.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn urllib_parse_survives_the_refusal_of_urllib_request() {
    let out = run("from urllib.parse import quote\nreturn quote(\"a b\")\n")
        .await
        .unwrap();
    assert_eq!(out, json!("a%20b"));
    let out = run("import urllib.parse\nreturn urllib.parse.urlencode({\"a\": 1})\n")
        .await
        .unwrap();
    assert_eq!(out, json!("a=1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_import_machinery_and_the_threads_are_refused_for_their_own_reasons() {
    let said = refusal("import importlib\nreturn 1\n").await;
    assert!(said.contains("importlib"), "{said}");
    assert!(said.contains("import machinery"), "{said}");

    let said = refusal("import threading\nreturn 1\n").await;
    // The reason is this runtime's, not a generic one: the deadline is enforced
    // on the run's own thread, so a thread a body starts is not stoppable.
    assert!(said.contains("one thread"), "{said}");

    let said = refusal("import shutil\nreturn 1\n").await;
    assert!(said.contains("fs(store)"), "{said}");
}

// ---------------------------------------------------------------------------
// What is allowed
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_rest_of_the_standard_library_is_ordinary() {
    let out = run(
        "import math\nimport json\nimport datetime\nfrom decimal import Decimal\n\
         return [math.floor(2.7), json.dumps({\"a\": 1}), str(Decimal(\"1.5\"))]\n",
    )
    .await
    .unwrap();
    assert_eq!(out, json!([2, "{\"a\": 1}", "1.5"]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn os_is_allowed_for_os_path_and_the_environment_is_not_readable() {
    let out = run("import os\nreturn os.path.join(\"a\", \"b\")\n")
        .await
        .unwrap();
    assert_eq!(out, json!("a/b"));
    // Either spelling of the import reaches the same stand-in.
    let out = run("import os.path\nreturn os.path.basename(\"/x/y.txt\")\n")
        .await
        .unwrap();
    assert_eq!(out, json!("y.txt"));
    let out = run("from os.path import splitext\nreturn splitext(\"y.txt\")[1]\n")
        .await
        .unwrap();
    assert_eq!(out, json!(".txt"));

    // And the environment is not there, however it is reached for.
    for code in [
        "import os\nreturn os.environ.get(\"PATH\")\n",
        "from os import environ\nreturn 1\n",
        "import os\nreturn os.getenv(\"PATH\")\n",
        "import os\nreturn os.system(\"true\")\n",
        "import os\nreturn os.execv(\"/bin/true\", [])\n",
    ] {
        let said = refusal(code).await;
        assert!(
            said.contains("os.") && !said.contains("no attribute"),
            "{code} => {said}"
        );
    }
    let said = refusal("import os\nreturn os.environ[\"PATH\"]\n").await;
    assert!(said.contains("belongs in Settings"), "{said}");
}

/// §1's other half: **every package installed in this server's Python
/// environment**.
///
/// The gate has no list of distributions and does not need one —
/// `interp::isolate_path` has already taken the host's own packages off
/// `sys.path` and put this server's environment on it, so "not standard library
/// and not denied" *is* "installed here". Both halves of that are asserted: a
/// name nobody installed fails as CPython's own `ModuleNotFoundError` rather
/// than as a refusal, and a package that **is** in the environment imports.
///
/// The second half is conditional on this test having been the one that started
/// the interpreter, and that is a real property rather than a weak test: there
/// is one interpreter per process and therefore one `sys.path`, read once at
/// boot from whichever runtime got there first (`interp::boot`). In a test
/// binary that is whichever test ran first. So the body is asked what `sys.path`
/// actually holds, and the assertion follows the answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_package_installed_in_the_environment_is_importable_and_an_absent_one_is_not_a_refusal() {
    let dir = tempdir("python-env");
    // Which interpreter is linked in is not known until it has booted, and the
    // boot is what reads this path — so the fixture is laid down for every
    // version that could be the one.
    for minor in 9..=20 {
        let packages = dir
            .join("lib")
            .join(format!("python3.{minor}"))
            .join("site-packages")
            .join("pretend_numpy");
        std::fs::create_dir_all(&packages).unwrap();
        std::fs::write(packages.join("__init__.py"), "VERSION = \"2.1\"\n").unwrap();
    }
    let runtime = PythonRuntime::new().with_env(PythonEnv {
        dir: Some(dir.clone()),
        bin: None,
    });
    let call = |code: &str| {
        runtime.run(CodeCall {
            code: code.to_owned(),
            ..CodeCall::default()
        })
    };

    // First, before anything else in this test starts an interpreter with some
    // other environment.
    let path = call("import sys\nreturn sys.path\n").await.unwrap();
    let ours = path.as_array().unwrap().iter().any(|entry| {
        entry
            .as_str()
            .is_some_and(|s| s.starts_with(&*dir.to_string_lossy()))
    });

    let said = call("import numpy\nreturn 1\n")
        .await
        .expect_err("nobody installed numpy here");
    let said = said.to_string();
    assert!(
        said.contains("No module named"),
        "an absent package is absent, not refused: {said}"
    );
    assert!(
        !said.contains("may not import"),
        "the gate has no opinion about a package: {said}"
    );

    if ours {
        let out = call("import pretend_numpy\nreturn pretend_numpy.VERSION\n")
            .await
            .unwrap();
        assert_eq!(out, json!("2.1"));
    } else {
        // Another test booted the interpreter first. `sys.path` is that run's,
        // which is what having one interpreter means.
        assert!(matches!(runtime.state(), PythonState::Running { .. }));
    }
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// What the gate is not
// ---------------------------------------------------------------------------

/// §10: there is no sandbox, and this suite says so rather than implying one.
///
/// `open` is bound, the type graph is reachable, and both are deliberate. The
/// bound on a code body is that authoring one is an administrator's capability
/// — the same bound `db.sql` and installing a module have.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gate_is_hygiene_rather_than_privilege() {
    let out = run("return [callable(open), len(().__class__.__mro__)]\n")
        .await
        .unwrap();
    assert_eq!(out, json!([true, 2]));
    // And this runtime's own machinery is deliberately not on the deny-list —
    // the bridge functions check the run's surfaces for themselves, so refusing
    // the module they live in would be a rule that only looked like a boundary.
    let out = run("import __sc\nreturn __sc.DbError.__name__\n").await.unwrap();
    assert_eq!(out, json!("DbError"));
    // The surface an author actually writes.
    let out = run("import saltcorn\nreturn saltcorn.DbError.__name__\n")
        .await
        .unwrap();
    assert_eq!(out, json!("DbError"));
}

/// The gate is on the **body's** import statement and not on the interpreter's.
///
/// This is why it is a `__builtins__` rather than a `sys.meta_path` finder: a
/// library reads its own module globals, so its imports are the real ones. Both
/// modules below are ordinary standard library that a body may import, and each
/// reaches something the gate would refuse the body — `uuid` calls
/// `os.urandom`, `tempfile` imports `shutil` — and each works, because neither
/// is the body.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_library_the_body_imports_makes_its_own_imports_unhindered() {
    let out = run("import uuid\nreturn len(uuid.uuid4().hex)\n")
        .await
        .unwrap();
    assert_eq!(out, json!(32));
    let out = run("import tempfile\nreturn tempfile.gettempdir() != \"\"\n")
        .await
        .unwrap();
    assert_eq!(out, json!(true));
}

/// A run's builtins are a **copy**, so a body that writes into them has written
/// into its own and not into the next body's. One interpreter gives separate
/// globals and not separate `sys.modules` (§1), and this is the edge of that.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_a_body_puts_in_its_builtins_does_not_reach_the_next_body() {
    let runtime = PythonRuntime::new();
    let call = |code: &str| {
        runtime.run(CodeCall {
            code: code.to_owned(),
            ..CodeCall::default()
        })
    };
    call("__builtins__[\"sneaky\"] = 1\nreturn 1").await.unwrap();
    let out = call("return \"sneaky\" in __builtins__").await.unwrap();
    assert_eq!(out, json!(false));
}

/// A directory of this test's own, removed by the caller.
fn tempdir(prefix: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sc-{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
