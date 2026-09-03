//! A run is a thread, admission is a number, and the caller serves the host.
//!
//! # A run is a thread, and threads are cheap
//!
//! A Python body is synchronous top to bottom — that is the surface's one deep
//! difference from the JavaScript one — so a run in a host call costs a thread
//! rather than a pending promise. Phase 0.2a measured what that is worth:
//! 34–42 KB per resident run, so the whole default admission budget of 32 is
//! about a megabyte, and the interpreter hosting them is less than half of one
//! V8 isolate. A finished thread goes back in an idle cache rather than being
//! reaped, so a trigger firing a thousand times spawns roughly as many threads
//! as it ever runs at once.
//!
//! # One admission bound over everything
//!
//! [`DEFAULT_MAX_INFLIGHT`] runs may be resident at once — code bodies and
//! (from phase 6) module calls alike, because there is one interpreter and one
//! thing being bounded. Past it a run queues **inside its own deadline**, which
//! is why the wait is a timeout and not a park.
//!
//! With one exception, and it is not a convenience (§6). A Python body may run a
//! trigger whose action is another Python body; if enough of those are in
//! flight, every admitted run waits for a slot held by a run waiting for it.
//! That deadlock only appears under load, so a run **nested inside another
//! Python run** is admitted past the bound on a thread of its own. It is safe
//! because the parent is blocked in a host call with the GIL released — nesting
//! adds threads, not interpreter contention — and because the cascade bound
//! already bounds how many there can be. Nesting is known from a task-local this
//! module sets while it serves a host call, so nothing has to be threaded
//! through the seam.
//!
//! # Who serves the host calls
//!
//! The future that is awaiting the run. The hosts on a [`CodeCall`] are
//! *borrowed*, and the borrow lives exactly as long as the future holding it —
//! so the plans travel back here over a channel and are answered here, rather
//! than anything `'static` being manufactured to cross to the thread.
//!
//! # The four bounds, in the order they fire
//!
//! 1. **The host refuses** every call once the deadline has passed, and bounds
//!    the call *already in flight* by what is left of the clock ([`crate::bridge`]).
//! 2. **`SetAsyncExc`** raises `Timeout` in the run's thread, which stops any
//!    pure-Python loop within a few milliseconds.
//! 3. **The caller stops waiting** at the deadline plus [`CALLER_GRACE`] and
//!    answers with the timeout, whatever the thread is doing.
//! 4. **The thread is quarantined**: dropped from the idle cache, counted as
//!    stuck, and reported. Past [`DEFAULT_MAX_STUCK`] new runs are refused with
//!    a named error rather than accumulating threads that will never come back —
//!    a quarantined thread is a leak, it is reported as one, and the remedy is a
//!    restart. A thread that *does* come back stops being counted and goes back
//!    to work.
//!
//! There is no memory bound. A V8 isolate has a heap limit and a near-limit
//! callback; CPython has neither, and `RLIMIT_AS` is process-wide, which would
//! take the server down instead of the body. Said here, and in the documentation
//! beside the timeout, rather than discovered in production.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use sc_error::{Error, Result};
use sc_expr::{CodeCall, MAX_CODE_TIMEOUT};
use serde_json::Value as Json;

use crate::CALLER_GRACE;
use crate::bridge::{Budgets, HostRequest, RunState, Surface, Surfaces};
use crate::interp;

tokio::task_local! {
    /// Set while this task is serving a Python run's host call. A
    /// `PythonRuntime::run` that sees it is nested inside another run and is
    /// admitted past the bound (§6).
    static SERVING: ();
}

/// Whether this task is inside another Python run's host call.
fn nested() -> bool {
    SERVING.try_with(|()| ()).is_ok()
}

/// The runtime's shared state: the bound, the idle threads, and what is stuck.
pub(crate) struct Inner {
    pub(crate) max_inflight: usize,
    pub(crate) max_stuck: usize,
    admission: tokio::sync::Semaphore,
    idle: Mutex<Vec<IdleThread>>,
    stuck: AtomicUsize,
    threads: AtomicUsize,
    shutdown: AtomicBool,
}

/// A thread parked on its channel, waiting for the next run.
struct IdleThread {
    jobs: Sender<Job>,
}

enum Job {
    Run(Box<RunJob>),
    /// Sent to every parked thread when the runtime is dropped.
    Stop,
}

struct RunJob {
    code: String,
    bindings: BTreeMap<String, Json>,
    state: RunState,
    shared: Arc<RunShared>,
    reply: tokio::sync::oneshot::Sender<Result<Json>>,
}

/// What the caller and the run's thread both need to see.
#[derive(Default)]
struct RunShared {
    /// The thread's Python identity, as `SetAsyncExc` names threads. Zero until
    /// the run starts, which is before any deadline can have passed.
    ident: AtomicU64,
    /// Whether the caller fired an async exception at this thread — so the
    /// thread knows to absorb one that was queued but never delivered.
    fired: AtomicBool,
    /// Whether the caller has given up. A thread that comes back to find this
    /// set has been counted as stuck and un-counts itself.
    abandoned: AtomicBool,
}

