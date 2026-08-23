//! **Modules in-process**: the Deno worker a module runs on, and the pool that
//! owns it (TODO "Modules in-process", phases 1 and 2).
//!
//! A module used to run in a `node` child process. Here it is a worker thread
//! instead, on the same V8 the rest of the server already links. The reason is
//! not memory — phase 0 measured that saving at ~45 MB and one
//! process, which is real but small. The reasons are that `node` stops being a
//! runtime requirement of a Saltcorn server, and that `deno_permissions` gives a
//! module something `node` has no way to offer: a permission set (phase 3).
//!
//! This is what every module call goes through. [`crate::host`]'s `ModuleHost`
//! is the façade the rest of the server names, and it is a few lines over this.
//!
//! ## Why this is a second pool and not the code isolates
//!
//! `sc_expr`'s `CodeRuntime` is a pool of bare V8 isolates with four ops and no
//! module loader; a module needs `require`, `node:net`, and an npm resolver. The
//! two could share a pool of node-capable isolates and must not, because a
//! module is **long-lived state** and a code-body isolate is **disposable by
//! design**. `@saltcorn/mqtt` holds a module-level client — a socket with a
//! reconnect timer and live callbacks that have to survive between calls —
//! while the JS-slice watchdog stops the isolate and everything resident on it.
//! Merging the pools would give one runaway `while(true)` in a trigger a
//! coin-flip chance of terminating a broker subscription. Today that chance is
//! zero, and stopping a runaway body is a routine handled event.
//!
//! The bounds differ by an order of magnitude for the same reason — see
//! [`crate::bounds`].
//!
//! ## The shape of one worker
//!
//! One OS thread per worker, a current-thread tokio runtime on it, and a
//! `deno_runtime` `MainWorker` on that. The tokio runtime is both the isolate's
//! anchor (`deno_core` registers the isolate against whatever runtime was
//! current when it was built) and what drives the worker's event loop — the
//! arrangement `sc_expr`'s `build_isolate` documents for the code pool.
//!
//! **A module is pinned to a worker** for its lifetime, because that is where
//! its `require` cache and its module-level state live. Loading the same module
//! on two workers would be two MQTT connections and two configurations, which is
//! not a second copy of a module but a second module.
//!
//! ## There is no transport
//!
//! A call is `globalThis.__scModuleHost(id, request)` — one V8 function call,
//! with the request converted straight into a V8 object — and the answer comes
//! back through native functions on the isolate's global. No framing, no pipe,
//! no reader thread, and no serialise-and-parse on the way in. What the ids buy
//! is what they always bought: many calls in flight at once, so a slow module's
//! action does not hold anybody else's. [`worker`] has the details, including
//! why these are functions on the global rather than `deno_core` ops.
//!
//! Phase 1 ran the sidecar's newline-JSON protocol over a pair of in-process
//! pipes, on purpose: `module-host.mjs`'s portability is what phase 0 proved,
//! and a step that changed the runtime *and* the protocol could not say which
//! half broke. Phase 2 is the second step, and the pipes and their two helper
//! threads went with it.
//!
//! ## What a dead worker costs
//!
//! Exactly what a dead sidecar costs, which is the behaviour this milestone had
//! to be careful not to lose. A module that calls `process.exit()` — or spins
//! past its JS slice, or exhausts the heap — ends **its own worker**: every call
//! in flight on it is failed by name, and the next call gets a fresh worker with
//! every load replayed into it. The server does not exit, and a module on
//! another worker does not notice.
//!
//! `process.exit()` reaching a worker rather than the process is
//! [`deno_os::WatcherExitHandle`](deno_runtime::deno_os::WatcherExitHandle): with
//! one in the worker's `OpState`, `op_exit` terminates that isolate and records
//! a marker instead of calling `std::process::exit`. The marker matters as much
//! as the termination — `terminate_execution` only throws out of *running*
//! JavaScript, so a module that exits while the event loop is parked on an idle
//! socket leaves the loop parked, and the host, not V8, is what must then drop
//! the worker.

mod wiring;
mod worker;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sc_error::{Error, Result};
use serde_json::{Value as Json, json};
use tokio::sync::{Mutex, oneshot};

use crate::bounds::{
    DEFAULT_CALL_TIMEOUT, DEFAULT_MODULE_JS_SLICE, DEFAULT_MODULE_MAX_HEAP, DEFAULT_MODULE_WORKERS,
};
use crate::host::ModuleManifest;

pub use worker::{Control, Job, LoadRequest, WorkerConfig};

/// The bounds every worker in a pool is built with.
#[derive(Debug, Clone, Copy)]
pub struct PoolBounds {
    /// The wall clock one call is given.
    pub timeout: Duration,
    /// How long a module's JavaScript may run without yielding.
    pub js_slice: Duration,
    /// The isolate's heap ceiling.
    pub max_heap: usize,
}

impl Default for PoolBounds {
    fn default() -> Self {
        PoolBounds {
            timeout: DEFAULT_CALL_TIMEOUT,
            js_slice: DEFAULT_MODULE_JS_SLICE,
            max_heap: DEFAULT_MODULE_MAX_HEAP,
        }
    }
}

