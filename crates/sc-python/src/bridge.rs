//! The seam: five functions in one extension module, each taking a plan and
//! answering one JSON value.
//!
//! ```text
//! __sc_db(plan) · __sc_fetch(request) · __sc_fs(op) · __sc_trigger(request)
//! __sc_modfn(request)
//! ```
//!
//! These are `sc-expr`'s five host traits and nothing else — the same plans a
//! JavaScript body's ops carry, so the two languages cannot disagree about
//! authority, budgets or events without somebody changing the trait they share.
//! The fluent surface an author actually writes (`db.invoices.where(…).rows()`)
//! is Python — `src/py/saltcorn.py` — and lowers to these plans; what is here is
//! everything underneath it.
//!
//! # Three things happen on every call
//!
//! **The GIL is released.** [`Python::detach`] wraps the blocking wait, so a run
//! parked on a query holds a thread and not the interpreter, and every other
//! resident run executes while it waits. This is the whole of the concurrency
//! claim (specification §1) and it is one line; getting it wrong would not fail,
//! it would merely make the runtime serial.
//!
//! **The wait is bounded by the run's own deadline.** Not only the *next* call
//! is refused past it (§4's first instrument): the call already in flight is a
//! `recv_timeout` against what is left of the wall clock, because the other
//! instrument — `SetAsyncExc` — provably cannot reach a thread blocked in a host
//! call (phase 0.3), and a body parked in a five-minute request would otherwise
//! be unstoppable.
//!
//! **The budget is spent.** One counter per surface, with the same numbers and
//! the same refusal sentences a JavaScript body gets, because they are bounds on
//! what the *host* is asked to do rather than on what a language does.
//!
//! # The run's identity is the thread
//!
//! A JavaScript body carries a token because many runs are resident on one
//! isolate. Here one thread is one run, so the run's state is a thread-local and
//! there is no name in the interpreter for anybody else's — a body cannot reach
//! another run's authority because it cannot spell it.

use std::cell::RefCell;
use std::sync::mpsc::{RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

use pyo3::prelude::*;
use pyo3::types::PyModule;
use sc_error::{Error, Result};
use serde_json::Value as Json;
use tokio::sync::mpsc::UnboundedSender;

use crate::convert;
use crate::errors::{
    DbError, FetchError, FileError, ModuleError, SaltcornError, Timeout, TriggerError,
};

/// Which of the five a request is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Surface {
    Db,
    Fetch,
    Files,
    Triggers,
    ModuleFns,
}

impl Surface {
    /// The exception this surface's refusals arrive as, at the call site.
    fn raise(self, message: impl Into<String>) -> PyErr {
        let message = message.into();
        match self {
            Surface::Db => DbError::new_err(message),
            Surface::Fetch => FetchError::new_err(message),
            Surface::Files => FileError::new_err(message),
            Surface::Triggers => TriggerError::new_err(message),
            Surface::ModuleFns => ModuleError::new_err(message),
        }
    }

    /// What a body is told when it reaches a surface this run does not have.
    /// Word for word what the JavaScript op says, because it is the same fact.
    fn absent(self) -> &'static str {
        match self {
            Surface::Db => "this code body has no database access",
            Surface::Fetch => "this code body cannot reach the network",
            Surface::Files => "this code body cannot reach a file store",
            Surface::Triggers => "this code body cannot run other triggers",
            Surface::ModuleFns => "this code body cannot call module functions",
        }
    }

    /// Whether this surface's plan carries a `timeout_ms` the host is expected
    /// to obey. The three that hand the run's clock to somebody else's code do;
    /// the database and the file store are this server's own.
    fn carries_timeout(self) -> bool {
        matches!(
            self,
            Surface::Fetch | Surface::Triggers | Surface::ModuleFns
        )
    }
}

/// One host call in flight: which surface, the plan, and the channel the answer
/// comes back on.
///
/// The reply is a **synchronous** channel because the thread waiting on it is a
/// Python thread with the GIL released, not a future — one rendezvous slot, so
/// the async side never blocks on the send.
pub(crate) struct HostRequest {
    pub(crate) surface: Surface,
    pub(crate) plan: Json,
    pub(crate) reply: SyncSender<Result<Json>>,
}

/// What a run may reach, and what it has spent.
///
/// Lives in a thread-local for the length of one run (see [`Enter`]). Absent
/// surfaces are absent from the map rather than present-and-refusing, so
/// `has_db == false` is the fact behind both the refusal here and the missing
/// binding in the run's globals.
pub(crate) struct RunState {
    pub(crate) requests: UnboundedSender<HostRequest>,
    pub(crate) deadline: Instant,
    pub(crate) timeout: Duration,
    pub(crate) has: Surfaces,
    pub(crate) left: Budgets,
    pub(crate) max: Budgets,
}

/// Which of the five this run was given.
#[derive(Clone, Copy, Default)]
pub(crate) struct Surfaces {
    pub(crate) db: bool,
    pub(crate) fetch: bool,
    pub(crate) files: bool,
    pub(crate) triggers: bool,
    pub(crate) module_fns: bool,
}

