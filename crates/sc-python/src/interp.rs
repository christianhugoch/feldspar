//! The interpreter: started once, per process, on first use.
//!
//! **One** CPython lives in this process and everything Python runs in it —
//! every code body, and (from phase 6) every plugin module's action. Not one per
//! feature, not one per module, not a pool: a second interpreter would be a
//! second copy of every imported package for an isolation CPython does not
//! deliver, and per-plugin subinterpreters are the only version of "more than
//! one" worth having later (specification §1, §11).
//!
//! Isolation between runs is therefore what one interpreter can give and no
//! more: separate globals, separate thread state, one shared `sys.modules`. A
//! body that mutates a module it imported has mutated it for the next body.
//!
//! # Started late
//!
//! Nothing here runs until the first Python body does. The interpreter costs
//! ~10 ms to start and ~6.5 MB resident (phase 0.1/0.2a) — about one of the V8
//! isolates the server already starts — so a server that fires no Python pays
//! for none of it, and `--python off` (phase 2.5) keeps a Python-capable binary
//! from starting one at all.
//!
//! # What a run costs, and what it does not
//!
//! Not a compile. A body is parsed and compiled once and kept under a content
//! key ([`BODY_CACHE`]), so a trigger firing a thousand times parses once: 35 µs
//! to compile against 1 µs to execute a cached code object (phase 0.5). What is
//! per run is what has to be — the globals, the bindings and the thread.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Duration;

use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyModule};
use sc_error::{Error, Result};
use serde_json::Value as Json;

use crate::PythonEnv;
use crate::bridge::{self, RunState, Surface};
use crate::convert;
use crate::errors::Timeout;

/// The Python half of the body pipeline, compiled into the binary. There is no
/// file to find, no version to skew and nothing to install.
const BOOT: &str = include_str!("py/boot.py");

/// The **surface** an app builder writes — the five handles and the errors —
/// compiled into the binary beside the pipeline and installed on the meta path
/// at boot.
///
/// `DB_PRELUDE`'s counterpart, and Python for the same reason it is JavaScript:
/// the Rust side sees plans, so a chain method is added here and nowhere else.
const SALTCORN: &str = include_str!("py/saltcorn.py");

/// The name the package is imported under, which is also what a body's
/// `import saltcorn` reaches.
const PACKAGE: &str = "saltcorn";

/// What the surface binds in a run's globals, and which host surface each name
/// depends on: the object is bound **only** where this run has the surface
/// behind it, so `db` on a body with no database is a `NameError` naming it
/// rather than a handle that fails on use.
///
/// The five are objects the package holds, not per-run factories: the run's
/// authority, its budgets and the names it may reach all live on the **thread**
/// (see [`crate::bridge`]), so one shared handle is this language's version of
/// the closure a JavaScript run is handed.
const SURFACES: [(&str, Surface); 5] = [
    ("db", Surface::Db),
    ("fetch", Surface::Fetch),
    ("fs", Surface::Files),
    ("trigger", Surface::Triggers),
    ("modfn", Surface::ModuleFns),
];

/// How many compiled bodies the interpreter keeps. Generous next to the number
/// of triggers an installation has, and each entry is a code object and its
/// source — so eviction is the path this will almost never take.
const BODY_CACHE_CAPACITY: usize = 256;

/// Whether the interpreter has been started, and what it said if it would not
/// start. Set once, read for the life of the process.
static START: OnceLock<std::result::Result<String, String>> = OnceLock::new();

/// The boot module, imported once.
static BOOT_MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

/// The `saltcorn` package, imported once — at boot, so the cost of parsing the
/// surface lands on the interpreter's clock rather than on somebody's trigger.
static PACKAGE_MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

/// The compiled bodies, keyed by a hash of the source with the source kept
/// beside it — so a collision is a miss rather than somebody else's body.
static BODY_CACHE: LazyLock<Mutex<BodyCache>> = LazyLock::new(|| Mutex::new(BodyCache::new()));

