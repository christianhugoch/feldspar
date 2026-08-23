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
//! It is pinned to a worker **whose permission set is its own** (phase 3): a
//! `PermissionsContainer` is handed to an isolate, so two modules share a worker
//! only when they may reach the same things. See [`DenoModuleHost`].
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
use crate::permissions::ModulePermissions;

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

/// One worker, from the pool's side: its channel, its thread, the permission
/// set it was built with, and how many modules are pinned to it.
struct WorkerHandle {
    control: tokio::sync::mpsc::UnboundedSender<Control>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// What the modules on this worker may reach. Fixed for the worker's life:
    /// a `PermissionsContainer` belongs to an isolate, so changing it means a
    /// different worker.
    permissions: ModulePermissions,
    /// How many modules are pinned here. A worker that reaches zero is stopped,
    /// which is what makes an admin editing a module's permissions cost one
    /// worker rather than one worker per edit.
    modules: usize,
}

/// The pool's mutable half: the workers, and which one each module is on.
///
/// Slots rather than a plain list, because a retired worker's index must not be
/// reused by a *different* worker while a module is still pinned to it — the
/// pinned map holds indices, and shifting them would route a call to somebody
/// else's isolate.
#[derive(Default)]
struct Pool {
    workers: Vec<Option<WorkerHandle>>,
    pinned: BTreeMap<String, usize>,
}

/// The module pool: Deno workers over one modules root, one permission set each.
///
/// Nothing is constructed until the first call: a deployment with no modules
/// builds no isolate and starts no thread, which is the bargain the sidecar
/// already made by not spawning `node` until it was needed.
///
/// ## Why a worker is per permission set (phase 3)
///
/// A [`PermissionsContainer`](deno_runtime::deno_permissions::PermissionsContainer)
/// is handed to a worker when its isolate is built, and there is no per-module
/// fence inside one isolate — so two modules may share a worker only if they may
/// reach the same things. The pool therefore pins by permission set: a module
/// joins a worker whose set equals its own, and starts a new one when none does.
/// The configured worker count (`--module-workers`) is how many workers modules
/// *sharing* a set may spread over, which leaves a server whose modules are all
/// closed — the default — running exactly the one worker it ran before.
///
/// The alternative would be to widen a worker's set to the union of its
/// modules', which is a module quietly acquiring a capability granted to
/// somebody else. That is the failure §2 is about, so it is not on offer.
pub struct DenoModuleHost {
    root: PathBuf,
    bounds: PoolBounds,
    /// How many workers modules sharing one permission set may spread over.
    per_set: usize,
    pool: Mutex<Pool>,
}

impl DenoModuleHost {
    /// A pool over `root` with [`DEFAULT_MODULE_WORKERS`] workers per permission
    /// set.
    pub fn new(root: impl Into<PathBuf>) -> DenoModuleHost {
        DenoModuleHost::with_workers(root, DEFAULT_MODULE_WORKERS)
    }

    /// A pool of `workers` workers per permission set (at least one) over
    /// `root`.
    pub fn with_workers(root: impl Into<PathBuf>, workers: usize) -> DenoModuleHost {
        DenoModuleHost::build(root.into(), workers, PoolBounds::default())
    }

