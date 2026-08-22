//! One module worker: its thread, its isolate, the four bounds it is under, and
//! what happens when it dies.
//!
//! The thread spends its life inside one `block_on`. The loop it runs there
//! serves three things at once — control messages from the pool, replies coming
//! back from the host script, and the worker's own event loop — and the fourth
//! branch, a 50 ms tick, is not a timer but an **instrument**: it is how the
//! thread proves to its watchdog that it is still coming round, and how it
//! notices a `process.exit()` that happened while the event loop was parked on
//! an idle socket with no JavaScript to throw out of.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use deno_core::OpState;
use deno_runtime::deno_os::{ExitCode, WatcherExitHandle, WatcherExited};
use deno_runtime::worker::MainWorker;
use sc_error::{Error, Result};
use serde_json::{Value as Json, json};
use tokio::sync::oneshot;

use crate::host::{HOST_SCRIPT, HOST_SCRIPT_NAME, reply_result};

/// How often the worker thread comes round its loop when nothing else wakes it.
///
/// Short enough that a `process.exit()` behind a parked event loop is noticed
/// promptly, and long enough that an idle worker costs nothing measurable.
const TICK: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------------------
// What crosses to a worker
// ---------------------------------------------------------------------------

/// A load, remembered so it can be replayed into a restarted worker.
#[derive(Debug, Clone)]
pub struct LoadRequest {
    /// The package directory the module was loaded from.
    pub dir: PathBuf,
    /// The object handed to v1's `actions(cfg)`.
    pub configuration: Json,
}

/// One call, as it crosses to a worker thread.
pub struct Job {
    /// The request as the protocol carries it, minus its `id` — the worker
    /// assigns that, because the ids have to be unique per worker and only the
    /// worker knows what it has already sent.
    pub request: Json,
    /// Set on a `load`: what to remember for a replay, and under what name.
    /// Recorded only once the load has **worked** — replaying a load that fails
    /// would fail the same way at every restart, and the module is reported as
    /// broken rather than pending.
    pub remember: Option<(String, LoadRequest)>,
    /// Where the answer goes.
    pub reply: oneshot::Sender<Result<Json>>,
}

/// What the pool says to a worker.
pub enum Control {
    /// Run this call.
    Job(Box<Job>),
    /// Drop this module from the replay table — it has been uninstalled, and a
    /// restarted worker must not reload a package that is no longer there.
    Forget(String),
    /// Stop, and say when you have.
    Stop(oneshot::Sender<()>),
}

/// What one worker thread is built with.
pub struct WorkerConfig {
    /// Its index in the pool, for the thread name and the log.
    pub index: usize,
    /// The modules root: the host script's directory, and the parent of the
    /// `node_modules` byonm resolves out of.
    pub root: PathBuf,
    /// How long JavaScript may run without yielding.
    pub js_slice: Duration,
    /// The isolate's heap ceiling.
    pub max_heap: usize,
}

// ---------------------------------------------------------------------------
// The watchdog
// ---------------------------------------------------------------------------

/// Why the watchdog stopped the isolate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trip {
    /// JavaScript ran past the slice without yielding.
    Slice,
    /// The isolate reached its heap ceiling.
    Heap,
}

/// Stops a runaway module through the isolate's thread-safe handle — the only
/// safe cross-thread operation on an isolate.
///
/// **How a slice is measured here, which is not how `sc_expr` measures one.** A
/// code body is entered and left by the worker that runs it, so its slice can be
/// armed around the call. A module host is never "entered": the host script owns
/// the event loop and JavaScript runs whenever a reply arrives on a socket.
/// What is observable instead is the *worker thread*, which comes round its loop
/// at least every [`TICK`] — unless a poll of the event loop has entered
/// JavaScript and not come back. So the thread stamps [`Watchdog::tick`] each
/// time round, and a stamp older than the slice means one thing only: JavaScript
/// has been running, uninterrupted, for that long.
///
/// The instrument is blunt in exactly the way `sc_expr`'s is — it stops the
/// isolate and everything resident on it — which is the argument for modules
/// having a pool of their own rather than sharing the code pool's.
struct Watchdog {
    isolate: deno_core::v8::IsolateHandle,
    /// Milliseconds since [`Watchdog::base`], stamped by the worker thread.
    tick: AtomicU64,
    base: Instant,
    slice: Duration,
    /// 0 for "not fired", or a [`Trip`] discriminant plus one.
    fired: AtomicUsize,
    stop: AtomicBool,
}