struct BodyCache {
    entries: HashMap<u64, CachedBody>,
    /// A logical clock: which entry was used last, without asking the operating
    /// system for the time on the hot path.
    clock: u64,
}

struct CachedBody {
    source: String,
    /// The pseudo-filename this body was compiled under, which is also the key
    /// its source is registered in `linecache` under.
    filename: String,
    code: Py<PyAny>,
    used: u64,
}

impl BodyCache {
    fn new() -> BodyCache {
        BodyCache {
            entries: HashMap::new(),
            clock: 0,
        }
    }

    fn key(source: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        source.hash(&mut hasher);
        hasher.finish()
    }
}

/// Start the interpreter if it is not started, and answer its version.
///
/// Idempotent and safe from any thread: the first caller through pays the ~20 ms
/// and everybody else reads the answer. A failure is remembered too — an
/// interpreter that would not start will not start on the next trigger either,
/// and re-trying it once per fire would turn one bad configuration into a
/// per-request cost.
pub(crate) fn ensure(env: &PythonEnv) -> Result<String> {
    let outcome = START.get_or_init(|| {
        // Before `initialize`, which is the only time an inittab entry can be
        // added: this is what makes `__sc` a **built-in** module rather than
        // something on a path.
        pyo3::append_to_inittab!(sc_module);
        Python::initialize();
        Python::attach(|py| boot(py, env)).map_err(|e| e.to_string())
    });
    match outcome {
        Ok(version) => Ok(version.clone()),
        Err(e) => Err(Error::config(format!(
            "this server could not start its Python interpreter: {e}"
        ))),
    }
}

/// Whether the interpreter has been started — the difference between §7's
/// second and third state, which the diagnostics screen (phase 4.2) reports.
pub(crate) fn started() -> Option<String> {
    match START.get() {
        Some(Ok(version)) => Some(version.clone()),
        _ => None,
    }
}

use crate::bridge::sc_module;

/// Everything that happens once, with the interpreter freshly up.
///
/// `env` is the environment of whichever runtime got here first, which is the
/// honest shape: there is one interpreter per process, so there is one
/// `sys.path`, and the second runtime to ask for it gets the first one's.
fn boot(py: Python<'_>, env: &PythonEnv) -> PyResult<String> {
    let module = PyModule::from_code(
        py,
        std::ffi::CString::new(BOOT)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?
            .as_c_str(),
        c"<saltcorn boot>",
        c"__sc_boot",
    )?;
    // The surface goes on the meta path before anything imports it, and is then
    // imported here rather than on the first run — `import saltcorn` from a body
    // finds it already in `sys.modules`, and a body that never mentions it has
    // still paid for it exactly once, at start.
    module
        .getattr("install_module")?
        .call1((PACKAGE, SALTCORN))?;
    let _ = BOOT_MODULE.set(py, module.unbind());
    let package = py.import(PACKAGE)?;
    let _ = PACKAGE_MODULE.set(py, package.unbind());
    // Imported here rather than on the first conversion, so the cost lands on
    // the interpreter's clock and not on somebody's trigger.
    convert::init(py)?;
    let info = py.import("sys")?.getattr("version_info")?;
    let major: u32 = info.getattr("major")?.extract()?;
    let minor: u32 = info.getattr("minor")?.extract()?;
    let micro: u32 = info.getattr("micro")?.extract()?;
    isolate_path(py, env, major, minor)?;
    Ok(format!("{major}.{minor}.{micro}"))
}

/// The directory an installed package lives in, under a virtual environment
/// this server owns.
///
/// Computed from the **embedded** interpreter's version, because that is the one
/// that will import what is there — and the version of the `python3` that built
/// the environment may not be it, which is exactly the mismatch §9's ABI check
/// (phase 5.2) exists to refuse.
fn site_packages(dir: &std::path::Path, major: u32, minor: u32) -> std::path::PathBuf {
    if cfg!(windows) {
        dir.join("Lib").join("site-packages")
    } else {
        dir.join("lib")
            .join(format!("python{major}.{minor}"))
            .join("site-packages")
    }
}

