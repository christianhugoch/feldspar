//! Phase 5 of the Python code adapter: **the environment**.
//!
//! One virtual environment the server owns, `pip` installing into it, and the
//! ABI check between the interpreter that installs and the interpreter that
//! imports (§9).
//!
//! **None of this is behind the `python-host` feature**, and neither are these
//! tests: creating an environment and installing into it are subprocesses, so a
//! build with no interpreter linked in still answers the Modules tab and still
//! lists what is installed. What the feature decides is only whether there is an
//! embedded version to *check against*, and the mismatch test supplies one by
//! hand rather than asking for a build that has it.
//!
//! **Nothing here reaches PyPI.** Every install is of a local fixture whose PEP
//! 517 backend is a file beside it, building the wheel with `zipfile` alone —
//! so pip's build isolation has nothing to download, and a test suite does not
//! depend on somebody else's network. The registry path differs only in the
//! specifier.

use std::path::{Path, PathBuf};

use sc_python::{PythonEnv, PythonEnvironment, PythonSource};

/// Say why a test did nothing, and return, so a skip is visible under
/// `-- --nocapture` rather than looking like a pass.
macro_rules! skip_without {
    ($cond:expr, $why:expr) => {
        if !$cond {
            eprintln!("skipping: {}", $why);
            return;
        }
    };
}

/// One of the fixture projects under `tests/fixtures`.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A throwaway environment directory, removed first so a rerun starts clean.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sc-python-env-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// An environment at `dir`, with `embedded` standing in for whatever
/// interpreter this process has (or has not) linked in.
fn environment(dir: &Path, embedded: Option<(u32, u32)>) -> PythonEnvironment {
    PythonEnvironment::new(
        &PythonEnv {
            dir: Some(dir.to_path_buf()),
            bin: None,
        },
        embedded,
    )
    .expect("an explicit --python-dir needs no default")
}

/// Whether this machine can build an environment at all.
async fn have_toolchain() -> bool {
    let bin = PathBuf::from(sc_python::DEFAULT_PYTHON_BIN);
    sc_python::have_python(&bin).await && sc_python::have_pip(&bin).await
}

// ---------------------------------------------------------------------------
// 5.1 — create, install, list, uninstall
// ---------------------------------------------------------------------------

