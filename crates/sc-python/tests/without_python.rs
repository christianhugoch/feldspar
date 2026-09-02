//! What a server built **without** `python-host` does, which is answer plainly.
//!
//! This is the build every other crate in the workspace links, and the one the
//! release tarball ships, so the behaviour under test here is the common case
//! rather than a degraded one: `run_python_code` is still registered, a trigger
//! configured with a Python body is still meaningful, and firing it says why it
//! cannot run instead of failing as a missing action.

use sc_expr::{CodeAdapter, CodeCall};
use sc_python::{PythonRuntime, PythonState};

#[tokio::test]
async fn every_entry_point_says_the_build_has_no_python() {
    let runtime = PythonRuntime::new();
    assert_eq!(runtime.state(), PythonState::NotBuilt);
    assert_eq!(runtime.language(), "python");

    let error = runtime
        .run(CodeCall {
            code: "return 1".to_owned(),
            ..CodeCall::default()
        })
        .await
        .expect_err("a build without Python must not pretend to run one");
    let said = error.to_string();
    assert!(said.contains("built without Python support"), "{said}");
    // The remedy is named, because no flag and no restart changes this one.
    assert!(said.contains("--features python"), "{said}");

    let error = runtime
        .initialise()
        .expect_err("there is no interpreter to initialise");
    assert!(
        error.to_string().contains("built without Python support"),
        "{error}"
    );
    assert_eq!(runtime.stuck(), 0);
    assert_eq!(runtime.threads(), 0);
}
