//! The **code runtime**: JavaScript code bodies, and the one host surface they
//! can reach (§10.1's `db`, the milestone "Tables in code").
//!
//! # Why this is not [`crate::eval`]
//!
//! A formula is a pure expression: one V8 isolate on one thread serves every
//! ownership check in the process, with no ops and a 250 ms watchdog. A code body
//! is different in kind — it calls out to the host, and the host call **blocks**
//! the isolate thread until the database answers. Giving the *formula* isolate a
//! blocking host call would put every authorization decision on the server behind
//! whatever a trigger's code is doing; worse, it would **deadlock** the moment a
//! delegated read's ownership formula needed the JS evaluator, because the thread
//! waiting for the host call is the thread the formula would have to run on.
//!
//! So a code body runs on [`CodeRuntime`]: a small pool of isolates of its own,
//! each with one op, its own watchdog and its own (longer) timeout. The formula
//! isolate stays exactly as pure as it was.
//!
//! # The seam
//!
//! What crosses into Rust is one plain JSON object per terminal — a *plan* — and
//! one JSON value back. That is [`CodeHost`], and it is deliberately the whole
//! interface: this crate sits below `sc-catalog` and `sc-api` and does not learn
//! what a table is. The fluent surface (`db.books.where(…).rows()`) is written in
//! JavaScript, in [`DB_PRELUDE`], and lowers to those plans; the table knowledge
//! lives in `sc-api`, where all of it already is.
//!
//! # Bounds
//!
//! Three, each with its own named error: the **wall clock** for the run, the
//! **call budget** (an accidental N+1 loop must not hammer the database quietly),
//! and the **JS watchdog** — which is *paused for the duration of a host call*, so
//! a slow query is never reported as "your code timed out". The row cap is the
//! host's business, not this crate's.
//!
//! The wall clock is enforced in two places, because one is not enough: the guest
//! is refused a host call once it is spent, *and* the caller stops waiting shortly
//! after it (see `CALLER_GRACE`). Only the second covers a run that holds its
//! caller without executing — one waiting for a worker while every worker is
//! blocked in a host call, or one whose single query never comes back. A watchdog
//! on an isolate that is not running cannot see either.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use sc_error::Result;
use serde_json::Value as Json;

#[cfg(feature = "eval")]
use std::cell::RefCell;
#[cfg(feature = "eval")]
use std::rc::Rc;
#[cfg(feature = "eval")]
use std::sync::Arc;
#[cfg(feature = "eval")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "eval")]
use std::sync::{Condvar, Mutex, mpsc};
#[cfg(feature = "eval")]
use std::time::Instant;

#[cfg(feature = "eval")]
use deno_core::OpState;
#[cfg(feature = "eval")]
use sc_error::Error;

/// How long a code body may run before it is stopped, when the caller names no
/// timeout of its own. Generous next to a formula's 250 ms: a body that reads,
/// loops and writes is doing real work.
pub const DEFAULT_CODE_TIMEOUT: Duration = Duration::from_secs(5);

/// The hard ceiling on a code body's timeout, whatever an action is configured
/// with. A trigger runs inside the request or the write that fired it, so an
/// unbounded body is an unbounded hold on that caller.
pub const MAX_CODE_TIMEOUT: Duration = Duration::from_secs(60);

/// How many host calls one run may make. A loop that reads a row per iteration
/// is the failure this bounds — not malice, an N+1 nobody noticed.
pub const DEFAULT_MAX_HOST_CALLS: u32 = 200;

/// How long past its own deadline the **caller** waits before giving up on a run.
///
/// The isolate has two bounds of its own — the watchdog and the deadline checked
/// on entry to a host call — and both name which one it was, so the caller's
/// timeout wants to lose that race: it exists for the runs those two cannot see.
#[cfg(feature = "eval")]
const CALLER_GRACE: Duration = Duration::from_millis(250);

/// The host surface a code body can reach: one JSON request in, one JSON value
/// out. **The** seam of §15 — a Python or Rust adapter implements the same trait
/// against the same plans, which is why this takes JSON rather than anything
/// shaped like a query.
///
/// `Err` is thrown into the guest as an ordinary `Error` at the call site, so a
/// body may catch it (a delegated write that is refused, say, and a fallback).
#[async_trait]
pub trait CodeHost: Send + Sync {
    /// Answer one plan. Called from an isolate thread, blocking it; the
    /// implementation must therefore not depend on that thread making progress.
    async fn call(&self, request: Json) -> Result<Json>;
}

/// One run of a JavaScript **code body**: the source, the values in scope, and
/// what it is allowed to reach and for how long.
///
/// Not a [`FormulaCall`](crate::FormulaCall): a formula is one expression in the
/// language this crate defines — parsed, validated against a schema shape,
/// normalised, and evaluable two ways — while this is opaque JavaScript
/// statements the host hands over verbatim. They share the sandbox and the
/// JSON boundary; they share nothing else, and collapsing them into one type
/// would have meant a `FormulaCall` whose `formula` was sometimes not a formula.
#[derive(Clone)]
pub struct CodeCall<'a> {
    /// The code body: statements, with `return` for the result. Run as the body
    /// of a function, so `return` at the top level is legal and everything it
    /// declares is local to the run.
    pub code: String,
    /// The values bound by name in the code's scope, as JSON — `row`, `user`,
    /// … Each name must be a plain JavaScript identifier; anything else is an
    /// error rather than something spliced into the script.
    pub bindings: BTreeMap<String, Json>,
    /// The table handle, or `None` for a **pure** body — exactly what
    /// `run_js_code` was before this milestone: `db` is not bound at all, so
    /// naming it is a `ReferenceError` rather than a silent `undefined`.
    ///
    /// **Borrowed**, not owned, because a real host holds the catalog: the one
    /// this server has (`sc_api::code_host::TableHost`) resolves every name in
    /// every plan through it, and the catalog is what the row layer and every
    /// action already have a reference to. The isolate a run happens on is a
    /// pool thread and what crosses to it must be `'static`, so the runtime
    /// bridges the two itself (see [`CodeRuntime::run`]) rather than making
    /// every caller find an `Arc<Catalog>` it does not have.
    pub host: Option<&'a dyn CodeHost>,
    /// The wall clock allowed for this run, clamped to [`MAX_CODE_TIMEOUT`];
    /// `None` is [`DEFAULT_CODE_TIMEOUT`].
    pub timeout: Option<Duration>,
    /// How many host calls this run may make.
    pub max_calls: u32,
}

impl Default for CodeCall<'_> {
    fn default() -> Self {
        CodeCall {
            code: String::new(),
            bindings: BTreeMap::new(),
            host: None,
            timeout: None,
            max_calls: DEFAULT_MAX_HOST_CALLS,
        }
    }
}

impl std::fmt::Debug for CodeCall<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeCall")
            .field("code", &self.code)
            .field("bindings", &self.bindings)
            .field("host", &self.host.is_some())
            .field("timeout", &self.timeout)
            .field("max_calls", &self.max_calls)
            .finish()
    }
}

/// The name the table handle binds under. Reserved when a host is present: a
/// caller that also bound `db` would produce a redeclaration deep inside the
/// generated wrapper, which is a bug nobody could find from the message.
///
/// Only the script builder reads it, and that is behind `eval` — without the
/// feature there is no engine to build a script for.
#[cfg(feature = "eval")]
pub(crate) const DB: &str = "db";

// ---------------------------------------------------------------------------
// The prelude (the fluent surface, in JavaScript)
// ---------------------------------------------------------------------------