/// One worker, from the pool's side: a channel and the modules pinned to it.
struct WorkerHandle {
    control: tokio::sync::mpsc::UnboundedSender<Control>,
    thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// The module pool: `n` Deno workers over one modules root.
///
/// Nothing is constructed until the first call. A deployment with no modules
/// pays for `n` parked OS threads and no isolates at all — the same bargain the
/// sidecar made by not spawning `node` until it was needed.
pub struct DenoModuleHost {
    root: PathBuf,
    bounds: PoolBounds,
    workers: Vec<WorkerHandle>,
    /// Which worker each module is pinned to, by module name.
    pinned: Mutex<BTreeMap<String, usize>>,
}

impl DenoModuleHost {
    /// A pool of [`DEFAULT_MODULE_WORKERS`] workers over `root`.
    pub fn new(root: impl Into<PathBuf>) -> DenoModuleHost {
        DenoModuleHost::with_workers(root, DEFAULT_MODULE_WORKERS)
    }

    /// A pool of `workers` workers (at least one) over `root`.
    pub fn with_workers(root: impl Into<PathBuf>, workers: usize) -> DenoModuleHost {
        DenoModuleHost::build(root.into(), workers, PoolBounds::default())
    }

    /// The one constructor the others go through, with every bound spelled out.
    /// The tests are what want to say a JS slice in milliseconds.
    pub fn build(root: PathBuf, workers: usize, bounds: PoolBounds) -> DenoModuleHost {
        let mut pool = Vec::new();
        for n in 0..workers.max(1) {
            let (control, rx) = tokio::sync::mpsc::unbounded_channel::<Control>();
            let config = WorkerConfig {
                index: n,
                root: root.clone(),
                js_slice: bounds.js_slice,
                max_heap: bounds.max_heap,
            };
            let thread = std::thread::Builder::new()
                .name(format!("sc-module-{n}"))
                .spawn(move || worker::worker_thread(config, rx))
                // Thread spawning fails only on process-level resource
                // exhaustion, and a call would then fail on a closed channel,
                // which is the honest symptom.
                .ok();
            pool.push(WorkerHandle {
                control,
                thread: std::sync::Mutex::new(thread),
            });
        }
        DenoModuleHost {
            root,
            bounds,
            workers: pool,
            pinned: Mutex::new(BTreeMap::new()),
        }
    }

    /// The wall clock a call with no bound of its own is given.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> DenoModuleHost {
        self.bounds.timeout = timeout;
        self
    }

    /// The modules root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Load (or reload) a module from `dir`, with `configuration` as the object
    /// handed to v1's `actions(cfg)`.
    ///
    /// Idempotent, and idempotent **on the same worker**: a reload after a
    /// configuration change replaces the module where its state already is,
    /// rather than leaving a second copy of it somewhere else.
    pub async fn load(
        &self,
        name: &str,
        dir: &Path,
        configuration: &Json,
    ) -> Result<ModuleManifest> {
        let index = self.pin(name).await;
        let request = LoadRequest {
            dir: dir.to_path_buf(),
            configuration: configuration.clone(),
        };
        let value = self
            .send(
                index,
                json!({
                    "op": "load",
                    "module": name,
                    "dir": request.dir.display().to_string(),
                    "configuration": request.configuration,
                }),
                Some((name.to_owned(), request)),
            )
            .await?;
        serde_json::from_value(value).map_err(|e| {
            Error::msg(format!(
                "the module host answered a load of `{name}` with something unreadable: {e}"
            ))
        })
    }

    /// Forget a module — after an uninstall, so a restarted worker does not
    /// reload a package that is no longer there.
    pub async fn unload(&self, name: &str) {
        let index = self.pinned.lock().await.remove(name);
        let Some(index) = index else { return };
        // The replay table is cleared even if the call fails: a worker that is
        // not running has already forgotten the module, and one that is must not
        // resurrect it at its next restart.
        if let Some(worker) = self.workers.get(index) {
            let _ = worker.control.send(Control::Forget(name.to_owned()));
        }
        let _ = self
            .send(index, json!({ "op": "unload", "module": name }), None)
            .await;
    }

    /// Run one action of one module with v1's argument object.
    ///
    /// Routed to the worker the module was loaded on, because that is where its
    /// state is. A module nobody has loaded is routed to the first worker, so
    /// the host script answers with its own "the module … is not loaded in this
    /// host" rather than this layer inventing a second wording for it.
    pub async fn run(&self, module: &str, action: &str, args: Json) -> Result<Json> {
        let index = self.pinned.lock().await.get(module).copied().unwrap_or(0);
        self.send(
            index,
            json!({
                "op": "run",
                "module": module,
                "action": action,
                "args": args,
            }),
            None,
        )
        .await
    }

    /// Call one function of one module with v1's positional arguments (§4a).
    ///
    /// Routed like [`run`](DenoModuleHost::run), and for a sharper reason: a v1
    /// function closes over what its module built at load time, so it has to
    /// execute on the isolate that module was loaded on. That is what makes this
    /// a hop at all — module state is a singleton, and no arrangement of pools
    /// changes it.
    pub async fn call(&self, module: &str, function: &str, args: Vec<Json>) -> Result<Json> {
        let index = self.pinned.lock().await.get(module).copied().unwrap_or(0);
        self.send(
            index,
            json!({
                "op": "call",
                "module": module,
                "function": function,
                "args": args,
            }),
            None,
        )
        .await
    }