/// Take the **host's** installed packages off `sys.path`, and put this server's
/// environment on it.
///
/// An embedded interpreter inherits the `sys.path` of the interpreter it was
/// linked against, which on this machine means three `dist-packages`
/// directories and the user's own `site-packages` (phase 0.4, which found the
/// spike importing the system's `numpy` without being asked). Left alone, what a
/// Python body may import would depend on what the operator happened to
/// `apt install` — so the amendment the gate made to §9 is that `--python-dir`
/// **isolates** rather than prepends.
///
/// What stays is the standard library, which is the interpreter's own and is
/// where the phase 4.1 import gate does its work. What goes is every installed
/// package directory, and the current directory with them: a body that could
/// import from wherever the server was started would be a different program per
/// deployment.
///
/// Creating the environment and installing into it is phase 5; this is only the
/// path, so a directory that does not exist yet is harmless — `sys.path` may
/// name one, and `pip` will fill it in.
fn isolate_path(py: Python<'_>, env: &PythonEnv, major: u32, minor: u32) -> PyResult<()> {
    let sys = py.import("sys")?;
    let path = sys.getattr("path")?;
    let existing: Vec<String> = path.extract()?;
    let mut kept: Vec<String> = existing
        .into_iter()
        .filter(|entry| {
            let trimmed = entry.trim_end_matches('/');
            !(trimmed.is_empty()
                || trimmed == "."
                || trimmed.ends_with("/site-packages")
                || trimmed.ends_with("/dist-packages"))
        })
        .collect();
    if let Some(dir) = env.dir.as_deref() {
        kept.push(
            site_packages(dir, major, minor)
                .to_string_lossy()
                .into_owned(),
        );
    }
    sys.setattr("path", kept)?;
    Ok(())
}

/// The `saltcorn` package, for the caller that binds a run's globals.
fn package(py: Python<'_>) -> PyResult<&Bound<'_, PyModule>> {
    match PACKAGE_MODULE.get(py) {
        Some(module) => Ok(module.bind(py)),
        None => Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
            "the Saltcorn Python surface was not installed",
        )),
    }
}

fn boot_module(py: Python<'_>) -> PyResult<&Bound<'_, PyModule>> {
    match BOOT_MODULE.get(py) {
        Some(module) => Ok(module.bind(py)),
        None => Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
            "the Saltcorn Python runtime was not initialised",
        )),
    }
}

/// This thread's Python identity, as `SetAsyncExc` names threads.
///
/// Asked of `threading` rather than of the C API because that is the number the
/// instrument takes, and cached because it is constant for the thread.
pub(crate) fn thread_ident(py: Python<'_>) -> PyResult<u64> {
    py.import("threading")?
        .getattr("get_ident")?
        .call0()?
        .extract()
}

/// §4's second instrument: raise [`Timeout`] in one run's thread.
///
/// CPython delivers it at the next bytecode boundary, which stops any pure
/// Python loop in single-digit milliseconds — and does **not** reach a thread
/// inside a C call or blocked in a host call (phase 0.3). Those are what the
/// bridge's bounded wait and the caller's grace are for. Answers whether CPython
/// found the thread.
pub(crate) fn interrupt(ident: u64) -> bool {
    Python::attach(|py| {
        let exception = py.get_type::<Timeout>();
        // Safety: the interpreter is initialised (a run is in flight), the GIL
        // is held, and `exception` is a live type object which CPython
        // increments a reference to itself.
        let affected = unsafe {
            pyo3::ffi::PyThreadState_SetAsyncExc(ident as std::os::raw::c_long, exception.as_ptr())
        };
        affected == 1
    })
}

/// Absorb an async exception that was fired at this thread but never delivered,
/// before the thread is reused. See `drain_async_exception` in the boot module.
pub(crate) fn drain_async_exception() {
    let _ = Python::attach(|py| -> PyResult<()> {
        boot_module(py)?.getattr("drain_async_exception")?.call0()?;
        Ok(())
    });
}

