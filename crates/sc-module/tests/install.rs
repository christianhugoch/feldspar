//! The installer, against real npm.
//!
//! Nothing here reaches the registry: every install is of a **local fixture
//! package**, which is the same code path (`npm install <path>`) with none of
//! the flakiness of somebody else's network in a test suite. The registry path
//! differs only in the specifier, and it is exercised by hand against
//! `@saltcorn/mqtt` (the milestone's definition of done).

#![allow(clippy::unwrap_used, clippy::expect_used)]
use crate::common;

use common::{fixture, have_npm, temp_root};
use sc_module::{Installer, ModuleSource};

#[tokio::test]
async fn a_local_package_installs_under_node_modules_with_its_own_name_and_version() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let root = temp_root("install-local");
    let installer = Installer::new(&root);

    let package = installer
        .install(
            ModuleSource::Local,
            &fixture("echo-module").display().to_string(),
        )
        .await
        .unwrap();

    // The name comes from the package, not from the path it was installed from.
    assert_eq!(package.name, "@saltcorn-test/echo");
    assert_eq!(package.version, "0.1.0");
    assert!(installer.is_installed("@saltcorn-test/echo"));
    assert!(
        installer
            .package_dir("@saltcorn-test/echo")
            .join("index.js")
            .exists()
    );
    // The project file this crate writes, with the module in its dependencies.
    let project =
        std::fs::read_to_string(root.join("package.json")).expect("the project file is written");
    assert!(project.contains("@saltcorn-test/echo"), "{project}");

    // And it goes away again.
    installer.uninstall("@saltcorn-test/echo").await.unwrap();
    assert!(!installer.is_installed("@saltcorn-test/echo"));

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_directory_that_is_not_a_package_is_refused_by_name() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let root = temp_root("install-not-a-package");
    let installer = Installer::new(&root);

    // A path that does not exist at all.
    let err = installer
        .install(ModuleSource::Local, "/no/such/directory/anywhere")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("/no/such/directory"), "{err}");

    // A directory with no `package.json` — npm would install *something* here
    // (an empty directory is a valid tarball to npm), so the check is ours.
    let empty = root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let err = installer
        .install(ModuleSource::Local, &empty.display().to_string())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("package.json"), "{err}");

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_failed_install_carries_npms_own_output() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let root = temp_root("install-failure");
    let installer = Installer::new(&root);

    // A specifier npm itself rejects, offline: an invalid name never reaches the
    // network, so this fails the same way with or without one.
    let err = installer
        .install(ModuleSource::Npm, "@@not a valid package name@@")
        .await
        .unwrap_err();
    let msg = sc_error::format_chain(&err);
    // npm's diagnosis, not just our exit code (§16).
    assert!(msg.contains("npm install"), "{msg}");
    assert!(
        msg.to_lowercase().contains("invalid") || msg.to_lowercase().contains("error"),
        "the error should carry npm's own words: {msg}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn uninstalling_from_a_root_that_was_never_used_is_quiet() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let root = temp_root("install-empty-root");
    let installer = Installer::new(&root);
    // No project, nothing installed: removing a module is a no-op rather than an
    // error, because the caller is deleting a row and the package is already
    // gone.
    assert_eq!(
        installer.uninstall("@saltcorn-test/echo").await.unwrap(),
        ""
    );
}

/// The shape of the v1 API stub redirection, pinned because npm is fussy about
/// exactly one part of it.
///
/// Each stubbed package gets **two** entries in the project: a direct
/// dependency on the stub directory, and an override that is the *reference*
/// `$<package>` rather than the stub's path. Writing the path into `overrides`
/// directly is what one would expect to work, and it is what npm 11.12 chokes
/// on the moment the dependent was installed with `--install-links`: it goes
/// looking for `<the dependent>/0/package.json` and aborts the whole install
/// with ENOENT. Nothing in the modules root would tell an admin that, so the
/// two entries are asserted here.
#[tokio::test]
async fn the_v1_api_stubs_are_a_dependency_and_the_overrides_reference_it() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let root = temp_root("install-stub-overrides");
    let installer = Installer::new(&root);

    installer.ensure_project().await.unwrap();
    let project: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("package.json")).expect("the project file is written"),
    )
    .expect("the project file is JSON");

    assert_eq!(
        project["overrides"]["@saltcorn/data"],
        serde_json::json!("$@saltcorn/data"),
        "{project:#}"
    );
    let stub = project["dependencies"]["@saltcorn/data"]
        .as_str()
        .unwrap_or_else(|| panic!("the stub is a dependency of the project: {project:#}"));
    // Absolute, so the spec is right wherever it is read from — and so a moved
    // modules root rewrites it on the next install rather than resolving to
    // nothing.
    assert_eq!(
        stub,
        format!(
            "file:{}",
            root.join("v1-api-stub").join("saltcorn-data").display()
        ),
        "{project:#}"
    );
    assert!(
        root.join("v1-api-stub/saltcorn-data/package.json")
            .is_file(),
        "the stub package the dependency points at is written"
    );

    // Idempotent: a second pass over a project that already says all this
    // leaves it alone rather than rewriting it on every install.
    let before = std::fs::read_to_string(root.join("package.json")).unwrap();
    installer.ensure_project().await.unwrap();
    assert_eq!(
        before,
        std::fs::read_to_string(root.join("package.json")).unwrap()
    );

    let _ = std::fs::remove_dir_all(&root);
}