    /// Ask a worker to say hello — what a test and a diagnostics screen use to
    /// find out whether the pool starts at all.
    pub async fn ping(&self) -> Result<Json> {
        self.send(0, json!({ "op": "ping" }), None).await
    }

    /// Which worker a module lives on, or `None` if it has never been loaded.
    /// The Modules tab's answer to "where is this thing running".
    pub async fn worker_of(&self, module: &str) -> Option<usize> {
        self.pinned.lock().await.get(module).copied()
    }

    /// Stop every worker and wait for its thread.
    ///
    /// Each worker is *asked*, and answers when its loop has ended and its
    /// isolate has been dropped — so a shutdown that returns is a shutdown in
    /// which no module's JavaScript is still running.
    pub async fn shutdown(&self) {
        let mut acks = Vec::new();
        for worker in &self.workers {
            let (ack, done) = oneshot::channel();
            if worker.control.send(Control::Stop(ack)).is_ok() {
                acks.push(done);
            }
        }
        for ack in acks {
            let _ = ack.await;
        }
        for worker in &self.workers {
            let thread = worker.thread.lock().ok().and_then(|mut t| t.take());
            if let Some(thread) = thread {
                let _ = thread.join();
            }
        }
        self.pinned.lock().await.clear();
    }

    /// The worker a module belongs to, assigning one on its first load.
    ///
    /// Fewest-modules wins. Not fewest *calls*: a worker's cost is the state its
    /// modules hold, and a module that is called a thousand times an hour costs
    /// the isolate no more than one that is called never.
    ///
    /// The pin is kept even when the load then fails, so a retry goes back to
    /// the worker that has whatever half-loaded state the failure left, rather
    /// than spreading one broken module's wreckage over the pool.
    async fn pin(&self, name: &str) -> usize {
        let mut pinned = self.pinned.lock().await;
        if let Some(index) = pinned.get(name) {
            return *index;
        }
        let mut counts = vec![0usize; self.workers.len().max(1)];
        for index in pinned.values() {
            if let Some(count) = counts.get_mut(*index) {
                *count += 1;
            }
        }
        let index = counts
            .iter()
            .enumerate()
            .min_by_key(|(_, count)| **count)
            .map_or(0, |(index, _)| index);
        pinned.insert(name.to_owned(), index);
        index
    }

    /// Send one request to one worker and await its reply, under the wall clock.
    async fn send(
        &self,
        index: usize,
        request: Json,
        remember: Option<(String, LoadRequest)>,
    ) -> Result<Json> {
        let worker = self
            .workers
            .get(index)
            .ok_or_else(|| Error::msg("the module pool has no such worker"))?;
        let (reply, answer) = oneshot::channel();
        worker
            .control
            .send(Control::Job(Box::new(Job {
                request,
                remember,
                reply,
            })))
            .map_err(|_| Error::config("the module pool's worker has stopped"))?;

        match tokio::time::timeout(self.bounds.timeout, answer).await {
            Ok(Ok(result)) => result,
            // The sender was dropped without a reply: the worker died and
            // cleared its call table.
            Ok(Err(_)) => Err(Error::config(
                "the module host stopped before answering this call",
            )),
            Err(_) => Err(Error::config(format!(
                "a module call took longer than {:?} and was given up on",
                self.bounds.timeout
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinning is the pool's own arithmetic and does not need an isolate to
    /// check: a module goes to the emptiest worker, and stays there.
    #[tokio::test]
    async fn a_module_is_pinned_to_one_worker_and_stays_there() {
        let pool = DenoModuleHost::with_workers("/nonexistent/modules", 3);
        let first = pool.pin("@saltcorn/mqtt").await;
        assert_eq!(pool.pin("@saltcorn/mqtt").await, first);
        assert_eq!(pool.worker_of("@saltcorn/mqtt").await, Some(first));
        assert_eq!(pool.worker_of("@saltcorn/proxmox").await, None);
    }

    #[tokio::test]
    async fn modules_spread_across_the_workers_before_they_double_up() {
        let pool = DenoModuleHost::with_workers("/nonexistent/modules", 3);
        let mut seen = std::collections::BTreeSet::new();
        for name in ["a", "b", "c"] {
            seen.insert(pool.pin(name).await);
        }
        assert_eq!(seen.len(), 3, "three modules, three workers: {seen:?}");
        // The fourth has to share, and shares with the first worker rather than
        // picking one at random.
        assert_eq!(pool.pin("d").await, 0);
    }

    #[tokio::test]
    async fn a_pool_with_no_workers_asked_for_still_has_one() {
        let pool = DenoModuleHost::with_workers("/nonexistent/modules", 0);
        assert_eq!(pool.workers.len(), 1);
        assert_eq!(pool.pin("a").await, 0);
        pool.shutdown().await;
    }
}