impl Surfaces {
    /// Whether this run was given `surface` — which is both whether the bridge
    /// answers for it and whether its name is bound in the run's globals.
    pub(crate) fn holds(self, surface: Surface) -> bool {
        match surface {
            Surface::Db => self.db,
            Surface::Fetch => self.fetch,
            Surface::Files => self.files,
            Surface::Triggers => self.triggers,
            Surface::ModuleFns => self.module_fns,
        }
    }
}

/// The five counters, as a budget remaining or as the bound it started at.
#[derive(Clone, Copy, Default)]
pub(crate) struct Budgets {
    pub(crate) calls: u32,
    pub(crate) fetches: u32,
    pub(crate) file_ops: u32,
    pub(crate) trigger_runs: u32,
    pub(crate) module_calls: u32,
}

impl Budgets {
    fn get(&self, surface: Surface) -> u32 {
        match surface {
            Surface::Db => self.calls,
            Surface::Fetch => self.fetches,
            Surface::Files => self.file_ops,
            Surface::Triggers => self.trigger_runs,
            Surface::ModuleFns => self.module_calls,
        }
    }

    fn spend(&mut self, surface: Surface) {
        let slot = match surface {
            Surface::Db => &mut self.calls,
            Surface::Fetch => &mut self.fetches,
            Surface::Files => &mut self.file_ops,
            Surface::Triggers => &mut self.trigger_runs,
            Surface::ModuleFns => &mut self.module_calls,
        };
        *slot = slot.saturating_sub(1);
    }
}

/// The refusal when a budget is spent — the JavaScript sentence, including the
/// clause saying what the bound is *for*, because an author who hits one is
/// entitled to know whether it is theirs to raise.
fn exhausted(surface: Surface, max: u32) -> String {
    match surface {
        Surface::Db => format!(
            "this code made more than {max} database calls in one run; \
             the bound exists so an accidental loop cannot hammer the database"
        ),
        Surface::Fetch => format!(
            "this code made more than {max} fetch requests in one run; \
             the bound exists so a loop cannot hammer somebody else's server"
        ),
        Surface::Files => format!(
            "this code made more than {max} file operations in one run; \
             the bound exists so a walk over a directory cannot run away"
        ),
        Surface::Triggers => format!(
            "this code ran more than {max} other triggers in one run; \
             the bound exists so a cascade cannot become unbounded"
        ),
        Surface::ModuleFns => format!(
            "this code called more than {max} module functions in one run; \
             the bound exists so a loop over rows cannot become a call per row"
        ),
    }
}

thread_local! {
    /// This thread's run, for as long as it is one.
    static RUN: RefCell<Option<RunState>> = const { RefCell::new(None) };
}

/// Installs a run's state on this thread and takes it away again on drop —
/// including on the way out of a panic, so a thread that goes back in the idle
/// cache never carries the last run's authority with it.
pub(crate) struct Enter;

impl Enter {
    pub(crate) fn new(state: RunState) -> Enter {
        RUN.with(|cell| *cell.borrow_mut() = Some(state));
        Enter
    }
}

impl Drop for Enter {
    fn drop(&mut self) {
        RUN.with(|cell| *cell.borrow_mut() = None);
    }
}

/// What this thread's run may reach, for the caller that binds the globals.
pub(crate) fn surfaces() -> Surfaces {
    RUN.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|run| run.has)
            .unwrap_or_default()
    })
}

/// What the bridge needs out of the run's state to make one call, taken in one
/// borrow so the `RefCell` is not held across the wait.
struct Ticket {
    requests: UnboundedSender<HostRequest>,
    remaining: Duration,
    timeout: Duration,
}

/// Check the three things that can refuse a call before it is made — no run, no
/// surface, no budget, past the deadline — and spend the budget if none do.
fn admit(surface: Surface) -> PyResult<Ticket> {
    RUN.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let Some(run) = borrow.as_mut() else {
            // Reachable only from code that got hold of the function object
            // outside a run — module code between calls, or a body that stashed
            // it on a global that outlived it.
            return Err(SaltcornError::new_err(
                "this Saltcorn surface is only available while a run is executing",
            ));
        };
        if !run.has.holds(surface) {
            return Err(surface.raise(surface.absent()));
        }
        if run.left.get(surface) == 0 {
            return Err(surface.raise(exhausted(surface, run.max.get(surface))));
        }
        // §4's first instrument. Checked before anything is sent: a body past
        // its deadline must not be able to write.
        let now = Instant::now();
        if now >= run.deadline {
            return Err(Timeout::new_err(format!(
                "this code exceeded its {} ms time limit",
                run.timeout.as_millis()
            )));
        }
        run.left.spend(surface);
        Ok(Ticket {
            requests: run.requests.clone(),
            remaining: run.deadline.saturating_duration_since(now),
            timeout: run.timeout,
        })
    })
}

