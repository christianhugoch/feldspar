//! The build script's one decision: does `cargo build` also build the admin UI
//! and the IDE (`ui/admin`, `ui/ide`)?
//!
//! It is on by default and turned off by `SC_BUILD_ADMIN=0|false|False|FALSE`.
//! Getting that predicate wrong is expensive in both directions — a build that
//! silently ships no admin UI, or a Rust-only CI job that suddenly needs a Node
//! toolchain — and neither shows up as a compile error, so it is asserted here.
//!
//! `build.rs` is not compiled as a test target by cargo, so it is pulled in here
//! as a module: the file has no dependencies beyond `std`, and this way the
//! function under test is literally the one the build runs, not a copy of it.
#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;

use build_script::build_requested;

#[test]
fn unset_builds_the_admin_ui() {
    assert!(build_requested(None));
}

#[test]
fn the_four_falsey_spellings_turn_it_off() {
    for value in ["0", "false", "False", "FALSE"] {
        assert!(
            !build_requested(Some(value)),
            "SC_BUILD_ADMIN={value} should disable the UI build"
        );
    }
}

#[test]
fn anything_else_builds() {
    // Including the values that used to be the opt-in, an empty value, and a
    // misspelling: the safe direction for an unrecognised value is the complete
    // binary, because a binary missing its admin UI is the harder failure to spot.
    for value in ["1", "true", "yes", "", "flase", "no", "off"] {
        assert!(
            build_requested(Some(value)),
            "SC_BUILD_ADMIN={value:?} should leave the UI build on"
        );
    }
}