impl Inner {
    pub(crate) fn new(max_inflight: usize, max_stuck: usize) -> Inner {
        let max_inflight = max_inflight.max(1);
        Inner {
            max_inflight,
            max_stuck,
            admission: tokio::sync::Semaphore::new(max_inflight),
            idle: Mutex::new(Vec::new()),
            stuck: AtomicUsize::new(0),
            threads: AtomicUsize::new(0),
            shutdown: AtomicBool::new(false),
        }
    }

    /// How many runs never came back. On the diagnostics screen, because it is
    /// the one number here that says a restart is owed.
    pub(crate) fn stuck(&self) -> usize {
        self.stuck.load(Ordering::Relaxed)
    }

    /// How many run threads exist, resident or idle.
    pub(crate) fn threads(&self) -> usize {
        self.threads.load(Ordering::Relaxed)
    }

    /// Run one body. See the module documentation for the shape of it.
    pub(crate) async fn run(
        self: &Arc<Inner>,
        call: CodeCall<'_>,
        default_timeout: Duration,
        env: &crate::PythonEnv,
    ) -> Result<Json> {
        interp::ensure(env)?;
        let timeout = call
            .timeout
            .unwrap_or(default_timeout)
            .min(MAX_CODE_TIMEOUT);
        let deadline = Instant::now() + timeout;

        let stuck = self.stuck();
        if stuck >= self.max_stuck {
            return Err(Error::msg(format!(
                "this server has {stuck} Python runs that never returned, which is its \
                 limit of {}; they cannot be reclaimed and new runs are refused until it \
                 is restarted",
                self.max_stuck
            )));
        }

        // The bound, and §6's exemption from it.
        let _permit = if nested() {
            None
        } else {
            match tokio::time::timeout(timeout, self.admission.acquire()).await {
                Ok(Ok(permit)) => Some(permit),
                Ok(Err(_)) => return Err(Error::msg("the Python runtime is shutting down")),
                Err(_) => {
                    return Err(Error::invalid(format!(
                        "this code waited for its whole {} ms time limit for one of the \
                         {} Python run slots",
                        timeout.as_millis(),
                        self.max_inflight
                    )));
                }
            }
        };

        let (requests, mut incoming) = tokio::sync::mpsc::unbounded_channel::<HostRequest>();
        let has = Surfaces {
            db: call.host.is_some(),
            fetch: call.fetch.is_some(),
            files: call.files.is_some(),
            triggers: call.triggers.is_some(),
            module_fns: call.module_fns.is_some(),
        };
        let budgets = Budgets {
            calls: call.max_calls,
            fetches: call.max_fetches,
            file_ops: call.max_file_ops,
            trigger_runs: call.max_trigger_runs,
            module_calls: call.max_module_calls,
        };
        let shared = Arc::new(RunShared::default());
        let (reply, answer) = tokio::sync::oneshot::channel::<Result<Json>>();
        self.dispatch(Box::new(RunJob {
            code: call.code,
            bindings: call.bindings,
            state: RunState {
                requests,
                deadline,
                timeout,
                has,
                left: budgets,
                max: budgets,
            },
            shared: Arc::clone(&shared),
            reply,
        }))?;

        let overdue = || {
            Error::invalid(format!(
                "this code exceeded its {} ms time limit",
                timeout.as_millis()
            ))
        };
        let soft = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
        let hard =
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline + CALLER_GRACE));
        tokio::pin!(soft, hard, answer);
        let mut fired = false;
        let mut serving = FuturesUnordered::new();
        loop {
            tokio::select! {
                // Biased so that a run which answered just as its deadline
                // passed is reported as what it said, not as a timeout.
                biased;
                outcome = &mut answer => {
                    return outcome.unwrap_or_else(|_| {
                        Err(Error::msg("the Python runtime dropped the reply"))
                    });
                }
                () = &mut soft, if !fired => {
                    fired = true;
                    let ident = shared.ident.load(Ordering::Acquire);
                    if ident != 0 {
                        shared.fired.store(true, Ordering::Release);
                        interp::interrupt(ident);
                    }
                }
                () = &mut hard => {
                    // Whatever the thread is doing, its caller stops waiting.
                    // Dropping this future drops the serving end of the bridge,
                    // so a run still inside a host call is unwound by the
                    // channel closing under it.
                    shared.abandoned.store(true, Ordering::Release);
                    self.stuck.fetch_add(1, Ordering::SeqCst);
                    return Err(overdue());
                }
                Some(request) = incoming.recv() => {
                    let host = call.host;
                    let fetch = call.fetch;
                    let files = call.files;
                    let triggers = call.triggers;
                    let module_fns = call.module_fns;
                    // The task-local §6 reads: everything this call leads to is
                    // nested inside this run.
                    serving.push(SERVING.scope((), async move {
                        let HostRequest { surface, plan, reply } = request;
                        let answer = match surface {
                            Surface::Db => match host {
                                Some(host) => host.call(plan).await,
                                None => Err(Error::msg("this code body has no database access")),
                            },
                            Surface::Fetch => match fetch {
                                Some(fetch) => fetch.fetch(plan).await,
                                None => Err(Error::msg("this code body cannot reach the network")),
                            },
                            Surface::Files => match files {
                                Some(files) => files.files(plan).await,
                                None => {
                                    Err(Error::msg("this code body cannot reach a file store"))
                                }
                            },
                            Surface::Triggers => match triggers {
                                Some(triggers) => triggers.run(plan).await,
                                None => {
                                    Err(Error::msg("this code body cannot run other triggers"))
                                }
                            },
                            Surface::ModuleFns => match module_fns {
                                Some(module_fns) => module_fns.call(plan).await,
                                None => Err(Error::msg(
                                    "this code body cannot call module functions",
                                )),
                            },
                        };
                        // A dropped receiver means the run stopped waiting for
                        // this one: the answer is simply not wanted.
                        let _ = reply.send(answer);
                    }));
                }
                Some(()) = serving.next(), if !serving.is_empty() => {}
            }
        }
    }

    /// Hand a run to an idle thread, or to a new one.
    fn dispatch(self: &Arc<Inner>, job: Box<RunJob>) -> Result<()> {
        let mut job = Job::Run(job);
        loop {
            let idle = match self.idle.lock() {
                Ok(mut idle) => idle.pop(),
                Err(_) => return Err(Error::msg("the Python runtime's thread cache is poisoned")),
            };
            match idle {
                Some(thread) => match thread.jobs.send(job) {
                    Ok(()) => return Ok(()),
                    // The thread went away between being parked and being
                    // handed this run. Take the job back and try the next.
                    Err(returned) => job = returned.0,
                },
                None => return self.spawn(job),
            }
        }
    }

    /// Start a thread for this run. It attaches to the interpreter once and then
    /// stays, serving run after run.
    fn spawn(self: &Arc<Inner>, job: Job) -> Result<()> {
        let (jobs, inbox) = std::sync::mpsc::channel::<Job>();
        let mine = jobs.clone();
        // Weak, so the runtime being dropped is a thing the thread can see —
        // an `Arc` here would be a cycle and the threads would outlive the
        // process's interest in them.
        let owner = Arc::downgrade(self);
        let n = self.threads.fetch_add(1, Ordering::SeqCst);
        std::thread::Builder::new()
            .name(format!("sc-python-{n}"))
            .spawn(move || thread_loop(&inbox, &mine, &owner))
            .map_err(|e| {
                self.threads.fetch_sub(1, Ordering::SeqCst);
                Error::msg(format!("the Python runtime could not start a thread: {e}"))
            })?;
        jobs.send(job)
            .map_err(|_| Error::msg("the Python runtime's new thread did not start"))
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Ok(mut idle) = self.idle.lock() {
            for thread in idle.drain(..) {
                let _ = thread.jobs.send(Job::Stop);
            }
        }
    }
}