    /// The one constructor the others go through, with every bound spelled out.
    /// The tests are what want to say a JS slice in milliseconds.
    pub fn build(root: PathBuf, workers: usize, bounds: PoolBounds) -> DenoModuleHost {
        DenoModuleHost {
            root,
            bounds,
            per_set: workers.max(1),
            pool: Mutex::new(Pool::default()),
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
    /// handed to v1's `actions(cfg)` and `permissions` as what its worker may
    /// reach.
    ///
    /// Idempotent, and idempotent **on the same worker**: a reload after a
    /// configuration change replaces the module where its state already is,
    /// rather than leaving a second copy of it somewhere else.
    ///
    /// Unless the *permissions* changed, in which case the module moves: it is
    /// unloaded from the worker it was on — taking its sockets and its
    /// module-level state with it — and loaded on one that grants what it now
    /// has. There is no other way round it, because the permission set belongs
    /// to the isolate.
    pub async fn load(
        &self,
        name: &str,
        dir: &Path,
        configuration: &Json,
        permissions: &ModulePermissions,
    ) -> Result<ModuleManifest> {
        let (index, moved_from) = self.pin(name, permissions).await;
        if let Some(old) = moved_from {
            self.evict(old, name).await;
        }
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
            .await
            .map_err(|e| denial(name, e))?;
        serde_json::from_value(value).map_err(|e| {
            Error::msg(format!(
                "the module host answered a load of `{name}` with something unreadable: {e}"
            ))
        })
    }

    /// Forget a module — after an uninstall, so a restarted worker does not
    /// reload a package that is no longer there.
    pub async fn unload(&self, name: &str) {
        let index = {
            let mut pool = self.pool.lock().await;
            let index = pool.pinned.remove(name);
            if let Some(index) = index {
                pool.release(index);
            }
            index
        };
        let Some(index) = index else { return };
        self.evict(index, name).await;
    }

    /// Run one action of one module with v1's argument object.
    ///
    /// Routed to the worker the module was loaded on, because that is where its
    /// state is. A module nobody has loaded is routed to any worker, so the host
    /// script answers with its own "the module … is not loaded in this host"
    /// rather than this layer inventing a second wording for it.
    pub async fn run(&self, module: &str, action: &str, args: Json) -> Result<Json> {
        let index = self.worker_for(module).await;
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
        .map_err(|e| denial(module, e))
    }

    /// Call one function of one module with v1's positional arguments (§4a).
    ///
    /// Routed like [`run`](DenoModuleHost::run), and for a sharper reason: a v1
    /// function closes over what its module built at load time, so it has to
    /// execute on the isolate that module was loaded on. That is what makes this
    /// a hop at all — module state is a singleton, and no arrangement of pools
    /// changes it.
    pub async fn call(&self, module: &str, function: &str, args: Vec<Json>) -> Result<Json> {
        let index = self.worker_for(module).await;
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
        .map_err(|e| denial(module, e))
    }

    /// Ask a worker to say hello — what a test and a diagnostics screen use to
    /// find out whether the pool starts at all.
    pub async fn ping(&self) -> Result<Json> {
        let index = self.worker_for("").await;
        self.send(index, json!({ "op": "ping" }), None).await
    }

    /// Which worker a module lives on, or `None` if it has never been loaded.
    /// The Modules tab's answer to "where is this thing running".
    pub async fn worker_of(&self, module: &str) -> Option<usize> {
        self.pool.lock().await.pinned.get(module).copied()
    }

    /// How many workers are running — what a permission change costs, made
    /// visible.
    pub async fn workers(&self) -> usize {
        self.pool
            .lock()
            .await
            .workers
            .iter()
            .filter(|slot| slot.is_some())
            .count()
    }

    /// Stop every worker and wait for its thread.
    ///
    /// Each worker is *asked*, and answers when its loop has ended and its
    /// isolate has been dropped — so a shutdown that returns is a shutdown in
    /// which no module's JavaScript is still running.
    pub async fn shutdown(&self) {
        let workers: Vec<WorkerHandle> = {
            let mut pool = self.pool.lock().await;
            pool.pinned.clear();
            pool.workers.drain(..).flatten().collect()
        };
        let mut acks = Vec::new();
        let mut threads = Vec::new();
        for mut worker in workers {
            let (ack, done) = oneshot::channel();
            if worker.control.send(Control::Stop(ack)).is_ok() {
                acks.push(done);
            }
            threads.extend(worker.thread.take());
        }
        for ack in acks {
            let _ = ack.await;
        }
        for thread in threads {
            let _ = thread.join();
        }
    }

    /// The worker a module belongs to, assigning one on its first load, and the
    /// worker it has to leave when its permissions changed under it.
    ///
    /// Fewest-modules wins **within a permission set**. Not fewest *calls*: a
    /// worker's cost is the state its modules hold, and a module that is called
    /// a thousand times an hour costs the isolate no more than one that is
    /// called never.
    ///
    /// The pin is kept even when the load then fails, so a retry goes back to
    /// the worker that has whatever half-loaded state the failure left, rather
    /// than spreading one broken module's wreckage over the pool.
    async fn pin(&self, name: &str, permissions: &ModulePermissions) -> (usize, Option<usize>) {
        let mut pool = self.pool.lock().await;
        let mut moved_from = None;
        if let Some(index) = pool.pinned.get(name).copied() {
            if pool.permissions_of(index) == Some(permissions) {
                return (index, None);
            }
            // The admin granted or withdrew something: the module leaves.
            pool.pinned.remove(name);
            pool.release(index);
            moved_from = Some(index);
        }
        let group: Vec<usize> = pool.group(permissions);
        let index = if group.len() < self.per_set {
            self.spawn(&mut pool, permissions)
        } else {
            group
                .into_iter()
                .min_by_key(|index| pool.workers[*index].as_ref().map_or(0, |w| w.modules))
                .unwrap_or(0)
        };
        pool.pinned.insert(name.to_owned(), index);
        if let Some(worker) = pool.workers.get_mut(index).and_then(Option::as_mut) {
            worker.modules += 1;
        }
        (index, moved_from)
    }

    /// The worker a call is routed to: the module's own, or any running one so
    /// that the host script answers "not loaded" in its own words.
    async fn worker_for(&self, module: &str) -> usize {
        let mut pool = self.pool.lock().await;
        if let Some(index) = pool.pinned.get(module).copied() {
            return index;
        }
        if let Some(index) = pool.any() {
            return index;
        }
        // Nothing is running: a `ping` before any module was loaded, or a run
        // of a module nobody installed. The worker it starts is the closed one,
        // which is what a module with no grants would have got anyway.
        self.spawn(&mut pool, &ModulePermissions::closed())
    }

    /// Start a worker for `permissions` and return its index.
    fn spawn(&self, pool: &mut Pool, permissions: &ModulePermissions) -> usize {
        let index = pool
            .workers
            .iter()
            .position(Option::is_none)
            .unwrap_or(pool.workers.len());
        let (control, rx) = tokio::sync::mpsc::unbounded_channel::<Control>();
        let config = WorkerConfig {
            index,
            root: self.root.clone(),
            js_slice: self.bounds.js_slice,
            max_heap: self.bounds.max_heap,
            permissions: permissions.clone(),
        };
        let thread = std::thread::Builder::new()
            .name(format!("sc-module-{index}"))
            .spawn(move || worker::worker_thread(config, rx))
            // Thread spawning fails only on process-level resource exhaustion,
            // and a call would then fail on a closed channel, which is the
            // honest symptom.
            .ok();
        let handle = WorkerHandle {
            control,
            thread,
            permissions: permissions.clone(),
            modules: 0,
        };
        if index == pool.workers.len() {
            pool.workers.push(Some(handle));
        } else {
            pool.workers[index] = Some(handle);
        }
        index
    }

    /// Take a module off a worker: forget it from the replay table, unload it
    /// from the isolate, and stop the worker if it was the last one on it.
    ///
    /// Both halves matter. Forgetting alone would leave the module resident with
    /// its sockets open until something killed the worker; unloading alone would
    /// bring it back at the next restart.
    async fn evict(&self, index: usize, name: &str) {
        if let Some(control) = self.sender(index).await {
            let _ = control.send(Control::Forget(name.to_owned()));
        }
        let _ = self
            .send(index, json!({ "op": "unload", "module": name }), None)
            .await;
        let retired = {
            let mut pool = self.pool.lock().await;
            pool.retire_if_empty(index)
        };
        if let Some(mut worker) = retired {
            let (ack, done) = oneshot::channel();
            if worker.control.send(Control::Stop(ack)).is_ok() {
                let _ = done.await;
            }
            if let Some(thread) = worker.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// One worker's channel, cloned out from under the lock so that awaiting a
    /// reply does not hold the pool.
    async fn sender(&self, index: usize) -> Option<tokio::sync::mpsc::UnboundedSender<Control>> {
        self.pool
            .lock()
            .await
            .workers
            .get(index)
            .and_then(Option::as_ref)
            .map(|worker| worker.control.clone())
    }

    /// Send one request to one worker and await its reply, under the wall clock.
    async fn send(
        &self,
        index: usize,
        request: Json,
        remember: Option<(String, LoadRequest)>,
    ) -> Result<Json> {
        let control = self
            .sender(index)
            .await
            .ok_or_else(|| Error::msg("the module pool has no such worker"))?;
        let (reply, answer) = oneshot::channel();
        control
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

impl Pool {
    /// The workers running with exactly this permission set.
    fn group(&self, permissions: &ModulePermissions) -> Vec<usize> {
        self.workers
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                slot.as_ref()
                    .filter(|worker| worker.permissions == *permissions)
                    .map(|_| index)
            })
            .collect()
    }

    /// What a worker's isolate was built with, if it is still running.
    fn permissions_of(&self, index: usize) -> Option<&ModulePermissions> {
        self.workers
            .get(index)
            .and_then(Option::as_ref)
            .map(|worker| &worker.permissions)
    }

    /// Any running worker.
    fn any(&self) -> Option<usize> {
        self.workers.iter().position(Option::is_some)
    }

    /// One module fewer on this worker.
    fn release(&mut self, index: usize) {
        if let Some(worker) = self.workers.get_mut(index).and_then(Option::as_mut) {
            worker.modules = worker.modules.saturating_sub(1);
        }
    }

    /// Take the worker out of the pool if nothing is pinned to it any more, so
    /// the caller can stop it. Its slot stays, empty, so no live pin is ever
    /// re-pointed at a different isolate.
    fn retire_if_empty(&mut self, index: usize) -> Option<WorkerHandle> {
        let empty = self
            .workers
            .get(index)
            .and_then(Option::as_ref)
            .is_some_and(|worker| worker.modules == 0);
        if empty {
            self.workers.get_mut(index).and_then(Option::take)
        } else {
            None
        }
    }
}

/// A module's failure, with a permission denial rewritten into something an
/// admin can act on (§2).
///
/// Deno words a denial for somebody holding a command line — "run again with the
/// --allow-net flag" — and there is no command line here. Anything that is not a
/// denial is left exactly as the module worded it.
fn denial(module: &str, error: Error) -> Error {
    match crate::permissions::explain_denial(module, &error.to_string()) {
        Some(sentence) => Error::config(sentence),
        None => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A set granting one host, for the tests that need two sets that differ.
    fn one_broker() -> ModulePermissions {
        ModulePermissions {
            net: vec!["broker.example:1883".into()],
            ..ModulePermissions::closed()
        }
    }

    /// Pinning is the pool's own arithmetic and does not need an isolate to
    /// check: a module goes to the emptiest worker of its own permission set,
    /// and stays there.
    #[tokio::test]
    async fn a_module_is_pinned_to_one_worker_and_stays_there() {
        let pool = DenoModuleHost::with_workers("/nonexistent/modules", 3);
        let closed = ModulePermissions::closed();
        let (first, moved) = pool.pin("@saltcorn/mqtt", &closed).await;
        assert_eq!(moved, None);
        assert_eq!(pool.pin("@saltcorn/mqtt", &closed).await, (first, None));
        assert_eq!(pool.worker_of("@saltcorn/mqtt").await, Some(first));
        assert_eq!(pool.worker_of("@saltcorn/proxmox").await, None);
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn modules_spread_across_the_workers_before_they_double_up() {
        let pool = DenoModuleHost::with_workers("/nonexistent/modules", 3);
        let closed = ModulePermissions::closed();
        let mut seen = std::collections::BTreeSet::new();
        for name in ["a", "b", "c"] {
            seen.insert(pool.pin(name, &closed).await.0);
        }
        assert_eq!(seen.len(), 3, "three modules, three workers: {seen:?}");
        // The fourth has to share, and shares with the emptiest worker rather
        // than picking one at random.
        assert_eq!(pool.pin("d", &closed).await.0, 0);
        pool.shutdown().await;
    }

    /// Phase 3: a permission set is a property of the isolate, so it decides
    /// which worker a module may live on.
    #[tokio::test]
    async fn two_permission_sets_are_two_workers_even_in_a_pool_of_one() {
        let pool = DenoModuleHost::with_workers("/nonexistent/modules", 1);
        let closed = pool.pin("closed", &ModulePermissions::closed()).await.0;
        let open = pool.pin("mqtt", &one_broker()).await.0;
        assert_ne!(
            closed, open,
            "a module granted a host must not share an isolate with one that was granted nothing"
        );
        // And two modules granted the *same* thing do share, because the pool is
        // one worker per set and not one per module.
        assert_eq!(pool.pin("mqtt2", &one_broker()).await.0, open);
        assert_eq!(pool.workers().await, 2);
        pool.shutdown().await;
    }

    /// Editing a module's permissions moves it: the old worker is told, and it
    /// is stopped when the module was the last one on it.
    #[tokio::test]
    async fn a_permission_change_moves_the_module_off_its_worker() {
        let pool = DenoModuleHost::with_workers("/nonexistent/modules", 1);
        let before = pool.pin("mqtt", &ModulePermissions::closed()).await.0;
        let (after, moved_from) = pool.pin("mqtt", &one_broker()).await;
        assert_ne!(after, before);
        assert_eq!(
            moved_from,
            Some(before),
            "the caller has to unload the module where its state still is"
        );
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn a_pool_with_no_workers_asked_for_still_has_one() {
        let pool = DenoModuleHost::with_workers("/nonexistent/modules", 0);
        assert_eq!(pool.per_set, 1);
        assert_eq!(pool.pin("a", &ModulePermissions::closed()).await.0, 0);
        pool.shutdown().await;
    }

    /// Nothing is started until something is asked for — the bargain the
    /// sidecar made, kept.
    #[tokio::test]
    async fn a_pool_nobody_has_used_runs_nothing() {
        let pool = DenoModuleHost::with_workers("/nonexistent/modules", 3);
        assert_eq!(pool.workers().await, 0);
        pool.shutdown().await;
    }
}