/// The `db` builder: chain methods are pure and return a new builder, terminals
/// send one plan and return its result.
///
/// This is JavaScript rather than something generated from Rust on purpose
/// (decision 4): Rust sees plans, so adding a chain method touches no Rust, and
/// the same plans will serve the other guest languages. It is emitted **inside**
/// each run's function scope (decision 5) — a body that assigns to `db` poisons
/// nothing, because the next run builds its own.
#[cfg(feature = "eval")]
pub(crate) const DB_PRELUDE: &str = r#"
const db = (function () {
  const send = __scDbCall;
  // A condition is the object DSL every other surface speaks, or a formula
  // string — the two spellings §3 gives, lowered to one plan field.
  const condition = (c) => {
    if (typeof c === "string") return { formula: c };
    if (c && typeof c === "object") return c;
    throw new Error("where() takes a condition object or a formula string");
  };
  // A projection is a field name (or Ⱶ-path), or an { alias: formula } object.
  const projections = (c) => {
    if (typeof c === "string") return [c];
    if (c && typeof c === "object") {
      return Object.keys(c).map((alias) => ({ alias: alias, formula: c[alias] }));
    }
    throw new Error("select() takes field names and { alias: formula } objects");
  };
  // An aggregate is written the way a formula writes one — `count()`,
  // `sum(price * qty)` — and lowers to the plan's own { alias, fn, arg }, which
  // is what the seam has always carried and what a scalar terminal sends.
  const aggregates = (spec) => {
    if (!spec || typeof spec !== "object") {
      throw new Error('aggregate() takes an object like { n: "count()", total: "sum(price)" }');
    }
    return Object.keys(spec).map((alias) => {
      const source = spec[alias];
      const m = /^\s*([A-Za-z_][A-Za-z_0-9]*)\s*\(([\s\S]*)\)\s*$/.exec(String(source));
      if (!m) {
        throw new Error(
          "`" + source + "` is not an aggregate: write count(), sum(field) or " +
          "sum(an expression)"
        );
      }
      const arg = m[2].trim();
      return { alias: alias, fn: m[1], arg: arg === "" ? null : arg };
    });
  };
  const query = (state) => {
    const derive = (patch) => query(Object.assign({}, state, patch));
    const plan = (op, extra) => {
      const p = { op: op, table: state.table, authority: state.authority };
      // Repeated .where() calls AND; one is itself, so the common plan is flat.
      if (state.where.length === 1) p.where = state.where[0];
      else if (state.where.length > 1) p.where = { and: state.where };
      if (state.select.length) p.select = state.select;
      if (state.order.length) p.order = state.order;
      if (state.group.length) p.group = state.group;
      if (state.having.length === 1) p.having = state.having[0];
      else if (state.having.length > 1) p.having = { and: state.having };
      if (state.aggregate.length) p.aggregate = state.aggregate;
      if (state.limit !== null) p.limit = state.limit;
      if (state.offset !== null) p.offset = state.offset;
      return Object.assign(p, extra || {});
    };
    // A whole table rewritten or emptied is not something an omitted call
    // should be able to cause. The host refuses this too; here it is named at
    // the place the author can see.
    const bounded = (op) => {
      if (state.where.length === 0) {
        throw new Error(
          "db." + state.table + "." + op +
          "() without a .where() would touch every row; add a .where()"
        );
      }
    };
    // A scalar terminal is one nameless group: the same op, the same plan, the
    // one value unwrapped. Grouped, there is no one value to unwrap, so it says
    // so rather than answering the first group's.
    const scalar = (fn, arg) => {
      if (state.group.length || state.aggregate.length) {
        throw new Error(
          "." + fn + "() answers one value, and this query groups; ask for it by name: " +
          '.aggregate({ ' + fn + ': "' + fn + "(" + (arg === undefined ? "" : arg) + ')" }).rows()'
        );
      }
      const r = send(plan("aggregate", {
        aggregate: [{ alias: "value", fn: fn, arg: arg === undefined ? null : arg }],
      }));
      return r === null || r === undefined || r.value === undefined ? null : r.value;
    };
    // What a terminal reads: the rows of a select, or the groups of an
    // aggregate — which is one object when there is nothing to group by.
    const read = (extra) => {
      if (!state.aggregate.length) {
        if (state.group.length) {
          throw new Error(
            "db." + state.table + ".groupBy(...) needs an .aggregate({ ... }): a group " +
            "answers aggregate values, so say which"
          );
        }
        return send(plan("select", extra));
      }
      const r = send(plan("aggregate", extra));
      return Array.isArray(r) ? r : [r];
    };
    // `.iter()`: the rows, one batch per host call, resuming each time from the
    // cursor the last batch answered with. The isolate holds one batch rather
    // than the whole answer, so a body can walk a table it could never fit in
    // memory — and a body that stops early has paid for only what it read,
    // because nothing is fetched until the loop asks for it.
    //
    // The order is the host's business: it appends the primary key to whatever
    // this query sorts by, so that no two rows tie and no batch boundary can
    // skip or repeat one.
    function* iterate(batchSize) {
      if (state.aggregate.length || state.group.length) {
        throw new Error(
          "db." + state.table + ".iter() streams rows, and this query aggregates them; " +
          "ask for the groups with .rows(), which answers them all at once"
        );
      }
      if (batchSize !== undefined &&
          (typeof batchSize !== "number" || !isFinite(batchSize) || batchSize < 1)) {
        throw new Error(
          "iter()'s argument is how many rows to read at a time, e.g. .iter(200)"
        );
      }
      // A `.limit()` bounds the **iteration**, not the batch: it is spent here,
      // by stopping, and never sent as the plan's own bound — which for a
      // streamed read is the size of one batch.
      const total = state.limit;
      let taken = 0;
      let after = null;
      for (;;) {
        let want = batchSize;
        if (total !== null) {
          const left = total - taken;
          if (left <= 0) return;
          if (want === undefined || left < want) want = left;
        }
        const p = plan("select", { cursor: true });
        if (want !== undefined) p.limit = want;
        if (after !== null) {
          p.after = after;
          // An `.offset()` skips rows once, at the start of the iteration. The
          // host refuses a resumed batch that carries one, which is the same
          // rule said where a guest cannot reach it.
          delete p.offset;
        }
        const reply = send(p);
        const batch = reply.rows;
        for (let i = 0; i < batch.length; i++) {
          yield batch[i];
          taken += 1;
          if (total !== null && taken >= total) return;
        }
        if (reply.cursor === null || reply.cursor === undefined) return;
        after = reply.cursor;
      }
    }
    return {
      where: (c) => derive({ where: state.where.concat([condition(c)]) }),
      select: (...cols) =>
        derive({ select: cols.reduce((acc, c) => acc.concat(projections(c)), state.select) }),
      orderBy: (field, dir) =>
        derive({ order: state.order.concat([{ field: field, dir: dir === undefined ? "asc" : dir }]) }),
      groupBy: (...fields) => derive({ group: state.group.concat(fields) }),
      aggregate: (spec) => derive({ aggregate: state.aggregate.concat(aggregates(spec)) }),
      having: (c) => derive({ having: state.having.concat([condition(c)]) }),
      limit: (n) => derive({ limit: n }),
      offset: (n) => derive({ offset: n }),
      asUser: () => derive({ authority: "user" }),
      asAdmin: () => derive({ authority: "admin" }),

      rows: () => read(),
      iter: (batchSize) => iterate(batchSize),
      first: () => { const r = read({ limit: 1 }); return r.length ? r[0] : null; },
      get: (pk) => { const r = send(plan("select", { pk: pk, limit: 1 })); return r.length ? r[0] : null; },
      exists: () => send(plan("select", { limit: 1 })).length > 0,
      count: () => scalar("count"),
      sum: (f) => scalar("sum", f),
      avg: (f) => scalar("avg", f),
      min: (f) => scalar("min", f),
      max: (f) => scalar("max", f),

      insert: (values) => send(plan("insert", { values: values })),
      update: (values) => { bounded("update"); return send(plan("update", { values: values })); },
      delete: () => { bounded("delete"); return send(plan("delete")); },
    };
  };
  const table = (authority, name) =>
    query({
      table: name, authority: authority,
      where: [], select: [], order: [], group: [], having: [], aggregate: [],
      limit: null, offset: null,
    });
  // `db.sql(text, params, options)`: the body's own SQL. The authority is the
  // handle's, unless the options object says otherwise — `{ asUser: true }` is
  // the third argument, and `db.asUser().sql(...)` is the same thing said
  // fluently. Unknown option keys are refused rather than ignored, because an
  // option that silently does nothing is the worst way to learn it was spelled
  // wrong.
  const sql = (authority, text, params, options) => {
    if (typeof text !== "string") {
      throw new Error('sql() takes the SQL text, e.g. db.sql("select 1 as n")');
    }
    if (params !== undefined && params !== null && !Array.isArray(params)) {
      throw new Error(
        "sql()'s second argument is the array of values its placeholders stand for"
      );
    }
    let asUser = null;
    if (options !== undefined && options !== null) {
      if (typeof options !== "object" || Array.isArray(options)) {
        throw new Error("sql()'s third argument is an options object, e.g. { asUser: true }");
      }
      Object.keys(options).forEach((key) => {
        if (key !== "asUser") {
          throw new Error("`" + key + "` is not an option of sql(); the options are: asUser");
        }
      });
      if (options.asUser !== undefined) asUser = !!options.asUser;
    }
    return send({
      op: "sql",
      authority: asUser === null ? authority : (asUser ? "user" : "admin"),
      sql: text,
      params: params === undefined || params === null ? [] : params,
    });
  };
  // `db.table("x")` is the general form; `db.x` is a Proxy over the same call.
  const handle = (authority) => {
    const base = {
      table: (name) => table(authority, name),
      sql: (text, params, options) => sql(authority, text, params, options),
      asUser: () => handle("user"),
      asAdmin: () => handle("admin"),
    };
    return new Proxy(base, {
      get: (target, prop) => {
        if (typeof prop !== "string") return undefined;
        if (Object.prototype.hasOwnProperty.call(target, prop)) return target[prop];
        return table(authority, prop);
      },
    });
  };
  return handle("admin");
})();
"#;