/// One run thread: attach, run, park, hand the answer back.
///
/// **Park before answering.** The caller may dispatch its next run the instant
/// it has this one's answer — a trigger fired in a loop does exactly that — and
/// a thread that has not yet said it is free would make that run spawn a second
/// one. The job channel is unbounded, so a run handed to a thread that is still
/// inside this iteration simply waits in its inbox for the next `recv`.
fn thread_loop(inbox: &Receiver<Job>, mine: &Sender<Job>, owner: &Weak<Inner>) {
    while let Ok(Job::Run(job)) = inbox.recv() {
        job.shared.ident.store(my_ident(), Ordering::Release);
        let outcome = interp::run_body(&job.code, &job.bindings, job.state);
        // An async exception fired at a body that had already finished would
        // otherwise be delivered to whatever ran next on this thread.
        if job.shared.fired.load(Ordering::Acquire) {
            interp::drain_async_exception();
        }
        // Park, unless there is nobody left to be parked for.
        let parked = match owner.upgrade() {
            Some(inner) if !inner.shutdown.load(Ordering::SeqCst) => match inner.idle.lock() {
                Ok(mut idle) => {
                    idle.push(IdleThread { jobs: mine.clone() });
                    true
                }
                Err(_) => false,
            },
            _ => false,
        };
        if job.shared.abandoned.load(Ordering::Acquire) {
            // Back from the dead: the caller gave up and counted this thread as
            // stuck, and it is not stuck after all.
            if let Some(inner) = owner.upgrade() {
                inner.stuck.fetch_sub(1, Ordering::SeqCst);
            }
        } else {
            let _ = job.reply.send(outcome);
        }
        if !parked {
            break;
        }
    }
    if let Some(inner) = owner.upgrade() {
        inner.threads.fetch_sub(1, Ordering::SeqCst);
    }
}

/// This thread's Python identity, asked once and remembered.
fn my_ident() -> u64 {
    thread_local! {
        static IDENT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    IDENT.with(|cell| {
        let known = cell.get();
        if known != 0 {
            return known;
        }
        let ident = pyo3::Python::attach(|py| interp::thread_ident(py).unwrap_or(0));
        cell.set(ident);
        ident
    })
}
