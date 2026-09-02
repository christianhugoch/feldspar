//! **The Python code adapter**: a code body run on one embedded CPython, over
//! `sc-expr`'s own [`CodeCall`] and its five host traits (design §15; TODO "The
//! Python code adapter").
//!
//! ```python
//! overdue = db.invoices.where(paid=False).order_by("due").limit(50).rows()
//! for inv in overdue:
//!     db.reminders.insert(invoice=inv["id"])
//! return {"chased": len(overdue)}
//! ```
//!
//! That surface is Python — `src/py/saltcorn.py`, shipped inside the binary and
//! installed on the meta path at interpreter start — and it lowers to the same
//! plans a JavaScript body's terminals lower to. `db` is here; `fetch`, `fs`,
//! `trigger` and `modfn` land in phase 3. Underneath it is everything this crate
//! began as: one interpreter, a run per thread, the four bounds, and the seam.
//!
//! # Why there is nothing new below the plans
//!
//! [`CodeCall`] was never JavaScript's. The source, the bindings, the five
//! borrowed host handles and the six budgets are the same question in any
//! language, so a second language is a second implementation of
//! [`CodeAdapter`] and **nothing else** — no second call type, no second host
//! trait, and therefore no way for two languages to drift apart about authority,
//! budgets or events. `ctx.adapter("python")` is how an action reaches this one.
//!
//! # The three states of Python in a server, which are not the same question
//!
//! | | what it is | how it changes |
//! | --- | --- | --- |
//! | `python-host` | a **Cargo feature** on this crate: links `libpython` in | a rebuild |
//! | `--python off\|auto` | a CLI flag (phase 2.5): whether this process *starts* the interpreter it has | a restart |
//! | started | whether a body or a module has actually needed it yet | the first Python trigger |
//!
//! PyO3 embeds CPython by **linking** it, so the feature is the whole of the
//! first decision: there is no flag that turns Python on in a binary built
//! without it. And the feature is **off by default**, for a reason phase 0
//! sharpened: this project's shipped artifact is a `+crt-static` static-PIE
//! binary, and the Python link does not succeed under `+crt-static` at all — nor
//! could an embedded interpreter `dlopen` the C extensions (`numpy` above all)
//! that are the point of having it. So the shipped tarball has no Python and a
//! Python-capable server is a separate, dynamically-linked build, which needs
//! `libpython3.x.so` present on the host to exec at all.
//!
//! A build **without** the feature still constructs a [`PythonRuntime`] and
//! still registers `run_python_code`, and every entry point answers "this server
//! was built without Python support". That is deliberate: a trigger's
//! configuration stays meaningful across deployments, and an admin gets a
//! sentence naming the reason instead of a missing action.
//! [`PythonRuntime::state`] is the same three states, for the screen that has to
//! show which one this process is in.
//!
//! # What one interpreter gives, and what it does not
//!
//! Concurrency, and not isolation. Every host call releases the GIL, so a run
//! waiting on a query holds a thread and not the interpreter and every other run
//! executes while it waits — measured, not asserted: eight runs each making one
//! 2 000 ms host call finish in 2 003 ms, against 16 013 ms for the same eight
//! with the GIL held (phase 0.2). Two **CPU-bound** bodies do serialise, with
//! about 35% of contention on top, and nothing in this milestone changes that.
//!
//! Isolation is separate globals, separate thread state and one shared
//! `sys.modules`: a body that mutates a module it imported has mutated it for
//! the next body. That is what one interpreter can give, it is said here rather
//! than discovered, and per-plugin subinterpreters are the version of "more
//! than one" worth having later.
//!
//! # The pieces
//!
//! - [`interp`] — the interpreter, the boot module, the surface, the body cache.
//! - [`bridge`] — the five host functions, the GIL release, the budgets.
//! - [`convert`] — JSON ↔ Python, and what has no JSON form.
//! - [`errors`] — the exception hierarchy, defined in Rust.
//! - [`runtime`] — the thread cache, the admission bound and the four bounds.

use std::time::Duration;

use async_trait::async_trait;
use sc_error::Result;
use sc_expr::{CodeAdapter, CodeCall, DEFAULT_CODE_TIMEOUT};
use serde_json::Value as Json;

#[cfg(feature = "python-host")]
mod bridge;
#[cfg(feature = "python-host")]
mod convert;
#[cfg(feature = "python-host")]
mod errors;
#[cfg(feature = "python-host")]
mod interp;
#[cfg(feature = "python-host")]
mod runtime;

/// The name this adapter is registered under, and the language an action's
/// stored configuration names.
///
/// `sc-expr`'s, re-exported: the action that asks `ctx.adapter("python")` and the
/// adapter that answers to it must agree, and they agree by sharing the string
/// rather than by both spelling it correctly.
pub use sc_expr::PYTHON;

/// How many Python runs may be resident at once — code bodies and module calls
/// alike, because there is one interpreter and one thing being bounded.
///
/// 32 costs about 1.3 MB of thread state (phase 0.2a). Past it a run queues
/// inside its own deadline, with the one exemption a nested run gets (§6).
pub const DEFAULT_MAX_INFLIGHT: usize = 32;

/// How many threads may be **stuck** — fired at, waited for, and never returned
/// — before new runs are refused.
///
/// A stuck thread is a leak: CPython cannot stop a thread inside a C call, so
/// there is no instrument that reclaims it and the remedy is a restart. Refusing
/// at a bound is better than accumulating threads that will never come back, and
/// the count is on the diagnostics screen so the leak is visible before the
/// bound is reached.
pub const DEFAULT_MAX_STUCK: usize = 8;