/// Installed once per isolate: the op handle and the run wrapper, as globals
/// that a code body **cannot replace**.
///
/// Tampering could never *escalate* — the host re-validates every plan against
/// the catalog and the authority, and a guest that deleted `__scDbCall` would
/// only lose its own database access. What it could do is break the *next*
/// trigger's `db`, since runs share an isolate. Hence `writable: false,
/// configurable: false`, and hence `Deno` going away afterwards: the op is
/// captured in a closure, so removing the global removes the only other way to
/// reach `Deno.core`.
#[cfg(feature = "eval")]
const SETUP: &str = r#"
(() => {
  const op = Deno.core.ops.op_sc_db;
  const fixed = (name, value) =>
    Object.defineProperty(globalThis, name, {
      value: value, writable: false, configurable: false, enumerable: false,
    });
  // One round trip: a plan in, a reply envelope out. A host error becomes an
  // ordinary JS Error at the call site, catchable like any other.
  fixed("__scDbCall", (plan) => {
    const reply = JSON.parse(op(JSON.stringify(plan)));
    if (reply.error !== undefined) throw new Error(reply.error);
    return reply.ok;
  });
  // The run wrapper: call the body, refuse a Promise (nothing in the sandbox is
  // awaitable, and JSON.stringify(promise) is `{}`, which would look exactly
  // like a result), and hand back the JSON text of what it returned.
  fixed("__scRun", (body, bindings) => {
    const result = body(bindings);
    if (result && typeof result.then === "function") {
      throw new Error("the code returned a Promise: run_js_code is synchronous, " +
                      "and the sandbox has nothing to await");
    }
    return JSON.stringify(result);
  });
})();
delete globalThis.Deno;
"#;

// ---------------------------------------------------------------------------
// The op
// ---------------------------------------------------------------------------

/// What one run may still spend, and what it may reach. Lives in the isolate's
/// `OpState` for the duration of the run and is taken out again afterwards, so a
/// finished run holds no host alive.
#[cfg(feature = "eval")]
struct RunState {
    host: Option<Arc<dyn CodeHost>>,
    /// The tokio handle captured **when the job was submitted** — the op has no
    /// runtime of its own to block on.
    handle: Option<tokio::runtime::Handle>,
    /// Wall clock: when this run may make no further host calls.
    deadline: Instant,
    /// What the deadline was, for the message.
    timeout: Duration,
    calls_left: u32,
    max_calls: u32,
    /// JS execution time still allowed. Host calls do not consume it.
    js_budget: Duration,
    /// When the current JS window was armed.
    armed_at: Instant,
    watchdog: Arc<Watchdog>,
}

#[cfg(feature = "eval")]
impl RunState {
    /// Stop the clock on JS execution: a host call is the database's time, not
    /// the guest's, and reporting a slow query as "your code timed out" sends an
    /// admin to rewrite code that was never the problem.
    fn pause_watchdog(&mut self) {
        self.js_budget = self.js_budget.saturating_sub(self.armed_at.elapsed());
        self.watchdog.disarm();
    }

    /// Start it again, with whatever JS time was left.
    fn resume_watchdog(&mut self) {
        self.armed_at = Instant::now();
        self.watchdog.arm(self.armed_at + self.js_budget);
    }
}

/// The reply envelope: `{"ok": …}` or `{"error": "…"}`. An envelope rather than
/// an op-level `Result` so the message reaches the guest as a plain `Error` it
/// can catch, and so this crate needs no error type from `deno_core`.
#[cfg(feature = "eval")]
fn refuse(message: impl Into<String>) -> Json {
    serde_json::json!({ "error": message.into() })
}

#[cfg(feature = "eval")]
#[deno_core::op2]
#[string]
fn op_sc_db(state: Rc<RefCell<OpState>>, #[string] request: String) -> String {
    let reply = host_call(&state, &request);
    serde_json::to_string(&reply).unwrap_or_else(|_| {
        r#"{"error":"the database reply could not be encoded as JSON"}"#.to_owned()
    })
}

/// One host call, from the isolate thread. Everything the call needs is read out
/// of `OpState` and the borrow released **before** blocking, so the state is not
/// held across the wait.
#[cfg(feature = "eval")]
fn host_call(state: &Rc<RefCell<OpState>>, request: &str) -> Json {
    let plan: Json = match serde_json::from_str(request) {
        Ok(plan) => plan,
        Err(e) => return refuse(format!("the database plan is not JSON: {e}")),
    };

    let (host, handle) = {
        let mut state = state.borrow_mut();
        let Some(run) = state.try_borrow_mut::<RunState>() else {
            return refuse("this code body has no database access");
        };
        // The bounds, checked before the watchdog is touched so that an early
        // return can never leave the guest unwatched.
        if run.calls_left == 0 {
            return refuse(format!(
                "this code made more than {} database calls in one run; \
                 the bound exists so an accidental loop cannot hammer the database",
                run.max_calls
            ));
        }
        if Instant::now() >= run.deadline {
            return refuse(format!(
                "this code exceeded its {} ms time limit",
                run.timeout.as_millis()
            ));
        }
        let (Some(host), Some(handle)) = (run.host.clone(), run.handle.clone()) else {
            return refuse("this code body has no database access");
        };
        run.calls_left -= 1;
        run.pause_watchdog();
        (host, handle)
    };

    // Legal because a code thread is not a tokio runtime thread: the pool exists
    // so that the thread blocked here is one nothing else depends on.
    let outcome = handle.block_on(host.call(plan));

    if let Some(run) = state.borrow_mut().try_borrow_mut::<RunState>() {
        run.resume_watchdog();
    }
    match outcome {
        Ok(value) => serde_json::json!({ "ok": value }),
        Err(e) => refuse(e.to_string()),
    }
}

#[cfg(feature = "eval")]
deno_core::extension!(sc_db_ext, ops = [op_sc_db]);

// ---------------------------------------------------------------------------
// The watchdog
// ---------------------------------------------------------------------------

/// Terminates a runaway body through the isolate's thread-safe handle — the only
/// safe cross-thread operation on an isolate.
///
/// Armed and disarmed by absolute deadline rather than by message, because a code
/// run does both several times (once per host call) and a message protocol has to
/// get the acknowledgement right in the middle of a race it can lose. Waiting on
/// a condvar means an idle worker costs nothing.
#[cfg(feature = "eval")]
struct Watchdog {
    /// The armed deadline, or `None` for disarmed.
    deadline: Mutex<Option<Instant>>,
    wake: Condvar,
    fired: AtomicBool,
    stop: AtomicBool,
}

#[cfg(feature = "eval")]
impl Watchdog {
    fn start(isolate: deno_core::v8::IsolateHandle) -> Arc<Watchdog> {
        let dog = Arc::new(Watchdog {
            deadline: Mutex::new(None),
            wake: Condvar::new(),
            fired: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let watched = Arc::clone(&dog);
        std::thread::Builder::new()
            .name("sc-code-watchdog".into())
            .spawn(move || watched.watch(&isolate))
            .ok();
        dog
    }

    fn watch(&self, isolate: &deno_core::v8::IsolateHandle) {
        let mut armed = self.deadline.lock().unwrap_or_else(|e| e.into_inner());
        while !self.stop.load(Ordering::SeqCst) {
            match *armed {
                None => {
                    armed = self.wake.wait(armed).unwrap_or_else(|e| e.into_inner());
                }
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        self.fired.store(true, Ordering::SeqCst);
                        isolate.terminate_execution();
                        *armed = None;
                    } else {
                        let (next, _) = self
                            .wake
                            .wait_timeout(armed, deadline - now)
                            .unwrap_or_else(|e| e.into_inner());
                        armed = next;
                    }
                }
            }
        }
    }

    fn set(&self, deadline: Option<Instant>) {
        *self.deadline.lock().unwrap_or_else(|e| e.into_inner()) = deadline;
        self.wake.notify_all();
    }

    fn arm(&self, deadline: Instant) {
        self.set(Some(deadline));
    }

    fn disarm(&self) {
        self.set(None);
    }

