//! One module worker: its thread, its isolate, the four bounds it is under, and
//! what happens when it dies.
//!
//! The thread spends its life inside one `block_on`. The loop it runs there
//! serves three things at once — control messages from the pool, answers coming
//! back out of the isolate, and the worker's own event loop — and the fourth
//! branch, a 50 ms tick, is not a timer but an **instrument**: it is how the
//! thread proves to its watchdog that it is still coming round, and how it
//! notices a `process.exit()` that happened while the event loop was parked on
//! an idle socket with no JavaScript to throw out of.
//!
//! ## The seam, which is three V8 functions and no protocol
//!
//! A call is `globalThis.__scModuleHost(id, request)` — one V8 function call
//! with the request converted straight into a V8 object — and the answer comes
//! back through two native functions this file installs on the isolate's global
//! before the host script is evaluated:
//!
//! | | |
//! |---|---|
//! | `__scDone(id, jsonText)` | the call answered; the text is `JSON.stringify`'s |
//! | `__scFail(id, message, stack)` | the call threw |
//! | `__scLog(level, module, message)` | one `console.*` line, into [`sc_log`] |
//!
//! **Native functions rather than ops**, which is not a preference: an op is
//! reached from JavaScript through `Deno.core.ops`, and `deno_runtime`'s worker
//! bootstrap ends by calling `removeImportedOps()`, which deletes every entry
//! there that is not on its own allow-list. A module host's ops would be gone
//! before its main module ran. A function set on `globalThis` is not, and it
//! costs less: `v8::Function::new` and a `v8::Global`.
//!
//! The `id` is still the worker's, because the reason for it never was the
//! transport: many calls are in flight at once and a slow module's action must
//! not hold anybody else's. What crosses back is `JSON.stringify`'s text and not
//! the value, because the value is a *module's* object — a function property, a
//! stream, a cycle — and `JSON.stringify` is the rule v1 itself applies to one.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use deno_core::OpState;
use deno_core::v8;
use deno_runtime::deno_os::{ExitCode, WatcherExitHandle, WatcherExited};
use deno_runtime::worker::MainWorker;
use sc_error::{Error, Result};
use serde_json::{Value as Json, json};
use tokio::sync::oneshot;

use crate::host::{HOST_SCRIPT, HOST_SCRIPT_NAME, module_error};

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
    /// The request the host script's entry point is called with, minus its `id`
    /// — the worker assigns that, because the ids have to be unique per worker
    /// and only the worker knows what it has already sent.
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
    isolate: v8::IsolateHandle,
    /// Milliseconds since [`Watchdog::base`], stamped by the worker thread.
    tick: AtomicU64,
    base: Instant,
    slice: Duration,
    /// 0 for "not fired", or a [`Trip`] discriminant plus one.
    fired: AtomicUsize,
    stop: AtomicBool,
}

