//! The installer, against real npm.
//!
//! Nothing here reaches the registry: every install is of a **local fixture
//! package**, which is the same code path (`npm install <path>`) with none of
//! the flakiness of somebody else's network in a test suite. The registry path
//! differs only in the specifier, and it is exercised by hand against
//! `@saltcorn/mqtt` (the milestone's definition of done).

#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;

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