    fn rearm_run(&self, deadline: Instant) {
        self.fired.store(false, Ordering::SeqCst);
        self.arm(deadline);
    }

    fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.set(None);
    }
}

// ---------------------------------------------------------------------------
// The pool
// ---------------------------------------------------------------------------

/// How many isolate threads a [`CodeRuntime`] runs by default. Two, not one: a
/// code body blocks its thread for the length of a host call, so a single worker
/// would serialise every trigger in the process behind the slowest query.
pub const DEFAULT_CODE_WORKERS: usize = 2;

/// One run, as it crosses to a worker: a [`CodeCall`] with its borrows resolved.
///
/// Everything here is owned, because the isolate is on another thread and the
/// job travels down a channel to reach it. The borrowed host is what makes this
/// a separate type from `CodeCall` rather than the same one: it becomes a
/// [`BridgeHost`], and the borrow stays behind with the caller's future.
#[cfg(feature = "eval")]
struct CodeRun {
    code: String,
    bindings: BTreeMap<String, Json>,
    host: Option<Arc<dyn CodeHost>>,
    /// Already defaulted and clamped, so the worker has no policy left to apply.
    timeout: Duration,
    max_calls: u32,
}

#[cfg(feature = "eval")]
struct CodeJob {
    run: CodeRun,
    /// Captured at submission: the op has to block on *some* runtime, and the
    /// caller's is the one the host's futures belong to.
    handle: Option<tokio::runtime::Handle>,
    reply: tokio::sync::oneshot::Sender<Result<Json>>,
}

/// One host call in flight over a [`BridgeHost`]: the plan, and where the answer
/// goes back to.
#[cfg(feature = "eval")]
struct HostRequest {
    plan: Json,
    reply: tokio::sync::oneshot::Sender<Result<Json>>,
}

/// The `'static` stand-in a **borrowed** host crosses to the isolate thread as.
///
/// A [`CodeCall`]'s host borrows (the real one holds this server's catalog), and
/// a job travelling down a channel to a pool thread cannot. So the job carries
/// this instead: the op's blocking call sends its plan down a channel, and the
/// other end is served — by the real host — inside [`CodeRuntime::run`], which is
/// the future that holds the borrow and is awaiting the run anyway.
///
/// It is also where the borrow *ends*: drop that future and the receiver goes
/// with it, so a further host call from a body whose caller has gone away is a
/// named error rather than a wait.
#[cfg(feature = "eval")]
struct BridgeHost {
    requests: tokio::sync::mpsc::UnboundedSender<HostRequest>,
}

#[cfg(feature = "eval")]
#[async_trait]
impl CodeHost for BridgeHost {
    async fn call(&self, request: Json) -> Result<Json> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.requests
            .send(HostRequest {
                plan: request,
                reply,
            })
            .map_err(|_| Error::msg("this code body's database connection has gone away"))?;
        answer
            .await
            .map_err(|_| Error::msg("this database request was dropped without an answer"))?
    }
}

/// A pool of isolates for **code bodies**, separate from the formula evaluator's
/// single pure isolate (decision 1). Cheap to share; dropping it shuts the
/// workers and their watchdogs down.
#[cfg(feature = "eval")]
pub struct CodeRuntime {
    tx: mpsc::Sender<CodeJob>,
    /// What a [`CodeCall`] with no `timeout` of its own gets.
    default_timeout: Duration,
}

/// Build a `JsRuntime` **inside a tokio context**, which `deno_core` requires:
/// it registers each isolate against the runtime that was current when the
/// isolate was created, and if V8 later posts a delayed foreground task (its GC
/// memory reducer does, under load) against an isolate with no runtime it
/// **aborts the process**. Entering for the length of the constructor is enough;
/// the guard is dropped straight afterwards so that the thread is free to
/// [`Handle::block_on`](tokio::runtime::Handle::block_on) a host call, which
/// would panic inside a runtime context.
///
/// The returned runtime, when there is one, is the isolate's anchor and must be
/// kept alive for as long as the isolate is — it is the fallback for a pool built
/// outside any runtime at all.
#[cfg(feature = "eval")]
pub(crate) fn build_isolate(
    anchor: Option<&tokio::runtime::Handle>,
    options: deno_core::RuntimeOptions,
) -> (deno_core::JsRuntime, Option<tokio::runtime::Runtime>) {
    let owned = match anchor {
        Some(_) => None,
        None => tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .ok(),
    };
    let runtime = {
        let _entered = anchor
            .map(tokio::runtime::Handle::enter)
            .or_else(|| owned.as_ref().map(tokio::runtime::Runtime::enter));
        deno_core::JsRuntime::new(options)
    };
    (runtime, owned)
}

#[cfg(feature = "eval")]
impl CodeRuntime {
    /// Start a pool of [`DEFAULT_CODE_WORKERS`] isolate threads.
    pub fn new() -> CodeRuntime {
        CodeRuntime::with_workers(DEFAULT_CODE_WORKERS)
    }

    /// Start a pool of `workers` isolate threads (at least one).
    pub fn with_workers(workers: usize) -> CodeRuntime {
        let (tx, rx) = mpsc::channel::<CodeJob>();
        let rx = Arc::new(Mutex::new(rx));
        let anchor = tokio::runtime::Handle::try_current().ok();
        for n in 0..workers.max(1) {
            let rx = Arc::clone(&rx);
            let anchor = anchor.clone();
            std::thread::Builder::new()
                .name(format!("sc-code-{n}"))
                .spawn(move || worker_thread(&rx, anchor.as_ref()))
                // Thread spawning fails only on resource exhaustion at process
                // level; there is no useful recovery, and a run would error on a
                // closed channel anyway.
                .ok();
        }
        CodeRuntime {
            tx,
            default_timeout: DEFAULT_CODE_TIMEOUT,
        }
    }

    /// Set what a call with no `timeout` of its own gets, in place of
    /// [`DEFAULT_CODE_TIMEOUT`]. Still clamped to [`MAX_CODE_TIMEOUT`].
    #[must_use]
    pub fn with_default_timeout(mut self, timeout: Duration) -> CodeRuntime {
        self.default_timeout = timeout;
        self
    }

    /// Run one code body to its JSON result.
    ///
    /// Two things happen here rather than one, when the call carries a host: the
    /// run is submitted to a worker, and this future then **serves that run's
    /// host calls** until it answers. The isolate thread blocks on each call
    /// (decision 2) and the plan travels back here over a [`BridgeHost`], which
    /// is what lets a host borrow — the future holding the borrow is the future
    /// awaiting the run, so the borrow lives exactly as long as it must.
    pub async fn run(&self, call: CodeCall<'_>) -> Result<Json> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        // The proxy goes to the worker and the borrowed host stays here, with
        // the receiving end of the channel between them. Only the run holds a
        // sender, so the run ending is the receiver closing.
        let mut bridged = None;
        let proxy: Option<Arc<dyn CodeHost>> = call.host.map(|host| {
            let (requests, incoming) = tokio::sync::mpsc::unbounded_channel();
            bridged = Some((host, incoming));
            Arc::new(BridgeHost { requests }) as Arc<dyn CodeHost>
        });
        let timeout = call
            .timeout
            .unwrap_or(self.default_timeout)
            .min(MAX_CODE_TIMEOUT);
        self.tx
            .send(CodeJob {
                run: CodeRun {
                    code: call.code,
                    bindings: call.bindings,
                    host: proxy,
                    timeout,
                    max_calls: call.max_calls,
                },
                handle: tokio::runtime::Handle::try_current().ok(),
                reply,
            })
            .map_err(|_| Error::msg("the code runtime has no workers left"))?;

        let dropped = || Error::msg("the code runtime dropped the reply");
        // The wall clock covers the **whole** call, queue time included, because
        // the two bounds inside the isolate cannot see either of the ways a run
        // holds its caller without running: waiting for a worker (every worker
        // blocked in a host call of its own — which is what a code body whose
        // write fires another code body does), and one host call that never comes
        // back. Neither is reachable from a watchdog on an isolate that is not
        // executing, and an unbounded hold on the request that fired the trigger
        // is exactly what `timeout` exists to prevent. Giving up here drops the
        // serving loop, so a run left behind fails at its next host call instead
        // of holding a worker for as long as the database takes.
        let expired =
            tokio::time::sleep_until(tokio::time::Instant::now() + timeout + CALLER_GRACE);
        tokio::pin!(expired);
        let overdue = || {
            Error::invalid(format!(
                "this code exceeded its {} ms time limit",
                timeout.as_millis()
            ))
        };

        let Some((host, mut incoming)) = bridged else {
            // A pure body asks for nothing; there is nothing to serve.
            return tokio::select! {
                outcome = answer => outcome.map_err(|_| dropped())?,
                () = &mut expired => Err(overdue()),
            };
        };
        tokio::pin!(answer);
        loop {
            tokio::select! {
                outcome = &mut answer => return outcome.map_err(|_| dropped())?,
                () = &mut expired => return Err(overdue()),
                // Disabled once the run's proxy is gone, which is the run being
                // over — the first branch is what then answers.
                Some(HostRequest { plan, reply }) = incoming.recv() => {
                    tokio::select! {
                        // A dropped receiver means the isolate stopped waiting
                        // (its watchdog fired): the answer is not wanted.
                        answered = host.call(plan) => { let _ = reply.send(answered); }
                        // The deadline has to be able to interrupt the call
                        // itself, not only the wait for the next one: one query
                        // that never comes back is the case this whole bound is
                        // for. Dropping `reply` fails the guest's blocked call at
                        // once, so the worker is not held for the query's own
                        // length either.
                        () = &mut expired => return Err(overdue()),
                    }
                }
            }
        }
    }
}