impl Watchdog {
    fn start(isolate: v8::IsolateHandle, slice: Duration) -> Arc<Watchdog> {
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
// The three functions the isolate answers through
// ---------------------------------------------------------------------------

/// One call, settled.
struct Settled {
    id: u64,
    outcome: Result<Json>,
}

/// What the native functions reach: the worker thread's own channel, and which
/// worker this is.
///
/// Lives in the isolate's [`OpState`], which is where a `deno_core` embedder's
/// per-isolate state goes and which outlives every JavaScript frame that can
/// reach it.
struct Bridge {
    settled: tokio::sync::mpsc::UnboundedSender<Settled>,
    index: usize,
}

/// Hand one settled call to the worker thread.
///
/// A closed channel is not an error worth a word: it means the worker has
/// already ended and is failing its calls by name, and this answer is one the
/// caller was told about a moment ago.
fn settle(scope: &mut v8::PinScope, id: u64, outcome: Result<Json>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let state = op_state.borrow();
    if let Some(bridge) = state.try_borrow::<Bridge>() {
        let _ = bridge.settled.send(Settled { id, outcome });
    }
}

/// `__scDone(id, jsonText)` — the module answered.
fn host_done(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let id = args.get(0).integer_value(scope).unwrap_or(0).max(0) as u64;
    let text = args.get(1).to_rust_string_lossy(scope);
    // The host script encodes with `JSON.stringify` and encodes a failure to do
    // so as a *failure*, so anything unreadable here is a bug in this pair
    // rather than a module's doing — `null` is what the pipe answered for it
    // too, and the call is still answered.
    let value = serde_json::from_str(&text).unwrap_or(Json::Null);
    settle(scope, id, Ok(value));
}

/// `__scFail(id, message, stack)` — the module threw.
///
/// The stack is taken but not carried: it is the module's own JavaScript, which
/// belongs in the log rather than in the sentence an admin is shown. The message
/// is [`module_error`]'s, so a module's throw is an **Application** error, as it
/// was over the pipe.
fn host_fail(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let id = args.get(0).integer_value(scope).unwrap_or(0).max(0) as u64;
    let message = args.get(1).to_rust_string_lossy(scope);
    settle(scope, id, Err(module_error(&message)));
}

/// `__scLog(level, module, message)` — one `console.*` line from a module.
///
/// Straight into [`sc_log`] from the worker thread, tagged with the module that
/// was running. Not forwarded through a pipe and not printed to stderr beside
/// the server's log: one log, one format, and a `console.log` in a module that
/// is findable by the module's name.
fn host_log(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let level = args.get(0).to_rust_string_lossy(scope);
    let module = args.get(1);
    let who = if module.is_string() {
        format!("module `{}`", module.to_rust_string_lossy(scope))
    } else {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let index = op_state
            .borrow()
            .try_borrow::<Bridge>()
            .map_or(0, |bridge| bridge.index);
        // Nobody's call was running: the host script itself, or a module's
        // callback that outlived every async context there was to inherit.
        format!("module worker {index}")
    };
    let message = args.get(2).to_rust_string_lossy(scope);
    match level.as_str() {
        "error" => sc_log::log_error!("saltcorn: {who}: {message}"),
        "warning" => sc_log::log_warn!("saltcorn: {who}: {message}"),
        "verbose" => sc_log::log_verbose!("saltcorn: {who}: {message}"),
        _ => sc_log::log_info!("saltcorn: {who}: {message}"),
    }
}

/// Put one native function on the isolate's global.
fn define(
    scope: &mut v8::PinScope,
    global: v8::Local<v8::Object>,
    name: &str,
    callback: impl v8::MapFnTo<v8::FunctionCallback>,
) -> Result<()> {
    let key = v8::String::new(scope, name)
        .ok_or_else(|| Error::msg(format!("the module worker could not name `{name}`")))?;
    let value = v8::Function::new(scope, callback)
        .ok_or_else(|| Error::msg(format!("the module worker could not build `{name}`")))?;
    global.set(scope, key.into(), value.into());
    Ok(())
}

// ---------------------------------------------------------------------------
// One running host
// ---------------------------------------------------------------------------

/// The isolate, the entry point into it, the answers coming back, and the
/// watchdog over it.
///
/// The fields are separate rather than behind methods because the worker's
/// `select!` borrows two of them at once — the event loop mutably and the answer
/// channel mutably — and they are disjoint.
struct Host {
    worker: MainWorker,
    /// The isolate's `OpState`, held here so `process.exit()` can be checked for
    /// without touching [`Host::worker`], which the event loop has borrowed.
    op_state: Rc<RefCell<OpState>>,
    /// `globalThis.__scModuleHost`, resolved once: a call is a call of this.
    entry: v8::Global<v8::Function>,
    /// Answers, from the three native functions above.
    settled: tokio::sync::mpsc::UnboundedReceiver<Settled>,
    watchdog: Arc<Watchdog>,
}

impl Drop for Host {
    fn drop(&mut self) {
        self.watchdog.halt();
    }
}

impl Host {
    /// Write the host script, build the worker, install the seam, and evaluate
    /// it.
    async fn start(config: &WorkerConfig) -> Result<Host> {
        std::fs::create_dir_all(&config.root).map_err(|e| {
            Error::config(format!(
                "creating the modules directory {}: {e}",
                config.root.display()
            ))
        })?;
        // Written from the binary at every start: the binary is the authority,
        // and a stale copy from an older version would be a bug nobody would
        // look for.
        let script = config.root.join(HOST_SCRIPT_NAME);
        std::fs::write(&script, HOST_SCRIPT)
            .map_err(|e| Error::config(format!("writing {}: {e}", script.display())))?;
        let cwd = std::env::current_dir()
            .map_err(|e| Error::config(format!("the server has no working directory: {e}")))?;
        let url = deno_core::resolve_path(script.to_string_lossy().as_ref(), &cwd)
            .map_err(|e| Error::config(format!("{}: {e}", script.display())))?;

        let mut worker = super::wiring::build_worker(&config.root, &url, config.max_heap);
        let op_state = worker.js_runtime.op_state();
        let handle = worker.js_runtime.v8_isolate().thread_safe_handle();

        // §4: with an exit handle in the `OpState`, `deno_os`'s `op_exit`
        // terminates *this isolate* and sets a marker instead of calling
        // `std::process::exit` — which in this process is the server.
        op_state.borrow_mut().put(WatcherExitHandle(handle.clone()));

        let (sender, settled) = tokio::sync::mpsc::unbounded_channel::<Settled>();
        op_state.borrow_mut().put(Bridge {
            settled: sender,
            index: config.index,
        });

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

        // **Before** the script is evaluated, because its first lines read the
        // three functions off the global and hold them.
        install_seam(&mut worker)?;

        watchdog.tick();
        worker
            .execute_main_module(&url)
            .await
            .map_err(|e| Error::config(format!("the module host script would not start: {e}")))?;
        let entry = entry_point(&mut worker)?;

        Ok(Host {
            worker,
            op_state,
            entry,
            settled,
            watchdog,
        })
    }

    /// Call `__scModuleHost(id, request)`.
    ///
    /// Synchronous, and it returns as soon as the host script's entry point does
    /// — which is at its first `await`. The answer arrives later, through
    /// [`host_done`] or [`host_fail`], while this worker's event loop is being
    /// polled.
    fn call(&mut self, id: u64, request: &Json) -> Result<()> {
        deno_core::scope!(scope, self.worker.js_runtime);
        v8::tc_scope!(let scope, scope);
        let entry = v8::Local::new(scope, &self.entry);
        let receiver: v8::Local<v8::Value> = v8::undefined(scope).into();
        #[expect(
            clippy::cast_precision_loss,
            reason = "an id is a per-worker counter; a worker that answered 2^53 calls has \
                      other problems"
        )]
        let id_value: v8::Local<v8::Value> = v8::Number::new(scope, id as f64).into();
        let request = deno_core::serde_v8::to_v8(scope, request)
            .map_err(|e| Error::msg(format!("encoding a module-host request: {e}")))?;
        entry.call(scope, receiver, &[id_value, request]);
        if scope.has_terminated() {
            return Err(Error::config(
                "the module worker was stopped while this call was being made",
            ));
        }
        if let Some(exception) = scope.exception() {
            // Unreachable while the host script settles everything through
            // `__scFail`, which is what it is written to do. Named rather than
            // swallowed, because the symptom of getting it wrong would otherwise
            // be a call that never answers.
            let message = exception.to_rust_string_lossy(scope);
            return Err(Error::msg(format!(
                "the module host's entry point threw: {message}"
            )));
        }
        Ok(())
    }
}