/// Compile one body, or find it already compiled. Answers the code object and
/// the pseudo-filename its frames and its `linecache` entry carry.
///
/// **The cache lock is never held across a call into Python**, and that is a
/// correctness requirement rather than a preference. CPython hands the GIL over
/// between bytecodes, so a thread compiling with the lock held can lose the GIL
/// mid-compile; a second thread that took the GIL and then blocked on this lock
/// would be holding the one thing the first needs to finish. Both wait forever,
/// and only when two *different* bodies are compiled at once. So: look up under
/// the lock, compile without it, insert under it again.
fn compiled(py: Python<'_>, source: &str) -> Result<(Py<PyAny>, String)> {
    let key = BodyCache::key(source);
    let filename = format!("<saltcorn body {key:016x}>");
    {
        let mut cache = lock()?;
        cache.clock += 1;
        let now = cache.clock;
        if let Some(entry) = cache.entries.get_mut(&key)
            && entry.source == source
        {
            entry.used = now;
            return Ok((entry.code.clone_ref(py), entry.filename.clone()));
        }
    }
    let boot = boot_module(py).map_err(|e| py_error(py, &e))?;
    let code = boot
        .getattr("compile_body")
        .and_then(|f| f.call1((source, filename.as_str())))
        .map_err(|e| syntax_error(py, &e, &filename))?;
    // A second thread may have compiled the same body while this one was
    // compiling it. Both code objects are equivalent, so the last writer wins and
    // nothing is lost but the duplicated work.
    let evicted = {
        let mut cache = lock()?;
        cache.clock += 1;
        let now = cache.clock;
        let evicted = if cache.entries.len() >= BODY_CACHE_CAPACITY {
            cache
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| *key)
                .and_then(|oldest| cache.entries.remove(&oldest))
                .map(|entry| entry.filename)
        } else {
            None
        };
        cache.entries.insert(
            key,
            CachedBody {
                source: source.to_owned(),
                filename: filename.clone(),
                code: code.clone().unbind(),
                used: now,
            },
        );
        evicted
    };
    if let Some(filename) = evicted {
        // The source goes with the code object: `linecache` is what renders a
        // traceback's lines, and an entry nothing can reach is a leak.
        let _ = boot
            .getattr("forget_source")
            .and_then(|f| f.call1((filename.as_str(),)));
    }
    Ok((code.unbind(), filename))
}

fn lock() -> Result<std::sync::MutexGuard<'static, BodyCache>> {
    BODY_CACHE
        .lock()
        .map_err(|_| Error::msg("the Python body cache is poisoned"))
}