#[cfg(feature = "eval")]
impl Default for CodeRuntime {
    fn default() -> Self {
        CodeRuntime::new()
    }
}

/// One worker: its own isolate, its own watchdog, jobs checked out of the shared
/// queue one at a time.
#[cfg(feature = "eval")]
fn worker_thread(rx: &Mutex<mpsc::Receiver<CodeJob>>, anchor: Option<&tokio::runtime::Handle>) {
    // `_anchor` is the isolate's tokio anchor and must outlive it — see
    // `build_isolate`.
    let (mut runtime, _anchor) = build_isolate(
        anchor,
        deno_core::RuntimeOptions {
            extensions: vec![sc_db_ext::init()],
            ..Default::default()
        },
    );

    // The op handle and the run wrapper, then `Deno` goes away — see SETUP. A
    // failure here would leave every run unable to reach the host, so say so
    // rather than serving bodies that fail one by one for no visible reason.
    if let Err(e) = runtime.execute_script("sc_code_setup.js", SETUP) {
        // Nothing to reply to yet; the first run's `__scRun is not defined` is
        // the symptom, and this is the cause it will be diagnosed from.
        debug_assert!(false, "code runtime setup failed: {e}");
    }
    // Code bodies get the aggregation prelude too, so `rows().sum("qty")` means
    // in a body what it means in a formula.
    let _ = runtime.execute_script("sc_agg.js", crate::eval::AGG_PRELUDE);

    let watchdog = Watchdog::start(runtime.v8_isolate().thread_safe_handle());
    let op_state = runtime.op_state();

    loop {
        // Check one job out; the guard is released before it runs, so the other
        // workers keep serving while this one blocks on a query.
        let job = {
            let queue = rx.lock().unwrap_or_else(|e| e.into_inner());
            queue.recv()
        };
        let Ok(CodeJob { run, handle, reply }) = job else {
            break; // The last CodeRuntime handle was dropped.
        };

        let script = match build_code_script(&run) {
            Ok(script) => script,
            Err(e) => {
                let _ = reply.send(Err(e));
                continue;
            }
        };
        let timeout = run.timeout;

        let started = Instant::now();
        op_state.borrow_mut().put(RunState {
            host: run.host.clone(),
            handle,
            deadline: started + timeout,
            timeout,
            calls_left: run.max_calls,
            max_calls: run.max_calls,
            js_budget: timeout,
            armed_at: started,
            watchdog: Arc::clone(&watchdog),
        });
        watchdog.rearm_run(started + timeout);
        let outcome = runtime.execute_script("sc_code.js", script);
        watchdog.disarm();
        let terminated = watchdog.fired();
        // Drop the run's host: a pool worker outlives the run by a long way.
        op_state.borrow_mut().try_take::<RunState>();

        let answer = match outcome {
            Ok(global) => {
                deno_core::scope!(scope, &mut runtime);
                let local = deno_core::v8::Local::new(scope, global);
                // `__scRun` returns the JSON text of the result;
                // `JSON.stringify(undefined)` is `undefined`, which reads as null.
                Ok(if local.is_string() {
                    let text = local.to_rust_string_lossy(scope);
                    serde_json::from_str(&text).unwrap_or(Json::Null)
                } else {
                    Json::Null
                })
            }
            Err(e) => {
                if terminated {
                    // Termination poisons the isolate until cancelled; restore
                    // it so the next run starts clean.
                    runtime.v8_isolate().cancel_terminate_execution();
                    Err(Error::invalid(format!(
                        "JavaScript code timed out after {timeout:?}"
                    )))
                } else {
                    Err(Error::invalid(format!("JavaScript code failed: {e}")))
                }
            }
        };
        let _ = reply.send(answer);
    }
    watchdog.stop();
}

// ---------------------------------------------------------------------------
// The script
// ---------------------------------------------------------------------------

/// Assemble the script for one code body: the bindings as `const`s, the prelude
/// (when there is a host) in the same scope, the code as the body of a nested
/// function — so a top-level `return` is legal and nothing it declares outlives
/// the run — and the whole thing handed to the fixed `__scRun` wrapper.
///
/// The code itself is **not** escaped, and cannot be: it is the admin's own
/// JavaScript, spliced in as source. That is not a hole — the wrapper is no
/// privilege boundary, and the host re-validates every plan that comes back out
/// of it. What *is* escaped is every value, which rides in as JSON exactly as a
/// formula's bindings do.
#[cfg(feature = "eval")]
fn build_code_script(call: &CodeRun) -> Result<String> {
    let mut bindings = serde_json::Map::new();
    let mut consts = String::new();
    for (name, value) in &call.bindings {
        if !is_plain_ident(name) {
            return Err(Error::msg(format!(
                "code binding `{name}` is not a JavaScript identifier"
            )));
        }
        if call.host.is_some() && name == DB {
            return Err(Error::msg(
                "code binding `db` collides with the table handle bound in a code body",
            ));
        }
        // `const x = __b["x"];` — the name was checked as an identifier; the key
        // lookup quotes via JSON escaping.
        let key =
            serde_json::to_string(name).map_err(|e| Error::msg(format!("encode binding: {e}")))?;
        consts.push_str(&format!("const {name} = __b[{key}];\n"));
        bindings.insert(name.clone(), value.clone());
    }
    let args = serde_json::to_string(&Json::Object(bindings))
        .map_err(|e| Error::msg(format!("encode bindings: {e}")))?;
    // A pure body gets no `db` at all: naming it is a ReferenceError, not a
    // handle that fails on use.
    let prelude = if call.host.is_some() { DB_PRELUDE } else { "" };
    let code = &call.code;
    Ok(format!(
        "__scRun(function (__b) {{ \"use strict\";\n\
         {consts}{prelude}\n\
         const __result = (function () {{\n{code}\n}})();\n\
         return __result;\n\
         }}, {args})"
    ))
}

/// Whether a binding name is a plain JavaScript identifier — what can be spliced
/// into `const <name> = …` without a thought. Deliberately stricter than JS
/// itself (no `Ⱶ`, no escapes): every caller of [`CodeCall`] binds names it wrote
/// itself, so anything else is a bug to report rather than a shape to support.
#[cfg(feature = "eval")]
fn is_plain_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