/// How long past its own deadline the **caller** waits before giving up on a
/// run and quarantining its thread.
///
/// The bounds inside the run — the host's refusal, the bounded wait and
/// `SetAsyncExc` — all name themselves, so this one wants to lose that race: it
/// exists for the runs none of them can reach.
pub const CALLER_GRACE: Duration = Duration::from_millis(250);

/// Which of the three states of §7 this process is in — the question behind "why
/// does my Python trigger not work", which has three different answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonState {
    /// This binary was built without the `python-host` feature. No flag changes
    /// it; a Python-capable server is a different build.
    NotBuilt,
    /// Built with Python, and the interpreter has not been needed yet.
    NotInitialised,
    /// Running, with the interpreter's version.
    Running {
        /// `3.13.2`, as `sys.version_info` gives it.
        version: String,
    },
}

/// The Python runtime: one interpreter, a run per thread, one admission bound.
///
/// Constructing one is free and starts nothing — the interpreter is started by
/// the first run, or eagerly by [`initialise`](PythonRuntime::initialise) for a
/// server that would rather pay at boot and say so.
pub struct PythonRuntime {
    default_timeout: Duration,
    #[cfg(feature = "python-host")]
    inner: std::sync::Arc<runtime::Inner>,
}

impl PythonRuntime {
    /// A runtime with the default bounds.
    #[must_use]
    pub fn new() -> PythonRuntime {
        PythonRuntime::with_bounds(DEFAULT_MAX_INFLIGHT, DEFAULT_MAX_STUCK)
    }

    /// A runtime with `max_inflight` resident runs and `max_stuck` threads
    /// tolerated — `--python-max-inflight` and `--python-max-stuck`.
    #[must_use]
    pub fn with_bounds(max_inflight: usize, max_stuck: usize) -> PythonRuntime {
        #[cfg(not(feature = "python-host"))]
        let _ = (max_inflight, max_stuck);
        PythonRuntime {
            default_timeout: DEFAULT_CODE_TIMEOUT,
            #[cfg(feature = "python-host")]
            inner: std::sync::Arc::new(runtime::Inner::new(max_inflight, max_stuck)),
        }
    }

    /// Set what a call with no `timeout` of its own gets, in place of
    /// [`DEFAULT_CODE_TIMEOUT`]. Still clamped to `MAX_CODE_TIMEOUT`.
    #[must_use]
    pub fn with_default_timeout(mut self, timeout: Duration) -> PythonRuntime {
        self.default_timeout = timeout;
        self
    }

    /// Start the interpreter now rather than at the first run, and answer its
    /// version.
    ///
    /// Idempotent. A server that wants "Python 3.13.2" in its boot log, or wants
    /// a broken environment reported at start rather than at the first trigger,
    /// calls this; nothing else needs to.
    pub fn initialise(&self) -> Result<String> {
        #[cfg(feature = "python-host")]
        {
            interp::ensure()
        }
        #[cfg(not(feature = "python-host"))]
        {
            Err(not_built())
        }
    }

    /// Which of §7's three states this process is in.
    #[must_use]
    pub fn state(&self) -> PythonState {
        #[cfg(feature = "python-host")]
        {
            match interp::started() {
                Some(version) => PythonState::Running { version },
                None => PythonState::NotInitialised,
            }
        }
        #[cfg(not(feature = "python-host"))]
        {
            PythonState::NotBuilt
        }
    }

    /// How many run threads never came back. Zero on a healthy server; anything
    /// else is a leak, and the remedy is a restart.
    #[must_use]
    pub fn stuck(&self) -> usize {
        #[cfg(feature = "python-host")]
        {
            self.inner.stuck()
        }
        #[cfg(not(feature = "python-host"))]
        {
            0
        }
    }

    /// How many run threads exist, resident or idle. The idle ones are the
    /// cache: a finished thread is reused rather than reaped.
    #[must_use]
    pub fn threads(&self) -> usize {
        #[cfg(feature = "python-host")]
        {
            self.inner.threads()
        }
        #[cfg(not(feature = "python-host"))]
        {
            0
        }
    }

    /// Run one Python body to its JSON result.
    pub async fn run(&self, call: CodeCall<'_>) -> Result<Json> {
        #[cfg(feature = "python-host")]
        {
            self.inner.run(call, self.default_timeout).await
        }
        #[cfg(not(feature = "python-host"))]
        {
            let _ = call;
            Err(not_built())
        }
    }
}

impl Default for PythonRuntime {
    fn default() -> Self {
        PythonRuntime::new()
    }
}

#[async_trait]
impl CodeAdapter for PythonRuntime {
    fn language(&self) -> &str {
        PYTHON
    }

    async fn run_code(&self, call: CodeCall<'_>) -> Result<Json> {
        self.run(call).await
    }
}

/// The one sentence a build without the feature answers with, everywhere.
///
/// It names the reason rather than the symptom, because the fix is a rebuild and
/// nothing an admin can do at run time will change it.
#[cfg(not(feature = "python-host"))]
fn not_built() -> sc_error::Error {
    sc_error::Error::config(
        "this server was built without Python support: it was compiled without the \
         `python` feature, so it has no interpreter linked in and no flag can turn one \
         on. A Python-capable server is a separate build (`cargo build -p sc-server \
         --features python`), which requires libpython on the host.",
    )
}