/// Run one body to its JSON result, on this thread, with the run's state
/// installed for the length of it.
///
/// Everything before the first host call and after the last one holds the GIL;
/// the waits in the middle do not (see [`crate::bridge`]).
pub(crate) fn run_body(
    source: &str,
    bindings: &BTreeMap<String, Json>,
    state: RunState,
) -> Result<Json> {
    let timeout = state.timeout;
    // Dropped at the end of the run, including through a panic: a thread that
    // goes back in the idle cache carries no authority with it.
    let _entered = bridge::Enter::new(state);
    Python::attach(|py| {
        let (code, filename) = compiled(py, source)?;
        let scope = PyDict::new(py);
        let bind = |name: &str, value: Bound<'_, PyAny>| -> Result<()> {
            scope.set_item(name, value).map_err(|e| py_error(py, &e))
        };
        // Not `__main__`: a body that inspects `__name__` should see something
        // that says what it is.
        bind(
            "__name__",
            pyo3::types::PyString::new(py, "<body>").into_any(),
        )?;
        for (name, value) in bindings {
            let value = convert::from_json(py, value).map_err(|e| py_error(py, &e))?;
            bind(name, value)?;
        }
        // Presence is scope: a surface this run was not given is not bound at
        // all, so reaching it is a `NameError` naming it rather than a call that
        // fails somewhere else later.
        let surfaces = bridge::surfaces();
        let sc = py.import("__sc").map_err(|e| py_error(py, &e))?;
        for (name, surface) in bridge::BOUND_NAMES {
            if surfaces.holds(surface) {
                let function = sc.getattr(name).map_err(|e| py_error(py, &e))?;
                bind(name, function)?;
            }
        }
        // And the surfaces themselves, over those same functions: `db`, `fetch`,
        // `fs`, `trigger` and `modfn` are what an author writes, and the bridge
        // function under each is what the plan crosses on.
        let package = package(py).map_err(|e| py_error(py, &e))?;
        for (name, surface) in SURFACES {
            if surfaces.holds(surface) {
                let handle = package.getattr(name).map_err(|e| py_error(py, &e))?;
                bind(name, handle)?;
            }
        }
        let boot = boot_module(py).map_err(|e| py_error(py, &e))?;
        let outcome = boot
            .getattr("execute")
            .and_then(|f| f.call1((code.bind(py), &scope)));
        match outcome {
            Ok(value) => convert::to_json(&value, "result").map_err(|e| {
                Error::invalid(format!(
                    "this Python code returned a value with no JSON form: {e}"
                ))
            }),
            Err(error) => Err(body_error(py, &error, &filename, timeout)),
        }
    })
}

/// A failure inside the body, as the error its caller reports.
fn body_error(py: Python<'_>, error: &PyErr, filename: &str, timeout: Duration) -> Error {
    // The deadline, however it was delivered — the host refusing, the bounded
    // wait, or `SetAsyncExc` landing between two bytecodes. All three raise the
    // same type, and the message is the run's rather than whatever the
    // instrument happened to carry.
    if error.is_instance_of::<Timeout>(py) {
        return Error::invalid(format!(
            "this code exceeded its {} ms time limit",
            timeout.as_millis()
        ));
    }
    Error::invalid(format!(
        "this Python code failed: {}",
        rendered(py, error, filename)
    ))
}

/// A compile failure, which is a `SyntaxError` with the author's own line and
/// column in it.
fn syntax_error(py: Python<'_>, error: &PyErr, filename: &str) -> Error {
    Error::invalid(format!(
        "this Python code has a syntax error: {}",
        rendered(py, error, filename)
    ))
}

/// The exception, trimmed to the author's own frames by the boot module. Falls
/// back to what PyO3 itself would say if the trimming is what fails.
fn rendered(py: Python<'_>, error: &PyErr, filename: &str) -> String {
    let trimmed = boot_module(py).ok().and_then(|boot| {
        boot.getattr("format_error")
            .and_then(|f| f.call1((error.value(py), filename)))
            .and_then(|s| s.extract::<String>())
            .ok()
    });
    trimmed.unwrap_or_else(|| error.to_string())
}

/// A PyO3 error that is not the body's — a binding that would not convert, a
/// module that would not import — as this crate's error.
fn py_error(py: Python<'_>, error: &PyErr) -> Error {
    Error::msg(format!(
        "the Python runtime failed: {}",
        rendered(py, error, "")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The environment's packages go where a virtual environment puts them, and
    /// under the **embedded** interpreter's version — the one that will import
    /// them, which is not necessarily the `python3` that built the environment.
    #[test]
    fn the_environment_directory_is_where_a_venv_puts_its_packages() {
        let dir = std::path::Path::new("/srv/feldspar/python");
        let found = site_packages(dir, 3, 13);
        if cfg!(windows) {
            assert!(found.ends_with("Lib/site-packages") || found.ends_with("Lib\\site-packages"));
        } else {
            assert_eq!(
                found,
                std::path::Path::new("/srv/feldspar/python/lib/python3.13/site-packages")
            );
        }
        // A different embedded version is a different directory, which is the
        // mismatch phase 5.2's ABI check refuses rather than segfaults on.
        assert_ne!(found, site_packages(dir, 3, 14));
    }
}