#[cfg(feature = "eval")]
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicU32;

    /// How a [`FakeHost`] answers one plan.
    type Answer = Box<dyn Fn(&Json) -> Result<Json> + Send + Sync>;

    /// A host that records every plan it was asked for and answers from a
    /// closure. The point of the seam: this crate can be tested end to end
    /// without a catalog, a database or `sc-api`.
    struct FakeHost {
        plans: Mutex<Vec<Json>>,
        answer: Answer,
        delay: Option<Duration>,
        calls: AtomicU32,
    }

    impl FakeHost {
        fn new(answer: impl Fn(&Json) -> Result<Json> + Send + Sync + 'static) -> Arc<FakeHost> {
            Arc::new(FakeHost {
                plans: Mutex::new(Vec::new()),
                answer: Box::new(answer),
                delay: None,
                calls: AtomicU32::new(0),
            })
        }

        fn rows(rows: Json) -> Arc<FakeHost> {
            FakeHost::new(move |_| Ok(rows.clone()))
        }

        fn plans(&self) -> Vec<Json> {
            self.plans.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl CodeHost for FakeHost {
        async fn call(&self, request: Json) -> Result<Json> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.plans.lock().unwrap().push(request.clone());
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            (self.answer)(&request)
        }
    }

    fn call(code: &str) -> CodeCall<'static> {
        CodeCall {
            code: code.to_owned(),
            ..CodeCall::default()
        }
    }

    /// A call against a host the caller keeps: the host is borrowed (§the
    /// bridge), so the `Arc` these tests hold is what owns it.
    fn with_host<'a>(code: &str, host: &'a dyn CodeHost) -> CodeCall<'a> {
        CodeCall {
            code: code.to_owned(),
            host: Some(host),
            ..CodeCall::default()
        }
    }

    #[tokio::test]
    async fn a_body_with_no_host_is_the_pure_body_it_always_was() {
        let rt = CodeRuntime::new();
        // Statements, a local declaration and a `return` — the thing a formula
        // (one expression) cannot be.
        let mut c = call("let t = 0; for (const n of payload.ns) t += n; return t;");
        c.bindings.insert("payload".into(), json!({ "ns": [2, 5] }));
        assert_eq!(rt.run(c).await.unwrap(), json!(7));
        // And `db` is not merely inert, it is absent: naming it is a
        // ReferenceError naming it, not a handle that fails on use.
        let err = rt
            .run(call("return db.books.rows();"))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("JavaScript code failed") && err.contains("db"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_chain_lowers_to_one_plan_and_one_round_trip() {
        let host = FakeHost::rows(json!([{ "id": 1, "amount": 3 }]));
        let rt = CodeRuntime::new();
        let out = rt
            .run(with_host(
                r#"return db.invoices
                     .where({ paid: false, due: { lt: "2026-08-17" } })
                     .select("id", "amount", "customerⱵemail", { chased: "remindersↃinvoice.length" })
                     .orderBy("due")
                     .limit(50)
                     .offset(0)
                     .rows();"#,
                &*host,
            ))
            .await
            .unwrap();
        assert_eq!(out, json!([{ "id": 1, "amount": 3 }]));
        let plans = host.plans();
        assert_eq!(plans.len(), 1, "one terminal is one round trip");
        assert_eq!(
            plans[0],
            json!({
                "op": "select",
                "table": "invoices",
                "authority": "admin",
                "where": { "paid": false, "due": { "lt": "2026-08-17" } },
                "select": [ "id", "amount", "customerⱵemail",
                            { "alias": "chased", "formula": "remindersↃinvoice.length" } ],
                "order": [ { "field": "due", "dir": "asc" } ],
                "limit": 50,
                "offset": 0
            })
        );
    }

    /// A host that answers a cursor plan the way [`sc_api`]'s does: `count` rows
    /// numbered from 1, one batch at a time, resuming after the cursor it last
    /// answered with. Nothing here knows what a table is — which is the point of
    /// testing the streaming *protocol* on this side of the seam.
    fn paging_host(count: i64) -> Arc<FakeHost> {
        FakeHost::new(move |plan: &Json| {
            let batch = plan["limit"].as_i64().unwrap_or(1000);
            let from = match plan["after"].as_array() {
                Some(after) => after[0].as_i64().unwrap() + 1,
                None => 1,
            } + plan["offset"].as_i64().unwrap_or(0);
            let ids: Vec<i64> = (from..=count).take(batch as usize).collect();
            let rows: Vec<Json> = ids.iter().map(|id| json!({ "id": id })).collect();
            let short = (rows.len() as i64) < batch;
            Ok(json!({
                "rows": rows,
                "cursor": match (short, ids.last()) {
                    (false, Some(last)) => json!([last]),
                    _ => Json::Null,
                },
            }))
        })
    }

    #[tokio::test]
    async fn iter_streams_one_batch_at_a_time_and_resumes_from_the_cursor() {
        let host = paging_host(5);
        let rt = CodeRuntime::new();
        let out = rt
            .run(with_host(
                r#"const seen = [];
                   for (const row of db.invoices.where({ paid: false }).iter(2)) {
                     seen.push(row.id);
                   }
                   return seen;"#,
                &*host,
            ))
            .await
            .unwrap();
        assert_eq!(out, json!([1, 2, 3, 4, 5]), "every row, once, in order");

        let plans = host.plans();
        assert_eq!(
            plans.len(),
            3,
            "two full batches and the short one that ends it: {plans:#?}"
        );
        assert_eq!(
            plans[0],
            json!({
                "op": "select",
                "table": "invoices",
                "authority": "admin",
                "where": { "paid": false },
                "cursor": true,
                "limit": 2,
            }),
            "the first batch carries the whole query and no cursor"
        );
        // Each later batch is the same plan, resumed — the filter rides along,
        // because a batch is a read of its own.
        assert_eq!(plans[1]["after"], json!([2]));
        assert_eq!(plans[1]["where"], json!({ "paid": false }));
        assert_eq!(plans[2]["after"], json!([4]));
    }

    #[tokio::test]
    async fn iter_fetches_nothing_until_it_is_asked_and_stops_when_the_loop_does() {
        let host = paging_host(1000);
        let rt = CodeRuntime::new();
        let out = rt
            .run(with_host(
                r#"const it = db.invoices.iter(10);
                   const before = db.invoices.count();
                   let first = null;
                   for (const row of it) { first = row.id; break; }
                   return { first: first, before: before };"#,
                &*host,
            ))
            .await
            .unwrap();
        assert_eq!(out["first"], json!(1));

        let plans = host.plans();
        assert_eq!(
            plans.len(),
            2,
            "the `.count()` ran before the iterator's first batch, so nothing was \
             fetched at `.iter()` itself: {plans:#?}"
        );
        assert_eq!(plans[0]["op"], json!("aggregate"), "the count went first");
        assert_eq!(
            plans[1]["cursor"],
            json!(true),
            "and the loop's first batch second — the only one, because it broke"
        );
    }

    #[tokio::test]
    async fn a_limit_bounds_the_iteration_and_the_batch_never_overshoots_it() {
        let host = paging_host(1000);
        let rt = CodeRuntime::new();
        let out = rt
            .run(with_host(
                r#"const seen = [];
                   for (const row of db.invoices.limit(3).iter(2)) seen.push(row.id);
                   return seen;"#,
                &*host,
            ))
            .await
            .unwrap();
        assert_eq!(
            out,
            json!([1, 2, 3]),
            "the limit is the total, not the batch"
        );
        let plans = host.plans();
        let limits: Vec<&Json> = plans.iter().map(|p| &p["limit"]).collect();
        assert_eq!(
            limits,
            vec![&json!(2), &json!(1)],
            "the last batch asks for what is left rather than a batch of it"
        );
    }

    #[tokio::test]
    async fn iter_refuses_what_it_cannot_stream_in_front_of_the_author() {
        let host = paging_host(10);
        let rt = CodeRuntime::new();
        async fn refused(rt: &CodeRuntime, host: &dyn CodeHost, code: &str) -> String {
            rt.run(with_host(code, host)).await.unwrap_err().to_string()
        }
        let refused = |code: &'static str| refused(&rt, &*host, code);
        let e = refused(
            "for (const g of db.invoices.groupBy('paid').aggregate({ n: 'count()' }).iter()) {}",
        )
        .await;
        assert!(e.contains("aggregates"), "{e}");
        let e = refused("for (const r of db.invoices.iter(0)) {}").await;
        assert!(e.contains("how many rows to read at a time"), "{e}");
        let e = refused("for (const r of db.invoices.iter('lots')) {}").await;
        assert!(e.contains("how many rows to read at a time"), "{e}");
    }

    #[tokio::test]
    async fn the_two_spellings_of_a_filter_and_repeated_wheres_and() {
        let host = FakeHost::rows(json!([]));
        let rt = CodeRuntime::new();
        rt.run(with_host(
            r#"db.books.where({ status: "draft" }).where('pages > 3').rows();
               db.table("books").where('status === "draft"').rows();
               return null;"#,
            &*host,
        ))
        .await
        .unwrap();
        let plans = host.plans();
        assert_eq!(
            plans[0]["where"],
            json!({ "and": [ { "status": "draft" }, { "formula": "pages > 3" } ] })
        );
        // `db.table(name)` is the general form of the `db.name` sugar.
        assert_eq!(plans[1]["table"], json!("books"));
        assert_eq!(
            plans[1]["where"],
            json!({ "formula": "status === \"draft\"" })
        );
    }

    #[tokio::test]
    async fn authority_is_admin_until_delegated_and_where_it_is_said_does_not_matter() {
        let host = FakeHost::rows(json!([]));
        let rt = CodeRuntime::new();
        rt.run(with_host(
            r#"db.invoices.rows();
               db.asUser().invoices.rows();
               db.invoices.asUser().where({ paid: false }).rows();
               db.invoices.where({ paid: false }).asUser().rows();
               db.asUser().invoices.asAdmin().rows();
               return null;"#,
            &*host,
        ))
        .await
        .unwrap();
        let plans = host.plans();
        let authority: Vec<&str> = plans
            .iter()
            .filter_map(|p| p["authority"].as_str())
            .collect();
        assert_eq!(
            authority,
            vec!["admin", "user", "user", "user", "admin"],
            "asUser() sets one field of the plan, wherever it is said"
        );
    }

    #[tokio::test]
    async fn the_bodys_own_sql_is_a_request_of_its_own_and_says_whose_authority_it_runs_under() {
        let host = FakeHost::rows(json!([{ "n": 3 }]));
        let rt = CodeRuntime::new();
        let out = rt
            .run(with_host(
                r#"const a = db.sql("select count(*) as n from books where pages > $1", [200]);
                   db.sql("select 1", [], { asUser: true });
                   db.asUser().sql("select 1");
                   db.asUser().sql("select 1", null, { asUser: false });
                   return a[0].n;"#,
                &*host,
            ))
            .await
            .unwrap();
        assert_eq!(out, json!(3), "the rows come back as they are");

        let plans = host.plans();
        assert_eq!(
            plans.len(),
            4,
            "one call is one round trip, as a terminal is"
        );
        assert_eq!(
            plans[0],
            json!({
                "op": "sql",
                "authority": "admin",
                "sql": "select count(*) as n from books where pages > $1",
                "params": [200],
            }),
            "the text is the body's and the values ride beside it"
        );
        let authority: Vec<&str> = plans
            .iter()
            .filter_map(|p| p["authority"].as_str())
            .collect();
        assert_eq!(
            authority,
            vec!["admin", "user", "user", "admin"],
            "the option and the handle say the same thing, and the option wins"
        );
        assert_eq!(
            plans[2]["params"],
            json!([]),
            "no arguments is an empty list"
        );
    }

    #[tokio::test]
    async fn a_malformed_sql_call_is_refused_before_anything_is_sent() {
        let host = FakeHost::rows(json!([]));
        let rt = CodeRuntime::new();
        // Each of these is a body that meant something the host would have to
        // guess at, so the prelude says what it takes instead — and says it
        // without a round trip.
        for (code, expected) in [
            (r#"return db.sql({ from: "books" });"#, "SQL text"),
            (
                r#"return db.sql("select 1", { id: 1 });"#,
                "array of values",
            ),
            (
                r#"return db.sql("select 1", [], { asuser: true });"#,
                "asuser",
            ),
        ] {
            let refused = rt
                .run(with_host(code, &*host))
                .await
                .unwrap_err()
                .to_string();
            assert!(refused.contains(expected), "{code}: {refused}");
        }
        assert!(
            host.plans().is_empty(),
            "nothing reached the host: each was refused in the guest"
        );
    }

    #[tokio::test]
    async fn the_terminals_carry_their_own_op_and_unwrap_their_own_result() {
        let host = FakeHost::new(|plan| {
            Ok(match plan["op"].as_str() {
                Some("aggregate") => json!({ "value": 12 }),
                Some("insert") => json!({ "id": 9 }),
                Some("update") => json!({ "updated": 2, "ids": [3, 7] }),
                Some("delete") => json!({ "deleted": 1, "ids": [7] }),
                _ => json!([{ "id": 4 }]),
            })
        });
        let rt = CodeRuntime::new();
        let out = rt
            .run(with_host(
                r#"return {
                     first:  db.books.first(),
                     get:    db.books.get(4),
                     exists: db.books.where({ id: 4 }).exists(),
                     count:  db.books.count(),
                     sum:    db.books.sum("qty * price"),
                     insert: db.books.insert({ title: "Orlando" }),
                     update: db.books.where({ id: 3 }).update({ shelf: 3 }),
                     del:    db.books.where({ id: 7 }).delete(),
                   };"#,
                &*host,
            ))
            .await
            .unwrap();
        assert_eq!(out["first"], json!({ "id": 4 }));
        assert_eq!(out["get"], json!({ "id": 4 }));
        assert_eq!(out["exists"], json!(true));
        assert_eq!(out["count"], json!(12));
        assert_eq!(out["sum"], json!(12));
        assert_eq!(out["update"], json!({ "updated": 2, "ids": [3, 7] }));
        assert_eq!(out["del"], json!({ "deleted": 1, "ids": [7] }));

        let plans = host.plans();
        let ops: Vec<&str> = plans.iter().filter_map(|p| p["op"].as_str()).collect();
        assert_eq!(
            ops,
            vec![
                "select",
                "select",
                "select",
                "aggregate",
                "aggregate",
                "insert",
                "update",
                "delete"
            ]
        );
        assert_eq!(plans[0]["limit"], json!(1), ".first() is LIMIT 1");
        assert_eq!(plans[1]["pk"], json!(4), ".get(pk) names the key");
        assert_eq!(
            plans[3]["aggregate"],
            json!([{ "alias": "value", "fn": "count", "arg": null }])
        );
        assert_eq!(
            plans[4]["aggregate"],
            json!([{ "alias": "value", "fn": "sum", "arg": "qty * price" }])
        );
        assert_eq!(plans[5]["values"], json!({ "title": "Orlando" }));
    }

    #[tokio::test]
    async fn a_grouped_aggregate_is_one_plan_and_answers_rows() {
        // The phase 6 chain: the group keys, the values written the way the
        // formula language writes an aggregate, a bound on the groups, and rows
        // out — one round trip, like every other terminal.
        let host = FakeHost::new(|_| Ok(json!([{ "author": 1, "n": 3, "total": "42" }])));
        let rt = CodeRuntime::new();
        let out = rt
            .run(with_host(
                r#"return db.books
                     .where({ shelf: 2 })
                     .groupBy("author")
                     .aggregate({ n: "count()", total: "sum(price * qty)" })
                     .having({ n: { gt: 2 } })
                     .orderBy("n", "desc")
                     .limit(10)
                     .rows();"#,
                &*host,
            ))
            .await
            .unwrap();
        assert_eq!(out, json!([{ "author": 1, "n": 3, "total": "42" }]));
        let plans = host.plans();
        assert_eq!(plans.len(), 1, "one terminal is one round trip");
        assert_eq!(
            plans[0],
            json!({
                "op": "aggregate",
                "table": "books",
                "authority": "admin",
                "where": { "shelf": 2 },
                "group": ["author"],
                "having": { "n": { "gt": 2 } },
                "aggregate": [
                    { "alias": "n", "fn": "count", "arg": null },
                    { "alias": "total", "fn": "sum", "arg": "price * qty" },
                ],
                "order": [ { "field": "n", "dir": "desc" } ],
                "limit": 10
            })
        );

        // Ungrouped, `.aggregate({…}).rows()` is the same plan without a `group`,
        // and the host's one object reads back as the one row it is.
        let host = FakeHost::new(|_| Ok(json!({ "n": 7 })));
        let out = rt
            .run(with_host(
                r#"return db.books.aggregate({ n: "count()" }).rows();"#,
                &*host,
            ))
            .await
            .unwrap();
        assert_eq!(out, json!([{ "n": 7 }]));
    }

    #[tokio::test]
    async fn a_group_without_values_and_a_scalar_terminal_over_groups_are_both_refused() {
        // Neither reaches the host: a group with nothing to compute is a
        // question with no answer, and `.count()` over many groups has no one
        // value to be.
        let host = FakeHost::rows(json!([]));
        let rt = CodeRuntime::new();
        for (body, wanted) in [
            (
                r#"return db.books.groupBy("author").rows();"#,
                "needs an .aggregate(",
            ),
            (
                r#"return db.books.groupBy("author").count();"#,
                "answers one value",
            ),
            (
                r#"return db.books.aggregate({ n: "count" }).rows();"#,
                "is not an aggregate",
            ),
        ] {
            let err = rt
                .run(with_host(body, &*host))
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains(wanted), "{err}");
        }
        assert!(host.plans().is_empty(), "nothing reached the host");
    }

    #[tokio::test]
    async fn an_unfiltered_update_or_delete_is_refused_before_it_is_sent() {
        let host = FakeHost::rows(json!([]));
        let rt = CodeRuntime::new();
        for body in [
            "return db.books.update({ shelf: 3 });",
            "return db.books.delete();",
        ] {
            let err = rt
                .run(with_host(body, &*host))
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("without a .where()"), "{err}");
        }
        assert!(host.plans().is_empty(), "nothing reached the host");
    }

    #[tokio::test]
    async fn a_host_error_is_thrown_into_the_body_and_is_catchable() {
        let host = FakeHost::new(|_| Err(Error::auth("the ownership formula does not grant this")));
        let rt = CodeRuntime::new();
        // Uncaught, it fails the run with the host's own message.
        let err = rt
            .run(with_host("return db.books.rows();", &*host))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("ownership formula does not grant"), "{err}");
        // Caught, the body carries on — §5's "try a delegated write and fall back".
        let out = rt
            .run(with_host(
                "try { db.books.asUser().rows(); } catch (e) { return e.message; } return null;",
                &*host,
            ))
            .await
            .unwrap();
        assert!(
            out.as_str()
                .is_some_and(|m| m.contains("the ownership formula does not grant this")),
            "{out}"
        );
    }

    #[tokio::test]
    async fn the_call_budget_is_a_named_error() {
        let host = FakeHost::rows(json!([]));
        let rt = CodeRuntime::new();
        let mut c = with_host(
            "for (let i = 0; i < 100; i++) db.books.rows(); return true;",
            &*host,
        );
        c.max_calls = 3;
        let err = rt.run(c).await.unwrap_err().to_string();
        assert!(err.contains("more than 3 database calls"), "{err}");
        assert_eq!(host.plans().len(), 3, "the budget is spent, not exceeded");
    }

    #[tokio::test]
    async fn the_pool_serves_two_bodies_at_once() {
        // Two workers, two bodies each blocking its thread on a slow host: if
        // they were serialised the pair would take twice one call.
        let slow = Arc::new(FakeHost {
            plans: Mutex::new(Vec::new()),
            answer: Box::new(|_| Ok(json!([]))),
            delay: Some(Duration::from_millis(300)),
            calls: AtomicU32::new(0),
        });
        let rt = Arc::new(CodeRuntime::with_workers(2));
        let started = Instant::now();
        // The host is borrowed, so each task owns its own `Arc` and lends it.
        let one = {
            let (rt, host) = (Arc::clone(&rt), Arc::clone(&slow));
            tokio::spawn(async move { rt.run(with_host("return db.a.rows();", &*host)).await })
        };
        let two = {
            let (rt, host) = (Arc::clone(&rt), Arc::clone(&slow));
            tokio::spawn(async move { rt.run(with_host("return db.b.rows();", &*host)).await })
        };
        one.await.unwrap().unwrap();
        two.await.unwrap().unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(550),
            "the two runs serialised: {:?}",
            started.elapsed()
        );
        assert_eq!(slow.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_body_that_spins_is_terminated_and_its_isolate_recovers() {
        let rt = CodeRuntime::with_workers(1);
        let mut c = call("while (true) {}");
        c.timeout = Some(Duration::from_millis(100));
        let err = rt.run(c).await.unwrap_err().to_string();
        assert!(err.contains("JavaScript code timed out"), "{err}");
        // The same worker serves the next run normally.
        assert_eq!(rt.run(call("return 1 + 1;")).await.unwrap(), json!(2));
    }

    #[tokio::test]
    async fn time_in_the_host_does_not_count_against_the_js_watchdog_but_does_against_the_deadline()
    {
        // Each call sleeps for a third of the run's whole budget. Three of them
        // outlast the deadline — but the *watchdog* must not fire, because the
        // guest's own JavaScript has run for microseconds.
        let slow = Arc::new(FakeHost {
            plans: Mutex::new(Vec::new()),
            answer: Box::new(|_| Ok(json!([]))),
            delay: Some(Duration::from_millis(120)),
            calls: AtomicU32::new(0),
        });
        let rt = CodeRuntime::with_workers(1);
        let mut c = with_host(
            "for (let i = 0; i < 10; i++) db.books.rows(); return true;",
            &*slow,
        );
        c.timeout = Some(Duration::from_millis(300));
        let err = rt.run(c).await.unwrap_err().to_string();
        assert!(
            err.contains("time limit") && !err.contains("timed out"),
            "the host's time was charged to the guest's watchdog: {err}"
        );
        // A body that only sleeps in the host, well inside the deadline, is fine
        // even though one host call alone exceeds a formula's whole timeout.
        let mut c = with_host("db.books.rows(); return true;", &*slow);
        c.timeout = Some(Duration::from_millis(1000));
        assert_eq!(rt.run(c).await.unwrap(), json!(true));
    }

    #[tokio::test]
    async fn the_globals_cannot_be_poisoned_for_the_next_run() {
        let host = FakeHost::rows(json!([{ "id": 1 }]));
        let rt = CodeRuntime::with_workers(1);
        // A body that tries to replace the op handle, the run wrapper and `db`.
        let out = rt
            .run(with_host(
                r#"let broke = [];
                   try { globalThis.__scDbCall = () => []; } catch (e) { broke.push("call"); }
                   try { delete globalThis.__scRun; } catch (e) { broke.push("run"); }
                   try { Object.defineProperty(globalThis, "__scDbCall", { value: 1 }); }
                     catch (e) { broke.push("define"); }
                   globalThis.db = "poisoned";
                   return broke;"#,
                &*host,
            ))
            .await
            .unwrap();
        assert_eq!(out, json!(["call", "run", "define"]), "strict mode throws");
        // The next run on the same isolate gets its own `db` and a working op.
        let out = rt
            .run(with_host("return db.books.rows();", &*host))
            .await
            .unwrap();
        assert_eq!(out, json!([{ "id": 1 }]));
    }

    #[tokio::test]
    async fn the_code_isolate_has_no_io_surface_of_its_own() {
        let rt = CodeRuntime::new();
        for probe in ["Deno", "fetch", "require", "process", "setTimeout"] {
            let out = rt
                .run(call(&format!("return typeof {probe} === 'undefined';")))
                .await
                .unwrap();
            assert_eq!(out, json!(true), "sandbox leak: {probe}");
        }
        // The op handle exists — that is the one surface — but it refuses a body
        // with no host rather than reaching anything.
        let err = rt
            .run(call(
                r#"return __scDbCall({ op: "select", table: "books" });"#,
            ))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("no database access"), "{err}");
    }

    #[tokio::test]
    async fn an_async_body_is_refused_rather_than_returning_an_empty_object() {
        let rt = CodeRuntime::new();
        let err = rt
            .run(call("return (async () => 1)();"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("Promise"), "{err}");
    }

    #[tokio::test]
    async fn a_binding_that_collides_with_the_handle_is_refused() {
        let host = FakeHost::rows(json!([]));
        let rt = CodeRuntime::new();
        let mut c = with_host("return 1;", &*host);
        c.bindings.insert("db".into(), json!(1));
        let err = rt.run(c).await.unwrap_err().to_string();
        assert!(err.contains("collides with the table handle"), "{err}");
        // With no host there is no handle, so the name is the caller's to use.
        let mut c = call("return db;");
        c.bindings.insert("db".into(), json!(1));
        assert_eq!(rt.run(c).await.unwrap(), json!(1));
    }

    #[tokio::test]
    async fn a_run_that_never_gets_a_worker_still_ends_at_its_own_deadline() {
        // One worker, blocked in a host call for far longer than either run's
        // timeout — which is what a code body whose write fires another code body
        // looks like from the pool's point of view. The second run is not
        // executing, so neither the watchdog nor the op's deadline check can see
        // it: without the caller's own bound it would wait for ever, and an
        // unbounded wait is an unbounded hold on the request that fired it.
        let slow = Arc::new(FakeHost {
            plans: Mutex::new(Vec::new()),
            answer: Box::new(|_| Ok(json!([]))),
            delay: Some(Duration::from_secs(30)),
            calls: AtomicU32::new(0),
        });
        let rt = Arc::new(CodeRuntime::with_workers(1));
        let blocked = {
            let (rt, host) = (Arc::clone(&rt), Arc::clone(&slow));
            tokio::spawn(async move {
                let mut c = with_host("return db.a.rows();", &*host);
                c.timeout = Some(Duration::from_secs(2));
                rt.run(c).await
            })
        };
        // Let the first run take the only worker.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut queued = call("return 1;");
        queued.timeout = Some(Duration::from_millis(200));
        let started = Instant::now();
        let err = rt.run(queued).await.unwrap_err().to_string();
        assert!(err.contains("200 ms time limit"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );

        // And the run that *is* executing is bounded the same way, rather than
        // holding its caller for as long as the database takes.
        let err = blocked
            .await
            .unwrap()
            .expect_err("a host call that never returns is not a run without a bound")
            .to_string();
        assert!(err.contains("time limit"), "{err}");
    }
}