/// The whole of 5.1 in the order an admin does it: an environment that did not
/// exist, a distribution installed from a **directory**, listed with the
/// version its own metadata declares, and then removed.
#[tokio::test]
async fn a_local_distribution_installs_lists_with_its_version_and_is_removed() {
    skip_without!(
        have_toolchain().await,
        "python3 with pip is not on the PATH"
    );
    let dir = temp_dir("install-local");
    let env = environment(&dir, None);

    // Nothing there yet: no environment, no packages, and asking is not an
    // error — a server that has never installed a Python module is the ordinary
    // case and the screen has to render for it.
    assert!(!env.exists());
    assert!(env.packages().is_empty());

    let installed = env
        .install(
            PythonSource::Local,
            &fixture("sc_fixture_pkg").display().to_string(),
        )
        .await
        .expect("the fixture installs");

    // The environment was created on the way, by this call rather than by a
    // setup step: `ensure` is idempotent and every install goes through it.
    assert!(env.exists(), "the environment was created");
    assert!(env.version().is_some(), "pyvenv.cfg names its interpreter");

    // The name and version are **pip's**, read from the distribution's own
    // metadata — not from the path the admin typed, which says neither.
    assert_eq!(installed.name.replace('_', "-"), "sc-fixture");
    assert_eq!(installed.version, "0.3.1");
    assert!(env.is_installed("sc-fixture"), "by the name pip reported");
    // And by any spelling of it: a distribution's metadata directory
    // normalises, and an admin does not.
    assert!(env.is_installed("SC_Fixture"));
    assert_eq!(
        env.installed_version("sc.fixture").as_deref(),
        Some("0.3.1")
    );

    // The listing the diagnostics screen shows is a directory read of the same
    // environment, and the fixture is in it.
    let listed = env.packages();
    assert!(
        listed.iter().any(|package| {
            package.name.replace('_', "-") == "sc-fixture"
                && package.version.as_deref() == Some("0.3.1")
        }),
        "{listed:?}"
    );
    // The package itself is on the disk where the embedded interpreter would
    // look for it.
    let site_packages = env.site_packages().expect("the environment names one");
    assert!(site_packages.join("sc_fixture/__init__.py").is_file());

    let log = env.uninstall("sc-fixture").await.expect("pip removes it");
    assert!(log.contains("sc"), "pip says what it did: {log}");
    assert!(!env.is_installed("sc-fixture"));
    assert!(!site_packages.join("sc_fixture/__init__.py").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

/// A directory that is not a Python project is refused by name — and refused
/// **before** an environment is built for it, because a typo should not cost a
/// `python3 -m venv` first.
#[tokio::test]
async fn a_directory_that_is_not_a_project_is_refused_before_anything_is_created() {
    let dir = temp_dir("not-a-project");
    let env = environment(&dir, None);

    let said = env
        .install(PythonSource::Local, "/no/such/directory/anywhere")
        .await
        .expect_err("there is nothing there")
        .to_string();
    assert!(said.contains("/no/such/directory/anywhere"), "{said}");

    let empty = temp_dir("empty-project");
    std::fs::create_dir_all(&empty).unwrap();
    let said = env
        .install(PythonSource::Local, &empty.display().to_string())
        .await
        .expect_err("a directory is not a distribution")
        .to_string();
    assert!(said.contains("pyproject.toml"), "{said}");
    assert!(!env.exists(), "nothing was built to refuse this");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// An empty specifier is the admin's mistake, said as such.
#[tokio::test]
async fn a_pypi_install_needs_a_package_name() {
    let dir = temp_dir("no-spec");
    let said = environment(&dir, None)
        .install(PythonSource::Pypi, "   ")
        .await
        .expect_err("there is nothing to install")
        .to_string();
    assert!(said.contains("package name"), "{said}");
}

// ---------------------------------------------------------------------------
// 5.2 — the ABI check, and the toolchain questions
// ---------------------------------------------------------------------------

/// The refusal of §9, with **both** versions named.
///
/// The mismatch is built by hand rather than by finding a machine with two
/// Pythons on it: what is under test is that the comparison happens and what it
/// says, and an environment's version is a line in a file it writes.
#[tokio::test]
async fn an_environment_built_by_another_python_is_refused_naming_both_versions() {
    let dir = temp_dir("abi-mismatch");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("pyvenv.cfg"),
        "home = /usr/bin\ninclude-system-site-packages = false\nversion = 3.9.18\n",
    )
    .unwrap();

    let env = environment(&dir, Some((3, 12)));
    let said = env
        .check_abi()
        .expect_err("3.12 cannot import 3.9's extensions")
        .to_string();
    assert!(
        said.contains("3.12"),
        "the interpreter that would import: {said}"
    );
    assert!(said.contains("3.9"), "the one that built it: {said}");
    // The remedy, which is one of two flags and not a guess.
    assert!(said.contains("--python-bin"), "{said}");
    assert!(said.contains(&dir.display().to_string()), "{said}");

    // And it is refused where it matters: `ensure` is what every install goes
    // through, so nothing is installed into an environment nothing here could
    // import from.
    let said = env
        .ensure()
        .await
        .expect_err("the same refusal")
        .to_string();
    assert!(said.contains("3.9") && said.contains("3.12"), "{said}");

    // The same environment, checked by a process whose interpreter *does* match,
    // is fine — the check is a comparison and not a policy about versions.
    assert!(environment(&dir, Some((3, 9))).check_abi().is_ok());
    // And a process with no embedded interpreter claims nothing either way: a
    // build without the feature can still list what is installed.
    assert!(environment(&dir, None).check_abi().is_ok());

    let _ = std::fs::remove_dir_all(&dir);
}

/// `have_python` and `have_pip`, which the Modules tab asks before an admin
/// types a distribution name — the way it already asks `have_npm`.
#[tokio::test]
async fn the_toolchain_is_answered_for_the_modules_tab() {
    // Whatever this machine has, the two questions answer without throwing and
    // an interpreter that is not there is `false` rather than a panic.
    let missing = PathBuf::from("/no/such/python-interpreter");
    assert!(!sc_python::have_python(&missing).await);
    assert!(!sc_python::have_pip(&missing).await);

    skip_without!(
        sc_python::have_python(Path::new(sc_python::DEFAULT_PYTHON_BIN)).await,
        "python3 is not on the PATH"
    );
    // pip is a separate question on the distributions that ship one without the
    // other, so it is only asserted where the interpreter is there to ask.
    let _ = sc_python::have_pip(Path::new(sc_python::DEFAULT_PYTHON_BIN)).await;
}

/// The environment directory has a default (§9): a server with no `--python-dir`
/// has an environment beside its modules root rather than none at all.
#[test]
fn the_environment_directory_defaults_to_the_platform_data_directory() {
    let Ok(default) = sc_python::default_python_dir() else {
        // A machine with neither HOME nor XDG_DATA_HOME — the error path, which
        // names the flag.
        return;
    };
    let plain = PythonEnv::default();
    assert_eq!(plain.directory(), Some(default.clone()));
    assert!(default.ends_with("feldspar/python"), "{default:?}");
    // And the operator's word wins over it.
    let named = PythonEnv {
        dir: Some(PathBuf::from("/srv/python")),
        bin: None,
    };
    assert_eq!(named.directory(), Some(PathBuf::from("/srv/python")));
    assert_eq!(
        named.interpreter(),
        PathBuf::from(sc_python::DEFAULT_PYTHON_BIN)
    );
}

// ---------------------------------------------------------------------------
// 5.4 — what a failure says
// ---------------------------------------------------------------------------

/// A pip failure carries **pip's own last lines**, because "pip exited 1" is not
/// a diagnosis and the traceback underneath it is what the admin acts on (§16).
#[tokio::test]
async fn a_pip_failure_is_reported_with_pips_own_words() {
    skip_without!(
        have_toolchain().await,
        "python3 with pip is not on the PATH"
    );
    let dir = temp_dir("pip-failure");
    let env = environment(&dir, None);

    let error = env
        .install(
            PythonSource::Local,
            &fixture("py_broken").display().to_string(),
        )
        .await
        .expect_err("the fixture's build backend refuses");
    let said = sc_error::format_chain(&error);

    // The command that failed, so an admin can run it themselves.
    assert!(said.contains("pip install"), "{said}");
    // And the reason, which came out of the build backend rather than out of
    // this crate.
    assert!(
        said.contains("this fixture backend refuses to build"),
        "pip's own output is missing: {said}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// What the ABI check is worth on a process that really has an interpreter
// ---------------------------------------------------------------------------

/// The version compared against is the **running interpreter's**, not a number
/// a test made up: `PythonRuntime::environment` starts the interpreter in order
/// to know it, and the same environment is accepted or refused depending on
/// what `pyvenv.cfg` says.
#[cfg(feature = "python-host")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_running_interpreters_own_version_is_what_an_environment_is_checked_against() {
    use sc_python::PythonRuntime;

    let dir = temp_dir("abi-live");
    std::fs::create_dir_all(&dir).unwrap();
    let runtime = PythonRuntime::new().with_env(PythonEnv {
        dir: Some(dir.clone()),
        bin: None,
    });
    let version = runtime.initialise().expect("this build has an interpreter");
    let (major, minor) = {
        let mut parts = version.split('.');
        let major: u32 = parts.next().unwrap().parse().unwrap();
        let minor: u32 = parts.next().unwrap().parse().unwrap();
        (major, minor)
    };

    // An environment this interpreter built: accepted, and its packages are
    // where it would look for them.
    std::fs::write(
        dir.join("pyvenv.cfg"),
        format!("home = /usr/bin\nversion = {major}.{minor}.0\n"),
    )
    .unwrap();
    let env = runtime.environment().expect("the interpreter is running");
    env.check_abi().expect("the versions agree");
    assert!(
        env.site_packages()
            .unwrap()
            .to_string_lossy()
            .contains(&format!("python{major}.{minor}"))
    );
    assert!(runtime.status().env_error.is_none(), "nothing to report");

    // The same directory, rebuilt by some other Python: refused, with the
    // running interpreter's own version in the sentence.
    std::fs::write(
        dir.join("pyvenv.cfg"),
        format!("home = /usr/bin\nversion = {}.0.1\n", major + 1),
    )
    .unwrap();
    let said = runtime
        .environment()
        .expect("the runtime still answers")
        .check_abi()
        .expect_err("a different version cannot be imported from")
        .to_string();
    assert!(said.contains(&format!("{major}.{minor}")), "{said}");
    assert!(said.contains(&format!("{}.0", major + 1)), "{said}");

    // And the screen says it, in the one place an admin would look for why
    // nothing they installed is importable.
    let status = runtime.status();
    assert!(status.env_error.is_some(), "{status:?}");
    assert!(
        status.packages.is_empty(),
        "an unusable environment lists nothing"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A mismatched environment never reaches `sys.path` (§9), because a C
/// extension built for another interpreter is a **segfault** when this one
/// imports it rather than an `ImportError`.
///
/// There is one interpreter per process and its `sys.path` is read once, by
/// whichever runtime boots it — in a test binary, whichever test ran first. So
/// what this asserts is the safe direction, which holds either way: a directory
/// whose `pyvenv.cfg` claims a version no CPython 3 could be is not on the path.
#[cfg(feature = "python-host")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mismatched_environment_is_left_off_the_path() {
    use sc_expr::CodeCall;
    use sc_python::PythonRuntime;

    let dir = temp_dir("abi-path");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pyvenv.cfg"), "version = 2.7.18\n").unwrap();
    // A package under every layout the running interpreter could look for, so
    // that "it is not imported" is about the refusal and not about the path
    // being wrong for another reason.
    for minor in 9..=20 {
        let packages = dir
            .join("lib")
            .join(format!("python3.{minor}"))
            .join("site-packages")
            .join("pretend_legacy");
        std::fs::create_dir_all(&packages).unwrap();
        std::fs::write(packages.join("__init__.py"), "VERSION = \"0\"\n").unwrap();
    }

    let runtime = PythonRuntime::new().with_env(PythonEnv {
        dir: Some(dir.clone()),
        bin: None,
    });
    let out = runtime
        .run(CodeCall {
            code: "import sys\nreturn sys.path\n".to_owned(),
            ..CodeCall::default()
        })
        .await
        .unwrap();
    let ours = dir.to_string_lossy().into_owned();
    for entry in out.as_array().unwrap() {
        assert!(
            !entry.as_str().unwrap_or_default().starts_with(&ours),
            "an environment built by another Python reached sys.path: {out}"
        );
    }
    // And so nothing in it is importable, which is the point.
    assert!(
        runtime
            .run(CodeCall {
                code: "import pretend_legacy\nreturn 1\n".to_owned(),
                ..CodeCall::default()
            })
            .await
            .is_err()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