/// Install `__scDone`, `__scFail` and `__scLog` on the isolate's global.
fn install_seam(worker: &mut MainWorker) -> Result<()> {
    deno_core::scope!(scope, worker.js_runtime);
    let context = scope.get_current_context();
    let global = context.global(scope);
    define(scope, global, "__scDone", host_done)?;
    define(scope, global, "__scFail", host_fail)?;
    define(scope, global, "__scLog", host_log)?;
    Ok(())
}

/// `globalThis.__scModuleHost`, as something callable for the worker's lifetime.
fn entry_point(worker: &mut MainWorker) -> Result<v8::Global<v8::Function>> {
    deno_core::scope!(scope, worker.js_runtime);
    let context = scope.get_current_context();
    let global = context.global(scope);
    let key = v8::String::new(scope, "__scModuleHost")
        .ok_or_else(|| Error::msg("the module worker could not name its entry point"))?;
    let value = global
        .get(scope, key.into())
        .ok_or_else(|| Error::config("the module host script defined no entry point"))?;
    let entry: v8::Local<v8::Function> = value.try_into().map_err(|_| {
        Error::config("the module host script's `__scModuleHost` is not a function")
    })?;
    Ok(v8::Global::new(scope, entry))
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
    /// The event loop failed. Its *finishing* is not an ending — see
    /// [`serve`]'s `resting`.
    EventLoop(deno_core::error::CoreError),
    /// The answer channel closed, which only the isolate going away can do.
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

/// What one turn of the worker's loop was woken by.
///
/// The `select!` produces one of these and does nothing else, because two of the
/// things that have to happen next — calling into the isolate, and polling its
/// event loop — both want the worker mutably, and a `select!` arm cannot borrow
/// what another arm's future is holding.
enum Woke {
    Control(Option<Control>),
    Settled(Option<Settled>),
    EventLoop(std::result::Result<(), deno_core::error::CoreError>),
    Tick,
}

/// The worker's life: idle, then a host, then — if the host dies — idle again.
async fn serve(config: WorkerConfig, mut rx: tokio::sync::mpsc::UnboundedReceiver<Control>) {
    let mut loads: BTreeMap<String, LoadRequest> = BTreeMap::new();

    'idle: loop {
        // **Lazily started.** A deployment with no modules never builds an
        // isolate, and never needs anything on its PATH — which is the bargain
        // the sidecar already made by not spawning `node` until it was needed.
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

        // Replayed **before** the first job is called, so no caller's `run` can
        // reach a fresh worker before its modules are back. Not awaited: the
        // host script's own `loading` map makes a `run` wait for a load that is
        // still in flight, which is the same guarantee for less machinery.
        for (name, request) in &loads {
            let id = next_id;
            next_id += 1;
            let request = json!({
                "op": "load",
                "module": name,
                "dir": request.dir.display().to_string(),
                "configuration": request.configuration,
            });
            if host.call(id, &request).is_ok() {
                pending.insert(id, Pending::Replay(name.clone()));
            }
        }
        submit(&mut host, &mut next_id, &mut pending, *first);

        // Whether the event loop has said it has nothing left to do.
        //
        // Over the pipe this could not happen — a `readline` on stdin is a
        // resource, so the loop was never finished — and `run_event_loop`
        // returning was an ending. It is not one now: it is what an idle module
        // host *is* between calls, and the branch is taken out of the `select!`
        // until the next call into the isolate gives it something to drive.
        // Leaving it in would be a spin at the speed of the scheduler.
        let mut resting = false;
        let mut stop_ack: Option<oneshot::Sender<()>> = None;
        let ended = loop {
            host.watchdog.tick();
            let woke = tokio::select! {
                biased;
                control = rx.recv() => Woke::Control(control),
                answer = host.settled.recv() => Woke::Settled(answer),
                result = host.worker.run_event_loop(false), if !resting => Woke::EventLoop(result),
                () = tokio::time::sleep(TICK) => Woke::Tick,
            };
            match woke {
                Woke::Control(None) => break End::Stop,
                Woke::Control(Some(Control::Stop(ack))) => {
                    stop_ack = Some(ack);
                    break End::Stop;
                }
                Woke::Control(Some(Control::Forget(name))) => {
                    loads.remove(&name);
                }
                Woke::Control(Some(Control::Job(job))) => {
                    resting = false;
                    submit(&mut host, &mut next_id, &mut pending, *job);
                }
                Woke::Settled(None) => break End::Silent,
                Woke::Settled(Some(answer)) => dispatch(&mut pending, &mut loads, answer),
                Woke::EventLoop(Ok(())) => resting = true,
                Woke::EventLoop(Err(e)) => break End::EventLoop(e),
                Woke::Tick => {
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
        // trip reason cannot become "the isolate went away".
        End::Watchdog => "the worker's watchdog stopped it".to_owned(),
        End::Exited => "a module called process.exit()".to_owned(),
        End::Silent => "the isolate went away".to_owned(),
        End::EventLoop(e) => format!("the host script failed: {e}"),
    }
}

/// Give a job an id, call the isolate with it, and remember who is waiting for
/// the answer.
fn submit(host: &mut Host, next_id: &mut u64, pending: &mut HashMap<u64, Pending>, job: Job) {
    let id = *next_id;
    *next_id += 1;
    let Job {
        request,
        remember,
        reply,
    } = job;
    if let Err(e) = host.call(id, &request) {
        let _ = reply.send(Err(e));
        return;
    }
    pending.insert(id, Pending::Call { reply, remember });
}

/// One answer: to whoever is waiting for it, and to the replay table if it was a
/// load that worked.
fn dispatch(
    pending: &mut HashMap<u64, Pending>,
    loads: &mut BTreeMap<String, LoadRequest>,
    answer: Settled,
) {
    let Some(entry) = pending.remove(&answer.id) else {
        return;
    };
    match entry {
        Pending::Call {
            reply: sender,
            remember,
        } => {
            if answer.outcome.is_ok()
                && let Some((name, request)) = remember
            {
                loads.insert(name, request);
            }
            let _ = sender.send(answer.outcome);
        }
        Pending::Replay(name) => {
            if let Err(e) = answer.outcome {
                sc_log::log_error!("saltcorn: replaying the load of `{name}` failed: {e}");
            }
        }
    }
}