impl Watchdog {
    fn start(isolate: deno_core::v8::IsolateHandle, slice: Duration) -> Arc<Watchdog> {
        let dog = Arc::new(Watchdog {
            isolate,
            tick: AtomicU64::new(0),
            base: Instant::now(),
            slice,
            fired: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        });
        let watched = Arc::clone(&dog);
        // A quarter of the slice: close enough that a runaway is stopped near
        // its deadline rather than near twice it, cheap enough that a watchdog
        // over a ten-second slice wakes six times a minute.
        let period = (slice / 4).max(Duration::from_millis(10));
        std::thread::Builder::new()
            .name("sc-module-watchdog".into())
            .spawn(move || {
                while !watched.stop.load(Ordering::SeqCst) {
                    std::thread::sleep(period);
                    if watched.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let stamped = Duration::from_millis(watched.tick.load(Ordering::SeqCst));
                    if watched.base.elapsed().saturating_sub(stamped) > watched.slice {
                        watched.trip(Trip::Slice);
                        // Stamped so the trip is not repeated every period
                        // while the worker unwinds and restarts.
                        watched.tick();
                    }
                }
            })
            .ok();
        dog
    }

    /// "I am still coming round." Called by the worker thread each time round
    /// its loop.
    fn tick(&self) {
        let now = u64::try_from(self.base.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.tick.store(now, Ordering::SeqCst);
    }

    /// Stop the isolate, and remember why. The first reason wins: a heap trip
    /// and a slice trip in the same instant are one termination, and the worker
    /// should report whichever actually stopped it.
    fn trip(&self, why: Trip) {
        let code = match why {
            Trip::Slice => 1,
            Trip::Heap => 2,
        };
        let _ = self
            .fired
            .compare_exchange(0, code, Ordering::SeqCst, Ordering::SeqCst);
        self.isolate.terminate_execution();
    }

    /// Whether the watchdog has stopped this isolate, without clearing the
    /// reason — the worker asks this every tick, and reads the reason once.
    fn tripped(&self) -> bool {
        self.fired.load(Ordering::SeqCst) != 0
    }

    fn took_fired(&self) -> Option<Trip> {
        match self.fired.swap(0, Ordering::SeqCst) {
            1 => Some(Trip::Slice),
            2 => Some(Trip::Heap),
            _ => None,
        }
    }

    fn halt(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// One running host
// ---------------------------------------------------------------------------

/// The isolate, the two ends of the protocol, and the watchdog over it.
///
/// The fields are separate rather than behind methods because the worker's
/// `select!` borrows three of them at once — the event loop mutably, the reply
/// channel mutably, the request channel immutably — and they are disjoint.
struct Host {
    worker: MainWorker,
    /// The isolate's `OpState`, held here so `process.exit()` can be checked for
    /// without touching [`Host::worker`], which the event loop has borrowed.
    op_state: Rc<RefCell<OpState>>,
    /// Lines to the host script, written by a thread so this one never blocks on
    /// a full pipe — the thread that would have to drain it is this one.
    requests: std::sync::mpsc::Sender<String>,
    /// Lines from the host script, read by a thread for the same reason.
    replies: tokio::sync::mpsc::UnboundedReceiver<String>,
    watchdog: Arc<Watchdog>,
}

impl Drop for Host {
    fn drop(&mut self) {
        self.watchdog.halt();
    }
}

/// A pipe end as the file the worker's stdio wants.
///
/// `deno_io`'s `StdioPipe::file` takes a `File`, and a `std::io::pipe` end is
/// one on both platforms — by a different name on each, which is the whole of
/// what these two lines are about.
#[cfg(unix)]
fn pipe_file(end: impl Into<std::os::fd::OwnedFd>) -> std::fs::File {
    std::fs::File::from(end.into())
}

#[cfg(windows)]
fn pipe_file(end: impl Into<std::os::windows::io::OwnedHandle>) -> std::fs::File {
    std::fs::File::from(end.into())
}

impl Host {
    /// Write the host script, build the worker, and evaluate it.
    async fn start(config: &WorkerConfig) -> Result<Host> {
        std::fs::create_dir_all(&config.root).map_err(|e| {
            Error::config(format!(
                "creating the modules directory {}: {e}",
                config.root.display()
            ))
        })?;
        // Written from the binary at every start, exactly as the sidecar does
        // it: the binary is the authority, and a stale copy from an older
        // version would be a bug nobody would look for.
        let script = config.root.join(HOST_SCRIPT_NAME);
        std::fs::write(&script, HOST_SCRIPT)
            .map_err(|e| Error::config(format!("writing {}: {e}", script.display())))?;
        let cwd = std::env::current_dir()
            .map_err(|e| Error::config(format!("the server has no working directory: {e}")))?;
        let url = deno_core::resolve_path(script.to_string_lossy().as_ref(), &cwd)
            .map_err(|e| Error::config(format!("{}: {e}", script.display())))?;

        let (host_stdin, our_stdin) =
            std::io::pipe().map_err(|e| Error::config(format!("a module host pipe: {e}")))?;
        let (our_stdout, host_stdout) =
            std::io::pipe().map_err(|e| Error::config(format!("a module host pipe: {e}")))?;

        let mut worker = super::wiring::build_worker(
            &config.root,
            &url,
            pipe_file(host_stdin),
            pipe_file(host_stdout),
            config.max_heap,
        );
        let op_state = worker.js_runtime.op_state();
        let handle = worker.js_runtime.v8_isolate().thread_safe_handle();

        // §4: with an exit handle in the `OpState`, `deno_os`'s `op_exit`
        // terminates *this isolate* and sets a marker instead of calling
        // `std::process::exit` — which in this process is the server.
        op_state.borrow_mut().put(WatcherExitHandle(handle.clone()));

        let watchdog = Watchdog::start(handle, config.js_slice);
        {
            // Near the ceiling, V8's own answer is to abort the process. This is
            // the answer instead: raise the limit by a grace so the terminated
            // JavaScript has somewhere to unwind into, and stop the isolate
            // through the same instrument a runaway loop goes through, so that
            // the failure, the naming and the restart are the ones already
            // written.
            let dog = Arc::clone(&watchdog);
            worker
                .js_runtime
                .add_near_heap_limit_callback(move |current, initial| {
                    let grace = (initial / 2).max(1);
                    let ceiling = initial.saturating_add(initial);
                    if current >= ceiling {
                        dog.trip(Trip::Heap);
                        return current.saturating_add(grace);
                    }
                    current.saturating_add(grace).min(ceiling)
                });
        }

        // The two blocking ends of the pipe, off this thread. Both die on their
        // own when the pipe closes, which is what dropping this `Host` does.
        let (requests, outgoing) = std::sync::mpsc::channel::<String>();
        std::thread::Builder::new()
            .name(format!("sc-module-{}-w", config.index))
            .spawn(move || {
                let mut pipe = our_stdin;
                while let Ok(line) = outgoing.recv() {
                    if pipe.write_all(line.as_bytes()).is_err() {
                        return;
                    }
                    let _ = pipe.flush();
                }
            })
            .map_err(|e| Error::config(format!("starting a module host writer: {e}")))?;

        let (incoming, replies) = tokio::sync::mpsc::unbounded_channel::<String>();
        std::thread::Builder::new()
            .name(format!("sc-module-{}-r", config.index))
            .spawn(move || {
                for line in BufReader::new(our_stdout)
                    .lines()
                    .map_while(std::io::Result::ok)
                {
                    if incoming.send(line).is_err() {
                        return;
                    }
                }
            })
            .map_err(|e| Error::config(format!("starting a module host reader: {e}")))?;

        watchdog.tick();
        worker
            .execute_main_module(&url)
            .await
            .map_err(|e| Error::config(format!("the module host script would not start: {e}")))?;

        Ok(Host {
            worker,
            op_state,
            requests,
            replies,
            watchdog,
        })
    }
}

/// Whether `op_exit` has run on this isolate — a module called `process.exit()`.
fn exited(op_state: &Rc<RefCell<OpState>>) -> bool {
    op_state.borrow().try_borrow::<WatcherExited>().is_some()
}

/// The exit code the module asked for, if it asked for one.
fn exit_code(op_state: &Rc<RefCell<OpState>>) -> Option<i32> {
    op_state
        .borrow()
        .try_borrow::<ExitCode>()
        .map(ExitCode::get)
}

// ---------------------------------------------------------------------------
// The call table
// ---------------------------------------------------------------------------

/// What is waiting on one request id.
enum Pending {
    /// A caller's call.
    Call {
        reply: oneshot::Sender<Result<Json>>,
        remember: Option<(String, LoadRequest)>,
    },
    /// A load replayed into a restarted worker. Nobody is waiting on it, but a
    /// failure has to reach the log by name rather than vanishing.
    Replay(String),
}

/// Why the serving loop stopped.
enum End {
    /// The pool asked.
    Stop,
    /// A module called `process.exit()`.
    Exited,
    /// The watchdog stopped the isolate — the slice or the heap.
    ///
    /// A separate ending from [`End::EventLoop`] because `deno_core` **does not
    /// report a termination**: `do_js_event_loop_tick` sees
    /// `is_execution_terminating` and returns "no ops", so the event loop goes
    /// back to sleep with a dead isolate under it and `run_event_loop` never
    /// comes back. The watchdog's own flag is the only evidence there is, and
    /// the tick is where it is read.
    Watchdog,
    /// The event loop ended, with whatever it ended with.
    EventLoop(std::result::Result<(), deno_core::error::CoreError>),
    /// The host script's stdout closed under us.
    Silent,
}

// ---------------------------------------------------------------------------
// The thread
// ---------------------------------------------------------------------------

/// One worker thread, from its spawn to the pool's shutdown.
pub(super) fn worker_thread(
    config: WorkerConfig,
    rx: tokio::sync::mpsc::UnboundedReceiver<Control>,
) {
    // The worker's own runtime, and not the caller's: it is the isolate's tokio
    // anchor (`deno_core` registers the isolate against whatever runtime was
    // current when it was built, and an isolate whose delayed foreground tasks
    // belong to a runtime nobody drives aborts the process) *and* what drives
    // the worker's event loop.
    let Ok(local) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        // Runtime construction fails only on resource exhaustion; a call would
        // then fail on a closed channel, which is the honest symptom.
        return;
    };
    local.block_on(serve(config, rx));
}

/// The worker's life: idle, then a host, then — if the host dies — idle again.
async fn serve(config: WorkerConfig, mut rx: tokio::sync::mpsc::UnboundedReceiver<Control>) {
    let mut loads: BTreeMap<String, LoadRequest> = BTreeMap::new();

    'idle: loop {
        // **Lazily started.** A deployment with no modules never builds an
        // isolate, and never needs anything on its PATH — which is the bargain
        // the sidecar already makes by not spawning `node` until it is needed.
        let first = loop {
            match rx.recv().await {
                None => return,
                Some(Control::Stop(ack)) => {
                    let _ = ack.send(());
                    return;
                }
                Some(Control::Forget(name)) => {
                    loads.remove(&name);
                }
                Some(Control::Job(job)) => break job,
            }
        };

        let mut host = match Host::start(&config).await {
            Ok(host) => host,
            Err(e) => {
                let _ = first.reply.send(Err(e));
                continue 'idle;
            }
        };

        let mut pending: HashMap<u64, Pending> = HashMap::new();
        let mut next_id: u64 = 1;

        // Replayed **before** the first job is sent, so no caller's `run` can
        // reach a fresh worker before its modules are back. Not awaited: the
        // host script's own `loading` map makes a `run` wait for a load that is
        // still in flight, which is the same guarantee for less machinery.
        for (name, request) in &loads {
            let id = next_id;
            next_id += 1;
            let line = json!({
                "id": id,
                "op": "load",
                "module": name,
                "dir": request.dir.display().to_string(),
                "configuration": request.configuration,
            });
            if write_line(&host.requests, &line) {
                pending.insert(id, Pending::Replay(name.clone()));
            }
        }
        submit(&host.requests, &mut next_id, &mut pending, *first);

        let mut stop_ack: Option<oneshot::Sender<()>> = None;
        let ended = loop {
            host.watchdog.tick();
            tokio::select! {
                biased;
                control = rx.recv() => match control {
                    None => break End::Stop,
                    Some(Control::Stop(ack)) => {
                        stop_ack = Some(ack);
                        break End::Stop;
                    }
                    Some(Control::Forget(name)) => { loads.remove(&name); }
                    Some(Control::Job(job)) => {
                        submit(&host.requests, &mut next_id, &mut pending, *job);
                    }
                },
                line = host.replies.recv() => match line {
                    None => break End::Silent,
                    Some(line) => dispatch(&mut pending, &mut loads, &line),
                },
                result = host.worker.run_event_loop(false) => break End::EventLoop(result),
                () = tokio::time::sleep(TICK) => {
                    // The finding phase 0 would otherwise have left for
                    // production: `terminate_execution` throws out of *running*
                    // JavaScript, so a module that exits while the event loop is
                    // parked on an idle socket leaves it parked. The marker is
                    // the evidence, and this is what acts on it.
                    if exited(&host.op_state) {
                        break End::Exited;
                    }
                    if host.watchdog.tripped() {
                        break End::Watchdog;
                    }
                }
            }
        };

        let reason = ending_reason(&host, &ended, config.js_slice);
        if !matches!(ended, End::Stop) {
            sc_log::log_error!(
                "saltcorn: module worker {} stopped: {reason}. It will be restarted, with every \
                 module replayed, on the next call",
                config.index
            );
        }
        // Everything that was in flight is failed **by name**: a call that will
        // never be answered must not be discovered by its 120-second timeout.
        for (_, entry) in pending.drain() {
            match entry {
                Pending::Call { reply, .. } => {
                    let _ = reply.send(Err(Error::config(format!(
                        "the module host stopped while this call was in flight: {reason}"
                    ))));
                }
                Pending::Replay(name) => {
                    sc_log::log_error!("saltcorn: the reload of `{name}` was lost: {reason}");
                }
            }
        }
        drop(host);
        if let Some(ack) = stop_ack {
            let _ = ack.send(());
            return;
        }
        if matches!(ended, End::Stop) {
            return;
        }
    }
}

/// Name what stopped a worker, in a sentence an admin can act on.
fn ending_reason(host: &Host, ended: &End, slice: Duration) -> String {
    if let Some(trip) = host.watchdog.took_fired() {
        return match trip {
            Trip::Slice => format!(
                "a module ran JavaScript for more than {} ms without yielding",
                slice.as_millis()
            ),
            Trip::Heap => "a module exhausted the worker's heap".to_owned(),
        };
    }
    if exited(&host.op_state) || matches!(ended, End::Exited) {
        return match exit_code(&host.op_state) {
            Some(code) => format!("a module called process.exit({code})"),
            None => "a module called process.exit()".to_owned(),
        };
    }
    match ended {
        End::Stop => "the server asked it to".to_owned(),
        // Unreachable in practice: `took_fired` above answers every watchdog
        // ending. Named rather than left to a catch-all so that a future third
        // trip reason cannot become "the host script stopped writing".
        End::Watchdog => "the worker's watchdog stopped it".to_owned(),
        End::Exited => "a module called process.exit()".to_owned(),
        End::Silent => "the host script stopped writing".to_owned(),
        End::EventLoop(Ok(())) => "the host script's event loop ran out of work".to_owned(),
        End::EventLoop(Err(e)) => format!("the host script failed: {e}"),
    }
}

/// Send one line to the host script, reporting whether it went.
fn write_line(requests: &std::sync::mpsc::Sender<String>, request: &Json) -> bool {
    match serde_json::to_string(request) {
        Ok(mut line) => {
            line.push('\n');
            requests.send(line).is_ok()
        }
        Err(e) => {
            sc_log::log_error!("saltcorn: serialising a module-host request: {e}");
            false
        }
    }
}

/// Give a job an id, send it, and remember who is waiting for the answer.
fn submit(
    requests: &std::sync::mpsc::Sender<String>,
    next_id: &mut u64,
    pending: &mut HashMap<u64, Pending>,
    job: Job,
) {
    let id = *next_id;
    *next_id += 1;
    let Job {
        mut request,
        remember,
        reply,
    } = job;
    if let Some(object) = request.as_object_mut() {
        object.insert("id".into(), json!(id));
    }
    if !write_line(requests, &request) {
        let _ = reply.send(Err(Error::config(
            "the module host could not be written to; it will be restarted for the next call",
        )));
        return;
    }
    pending.insert(id, Pending::Call { reply, remember });
}

/// One reply line: to whoever is waiting for it, and to the replay table if it
/// was a load that worked.
fn dispatch(
    pending: &mut HashMap<u64, Pending>,
    loads: &mut BTreeMap<String, LoadRequest>,
    line: &str,
) {
    let Ok(reply) = serde_json::from_str::<Json>(line) else {
        sc_log::log_error!("saltcorn: unreadable module-host reply: {line}");
        return;
    };
    let Some(id) = reply.get("id").and_then(Json::as_u64) else {
        return;
    };
    let Some(entry) = pending.remove(&id) else {
        return;
    };
    let result = reply_result(&reply);
    match entry {
        Pending::Call {
            reply: sender,
            remember,
        } => {
            if result.is_ok()
                && let Some((name, request)) = remember
            {
                loads.insert(name, request);
            }
            let _ = sender.send(result);
        }
        Pending::Replay(name) => {
            if let Err(e) = result {
                sc_log::log_error!("saltcorn: replaying the load of `{name}` failed: {e}");
            }
        }
    }
}