/// One host call: plan out, GIL released, one JSON value or one exception back.
fn call(py: Python<'_>, surface: Surface, request: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    let mut plan = convert::to_json(request, "the plan").map_err(|e| surface.raise(e))?;
    let ticket = admit(surface)?;
    // What is left of the run's wall clock, handed to whoever will spend it.
    // The three surfaces that call out to code this server does not own are the
    // ones that need it; the implementation may take it as given.
    if surface.carries_timeout()
        && let Some(object) = plan.as_object_mut()
    {
        let ms = u64::try_from(ticket.remaining.as_millis()).unwrap_or(u64::MAX);
        let ms = match object.get("timeout_ms").and_then(Json::as_u64) {
            Some(asked) => asked.min(ms),
            None => ms,
        };
        object.insert("timeout_ms".to_owned(), Json::from(ms));
    }
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Json>>(1);
    if ticket
        .requests
        .send(HostRequest {
            surface,
            plan,
            reply: tx,
        })
        .is_err()
    {
        // The caller stopped serving this run: it has already been answered,
        // with its own timeout or with an error of its own. Nothing this body
        // does now can be reported to anyone, so the run is unwound.
        return Err(Timeout::new_err(format!(
            "this code exceeded its {} ms time limit",
            ticket.timeout.as_millis()
        )));
    }
    // The one line the concurrency claim rests on.
    let answer = py.detach(move || rx.recv_timeout(ticket.remaining));
    match answer {
        Ok(Ok(value)) => Ok(convert::from_json(py, &value)?.unbind()),
        Ok(Err(e)) => Err(surface.raise(refusal(&e))),
        // §4, amended by phase 0.3: the *pending* call is bounded too. Without
        // this a body inside a slow request reaches no bytecode boundary, so
        // neither the deadline check above nor `SetAsyncExc` can reach it.
        // `Disconnected` is the same fact arriving from the other side — the
        // caller has stopped serving this run — and unwinds it the same way.
        Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
            Err(Timeout::new_err(format!(
                "this code exceeded its {} ms time limit",
                ticket.timeout.as_millis()
            )))
        }
    }
}

/// A host's `Err` as the sentence the body sees. The error's own message, not
/// its chain: what a `except DbError as e` prints should be the refusal, and the
/// server's log is where the rest of it belongs.
fn refusal(error: &Error) -> String {
    error.to_string()
}

// Each of the five is named for its surface rather than for the Python name it
// is bound under: `wrap_pyfunction!` expands to a module of the same name, and a
// `sc_db` here would be ambiguous with the `sc-db` crate in any build that has
// one in scope. The name a body sees is the `#[pyo3(name)]` attribute.
#[pyfunction]
#[pyo3(name = "__sc_db")]
fn db(py: Python<'_>, plan: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    call(py, Surface::Db, plan)
}

#[pyfunction]
#[pyo3(name = "__sc_fetch")]
fn fetch(py: Python<'_>, request: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    call(py, Surface::Fetch, request)
}

#[pyfunction]
#[pyo3(name = "__sc_fs")]
fn fs(py: Python<'_>, op: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    call(py, Surface::Files, op)
}

#[pyfunction]
#[pyo3(name = "__sc_trigger")]
fn trigger(py: Python<'_>, request: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    call(py, Surface::Triggers, request)
}

#[pyfunction]
#[pyo3(name = "__sc_modfn")]
fn modfn(py: Python<'_>, request: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    call(py, Surface::ModuleFns, request)
}

/// The bridge module, appended to the interpreter's inittab before it starts —
/// so it is a **built-in**, importable with no file on disk, no `sys.path` entry
/// and nothing to install.
#[pymodule]
#[pyo3(name = "__sc")]
pub(crate) fn sc_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add_function(wrap_pyfunction!(db, m)?)?;
    m.add_function(wrap_pyfunction!(fetch, m)?)?;
    m.add_function(wrap_pyfunction!(fs, m)?)?;
    m.add_function(wrap_pyfunction!(trigger, m)?)?;
    m.add_function(wrap_pyfunction!(modfn, m)?)?;
    m.add("SaltcornError", py.get_type::<SaltcornError>())?;
    m.add("DbError", py.get_type::<DbError>())?;
    m.add("FetchError", py.get_type::<FetchError>())?;
    m.add("FileError", py.get_type::<FileError>())?;
    m.add("TriggerError", py.get_type::<TriggerError>())?;
    m.add("ModuleError", py.get_type::<ModuleError>())?;
    m.add("Timeout", py.get_type::<Timeout>())?;
    Ok(())
}

/// The name each surface's bridge function is bound under in a run's globals,
/// paired with the flag that says whether this run has it.
pub(crate) const BOUND_NAMES: [(&str, Surface); 5] = [
    ("__sc_db", Surface::Db),
    ("__sc_fetch", Surface::Fetch),
    ("__sc_fs", Surface::Files),
    ("__sc_trigger", Surface::Triggers),
    ("__sc_modfn", Surface::ModuleFns),
];
