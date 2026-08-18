//! The **code runtime**: JavaScript code bodies, and the one host surface they
//! can reach (§10.1's `db`, the milestone "Tables in code").
//!
//! # Why this is not [`crate::eval`]
//!
//! A formula is a pure expression: one V8 isolate on one thread serves every
//! ownership check in the process, with no ops and a 250 ms watchdog. A code body
//! is different in kind — it calls out to the host, and it **suspends** until the
//! database answers. Giving the *formula* isolate a host call would put every
//! authorization decision on the server behind whatever a trigger's code is
//! doing; worse, it would **deadlock** the moment a delegated read's ownership
//! formula needed the JS evaluator, because the isolate awaiting the host call is
//! the isolate the formula would have to run on.
//!
//! So a code body runs on [`CodeRuntime`]: a small pool of isolates of its own,
//! each with its own ops, its own event loop, its own watchdog and its own
//! (longer) timeout. The formula isolate stays exactly as pure as it was.
//!
//! # Asynchrony
//!
//! The guest surface is **awaitable**: every terminal answers a promise, the body
//! is wrapped in an `async function`, and `op_sc_db` is an ordinary async op. A
//! run in a host call therefore costs a pending promise rather than a thread —
//! which is what lets one isolate serve many runs at once, and what keeps a body
//! whose write fires a second code body from needing a second worker to finish.
//! The tax is the forgotten `await`, and [`SETUP`]'s `DbPromise` is where it is
//! paid.
//!
//! # Many runs per isolate
//!
//! A run is therefore not "what the isolate is doing" but an **entry in a
//! table** ([`RunTable`]), keyed by a token minted per run and bound as a `const`
//! in that run's own function scope. The host, the deadline, the call budget and
//! the reply channel are per entry; the token is 128 random bits rather than an
//! index, because two resident runs may carry different authority and a body must
//! not be able to reach another's host by writing `1`.
//!
//! Which moves where a run *ends*. `execute_script` starts one — the body is an
//! async function, so it returns at the first `await` — and a **completion op**
//! (`__scDone` / `__scFail`) delivers the answer once the event loop has carried
//! the body to it. The worker thread is one `block_on` around a loop that admits
//! jobs while pumping that event loop, and parks on the job channel when nothing
//! is resident.
//!
//! Occupancy is what is bounded instead of threads: each resident run holds its
//! scope, its bindings and a capped read in the V8 heap, so a worker admits at
//! most [`DEFAULT_MAX_INFLIGHT`] of them and the rest queue — with the queue time
//! still inside the run's own deadline. Past that the ceiling is the database
//! connection pool, which is the right place for it.
//!
//! # What a run costs
//!
//! Not a compile. The `db` surface is compiled once per isolate as a factory
//! ([`DB_PRELUDE`]'s `__scMakeDb`), and each body is compiled once per isolate
//! and kept under a content key ([`BodyCache`]) — so a trigger firing a thousand
//! times parses its source once, and every run after that is
//! `__scInvoke(token, key, bindings)`: a map lookup, a fresh `db`, a call. What
//! is still per run is what has to be — the token, the bindings and the scope.
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
//! # The two clocks
//!
//! Four bounds, each with its own named error: the **wall clock** for the run,
//! the **JS slice**, the **call budget** (an accidental N+1 loop must not hammer
//! the database quietly) and the isolate's **heap**. The row cap is the host's
//! business, not this crate's.
//!
//! The wall clock ([`CodeCall::timeout`]) is how long the run may take, and it
//! is mostly the database's time. It is enforced in three places, because no one
//! of them is enough: the guest is refused a host call once it is spent, the
//! worker reaps a resident run whose deadline has passed while it was suspended,
//! and the caller stops waiting shortly after it (see `CALLER_GRACE`). Only the
//! last covers a run that holds its caller without ever being admitted; only the
//! middle one covers a run whose single query never comes back. None of them
//! stops anyone else's body, which is the point of enforcing it there.
//!
//! The JS slice ([`DEFAULT_JS_SLICE`]) is how long a body may run **without
//! yielding**, and it is the *watchdog's* bound — the only instrument that stops
//! JavaScript, and a blunt one, because it stops the isolate and everything
//! resident on it. So it is armed at the slice of the run that is executing
//! (which the guest marks as it resumes), and a body that overruns it is the one
//! told so; its co-residents are re-queued if they have made no host call at all
//! and answered with an error of their own if they have. Never re-run after a
//! write: a body that has inserted rows is not idempotent, and running it twice
//! is a worse failure than the one being handled.
//!
//! The heap is the fourth. Occupancy is bounded in runs, which is a proxy for
//! memory and not a measure of it, so the isolate is given
//! [`DEFAULT_MAX_HEAP`] and a near-heap-limit callback: reaching it stops
//! admission until the resident runs give the heap back, rather than aborting
//! the process, and a body that fills even the callback's grace is stopped the
//! way any other runaway is.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use sc_error::Result;
use serde_json::Value as Json;

#[cfg(feature = "eval")]
use std::cell::RefCell;
#[cfg(feature = "eval")]
use std::collections::HashMap;
#[cfg(feature = "eval")]
use std::rc::Rc;
#[cfg(feature = "eval")]
use std::sync::Arc;
#[cfg(feature = "eval")]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(feature = "eval")]
use std::sync::{Condvar, Mutex};
#[cfg(feature = "eval")]
use std::time::Instant;

#[cfg(feature = "eval")]
use deno_core::OpState;
#[cfg(feature = "eval")]
use deno_core::futures::StreamExt;
#[cfg(feature = "eval")]
use deno_core::futures::stream::FuturesUnordered;
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

/// How many **outbound HTTP requests** one run may make.
///
/// A budget of its own rather than a share of [`DEFAULT_MAX_HOST_CALLS`],
/// because the two bound different things. A database call is this server's own
/// pooled query and 200 of them is an N+1 to notice; a `fetch` leaves the
/// building, and fifty of them at somebody else's endpoint is a different kind
/// of accident — one that a retry loop in a trigger can turn into a denial of
/// service against a third party. Small enough that such a loop stops, large
/// enough for the fan-out a body legitimately writes.
pub const DEFAULT_MAX_FETCHES: u32 = 50;

/// What one `fetch` gets when the body names no `timeout_ms` of its own.
///
/// Always clamped to what is left of the run's wall clock, which is the bound
/// that actually matters: the default run has five seconds for everything it
/// does, so this ceiling is only reached by a body that was given a longer one.
pub const DEFAULT_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// How much of the run's remaining time a request is **not** given.
///
/// A request clamped to exactly what is left would time out at the same instant
/// the run does, and the body would never see it: what the trigger's caller gets
/// is "this code exceeded its time limit" rather than the `catch` the author
/// wrote. Leaving a slice back means a hung endpoint fails *inside* the body,
/// where it can be caught, logged, or answered with a fallback — which is the
/// difference between a bound and a trap.
#[cfg(feature = "eval")]
const FETCH_MARGIN: Duration = Duration::from_millis(250);

/// The least time worth starting a request with. Below this the run is refused
/// one and told why, rather than sent to an endpoint it cannot wait for.
#[cfg(feature = "eval")]
const MIN_FETCH_WINDOW: Duration = Duration::from_millis(50);

/// The **JS slice**: how long a body may run without yielding.
///
/// Not the same clock as [`DEFAULT_CODE_TIMEOUT`], and this is the milestone's
/// point. A run's wall clock is mostly the *database's* time, and enforcing it
/// by terminating the isolate would stop every other body resident on it; a
/// slice is the guest's own time, between one `await` and the next, and a body
/// that computes for a whole second between two queries is already pathological.
/// So the wall clock is enforced where it costs nobody else anything (the op
/// refuses a call past it, the caller stops waiting, the worker reaps), and the
/// watchdog — the one instrument that *does* stop everyone — is pointed at this.
///
/// Clamped down to whatever is left of the run's own wall clock: a body with
/// 100 ms to live cannot spin for a second.
pub const DEFAULT_JS_SLICE: Duration = Duration::from_secs(1);

/// How much V8 heap one code isolate may fill before it stops admitting runs.
///
/// Occupancy is bounded by [`DEFAULT_MAX_INFLIGHT`] in runs, which is a proxy
/// for memory and not a measure of it: one run holding a 1000-row read is not
/// the same as one holding `{}`. This is the measure. Reaching it does not abort
/// the process — a near-heap-limit callback raises the limit for as long as it
/// takes the resident runs to finish, and the worker admits nothing new until
/// they have.
pub const DEFAULT_MAX_HEAP: usize = 256 * 1024 * 1024;

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
    /// Answer one plan. Awaited by the guest rather than blocking it: the
    /// isolate is free while this future is pending, so a slow answer costs a
    /// pending promise and not a thread.
    async fn call(&self, request: Json) -> Result<Json>;
}

/// The **second** host surface: one outbound HTTP request in, one response out.
///
/// Separate from [`CodeHost`] although the shape is the same, for two reasons
/// worth keeping apart. It is a different capability — a body may have tables
/// and no network, or the reverse — and it is implemented somewhere else: the
/// table host holds this server's catalog, while this holds an HTTP client, and
/// nothing sensible implements both. A run carries at most one of each.
///
/// The request and the response are plain JSON, exactly as a plan is, so §15's
/// other guest languages inherit `fetch` the way they inherit `db`:
///
/// ```json
/// { "url": "https://api.example.com/hooks", "method": "POST",
///   "headers": [["content-type", "application/json"]],
///   "body": "{\"id\":1}", "body_base64": false, "timeout_ms": 4000 }
/// ```
///
/// ```json
/// { "status": 200, "status_text": "OK", "url": "https://api.example.com/hooks",
///   "redirected": false, "headers": [["content-type", "application/json"]],
///   "text": "{\"ok\":true}" }
/// ```
///
/// `timeout_ms` is filled in by the op from what the guest asked for and what is
/// left of the run's wall clock, so an implementation may take it as given. An
/// `Err` is a **transport** failure and is thrown into the guest as a
/// `TypeError`, which is what the web API does; a response with a status the
/// server did not like is not an error at all — it comes back as an ordinary
/// response whose `ok` is false, again as the web API has it.
#[async_trait]
pub trait FetchHost: Send + Sync {
    /// Send one request and answer its response.
    async fn fetch(&self, request: Json) -> Result<Json>;
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
    /// The HTTP surface, or `None` for a body that cannot reach the network —
    /// in which case `fetch` is not bound at all, so naming it is a
    /// `ReferenceError` rather than a call that fails.
    ///
    /// Borrowed for the same reason `host` is, and bridged the same way.
    pub fetch: Option<&'a dyn FetchHost>,
    /// The wall clock allowed for this run, clamped to [`MAX_CODE_TIMEOUT`];
    /// `None` is [`DEFAULT_CODE_TIMEOUT`].
    pub timeout: Option<Duration>,
    /// How many host calls this run may make.
    pub max_calls: u32,
    /// How many outbound HTTP requests this run may make
    /// ([`DEFAULT_MAX_FETCHES`]).
    pub max_fetches: u32,
}

impl Default for CodeCall<'_> {
    fn default() -> Self {
        CodeCall {
            code: String::new(),
            bindings: BTreeMap::new(),
            host: None,
            fetch: None,
            timeout: None,
            max_calls: DEFAULT_MAX_HOST_CALLS,
            max_fetches: DEFAULT_MAX_FETCHES,
        }
    }
}

impl std::fmt::Debug for CodeCall<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeCall")
            .field("code", &self.code)
            .field("bindings", &self.bindings)
            .field("host", &self.host.is_some())
            .field("fetch", &self.fetch.is_some())
            .field("timeout", &self.timeout)
            .field("max_calls", &self.max_calls)
            .field("max_fetches", &self.max_fetches)
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

/// The name the HTTP surface binds under, reserved when a fetch host is present
/// for the reason [`DB`] is. It is `fetch` because that is what the web calls
/// it, and a body's author knows the name before they read anything of ours.
#[cfg(feature = "eval")]
pub(crate) const FETCH: &str = "fetch";

// ---------------------------------------------------------------------------
// The prelude (the fluent surface, in JavaScript)
// ---------------------------------------------------------------------------

/// The `db` builder: chain methods are pure and return a new builder, terminals
/// send one plan and return a **promise** of its result.
///
/// This is JavaScript rather than something generated from Rust on purpose
/// (decision 4): Rust sees plans, so adding a chain method touches no Rust, and
/// the same plans will serve the other guest languages.
///
/// It is compiled **once per isolate**, as a factory: `__scMakeDb(token)` builds
/// a fresh handle over a fresh closure for one run, and every run of every body
/// on that isolate calls it rather than compiling this text again. What that
/// preserves is decision 5 — a body that assigns to `db` poisons nothing,
/// because the next run is handed its own — and what it stops paying is a few
/// hundred lines of parse per run.
///
/// Only the terminals are asynchronous. The chain itself
/// (`db.invoices.where(…).orderBy(…)`) is pure and synchronous: it builds a plan
/// and touches nothing, so `await` belongs at the end of a chain and nowhere
/// inside it. Each terminal answers a `DbPromise` — see [`SETUP`] — and the
/// `.then()` that unwraps a reply preserves that class through species, so a
/// forgotten `await` is a named error wherever the chain ended.
#[cfg(feature = "eval")]
pub(crate) const DB_PRELUDE: &str = r#"
Object.defineProperty(globalThis, "__scMakeDb", {
  writable: false, configurable: false, enumerable: false,
  // One run's `db`, over one run's token. The token is the factory's argument
  // and lives in the closure it returns: with many runs resident on one isolate
  // it is what tells the host *whose* call this is, and a body is handed the
  // handle rather than the token, so there is nothing for another body to guess.
  value: (__scTok) => {
  // Every plan carries the run's own token.
  const send = (plan) => __scDbCall(__scTok, plan);
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
      return send(plan("aggregate", {
        aggregate: [{ alias: "value", fn: fn, arg: arg === undefined ? null : arg }],
      })).then((r) =>
        r === null || r === undefined || r.value === undefined ? null : r.value
      );
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
      return send(plan("aggregate", extra)).then((r) => (Array.isArray(r) ? r : [r]));
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
    //
    // An **async** generator, walked with `for await`: a batch is a host call,
    // and a host call is a promise. Its argument checks still throw, but at the
    // first `.next()` rather than at the call — which is where the loop is, so
    // the author sees them in the same place either way.
    async function* iterate(batchSize) {
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
        const reply = await send(p);
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
      first: () => read({ limit: 1 }).then((r) => (r.length ? r[0] : null)),
      get: (pk) =>
        send(plan("select", { pk: pk, limit: 1 })).then((r) => (r.length ? r[0] : null)),
      exists: () => send(plan("select", { limit: 1 })).then((r) => r.length > 0),
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
  },
});
"#;

/// The `fetch` surface: the web API's shape, over one JSON request and one JSON
/// response.
///
/// Compiled **once per isolate**, like [`DB_PRELUDE`] and for the same reason,
/// and split the same way: `Headers` and `Response` are ordinary globals because
/// they are inert — they hold no authority and a body cannot reach anything by
/// having them — while `fetch` itself is minted per run by `__scMakeFetch(token)`
/// and handed to the body as a parameter. That is the whole of the isolation: a
/// resident body holds a function closed over *its* token, so it cannot spend
/// another run's budget or borrow another run's network.
///
/// # What is the web's, and what is not
///
/// The common surface is the web's, deliberately, because an author already
/// knows it: `await fetch(url, { method, headers, body })` answers a `Response`
/// with `ok`, `status`, `statusText`, `headers`, `url`, and `text()` / `json()` /
/// `arrayBuffer()` / `bytes()` / `clone()`. A status the server did not like is
/// **not** an error — `res.ok` is false and nothing throws — while a transport
/// failure rejects with a `TypeError`, which is what a browser does. A body is
/// read once; reading it twice throws, and `clone()` is the answer.
///
/// Four differences, each of them the sandbox showing through rather than an
/// oversight:
///
/// - **No streaming**: `res.body` is not a `ReadableStream`, because the seam
///   carries one JSON value and a stream is not one. `text()` is the whole body.
/// - **No `AbortSignal`**: there are no timers in the sandbox to drive one, and
///   the bound that matters is already there — the run's wall clock, which
///   every request is clamped to. A `signal` in the options is refused by name
///   rather than ignored, so a body that thinks it can cancel is told it cannot.
/// - **`timeout_ms`** is an option of our own, since the browser's answer to
///   that question is the `AbortSignal` we do not have.
/// - **An object body** is JSON: `body: { id: 1 }` sends
///   `application/json`, because the alternative — `[object Object]` on the
///   wire, which is what the web does — is a bug every time it happens.
///
/// The options the browser needs and a server does not (`mode`, `credentials`,
/// `cache`, `referrer`, `integrity`, `keepalive`) are accepted and ignored, so
/// code that carries them works; anything else in the options is refused by
/// name, because a misspelled `header:` that did nothing would be exactly the
/// silent failure principle 5 is about.
#[cfg(feature = "eval")]
pub(crate) const FETCH_PRELUDE: &str = r#"
(() => {
  const fixed = (name, value) =>
    Object.defineProperty(globalThis, name, {
      value: value, writable: false, configurable: false, enumerable: false,
    });

  // --- header names and values -------------------------------------------
  // A header a body builds must not be able to become two headers, so the
  // checks are here rather than left to the host: the message wants to name the
  // line in the body that wrote it.
  const NAME_OK = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
  const headerName = (name) => {
    const text = String(name);
    if (!NAME_OK.test(text)) {
      throw new TypeError("`" + text + "` is not a valid HTTP header name");
    }
    return text.toLowerCase();
  };
  const headerValue = (value) => {
    const text = String(value).replace(/^[\t\n\r ]+|[\t\n\r ]+$/g, "");
    if (/[\0\r\n]/.test(text)) {
      throw new TypeError("an HTTP header value may not contain a newline");
    }
    return text;
  };

  class Headers {
    #list = [];
    constructor(init) {
      if (init === undefined || init === null) return;
      if (init instanceof Headers) {
        // The pairs as written rather than as read: a repeated header stays
        // two headers on the wire, which is the whole difference for the one
        // header (`set-cookie`) where joining them with a comma is wrong.
        for (const pair of init.__scPairs()) this.append(pair[0], pair[1]);
        return;
      }
      if (Array.isArray(init)) {
        for (const pair of init) {
          if (!Array.isArray(pair) || pair.length !== 2) {
            throw new TypeError("Headers takes [name, value] pairs");
          }
          this.append(pair[0], pair[1]);
        }
        return;
      }
      if (typeof init === "object") {
        for (const name of Object.keys(init)) this.append(name, init[name]);
        return;
      }
      throw new TypeError(
        "Headers takes an object, an array of [name, value] pairs, or Headers"
      );
    }
    append(name, value) { this.#list.push([headerName(name), headerValue(value)]); }
    set(name, value) {
      const key = headerName(name);
      const text = headerValue(value);
      this.#list = this.#list.filter((pair) => pair[0] !== key);
      this.#list.push([key, text]);
    }
    // Repeated headers join with ", ", as the web API's does — one `set-cookie`
    // and three `set-cookie`s should not need two ways of being read.
    get(name) {
      const key = headerName(name);
      const found = this.#list.filter((pair) => pair[0] === key).map((pair) => pair[1]);
      return found.length === 0 ? null : found.join(", ");
    }
    has(name) {
      const key = headerName(name);
      return this.#list.some((pair) => pair[0] === key);
    }
    delete(name) {
      const key = headerName(name);
      this.#list = this.#list.filter((pair) => pair[0] !== key);
    }
    // Sorted and combined, which is the order the web API iterates in.
    #combined() {
      const names = [...new Set(this.#list.map((pair) => pair[0]))].sort();
      return names.map((name) => [name, this.get(name)]);
    }
    forEach(callback, thisArg) {
      for (const [name, value] of this.#combined()) {
        callback.call(thisArg, value, name, this);
      }
    }
    *entries() { yield* this.#combined(); }
    *keys() { for (const [name] of this.#combined()) yield name; }
    *values() { for (const [, value] of this.#combined()) yield value; }
    [Symbol.iterator]() { return this.entries(); }
    // What crosses the seam: the pairs as written, uncombined, because the host
    // is the one that knows how a repeated header is sent.
    __scPairs() { return this.#list.map((pair) => [pair[0], pair[1]]); }
  }
  fixed("Headers", Headers);

  // --- bytes --------------------------------------------------------------
  // The seam is JSON, so a body that is not text travels base64. Both codecs are
  // written out here because the sandbox has no `TextEncoder` and no `atob` —
  // and because a response nobody asks for the bytes of pays for neither.
  const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const encodeUtf8 = (text) => {
    const out = [];
    for (let i = 0; i < text.length; i++) {
      let code = text.charCodeAt(i);
      if (code >= 0xd800 && code <= 0xdbff && i + 1 < text.length) {
        const low = text.charCodeAt(i + 1);
        if (low >= 0xdc00 && low <= 0xdfff) {
          code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
          i++;
        }
      }
      if (code < 0x80) out.push(code);
      else if (code < 0x800) out.push(0xc0 | (code >> 6), 0x80 | (code & 63));
      else if (code < 0x10000) {
        out.push(0xe0 | (code >> 12), 0x80 | ((code >> 6) & 63), 0x80 | (code & 63));
      } else {
        out.push(
          0xf0 | (code >> 18), 0x80 | ((code >> 12) & 63),
          0x80 | ((code >> 6) & 63), 0x80 | (code & 63)
        );
      }
    }
    return new Uint8Array(out);
  };
  const toBase64 = (bytes) => {
    let out = "";
    for (let i = 0; i < bytes.length; i += 3) {
      const a = bytes[i];
      const b = i + 1 < bytes.length ? bytes[i + 1] : 0;
      const c = i + 2 < bytes.length ? bytes[i + 2] : 0;
      out += B64[a >> 2];
      out += B64[((a & 3) << 4) | (b >> 4)];
      out += i + 1 < bytes.length ? B64[((b & 15) << 2) | (c >> 6)] : "=";
      out += i + 2 < bytes.length ? B64[c & 63] : "=";
    }
    return out;
  };
  // Bytes back to text, the way a browser decodes a response body: UTF-8, with
  // U+FFFD where the bytes are not. Built in chunks rather than by spreading the
  // whole array into `String.fromCharCode`, because a megabyte of arguments is a
  // stack overflow and a body that fetched a megabyte did nothing wrong.
  const decodeUtf8 = (bytes) => {
    const units = [];
    let out = "";
    const flush = () => {
      if (units.length === 0) return;
      out += String.fromCharCode.apply(null, units);
      units.length = 0;
    };
    for (let i = 0; i < bytes.length; ) {
      const byte = bytes[i];
      let code;
      let width;
      if (byte < 0x80) { code = byte; width = 1; }
      else if ((byte & 0xe0) === 0xc0) { code = byte & 0x1f; width = 2; }
      else if ((byte & 0xf0) === 0xe0) { code = byte & 0x0f; width = 3; }
      else if ((byte & 0xf8) === 0xf0) { code = byte & 0x07; width = 4; }
      else { units.push(0xfffd); i++; continue; }
      if (i + width > bytes.length) { units.push(0xfffd); i++; continue; }
      let ok = true;
      for (let n = 1; n < width; n++) {
        const next = bytes[i + n];
        if ((next & 0xc0) !== 0x80) { ok = false; break; }
        code = (code << 6) | (next & 63);
      }
      if (!ok) { units.push(0xfffd); i++; continue; }
      i += width;
      if (code > 0x10ffff) units.push(0xfffd);
      else if (code > 0xffff) {
        code -= 0x10000;
        units.push(0xd800 + (code >> 10), 0xdc00 + (code & 0x3ff));
      } else units.push(code);
      if (units.length >= 4096) flush();
    }
    flush();
    return out;
  };
  const fromBase64 = (text) => {
    const clean = String(text).replace(/[^A-Za-z0-9+/]/g, "");
    const out = new Uint8Array((clean.length * 3) >> 2);
    let at = 0;
    for (let i = 0; i < clean.length; i += 4) {
      const a = B64.indexOf(clean[i]);
      const b = B64.indexOf(clean[i + 1]);
      const c = B64.indexOf(clean[i + 2]);
      const d = B64.indexOf(clean[i + 3]);
      if (b >= 0) out[at++] = (a << 2) | (b >> 4);
      if (c >= 0) out[at++] = ((b & 15) << 4) | (c >> 2);
      if (d >= 0) out[at++] = ((c & 3) << 6) | d;
    }
    return out.subarray(0, at);
  };
  const asBytes = (value) => {
    if (value instanceof Uint8Array) return value;
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    if (ArrayBuffer.isView(value)) {
      return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    }
    return null;
  };

  // --- the response -------------------------------------------------------
  // Built by `fetch` from what the host answered, and constructible by a body
  // for the same reason the web makes it constructible: a function that answers
  // a Response is easier to test than one that answers a shape.
  const INTERNAL = Symbol("sc.response");
  class Response {
    #text; #base64; #used = false;
    constructor(body, init, internal) {
      const options = init === undefined || init === null ? {} : init;
      if (internal === INTERNAL) {
        this.#text = options.text;
        this.#base64 = options.base64;
      } else if (body === undefined || body === null) {
        this.#text = "";
      } else {
        const bytes = asBytes(body);
        if (bytes !== null) {
          this.#text = null;
          this.#base64 = toBase64(bytes);
        } else {
          this.#text = typeof body === "string" ? body : JSON.stringify(body);
        }
      }
      const status = options.status === undefined ? 200 : Number(options.status);
      if (!Number.isInteger(status) || status < 200 || status > 599) {
        throw new RangeError("a response status must be a whole number from 200 to 599");
      }
      Object.defineProperties(this, {
        status: { value: status, enumerable: true },
        statusText: {
          value: options.statusText === undefined ? "" : String(options.statusText),
          enumerable: true,
        },
        url: { value: options.url === undefined ? "" : String(options.url), enumerable: true },
        redirected: { value: options.redirected === true, enumerable: true },
        headers: { value: new Headers(options.headers), enumerable: true },
        type: { value: "basic", enumerable: true },
        ok: { value: status >= 200 && status < 300, enumerable: true },
      });
    }
    get bodyUsed() { return this.#used; }
    #take() {
      if (this.#used) {
        throw new TypeError("this response's body has already been read — use res.clone()");
      }
      this.#used = true;
    }
    // Asynchronous, as the web's are, although nothing is waited for: the body
    // arrived with the response. Keeping the shape means `await res.json()` is
    // written the same way here as everywhere else.
    async text() {
      this.#take();
      // The host sends the text of every response it could read as text, so the
      // decode below is only for a `Response` a body built out of bytes itself.
      if (this.#text !== null && this.#text !== undefined) return this.#text;
      return decodeUtf8(fromBase64(this.#base64));
    }
    async json() {
      const text = await this.text();
      try {
        return JSON.parse(text);
      } catch (e) {
        throw new SyntaxError("the response body is not JSON: " + e.message);
      }
    }
    async bytes() {
      this.#take();
      if (this.#base64 !== null && this.#base64 !== undefined) return fromBase64(this.#base64);
      return encodeUtf8(this.#text === null || this.#text === undefined ? "" : this.#text);
    }
    async arrayBuffer() {
      const bytes = await this.bytes();
      return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    }
    // A second reader of the same body, which is what makes reading once
    // enforceable without being a trap.
    clone() {
      if (this.#used) {
        throw new TypeError("a response whose body has been read cannot be cloned");
      }
      return new Response(null, {
        text: this.#text, base64: this.#base64,
        status: this.status, statusText: this.statusText, url: this.url,
        redirected: this.redirected, headers: this.headers,
      }, INTERNAL);
    }
  }
  fixed("Response", Response);

  // --- the request --------------------------------------------------------
  // Everything the browser needs and a server does not. Accepted and ignored
  // rather than refused, so that code carrying them runs unchanged.
  const IGNORED = [
    "mode", "credentials", "cache", "referrer", "referrerPolicy", "integrity",
    "keepalive", "window", "priority", "duplex",
  ];
  const KNOWN = ["method", "headers", "body", "redirect", "signal", "timeout_ms"].concat(IGNORED);
  const METHODS = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

  const plan = (input, init) => {
    const options = init === undefined || init === null ? {} : init;
    if (typeof options !== "object" || Array.isArray(options)) {
      throw new TypeError("fetch()'s second argument is an options object");
    }
    for (const key of Object.keys(options)) {
      if (!KNOWN.includes(key)) {
        throw new TypeError(
          "`" + key + "` is not an option of fetch(); the options are: " + KNOWN.join(", ")
        );
      }
    }
    let url = input;
    if (url !== null && typeof url === "object") url = url.url;
    if (typeof url !== "string" || url.trim() === "") {
      throw new TypeError("fetch() takes an absolute http(s) URL as its first argument");
    }
    const method = String(options.method === undefined ? "GET" : options.method).toUpperCase();
    if (!METHODS.includes(method)) {
      throw new TypeError(
        "`" + method + "` is not a method fetch() sends; the methods are: " + METHODS.join(", ")
      );
    }
    if (options.signal !== undefined && options.signal !== null) {
      throw new TypeError(
        "fetch() takes no `signal` here — there are no timers in the sandbox to " +
        "drive one. Use `timeout_ms`, which is clamped to what is left of this " +
        "code's own time limit"
      );
    }
    if (options.redirect !== undefined && options.redirect !== null &&
        String(options.redirect) !== "follow") {
      throw new TypeError(
        "fetch() only follows redirects here; `redirect: \"" + options.redirect + "\"` is not supported"
      );
    }
    const headers = new Headers(options.headers);
    let body = null;
    let base64 = false;
    if (options.body !== undefined && options.body !== null) {
      if (method === "GET" || method === "HEAD") {
        throw new TypeError("a " + method + " request cannot carry a body");
      }
      const bytes = asBytes(options.body);
      if (bytes !== null) {
        body = toBase64(bytes);
        base64 = true;
        if (!headers.has("content-type")) headers.set("content-type", "application/octet-stream");
      } else if (typeof options.body === "string") {
        body = options.body;
        if (!headers.has("content-type")) headers.set("content-type", "text/plain;charset=UTF-8");
      } else if (typeof options.body === "object") {
        // Ours, not the web's: an object on the web is `[object Object]` on the
        // wire, which is a mistake every single time it happens.
        try {
          body = JSON.stringify(options.body);
        } catch (e) {
          throw new TypeError("fetch()'s body could not be encoded as JSON: " + e.message);
        }
        if (!headers.has("content-type")) headers.set("content-type", "application/json");
      } else {
        body = String(options.body);
        if (!headers.has("content-type")) headers.set("content-type", "text/plain;charset=UTF-8");
      }
    }
    let timeout = null;
    if (options.timeout_ms !== undefined && options.timeout_ms !== null) {
      timeout = Number(options.timeout_ms);
      if (!Number.isFinite(timeout) || timeout <= 0) {
        throw new TypeError("fetch()'s `timeout_ms` must be a positive number of milliseconds");
      }
    }
    return {
      url: url.trim(),
      method: method,
      headers: headers.__scPairs(),
      body: body,
      body_base64: base64,
      timeout_ms: timeout,
    };
  };

  // One run's `fetch`, over one run's token — the same shape `__scMakeDb` has,
  // and for the same reason: what a body holds is a function closed over its own
  // authority, not a name it shares with everything else resident on the isolate.
  fixed("__scMakeFetch", (__scTok) => (input, init) =>
    __scFetchCall(__scTok, () => plan(input, init), (answer) =>
      new Response(null, {
        text: answer.text,
        base64: answer.base64,
        status: answer.status,
        statusText: answer.status_text,
        url: answer.url,
        redirected: answer.redirected === true,
        headers: answer.headers,
      }, INTERNAL)
    )
  );
})();
"#;

/// Installed once per isolate: the op handles, the promise a database call
/// answers, and the run wrapper — as globals that a code body **cannot
/// replace**.
///
/// Tampering could never *escalate* — the host re-validates every plan against
/// the catalog and the authority, and a guest that deleted `__scDbCall` would
/// only lose its own database access. What it could do is break the *other*
/// trigger's `db`, since runs share an isolate — and now share it at the same
/// time. Hence `writable: false, configurable: false`, and hence `Deno` going
/// away afterwards: the ops are captured in a closure, so removing the global
/// removes the only other way to reach `Deno.core`.
///
/// # `DbPromise`, and the forgotten `await`
///
/// Asynchrony levies one tax, and it is paid here rather than by every trigger
/// author. A plain promise that was meant to be awaited fails *quietly*:
/// `JSON.stringify(promise)` is `{}`, `` `${promise}` `` is
/// `[object Promise]`, and `for (const r of promise)` is a bare `TypeError`
/// about something not being iterable — three ways for a missing `await` to
/// look like a wrong answer rather than a mistake.
///
/// So a terminal answers a `DbPromise`: a `Promise` subclass whose `toJSON`,
/// `Symbol.toPrimitive` and `Symbol.iterator` all throw the same named error.
/// It costs nothing when the body is right, because `await` reaches none of
/// them — and `Promise.prototype.then` builds the derived promise through
/// `Symbol.species`, so an unwrapping `.then()` inside the prelude keeps the
/// class rather than losing it.
///
/// # How a run ends
///
/// Not by `execute_script` returning. With runs resident the script evaluates to
/// nothing useful — the body is an `async function`, and the event loop, not the
/// call, is what finishes it — so the run's answer is delivered by a **completion
/// op**: `__scDone` with the JSON text of the result, or `__scFail` with the
/// stack of what was thrown. Both name the run by its token, which is how the
/// Rust side finds the caller waiting for it.
///
/// # Compiled once
///
/// A body is **defined** here and **invoked** per run. The isolate keeps its
/// compiled bodies in a map keyed by a content key ([`BodyCache`]), so a trigger
/// that fires a thousand times is one compile and a thousand calls: the Rust
/// side sends the source only when it knows this isolate has not got it, and
/// every other run's script is `__scInvoke(token, key, bindings)`. The two sides
/// agree because Rust records a definition only once the script that carried it
/// has run.
///
/// # Which run is running
///
/// The watchdog stops *the isolate*, so before it fires something has to know
/// whose JavaScript is on it. Rust can see a run start (`execute_script`) and
/// see the isolate go idle (a poll of the event loop returning), but not the
/// moment a suspended body resumes — that happens inside a microtask drain. So
/// the guest says so: one cheap synchronous op at the point a host call's answer
/// comes back, naming the run whose continuation is about to run.
///
/// # Rejections nobody awaited
///
/// A promise a body creates and discards must not fail *other* runs. Left to
/// `deno_core`'s default, an unhandled rejection halts the whole event loop —
/// which, with runs multiplexed, is every resident body punished for one body's
/// dropped `db` call. The run's own failure never comes this way (`__scInvoke`
/// attaches a rejection handler to the body's promise), so the handler here can
/// say "handled" and mean it.
#[cfg(feature = "eval")]
const SETUP: &str = r#"
(() => {
  const call = Deno.core.ops.op_sc_db;
  const send = Deno.core.ops.op_sc_fetch;
  const done = Deno.core.ops.op_sc_done;
  const fail = Deno.core.ops.op_sc_fail;
  const mark = Deno.core.ops.op_sc_mark;
  Deno.core.setUnhandledPromiseRejectionHandler(() => true);
  const fixed = (name, value) =>
    Object.defineProperty(globalThis, name, {
      value: value, writable: false, configurable: false, enumerable: false,
    });
  const notAwaited = () =>
    new Error(
      "this database call was not awaited — write `await db.invoices.rows()`"
    );
  // The promise a terminal answers: everything a body might do to it *instead*
  // of awaiting it says so by name.
  class DbPromise extends Promise {
    toJSON() { throw notAwaited(); }
    [Symbol.toPrimitive]() { throw notAwaited(); }
    [Symbol.iterator]() { throw notAwaited(); }
  }
  // The same guard for the other surface, in that surface's own words: what a
  // forgotten `await fetch(…)` reaches for is `res.status`, and `undefined` is a
  // worse answer than a sentence.
  const notAwaitedFetch = () =>
    new Error("this fetch was not awaited — write `await fetch(url)`");
  class FetchPromise extends Promise {
    toJSON() { throw notAwaitedFetch(); }
    [Symbol.toPrimitive]() { throw notAwaitedFetch(); }
    [Symbol.iterator]() { throw notAwaitedFetch(); }
  }
  // One round trip: a plan in, a reply envelope out. A host error becomes an
  // ordinary JS Error at the await point, catchable like any other. The token
  // says which run is asking — a body may only pass its own, because that is
  // the only one in its scope.
  fixed("__scDbCall", (token, plan) => new DbPromise((resolve, reject) => {
    let request;
    try {
      request = JSON.stringify(plan);
    } catch (e) {
      reject(e);
      return;
    }
    call(token, request).then((answer) => {
      // The resumption mark: from here the JavaScript about to run is this
      // run's, so this is whose slice the watchdog should be watching and whose
      // trigger an overrun should name. One sync op, once per host call.
      mark(token);
      const reply = JSON.parse(answer);
      if (reply.error !== undefined) reject(new Error(reply.error));
      else resolve(reply.ok);
    }, reject);
  }));
  // One outbound request. `build` is called here rather than by the caller so
  // that a bad option **rejects** rather than throwing where the web API would
  // have rejected, and `wrap` turns the host's answer into a `Response` — both
  // live in the fetch prelude, which is where the web's shapes are.
  //
  // A failure is a `TypeError`, which is what a browser rejects a failed
  // request with; a status the server did not like is not a failure at all and
  // arrives here as an ordinary answer.
  fixed("__scFetchCall", (token, build, wrap) => new FetchPromise((resolve, reject) => {
    let request;
    try {
      request = JSON.stringify(build());
    } catch (e) {
      reject(e);
      return;
    }
    send(token, request).then((answer) => {
      mark(token);
      const reply = JSON.parse(answer);
      if (reply.error !== undefined) {
        reject(new TypeError(reply.error));
        return;
      }
      try {
        resolve(wrap(reply.ok));
      } catch (e) {
        reject(e);
      }
    }, reject);
  }));
  // What an admin should be shown: the stack when there is one, because a body
  // of any size wants the line, and the value itself when there is not.
  const describe = (e) => {
    if (e instanceof Error) return e.stack ? e.stack : String(e);
    try { return String(e); } catch (_) { return "the code threw a value it cannot describe"; }
  };
  // The compiled bodies of this isolate, by content key. A trigger that fires a
  // thousand times is one compile: the Rust side knows what it has defined here,
  // so a run's script carries the source only the first time and is
  // `__scInvoke(token, key, bindings)` every time after.
  const bodies = new Map();
  fixed("__scDefine", (key, wantsDb, wantsFetch, body) => {
    bodies.set(key, { body: body, wantsDb: wantsDb, wantsFetch: wantsFetch });
  });
  // Dropped when the cache is full and this body is the one least recently run.
  // A run already executing keeps its own reference, so forgetting a body can
  // never pull one out from under a resident run — it only means the next run of
  // it arrives with its source again.
  fixed("__scForget", (key) => { bodies.delete(key); });
  // The run wrapper: start the body — an async function, so what comes back is
  // a promise — and report what it settles to through the completion ops. The
  // refusal of a returned Promise this used to carry has inverted: a promise is
  // what a body now answers with, and awaiting it is the point.
  //
  // The `db` is made here rather than compiled into the body, from the token
  // this run was invoked with: one factory call per run, and a handle that is
  // this run's alone. A body with no host is defined to take one argument, so
  // there is no `db` in its scope to name — a ReferenceError, as it has always
  // been, rather than a handle that fails on use.
  fixed("__scInvoke", (token, key, bindings) => {
    const entry = bodies.get(key);
    if (entry === undefined) {
      // Unreachable while the Rust side and this map agree, which they do
      // because Rust records a definition only once the script defining it has
      // run. Named rather than silent, because the symptom of getting it wrong
      // would otherwise be a run that never answers.
      fail(token, "this code body is not compiled on the isolate it was sent to");
      return;
    }
    let running;
    try {
      // The handles this body was compiled to take, in the order its parameter
      // list has them. A body with neither is the pure one `run_js_code` began
      // as: nothing in its scope to reach anything with.
      const handles = [bindings];
      if (entry.wantsDb) handles.push(__scMakeDb(token));
      if (entry.wantsFetch) handles.push(__scMakeFetch(token));
      running = entry.body(...handles);
    } catch (e) {
      fail(token, describe(e));
      return;
    }
    Promise.resolve(running).then(
      (result) => {
        let text;
        try {
          text = JSON.stringify(result);
        } catch (e) {
          fail(token, describe(e));
          return;
        }
        // `JSON.stringify(undefined)` is `undefined`; a body that returns
        // nothing answers null, as it always has.
        done(token, text === undefined ? "null" : text);
      },
      (e) => fail(token, describe(e))
    );
  });
})();
delete globalThis.Deno;
"#;

// ---------------------------------------------------------------------------
// The op
// ---------------------------------------------------------------------------

/// What one run may still spend, where its answer goes, and what it may reach.
///
/// One **entry in a table** rather than the isolate's single current state: many
/// runs are resident at once, each suspended in a host call of its own, and each
/// carries its own authority — so "what the isolate is doing" is no longer a
/// thing there is one of. The entry lives from the moment the run's script is
/// executed until it answers, and taking it out is what ends the run: whatever
/// is left of a finished body then finds no host.
#[cfg(feature = "eval")]
struct RunState {
    host: Option<Arc<dyn CodeHost>>,
    /// The network, when this run has it. Separate from `host` because it is a
    /// separate capability: a body may have tables and no network.
    fetch: Option<Arc<dyn FetchHost>>,
    /// Wall clock: when this run may make no further host calls.
    deadline: Instant,
    /// What the deadline was, for the message.
    timeout: Duration,
    calls_left: u32,
    max_calls: u32,
    /// The outbound-request budget, counted apart from `calls_left` because a
    /// call that leaves the building is a different thing to bound.
    fetches_left: u32,
    max_fetches: u32,
    /// How long this run may execute JavaScript without yielding, before the
    /// watchdog stops it: [`DEFAULT_JS_SLICE`], never more than its own timeout.
    /// A *fresh* window each time it resumes, not a budget it spends — the
    /// database's time is not the guest's, and a body that awaits fifty queries
    /// has yielded fifty times.
    slice: Duration,
    /// The job this run was admitted with, kept only while the run has made
    /// **no host call** — which is exactly while re-running it would provably
    /// repeat no side effect. Dropped at the first call, so a body that has
    /// written is one this can no longer offer to re-queue.
    retry: Option<Box<CodeRun>>,
    /// Where the answer goes. The run's oneshot lives here rather than with the
    /// worker's loop, because the loop no longer waits for one run: a completion
    /// op finds the run by its token and answers whoever asked for it.
    reply: Option<tokio::sync::oneshot::Sender<Result<Json>>>,
}

#[cfg(feature = "eval")]
impl RunState {
    /// Answer the caller, once. A run answers exactly one thing, and which of
    /// the several places that can happen from got there first does not matter
    /// to the admin waiting for it.
    fn answer(&mut self, outcome: Result<Json>) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(outcome);
        }
    }
}

/// The runs resident on one isolate, by token.
///
/// Keyed by 128 random bits rather than by an index, because two runs on one
/// isolate may carry different authority — `db.asUser()` delegates to *this*
/// event's caller — and a body must not be able to reach another run's host by
/// writing `1`.
///
/// The table also owns four things that are facts about the whole table rather
/// than about any run:
///
/// - **which run is executing**, and when its JS slice runs out. One body's
///   JavaScript is on the isolate at a time, and the watchdog stops the isolate,
///   so this is both what the watchdog is armed at and who an overrun is blamed
///   on (every mutation ends in [`RunTable::rearm`]);
/// - the isolate's **watchdog**, which is now the JS slice's instrument and only
///   that — a run's wall clock is enforced in three places that cost its
///   co-residents nothing;
/// - the worker's **outstanding count**, which the dispatcher reads to choose
///   between workers — a run leaving the table is what makes it drop, so
///   [`RunTable::take`] is the one place that has to be right;
/// - a **notification** that a run left, because the worker admits from a queue
///   and needs to hear about a freed slot without polling for one. A run
///   finishes *inside* a poll of the event loop, which does not return while
///   other runs are still in flight, so without this the freed place would go
///   unused until something else woke the loop.
#[cfg(feature = "eval")]
struct RunTable {
    runs: HashMap<String, RunState>,
    /// The run whose JavaScript is on the isolate, and when its slice expires;
    /// `None` when the isolate is idle between polls, which is when nothing can
    /// overrun a slice and the watchdog has nothing to point at.
    running: Option<(String, Instant)>,
    watchdog: Arc<Watchdog>,
    outstanding: Arc<AtomicUsize>,
    freed: Arc<tokio::sync::Notify>,
}

#[cfg(feature = "eval")]
impl RunTable {
    fn new(
        watchdog: Arc<Watchdog>,
        outstanding: Arc<AtomicUsize>,
        freed: Arc<tokio::sync::Notify>,
    ) -> RunTable {
        RunTable {
            runs: HashMap::new(),
            running: None,
            watchdog,
            outstanding,
            freed,
        }
    }

    /// This run's JavaScript is what runs next: give it a fresh slice and point
    /// the watchdog at it.
    ///
    /// Called from two places, which between them are every way JavaScript can
    /// start running: a run being admitted, and a host call's answer coming back
    /// (`op_sc_mark`).
    fn enter(&mut self, token: &str) {
        let Some(run) = self.runs.get(token) else {
            return;
        };
        let now = Instant::now();
        // Clamped to what is left of the wall clock — a body with 100 ms to live
        // cannot spin for a second — but only while there *is* some left. A run
        // already past its deadline still gets a whole slice, because the
        // alternative is terminating the isolate (and every co-resident on it)
        // to enforce a bound that the op's own refusal and the reaper are about
        // to enforce for free.
        let until = if run.deadline > now {
            (now + run.slice).min(run.deadline)
        } else {
            now + run.slice
        };
        self.running = Some((token.to_owned(), until));
        self.rearm();
    }

    /// No JavaScript is on the isolate: whatever was running has yielded, and a
    /// suspended body cannot overrun anything.
    fn leave(&mut self) {
        self.running = None;
        self.rearm();
    }

    /// Point the watchdog at the running run's slice; disarm it when nothing is
    /// running.
    fn rearm(&self) {
        match self.running {
            Some((_, until)) => self.watchdog.arm(until),
            None => self.watchdog.disarm(),
        }
    }

    /// Take a run out of the table: the run is over, and nothing left of it in
    /// the isolate can reach a host any more.
    ///
    /// Every way a run can end goes through here — the completion ops, the
    /// reaper, a termination, the stuck sweep — which is why the occupancy
    /// accounting is here and not at each of those call sites.
    fn take(&mut self, token: &str) -> Option<RunState> {
        let run = self.runs.remove(token)?;
        self.outstanding.fetch_sub(1, Ordering::SeqCst);
        self.rearm();
        self.freed.notify_one();
        Some(run)
    }

    /// Take every run out, with its token, for a failure that is the isolate's
    /// rather than any one run's.
    fn drain(&mut self) -> Vec<(String, RunState)> {
        let tokens: Vec<String> = self.runs.keys().cloned().collect();
        tokens
            .into_iter()
            .filter_map(|t| self.take(&t).map(|run| (t, run)))
            .collect()
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
async fn op_sc_db(
    state: Rc<RefCell<OpState>>,
    #[string] token: String,
    #[string] request: String,
) -> String {
    let reply = host_call(&state, &token, &request).await;
    serde_json::to_string(&reply).unwrap_or_else(|_| {
        r#"{"error":"the database reply could not be encoded as JSON"}"#.to_owned()
    })
}

#[cfg(feature = "eval")]
#[deno_core::op2]
#[string]
async fn op_sc_fetch(
    state: Rc<RefCell<OpState>>,
    #[string] token: String,
    #[string] request: String,
) -> String {
    let reply = fetch_call(&state, &token, &request).await;
    serde_json::to_string(&reply).unwrap_or_else(|_| {
        r#"{"error":"the fetch reply could not be encoded as JSON"}"#.to_owned()
    })
}

/// One outbound request. [`host_call`]'s twin, and deliberately its own function
/// rather than a flag on it: the budget it spends is a different budget, the
/// capability it needs is a different capability, and every refusal here has to
/// say `fetch` rather than "database" to be worth reading.
///
/// The one thing it does that `host_call` does not is **fill in the clock**. A
/// request may not outlive the run that made it, so what the guest asked for
/// (or [`DEFAULT_FETCH_TIMEOUT`]) is clamped to what is left of the wall clock
/// and written into the plan — leaving the implementation nothing to decide and
/// no way to hold the caller past its deadline.
#[cfg(feature = "eval")]
async fn fetch_call(state: &Rc<RefCell<OpState>>, token: &str, request: &str) -> Json {
    let mut plan: Json = match serde_json::from_str(request) {
        Ok(plan) => plan,
        Err(e) => return refuse(format!("the fetch request is not JSON: {e}")),
    };

    let (host, remaining, run_timeout) = {
        let mut state = state.borrow_mut();
        let Some(table) = state.try_borrow_mut::<RunTable>() else {
            return refuse("this code body cannot reach the network");
        };
        let Some(run) = table.runs.get_mut(token) else {
            return refuse("this fetch belongs to a code run that has already finished");
        };
        if run.fetches_left == 0 {
            let max = run.max_fetches;
            return refuse(format!(
                "this code made more than {max} fetch requests in one run;                  the bound exists so a loop cannot hammer somebody else's server"
            ));
        }
        let now = Instant::now();
        if now >= run.deadline {
            let ms = run.timeout.as_millis();
            return refuse(format!("this code exceeded its {ms} ms time limit"));
        }
        let Some(host) = run.fetch.clone() else {
            return refuse("this code body cannot reach the network");
        };
        run.fetches_left -= 1;
        run.retry = None;
        (
            host,
            run.deadline.saturating_duration_since(now),
            run.timeout,
        )
    };

    // What the body asked for, bounded by what the run has left — less
    // `FETCH_MARGIN`, so that a request which does not come back fails where the
    // body can catch it rather than at the same moment the run itself expires.
    let usable = remaining.saturating_sub(FETCH_MARGIN);
    if usable < MIN_FETCH_WINDOW {
        let ms = run_timeout.as_millis();
        return refuse(format!(
            "this code has too little of its {ms} ms time limit left to make a request"
        ));
    }
    let asked = plan
        .get("timeout_ms")
        .and_then(Json::as_f64)
        .filter(|ms| ms.is_finite() && *ms > 0.0)
        .map_or(DEFAULT_FETCH_TIMEOUT, |ms| {
            Duration::from_secs_f64(ms / 1000.0)
        });
    let allowed = asked.min(usable);
    if let Some(object) = plan.as_object_mut() {
        object.insert(
            "timeout_ms".to_owned(),
            Json::from(u64::try_from(allowed.as_millis()).unwrap_or(u64::MAX)),
        );
    }

    match host.fetch(plan).await {
        Ok(value) => serde_json::json!({ "ok": value }),
        Err(e) => refuse(e.to_string()),
    }
}

/// One host call. An **ordinary async op**: it awaits the host rather than
/// blocking the isolate thread on it, so the run costs a pending promise and the
/// isolate is free to serve every other resident run while the database works.
/// The future needs no reactor of its own — a [`BridgeHost`] only sends on a
/// channel and awaits a oneshot — so the isolate's own event loop is what drives
/// it.
///
/// Everything the call needs is read out of `OpState` and the borrow released
/// **before** the await, because an `OpState` borrow held across a suspension
/// point is a `RefCell` panic waiting for the next op.
#[cfg(feature = "eval")]
async fn host_call(state: &Rc<RefCell<OpState>>, token: &str, request: &str) -> Json {
    let plan: Json = match serde_json::from_str(request) {
        Ok(plan) => plan,
        Err(e) => return refuse(format!("the database plan is not JSON: {e}")),
    };

    let host = {
        let mut state = state.borrow_mut();
        let Some(table) = state.try_borrow_mut::<RunTable>() else {
            return refuse("this code body has no database access");
        };
        let Some(run) = table.runs.get_mut(token) else {
            // Not reachable from a body that kept its own token: the run it
            // names is over, which is what an abandoned continuation resuming
            // after its run was answered looks like.
            return refuse("this database call belongs to a code run that has already finished");
        };
        // The bounds, checked before the clock is touched so that an early
        // return can never leave the guest unwatched.
        if run.calls_left == 0 {
            let max = run.max_calls;
            return refuse(format!(
                "this code made more than {max} database calls in one run; \
                 the bound exists so an accidental loop cannot hammer the database"
            ));
        }
        if Instant::now() >= run.deadline {
            let ms = run.timeout.as_millis();
            return refuse(format!("this code exceeded its {ms} ms time limit"));
        }
        let Some(host) = run.host.clone() else {
            return refuse("this code body has no database access");
        };
        run.calls_left -= 1;
        // Past this point the run has reached the database, so it is no longer
        // one that could be re-run without repeating whatever it did there.
        run.retry = None;
        host
    };

    // No clock is stopped here. The slice is not a budget to pause: it is a
    // fresh window granted at each resumption (`op_sc_mark`), and the isolate is
    // free to serve every other run while this call is in flight — so the time
    // the database takes is charged to nobody's JavaScript.
    let outcome = host.call(plan).await;

    match outcome {
        Ok(value) => serde_json::json!({ "ok": value }),
        Err(e) => refuse(e.to_string()),
    }
}

/// A run's continuation is about to run: give it a fresh JS slice, and make it
/// the run an overrun is blamed on.
///
/// The guest has to say this because Rust cannot see it. A suspended body
/// resumes inside a microtask drain, several levels below the op whose answer
/// woke it, and the only Rust either side of that drain is one poll of the event
/// loop. One synchronous op per host call is what that costs.
///
/// **Attribution is exact for the run that is alone in its tick, and best-effort
/// otherwise.** Several answers can arrive in one turn of the event loop; V8
/// then runs every resumption handler before any of the bodies they woke, so the
/// mark that stands when a body overruns is the last one of that turn rather
/// than necessarily that body's. Naming a co-resident is a wrong name in a rare
/// case; the alternative — releasing one answer per turn, so that each body
/// resumes alone — is a serialisation of the hot path to improve a message.
#[cfg(feature = "eval")]
#[deno_core::op2(fast)]
fn op_sc_mark(state: &mut OpState, #[string] token: &str) {
    if let Some(table) = state.try_borrow_mut::<RunTable>() {
        table.enter(token);
    }
}

/// A run answered. The JSON text is what `JSON.stringify` made of the body's
/// result, parsed back here so the caller gets a `Json` and not a string.
#[cfg(feature = "eval")]
#[deno_core::op2(fast)]
fn op_sc_done(state: &mut OpState, #[string] token: &str, #[string] result: &str) {
    finish(
        state,
        token,
        Ok(serde_json::from_str(result).unwrap_or(Json::Null)),
    );
}

/// A run threw. `message` is the stack when V8 had one, so what an admin sees
/// names the line rather than only the message.
#[cfg(feature = "eval")]
#[deno_core::op2(fast)]
fn op_sc_fail(state: &mut OpState, #[string] token: &str, #[string] message: &str) {
    finish(
        state,
        token,
        Err(Error::invalid(format!("JavaScript code failed: {message}"))),
    );
}

/// End a run: answer its caller and take it out of the table. A token that names
/// nothing is not an error — a run abandoned at its deadline was taken out
/// already, and its continuation reporting afterwards is exactly that.
#[cfg(feature = "eval")]
fn finish(state: &mut OpState, token: &str, outcome: Result<Json>) {
    let Some(table) = state.try_borrow_mut::<RunTable>() else {
        return;
    };
    if let Some(mut run) = table.take(token) {
        run.answer(outcome);
    }
}

#[cfg(feature = "eval")]
deno_core::extension!(
    sc_db_ext,
    ops = [op_sc_db, op_sc_fetch, op_sc_done, op_sc_fail, op_sc_mark]
);

// ---------------------------------------------------------------------------
// The watchdog
// ---------------------------------------------------------------------------

/// Why the isolate was terminated. Two instruments, one blunt tool: the
/// distinction is what the run that caused it is told, and it is a different
/// mistake in each case.
#[cfg(feature = "eval")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Trip {
    /// A body ran JavaScript for longer than its slice without yielding.
    Slice,
    /// A body filled the isolate's heap past the grace the near-heap-limit
    /// callback could buy it.
    Heap,
}

/// Terminates a runaway body through the isolate's thread-safe handle — the only
/// safe cross-thread operation on an isolate.
///
/// Armed and disarmed by absolute deadline rather than by message, because a
/// resumption does both several times (once per host call) and a message
/// protocol has to get the acknowledgement right in the middle of a race it can
/// lose. Waiting on a condvar means an idle worker costs nothing.
///
/// What it is armed at is the **JS slice** of the run that is executing, and
/// nothing else. A run's wall clock was the other thing it used to enforce, and
/// it is no longer: terminating an isolate to bound one run's total time stops
/// every body resident on it, and for an I/O-bound body that total is mostly the
/// database's time anyway. The wall clock is enforced where it costs the
/// co-residents nothing — the op refuses a call past it, the worker reaps a
/// suspended run past it, and the caller stops waiting `CALLER_GRACE` later.
///
/// The handle is held rather than moved into the watching thread because the
/// near-heap-limit callback trips the same instrument from the isolate's own
/// thread: heap exhaustion and a runaway loop want exactly the same unwinding.
#[cfg(feature = "eval")]
struct Watchdog {
    isolate: deno_core::v8::IsolateHandle,
    /// The armed deadline, or `None` for disarmed.
    deadline: Mutex<Option<Instant>>,
    wake: Condvar,
    /// 0 for "not fired", or a [`Trip`] discriminant plus one.
    fired: AtomicUsize,
    stop: AtomicBool,
}

#[cfg(feature = "eval")]
impl Watchdog {
    fn start(isolate: deno_core::v8::IsolateHandle) -> Arc<Watchdog> {
        let dog = Arc::new(Watchdog {
            isolate,
            deadline: Mutex::new(None),
            wake: Condvar::new(),
            fired: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        });
        let watched = Arc::clone(&dog);
        std::thread::Builder::new()
            .name("sc-code-watchdog".into())
            .spawn(move || watched.watch())
            .ok();
        dog
    }

    fn watch(&self) {
        let mut armed = self.deadline.lock().unwrap_or_else(|e| e.into_inner());
        while !self.stop.load(Ordering::SeqCst) {
            match *armed {
                None => {
                    armed = self.wake.wait(armed).unwrap_or_else(|e| e.into_inner());
                }
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        self.trip(Trip::Slice);
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

    /// Stop the isolate, and remember why. The first reason wins: a heap trip
    /// and a slice trip in the same instant are one termination, and the run
    /// being told about it should hear whichever actually stopped it.
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

    /// Why the watchdog terminated the isolate since this was last cleared, if
    /// it did. Taken rather than read, because the worker acts on it exactly
    /// once: it cancels the termination, and the next JS to run must not be
    /// treated as the terminated one.
    fn took_fired(&self) -> Option<Trip> {
        match self.fired.swap(0, Ordering::SeqCst) {
            1 => Some(Trip::Slice),
            2 => Some(Trip::Heap),
            _ => None,
        }
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.set(None);
    }
}

// ---------------------------------------------------------------------------
// The pool
// ---------------------------------------------------------------------------

/// How many isolate threads a [`CodeRuntime`] runs by default. Two, not one:
/// two isolates are two CPUs' worth of JavaScript, and a body that computes
/// while another body computes is the only thing a second isolate now buys.
/// Concurrency is no longer among them — a run costs a pending promise, so one
/// isolate serves hundreds at once (decision 2).
pub const DEFAULT_CODE_WORKERS: usize = 2;

/// How many runs one worker keeps **resident** at once. Concurrency is not free
/// of everything: each resident run holds its scope, its bindings and up to a
/// capped read in the V8 heap. Past this the rest queue exactly as they did when
/// the number was one, with the queue time still inside the run's own deadline.
pub const DEFAULT_MAX_INFLIGHT: usize = 256;

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
    fetch: Option<Arc<dyn FetchHost>>,
    /// Already defaulted and clamped, so the worker has no policy left to apply.
    timeout: Duration,
    max_calls: u32,
    max_fetches: u32,
    /// When the wall clock this run is being measured against started — set only
    /// on a run that is being **re-queued** after its isolate was terminated
    /// under it. A second start is not a second timeout: the caller is still
    /// waiting on the first one, and giving the retry a fresh deadline would let
    /// it outlive the future that will answer with it.
    started: Option<Instant>,
}

#[cfg(feature = "eval")]
struct CodeJob {
    run: Box<CodeRun>,
    reply: tokio::sync::oneshot::Sender<Result<Json>>,
}

/// One host call in flight over a [`BridgeHost`]: which surface it is for, the
/// plan, and where the answer goes back to.
///
/// Both surfaces share one channel and one serving loop, so a body's
/// `Promise.all([db…, fetch…])` really does issue the query and the request
/// together — which two channels would not have given without two loops.
#[cfg(feature = "eval")]
struct HostRequest {
    surface: Surface,
    plan: Json,
    reply: tokio::sync::oneshot::Sender<Result<Json>>,
}

/// Which borrowed host answers a bridged request.
#[cfg(feature = "eval")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Surface {
    Db,
    Fetch,
}

/// The `'static` stand-in a **borrowed** host crosses to the isolate thread as.
///
/// A [`CodeCall`]'s host borrows (the real one holds this server's catalog), and
/// a job travelling down a channel to a pool thread cannot. So the job carries
/// this instead: the op sends its plan down a channel, and the other end is
/// served — by the real host — inside [`CodeRuntime::run`], which is the future
/// that holds the borrow and is awaiting the run anyway.
///
/// It is also where the borrow *ends*: drop that future and the receiver goes
/// with it, so a further host call from a body whose caller has gone away is a
/// named error rather than a wait.
#[cfg(feature = "eval")]
struct BridgeHost {
    requests: tokio::sync::mpsc::UnboundedSender<HostRequest>,
}

#[cfg(feature = "eval")]
impl BridgeHost {
    /// Send one request over the bridge and wait for the answer. `gone` and
    /// `dropped` are the two ways there is no answer, in the words of whichever
    /// surface asked.
    async fn bridged(
        &self,
        surface: Surface,
        plan: Json,
        gone: &str,
        dropped: &str,
    ) -> Result<Json> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.requests
            .send(HostRequest {
                surface,
                plan,
                reply,
            })
            .map_err(|_| Error::msg(gone.to_owned()))?;
        answer.await.map_err(|_| Error::msg(dropped.to_owned()))?
    }
}

#[cfg(feature = "eval")]
#[async_trait]
impl CodeHost for BridgeHost {
    async fn call(&self, request: Json) -> Result<Json> {
        self.bridged(
            Surface::Db,
            request,
            "this code body's database connection has gone away",
            "this database request was dropped without an answer",
        )
        .await
    }
}

#[cfg(feature = "eval")]
#[async_trait]
impl FetchHost for BridgeHost {
    async fn fetch(&self, request: Json) -> Result<Json> {
        self.bridged(
            Surface::Fetch,
            request,
            "this code body's network access has gone away",
            "this fetch was dropped without an answer",
        )
        .await
    }
}

/// One isolate thread, from the dispatcher's side: where to send it work, and
/// how much work it already has.
///
/// `outstanding` counts jobs sent and not yet answered — queued *and* resident —
/// which is what makes the choice between workers load-aware. It is an atomic
/// rather than a lock because it is read once per submission and written twice
/// per run, and because the alternative (one shared queue behind a mutex, which
/// is what this replaces) hands every job to whichever worker happens to be
/// waiting rather than to the one with room.
#[cfg(feature = "eval")]
struct Worker {
    jobs: tokio::sync::mpsc::UnboundedSender<CodeJob>,
    outstanding: Arc<AtomicUsize>,
}

/// A pool of isolates for **code bodies**, separate from the formula evaluator's
/// single pure isolate (decision 1). Cheap to share; dropping it shuts the
/// workers and their watchdogs down.
#[cfg(feature = "eval")]
pub struct CodeRuntime {
    workers: Vec<Worker>,
    /// What a [`CodeCall`] with no `timeout` of its own gets.
    default_timeout: Duration,
}

/// Build a `JsRuntime` **inside a tokio context**, which `deno_core` requires:
/// it registers each isolate against the runtime that was current when the
/// isolate was created, and if V8 later posts a delayed foreground task (its GC
/// memory reducer does, under load) against an isolate with no runtime it
/// **aborts the process**. Entering for the length of the constructor is enough.
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

    /// Start a pool of `workers` isolate threads (at least one), each admitting
    /// [`DEFAULT_MAX_INFLIGHT`] runs at a time.
    pub fn with_workers(workers: usize) -> CodeRuntime {
        CodeRuntime::with_workers_and_inflight(workers, DEFAULT_MAX_INFLIGHT)
    }

    /// Start a pool of `workers` isolate threads, each admitting `max_inflight`
    /// runs at a time and queueing the rest.
    pub fn with_workers_and_inflight(workers: usize, max_inflight: usize) -> CodeRuntime {
        CodeRuntime::build(workers, max_inflight, DEFAULT_MAX_HEAP)
    }

    /// The one constructor the others go through, with the heap bound spelled
    /// out. Not public: [`DEFAULT_MAX_HEAP`] is a bound on the *engine*, not a
    /// per-installation policy, and the tests are what want to say it in
    /// megabytes rather than hundreds of them.
    fn build(workers: usize, max_inflight: usize, max_heap: usize) -> CodeRuntime {
        let max_inflight = max_inflight.max(1);
        let mut pool = Vec::new();
        for n in 0..workers.max(1) {
            let (jobs, rx) = tokio::sync::mpsc::unbounded_channel::<CodeJob>();
            let outstanding = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&outstanding);
            std::thread::Builder::new()
                .name(format!("sc-code-{n}"))
                .spawn(move || worker_thread(rx, &counted, max_inflight, max_heap))
                // Thread spawning fails only on resource exhaustion at process
                // level; there is no useful recovery, and a run would error on a
                // closed channel anyway.
                .ok();
            pool.push(Worker { jobs, outstanding });
        }
        CodeRuntime {
            workers: pool,
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
    /// host calls** until it answers. The plan travels back here over a
    /// [`BridgeHost`], which is what lets a host borrow — the future holding the
    /// borrow is the future awaiting the run, so the borrow lives exactly as
    /// long as it must.
    ///
    /// The calls are served in a [`FuturesUnordered`] rather than one after the
    /// other, because a body's `Promise.all([…])` issues several at once and
    /// serving them in turn would quietly make that sequential — the parallelism
    /// the body asked for is real, and this is where it is honoured.
    pub async fn run(&self, call: CodeCall<'_>) -> Result<Json> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        // The proxies go to the worker and the borrowed hosts stay here, with
        // the receiving end of one channel between them. Only the run holds a
        // sender — the one made here is dropped below — so the run ending is the
        // receiver closing. One bridge serves both surfaces, so a body that
        // issues a query and a request together has them served together.
        let (requests, incoming) = tokio::sync::mpsc::unbounded_channel();
        let bridge = Arc::new(BridgeHost { requests });
        let proxy: Option<Arc<dyn CodeHost>> =
            call.host.map(|_| Arc::clone(&bridge) as Arc<dyn CodeHost>);
        let net: Option<Arc<dyn FetchHost>> = call
            .fetch
            .map(|_| Arc::clone(&bridge) as Arc<dyn FetchHost>);
        // Whatever the run did not get a proxy for, nothing can ask for.
        drop(bridge);
        let bridged = (call.host.is_some() || call.fetch.is_some())
            .then_some((call.host, call.fetch, incoming));
        let timeout = call
            .timeout
            .unwrap_or(self.default_timeout)
            .min(MAX_CODE_TIMEOUT);
        // Least-outstanding wins. The count is bumped before the send so that a
        // burst of submissions spreads rather than piling onto whichever worker
        // was idlest when the first of them looked.
        let worker = self
            .workers
            .iter()
            .min_by_key(|w| w.outstanding.load(Ordering::Relaxed))
            .ok_or_else(|| Error::msg("the code runtime has no workers"))?;
        worker.outstanding.fetch_add(1, Ordering::SeqCst);
        worker
            .jobs
            .send(CodeJob {
                run: Box::new(CodeRun {
                    code: call.code,
                    bindings: call.bindings,
                    host: proxy,
                    fetch: net,
                    timeout,
                    max_calls: call.max_calls,
                    max_fetches: call.max_fetches,
                    started: None,
                }),
                reply,
            })
            .map_err(|_| {
                worker.outstanding.fetch_sub(1, Ordering::SeqCst);
                Error::msg("the code runtime has no workers left")
            })?;

        let dropped = || Error::msg("the code runtime dropped the reply");
        // The wall clock covers the **whole** call, queue time included, because
        // the bounds inside the isolate cannot see a run that is not executing:
        // one waiting behind a saturated worker, and one host call that never
        // comes back. An unbounded hold on the request that fired the trigger is
        // exactly what `timeout` exists to prevent. Giving up here drops the
        // serving loop, so a run left behind fails at its next host call instead
        // of holding its place for as long as the database takes.
        let expired =
            tokio::time::sleep_until(tokio::time::Instant::now() + timeout + CALLER_GRACE);
        tokio::pin!(expired);
        let overdue = || {
            Error::invalid(format!(
                "this code exceeded its {} ms time limit",
                timeout.as_millis()
            ))
        };

        let Some((host, fetcher, mut incoming)) = bridged else {
            // A pure body asks for nothing; there is nothing to serve.
            return tokio::select! {
                outcome = answer => outcome.map_err(|_| dropped())?,
                () = &mut expired => Err(overdue()),
            };
        };
        tokio::pin!(answer);
        let mut serving = FuturesUnordered::new();
        loop {
            tokio::select! {
                outcome = &mut answer => return outcome.map_err(|_| dropped())?,
                () = &mut expired => return Err(overdue()),
                // Disabled once the run's proxy is gone, which is the run being
                // over — the first branch is what then answers.
                Some(HostRequest { surface, plan, reply }) = incoming.recv() => {
                    serving.push(async move {
                        // A dropped receiver means the isolate stopped waiting
                        // for this one: the answer is simply not wanted.
                        let answer = match surface {
                            // Unreachable with no host: the op refuses the call
                            // before it reaches the bridge, because the run
                            // table has no host to hand it either.
                            Surface::Db => match host {
                                Some(host) => host.call(plan).await,
                                None => Err(Error::msg("this code body has no database access")),
                            },
                            Surface::Fetch => match fetcher {
                                Some(fetcher) => fetcher.fetch(plan).await,
                                None => Err(Error::msg("this code body cannot reach the network")),
                            },
                        };
                        let _ = reply.send(answer);
                    });
                }
                // Draining what is in flight. The deadline above interrupts the
                // calls themselves and not only the wait for the next one: one
                // query that never comes back is the case this whole bound is
                // for, and returning here drops every future in the set at once.
                Some(()) = serving.next(), if !serving.is_empty() => {}
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

/// One worker: its own isolate, its own watchdog, and **many runs at once**.
///
/// The thread owns a current-thread tokio runtime and spends its life inside one
/// `block_on`: the loop admits jobs while pumping the isolate's event loop, and
/// parks on the job channel when nothing is resident. Nothing here blocks on a
/// host call any more, which is the whole point — the isolate is free between a
/// body's `await` and its answer, and what it does with that freedom is serve
/// every other body.
#[cfg(feature = "eval")]
fn worker_thread(
    rx: tokio::sync::mpsc::UnboundedReceiver<CodeJob>,
    outstanding: &Arc<AtomicUsize>,
    max_inflight: usize,
    max_heap: usize,
) {
    // The worker's runtime is both the isolate's tokio anchor (see
    // `build_isolate`) and what drives the isolate's event loop. Its own, rather
    // than the caller's: nothing about a run needs the submitter's runtime, and
    // an isolate whose delayed foreground tasks belong to a runtime it does not
    // control is the failure `build_isolate` exists to document.
    let Ok(local) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        // Runtime construction fails only on resource exhaustion; a run would
        // then error on a closed channel, which is the honest symptom.
        return;
    };
    let (mut runtime, _anchor) = build_isolate(
        Some(local.handle()),
        deno_core::RuntimeOptions {
            extensions: vec![sc_db_ext::init()],
            // The heap the resident runs share. Without a limit the isolate is
            // bounded only by the machine, and the failure mode of that is the
            // process — hundreds of resident runs are hundreds of scopes and
            // their reads, and admission counts them without weighing them.
            create_params: Some(deno_core::v8::CreateParams::default().heap_limits(0, max_heap)),
            ..Default::default()
        },
    );

    let watchdog = Watchdog::start(runtime.v8_isolate().thread_safe_handle());
    // Near the limit, V8's own answer is to abort the process. This is the
    // answer instead: raise the limit by a grace, and tell the worker to admit
    // nothing new until the runs holding the heap have finished with it. A body
    // that fills even the grace is stopped like any other runaway — through the
    // watchdog, so that the unwinding, the attribution and the re-queueing are
    // the ones already written.
    //
    // V8 keeps whatever limit this returns, so an isolate that has been through
    // one heap incident is left strict rather than generous: the grace has
    // already been spent, and the next body to reach the raised limit is past
    // the ceiling on arrival and stopped at once.
    let pressure = Arc::new(AtomicBool::new(false));
    {
        let (dog, flag) = (Arc::clone(&watchdog), Arc::clone(&pressure));
        runtime.add_near_heap_limit_callback(move |current, initial| {
            flag.store(true, Ordering::SeqCst);
            let grace = (initial / 2).max(1);
            let ceiling = initial.saturating_add(initial);
            if current >= ceiling {
                dog.trip(Trip::Heap);
                // Never the same limit twice: returning `current` is the abort
                // this exists to avoid, and the terminated body needs somewhere
                // to unwind into.
                return current.saturating_add(grace);
            }
            current.saturating_add(grace).min(ceiling)
        });
    }

    // The op handles and the run wrapper, then `Deno` goes away — see SETUP. A
    // failure here would leave every run unable to reach the host, so say so
    // rather than serving bodies that fail one by one for no visible reason.
    if let Err(e) = runtime.execute_script("sc_code_setup.js", SETUP) {
        // Nothing to reply to yet; the first run's `__scInvoke is not defined`
        // is the symptom, and this is the cause it will be diagnosed from.
        debug_assert!(false, "code runtime setup failed: {e}");
    }
    // The `db` factory: a few hundred lines of JavaScript compiled **once** for
    // this isolate rather than spliced into every run's script. Each run still
    // gets a handle of its own — `__scMakeDb(token)` builds one — which is
    // decision 5 preserved by the factory instead of by recompilation.
    if let Err(e) = runtime.execute_script("sc_db.js", DB_PRELUDE) {
        debug_assert!(false, "the db prelude failed to compile: {e}");
    }
    // The `fetch` factory and the two web shapes it answers with, on the same
    // terms: compiled once, and a run is handed a function closed over its own
    // token rather than a global anything could call.
    if let Err(e) = runtime.execute_script("sc_fetch.js", FETCH_PRELUDE) {
        debug_assert!(false, "the fetch prelude failed to compile: {e}");
    }
    // Code bodies get the aggregation prelude too, so `rows().sum("qty")` means
    // in a body what it means in a formula.
    let _ = runtime.execute_script("sc_agg.js", crate::eval::AGG_PRELUDE);

    let freed = Arc::new(tokio::sync::Notify::new());
    let op_state = runtime.op_state();
    op_state.borrow_mut().put(RunTable::new(
        Arc::clone(&watchdog),
        Arc::clone(outstanding),
        Arc::clone(&freed),
    ));

    local.block_on(serve(
        &mut runtime,
        &op_state,
        rx,
        max_inflight,
        &watchdog,
        &freed,
        &pressure,
    ));
    watchdog.stop();
}

/// What ended one turn of the worker's loop.
#[cfg(feature = "eval")]
enum Tick {
    /// The event loop went quiet: every op has settled and every microtask has
    /// run.
    Quiet(std::result::Result<(), String>),
    /// A job arrived, or the channel closed.
    Job(Option<CodeJob>),
    /// A run left the table, so there may be room for a queued one.
    Freed,
    /// A deadline came due — some run's, or the watchdog's.
    Due,
}

/// The worker's loop: admit, pump, answer, reap.
///
/// The four things it waits on at once are what make many runs per isolate work.
/// The **event loop** is what advances every resident run. The **job channel** is
/// what admits new ones without waiting for the resident ones to finish. The
/// **freed** notification is what fills a place the moment one opens, since a run
/// finishes inside a poll that does not return while its co-residents are still
/// in flight. And the **timer** is what notices a deadline while every run is
/// suspended — nothing is executing then, so neither the watchdog nor the op's
/// own check can see it.
///
/// Two things gate admission besides the occupancy bound: runs handed back by a
/// termination go in front of the queue (they were admitted once already, and
/// their caller is still waiting on the clock that started then), and heap
/// `pressure` stops admission entirely until the runs holding the heap have
/// given it back.
#[cfg(feature = "eval")]
async fn serve(
    runtime: &mut deno_core::JsRuntime,
    op_state: &Rc<RefCell<OpState>>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<CodeJob>,
    max_inflight: usize,
    watchdog: &Watchdog,
    freed: &tokio::sync::Notify,
    pressure: &AtomicBool,
) {
    let mut closed = false;
    // Runs whose isolate was terminated under them before they had reached the
    // database. They are owed another go, and nothing else is.
    let mut requeued: std::collections::VecDeque<CodeJob> = std::collections::VecDeque::new();
    // What this isolate has already compiled. It lives as long as the isolate
    // does, which is what makes the second run of a body cheap.
    let mut bodies = BodyCache::new();
    loop {
        // Admit whatever is already waiting, up to the occupancy bound.
        while resident(op_state) < max_inflight {
            if let Some(job) = requeued.pop_front() {
                start_run(runtime, op_state, job, &mut requeued, watchdog, &mut bodies);
                continue;
            }
            if closed || pressure.load(Ordering::SeqCst) {
                break;
            }
            match rx.try_recv() {
                Ok(job) => start_run(runtime, op_state, job, &mut requeued, watchdog, &mut bodies),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    closed = true;
                }
            }
        }
        if resident(op_state) == 0 {
            // An empty isolate is one that has given its heap back, whether or
            // not there was anything queued to admit into it.
            pressure.store(false, Ordering::SeqCst);
            // And one with nothing left to run: an armed watchdog here would
            // terminate an idle isolate, which is a termination the *next* run's
            // JavaScript would be the one to suffer.
            idle(op_state);
            if closed {
                return; // The last CodeRuntime handle was dropped.
            }
            // Nothing to pump: park on the channel rather than poll an empty
            // event loop for ever.
            match rx.recv().await {
                Some(job) => {
                    start_run(runtime, op_state, job, &mut requeued, watchdog, &mut bodies)
                }
                None => closed = true,
            }
            continue;
        }

        let admit =
            !closed && !pressure.load(Ordering::SeqCst) && resident(op_state) < max_inflight;
        let due = next_deadline(op_state);
        let tick = {
            let pump = runtime.run_event_loop(deno_core::PollEventLoopOptions::default());
            tokio::pin!(pump);
            // The event loop yielding is the one moment Rust can see JavaScript
            // *stop*: every resumption is inside this poll, and when it comes
            // back pending there is nothing on the isolate. That is where the
            // running mark is cleared — not after the `select!`, which may then
            // wait seconds for a host answer with the watchdog still armed at a
            // slice belonging to a body that suspended long ago, and terminate an
            // idle isolate out from under whoever runs next.
            let mut pump = std::future::poll_fn(|cx| {
                let polled = std::future::Future::poll(pump.as_mut(), cx);
                if polled.is_pending() {
                    idle(op_state);
                }
                polled
            });
            tokio::select! {
                outcome = &mut pump => Tick::Quiet(outcome.map_err(|e| e.to_string())),
                job = rx.recv(), if admit => Tick::Job(job),
                () = freed.notified(), if !admit => Tick::Freed,
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(due)) => Tick::Due,
            }
        };

        // A termination during the pump, whatever else happened: the isolate is
        // poisoned until it is cancelled, and every resident run went down with
        // the one that overran it.
        handle_terminated(runtime, op_state, &mut requeued, watchdog);
        // The pump has yielded, so no JavaScript is on the isolate: nothing can
        // overrun a slice until something resumes, and the watchdog must not be
        // left pointed at a body that is no longer running.
        idle(op_state);

        match tick {
            // The event loop emptied with runs still resident: nothing is
            // executing and nothing is in flight, so nothing will ever resolve
            // what they are waiting on. `await new Promise(() => {})` is the
            // whole shape, and answering it now is better than at the deadline.
            Tick::Quiet(Ok(())) => {
                for mut run in drain_runs(op_state) {
                    run.answer(Err(Error::invalid(
                        "this code awaited something that never happens; `db` is the only \
                         awaitable thing in the sandbox, and there are no timers",
                    )));
                }
            }
            // The event loop itself failed. Not one run's error — a body's own
            // throw is reported through `__scFail` — so it belongs to whoever
            // was resident.
            Tick::Quiet(Err(e)) => {
                for mut run in drain_runs(op_state) {
                    run.answer(Err(Error::invalid(format!("JavaScript code failed: {e}"))));
                }
            }
            Tick::Job(Some(job)) => {
                start_run(runtime, op_state, job, &mut requeued, watchdog, &mut bodies)
            }
            Tick::Job(None) => closed = true,
            Tick::Freed | Tick::Due => {}
        }

        reap_expired(op_state);
    }
}

/// How many runs this isolate is holding.
#[cfg(feature = "eval")]
fn resident(op_state: &Rc<RefCell<OpState>>) -> usize {
    op_state
        .borrow()
        .try_borrow::<RunTable>()
        .map_or(0, |table| table.runs.len())
}

/// Every resident run, taken out of the table.
#[cfg(feature = "eval")]
fn drain_runs(op_state: &Rc<RefCell<OpState>>) -> Vec<RunState> {
    let mut state = op_state.borrow_mut();
    state
        .try_borrow_mut::<RunTable>()
        .map_or_else(Vec::new, |table| {
            table.drain().into_iter().map(|(_, run)| run).collect()
        })
}

/// No JavaScript is on the isolate. Called wherever JS has just stopped running
/// — after a run's script, after a poll of the event loop — because the watchdog
/// is armed at the *running* run's slice and a body that has yielded is not
/// running.
#[cfg(feature = "eval")]
fn idle(op_state: &Rc<RefCell<OpState>>) {
    if let Some(table) = op_state.borrow_mut().try_borrow_mut::<RunTable>() {
        table.leave();
    }
}

/// If the watchdog terminated the isolate: cancel that, tell the body that
/// caused it, and deal with the ones that were merely in the room.
///
/// Termination is the blunt instrument — it stops the isolate, not a run — so
/// the co-residents are the part that has to be got right. They must **not** be
/// silently re-run: a body that has already inserted rows is not idempotent, and
/// re-executing it is a worse failure than the one being handled. So a run that
/// has made **zero** host calls goes back in the queue (it still holds the job it
/// was admitted with, which is only true while that is so), and every other
/// resident is answered with its own named error saying what happened to it.
///
/// Cancelling comes first and unconditionally, because the alternative is
/// leaving the isolate terminated so that the *next* run's JavaScript is aborted
/// in place of the guilty one's.
#[cfg(feature = "eval")]
fn handle_terminated(
    runtime: &mut deno_core::JsRuntime,
    op_state: &Rc<RefCell<OpState>>,
    requeued: &mut std::collections::VecDeque<CodeJob>,
    watchdog: &Watchdog,
) {
    let Some(why) = watchdog.took_fired() else {
        return;
    };
    runtime.v8_isolate().cancel_terminate_execution();

    let mut failed: Vec<(RunState, Error)> = Vec::new();
    {
        let mut state = op_state.borrow_mut();
        let Some(table) = state.try_borrow_mut::<RunTable>() else {
            return;
        };
        // Whose JavaScript was on the isolate. `None` — an overrun in a turn
        // where the mark had already been cleared — is not a reason to guess:
        // every resident is then treated as a co-resident, which fails a body
        // with the wrong reason but never re-runs one that wrote.
        let guilty =
            table
                .running
                .take()
                .map(|(token, _)| token)
                .or_else(|| match table.runs.len() {
                    // Not a guess: with one run resident, whatever JavaScript was on
                    // the isolate can only have been that run's.
                    1 => table.runs.keys().next().cloned(),
                    _ => None,
                });
        let now = Instant::now();
        for (token, mut run) in table.drain() {
            if Some(&token) == guilty.as_ref() {
                let error = blame(why, &run, now);
                failed.push((run, error));
                continue;
            }
            match run.retry.take() {
                // Provably no side effects yet, and its wall clock is what
                // decides whether there is still any point in another go.
                Some(job) if now < run.deadline => {
                    if let Some(reply) = run.reply.take() {
                        table.outstanding.fetch_add(1, Ordering::SeqCst);
                        requeued.push_back(CodeJob { run: job, reply });
                    }
                }
                Some(_) => {
                    let ms = run.timeout.as_millis();
                    let error =
                        Error::invalid(format!("this code exceeded its {ms} ms time limit"));
                    failed.push((run, error));
                }
                None => failed.push((run, bystander(why))),
            }
        }
    }
    for (mut run, error) in failed {
        run.answer(Err(error));
    }
}

/// What the body that stopped the isolate is told.
///
/// Two clocks, two messages, and the difference matters to whoever reads it: a
/// run out of *wall* clock spent its time somewhere (very likely in the database)
/// and wants a longer timeout or a smaller job, while a run out of *slice* did
/// not yield — it computed, in one go, for longer than a body sharing an isolate
/// may.
#[cfg(feature = "eval")]
fn blame(why: Trip, run: &RunState, now: Instant) -> Error {
    match why {
        Trip::Heap => Error::invalid(
            "this code used more memory than the JavaScript engine has, and was stopped; \
             read fewer rows at a time (`.iter()` streams them in batches) or keep less of \
             what you read",
        ),
        // The slice is clamped to what is left of the wall clock, so a body that
        // spins through the end of its timeout trips the watchdog at the
        // deadline rather than at the slice. That is the wall clock catching it,
        // and saying so keeps one bound with one wording.
        Trip::Slice if now >= run.deadline => Error::invalid(format!(
            "this code exceeded its {} ms time limit",
            run.timeout.as_millis()
        )),
        Trip::Slice => Error::invalid(format!(
            "this code ran for {} ms without awaiting anything, and was stopped; a code body \
             shares its isolate with every other body, so it must not compute for that long \
             between two `await`s",
            run.slice.as_millis()
        )),
    }
}

/// What a run that was merely *on* the isolate is told.
///
/// It is not being re-run, and the reason is the point: it had already reached
/// the database, and a body that has written rows is not one to execute twice
/// because something else misbehaved. Rare, loud, and never a duplicated write.
#[cfg(feature = "eval")]
fn bystander(why: Trip) -> Error {
    let what = match why {
        Trip::Slice => "ran without yielding",
        Trip::Heap => "used more memory than the JavaScript engine has",
    };
    Error::invalid(format!(
        "another code body on the same isolate {what} and had to be stopped, which stopped \
         this one with it. It has not been re-run, because it had already reached the \
         database and re-running it could repeat what it did there"
    ))
}

/// How often the loop looks again when it has nothing to look at: the floor under
/// the timer, so that a deadline already in the past cannot spin it.
#[cfg(feature = "eval")]
const TICK_FLOOR: Duration = Duration::from_millis(1);

/// When the loop next has something to do that is not an event: the earliest
/// wall-clock deadline among the resident runs, or the slice the watchdog is
/// armed at — whichever comes first.
///
/// The watchdog's own deadline is here because the watchdog fires on a thread of
/// its own: if it terminates the isolate while every run is suspended, the event
/// loop stays pending and nobody would notice until the next run's JavaScript
/// was aborted in its place.
#[cfg(feature = "eval")]
fn next_deadline(op_state: &Rc<RefCell<OpState>>) -> Instant {
    let state = op_state.borrow();
    let now = Instant::now();
    state
        .try_borrow::<RunTable>()
        .and_then(|table| {
            table
                .runs
                .values()
                .map(|run| run.deadline)
                .chain(table.running.iter().map(|(_, until)| *until))
                .min()
        })
        .unwrap_or(now + TICK_FLOOR)
        .max(now + TICK_FLOOR)
}

/// Answer every run whose wall clock has run out, and take it out of the table.
///
/// This is what keeps an abandoned run from being resident for ever. Its caller
/// has given up (or is about to), its bridge is gone, and whatever is left of it
/// inside the isolate will find no host at its next call — which is the same
/// isolation the token already gives it, said on the clock rather than on the
/// next question it asks.
#[cfg(feature = "eval")]
fn reap_expired(op_state: &Rc<RefCell<OpState>>) {
    let expired = {
        let mut state = op_state.borrow_mut();
        let Some(table) = state.try_borrow_mut::<RunTable>() else {
            return;
        };
        let now = Instant::now();
        let overdue: Vec<String> = table
            .runs
            .iter()
            .filter(|(_, run)| now >= run.deadline)
            .map(|(token, _)| token.clone())
            .collect();
        overdue
            .iter()
            .filter_map(|token| table.take(token))
            .collect::<Vec<RunState>>()
    };
    for mut run in expired {
        let ms = run.timeout.as_millis();
        run.answer(Err(Error::invalid(format!(
            "this code exceeded its {ms} ms time limit"
        ))));
    }
}

/// Admit one run: mint its token, put its state in the table, and execute its
/// script.
///
/// `execute_script` **starts** a run rather than finishing one. The body is an
/// async function, so the script returns as soon as the body reaches its first
/// `await`; from there the event loop carries it, and a completion op is what
/// answers the caller. A syntax error still surfaces synchronously here, and
/// still should — there is nothing to wait for.
///
/// A body that spins without ever yielding is the other thing that can come back
/// synchronously: it never reached an `await`, so the watchdog terminated it
/// inside this call. That is handled before returning, because the isolate must
/// not still be poisoned when the next job is admitted.
#[cfg(feature = "eval")]
fn start_run(
    runtime: &mut deno_core::JsRuntime,
    op_state: &Rc<RefCell<OpState>>,
    job: CodeJob,
    requeued: &mut std::collections::VecDeque<CodeJob>,
    watchdog: &Watchdog,
    bodies: &mut BodyCache,
) {
    let CodeJob { run, reply } = job;
    // A job that never enters the table has to give its place back by hand;
    // everything after the insert below is accounted for by `RunTable::take`.
    let Some(token) = new_token() else {
        give_back(op_state);
        let _ = reply.send(Err(Error::msg(
            "the code runtime could not draw a run token from the operating system",
        )));
        return;
    };
    let scripts = match build_run_scripts(&run) {
        Ok(scripts) => scripts,
        Err(e) => {
            give_back(op_state);
            let _ = reply.send(Err(e));
            return;
        }
    };
    // The body's source travels only when this isolate has not compiled it: a
    // trigger firing repeatedly sends a token, a key and its bindings.
    let key = BodyCache::key(&scripts.definition);
    let held = bodies.holds(key, &scripts.definition);
    let script = build_script(&scripts, &token, key, held);
    // A re-queued run carries the clock it was first admitted with: its caller
    // has been waiting since then, and a retry with a fresh deadline would
    // outlive the future that is going to answer with it.
    let started = run.started.unwrap_or_else(Instant::now);
    {
        let mut state = op_state.borrow_mut();
        let Some(table) = state.try_borrow_mut::<RunTable>() else {
            // Unreachable: the worker puts the table in before it serves
            // anything. There is nothing to give back either, because the count
            // it would be given back to lives in the table that is missing.
            drop(state);
            let _ = reply.send(Err(Error::msg("the code runtime has no run table")));
            return;
        };
        table.runs.insert(
            token.clone(),
            RunState {
                host: run.host.clone(),
                fetch: run.fetch.clone(),
                deadline: started + run.timeout,
                timeout: run.timeout,
                calls_left: run.max_calls,
                max_calls: run.max_calls,
                fetches_left: run.max_fetches,
                max_fetches: run.max_fetches,
                slice: DEFAULT_JS_SLICE.min(run.timeout),
                // Kept until the first host call, which is exactly as long as
                // re-running this body would provably repeat nothing.
                retry: Some(Box::new(CodeRun {
                    started: Some(started),
                    ..*run
                })),
                reply: Some(reply),
            },
        );
        // This run's JavaScript is what is about to run.
        table.enter(&token);
    }
    let outcome = runtime.execute_script("sc_code.js", script);
    // A terminated run is already answered by name, so this comes first.
    handle_terminated(runtime, op_state, requeued, watchdog);
    // The isolate has this body only if the script that defined it ran, so this
    // is recorded here and not before: a syntax error, or a termination inside
    // this very call, leaves the cache saying what is true — that the next run
    // of this body must carry its source again.
    if !held && outcome.is_ok() {
        for gone in bodies.store(key, scripts.definition) {
            // Rare (one distinct body past the cache's capacity), and cheap
            // enough not to be worth batching into the next run's script, where
            // it would have to be carried until there was a next run.
            let _ = runtime.execute_script("sc_forget.js", format!("__scForget(\"{gone:016x}\");"));
        }
    }
    // The mark is deliberately **left standing** here. `execute_script`
    // returning does not mean the body has stopped running: an `async function`
    // that awaits anything but a host call — `await null` is the whole shape —
    // suspends into a microtask that the next poll of the event loop drains,
    // with no op to mark it and no way for Rust to see it start. Keeping this
    // run marked until the pump yields is what keeps that JavaScript watched;
    // the alternative is a body that loops on microtasks holding its isolate for
    // ever with the watchdog disarmed.
    if let Err(e) = outcome {
        let mut state = op_state.borrow_mut();
        if let Some(table) = state.try_borrow_mut::<RunTable>()
            && let Some(mut failed) = table.take(&token)
        {
            failed.answer(Err(Error::invalid(format!("JavaScript code failed: {e}"))));
        }
    }
}

/// Give a job's place in the occupancy count back, for a job that never made it
/// into the table.
#[cfg(feature = "eval")]
fn give_back(op_state: &Rc<RefCell<OpState>>) {
    if let Some(table) = op_state.borrow().try_borrow::<RunTable>() {
        table.outstanding.fetch_sub(1, Ordering::SeqCst);
        table.freed.notify_one();
    }
}

/// A fresh run token: 128 bits from the operating system, in hex.
///
/// Random rather than sequential because the token is the only thing separating
/// two resident runs' authority: `db.asUser()` delegates to *this* event's
/// caller, so a body that could name another run's token could ask the database
/// questions in that caller's name. `None` when the OS refuses entropy, which is
/// a refusal to run rather than a reason to invent a guessable one.
#[cfg(feature = "eval")]
fn new_token() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    let mut hex = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    Some(hex)
}

// ---------------------------------------------------------------------------
// The script
// ---------------------------------------------------------------------------

/// The two halves of one run's script: what depends on the **body** and what
/// depends on this **run**.
///
/// The split is the whole of the caching. The definition is a function of the
/// code and the binding *names* alone, so it is what the isolate keeps and what
/// a content key is taken over; the arguments are this run's binding *values*,
/// which travel every time because they are what differs.
#[cfg(feature = "eval")]
struct RunScripts {
    /// The compiled form: `async function (__b, db) { … }`, with the bindings
    /// destructured into `const`s and the code as the body of a nested async
    /// function.
    definition: String,
    /// This run's bindings, as the JSON object the definition reads from.
    args: String,
    /// Whether the definition takes the `db` handle — a body with no host does
    /// not, so naming `db` in it is a ReferenceError rather than a handle that
    /// fails on use.
    wants_db: bool,
    /// Whether it takes `fetch`, on exactly the same terms.
    wants_fetch: bool,
}

/// Build one code body's definition and one run's arguments: the bindings as
/// `const`s, and the code as the body of a nested **async** function — so a
/// top-level `return` is legal, a top-level `await` is legal, and nothing it
/// declares outlives the run.
///
/// The token is *not* in here, and that is what makes the definition reusable:
/// it arrives at `__scInvoke`, which is what builds this run's `db` over it. A
/// body is handed the handle rather than the token, so there is nothing in its
/// scope to pass to another run's host even if it could guess one.
///
/// The code itself is **not** escaped, and cannot be: it is the admin's own
/// JavaScript, spliced in as source. That is not a hole — the wrapper is no
/// privilege boundary, and the host re-validates every plan that comes back out
/// of it. What *is* escaped is every value, which rides in as JSON exactly as a
/// formula's bindings do.
#[cfg(feature = "eval")]
fn build_run_scripts(call: &CodeRun) -> Result<RunScripts> {
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
        if call.fetch.is_some() && name == FETCH {
            return Err(Error::msg(
                "code binding `fetch` collides with the HTTP surface bound in a code body",
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
    let wants_db = call.host.is_some();
    let wants_fetch = call.fetch.is_some();
    // The parameter list is the only difference a capability makes: no `fetch`
    // parameter is no `fetch` in scope, which is a ReferenceError naming it
    // rather than a call that fails somewhere in the host. It also means a body
    // compiled with the network and one compiled without are different text,
    // and so different entries in the body cache — which is what stops a cached
    // body from being invoked with a scope it was not compiled for.
    let params = match (wants_db, wants_fetch) {
        (true, true) => "__b, db, fetch",
        (true, false) => "__b, db",
        (false, true) => "__b, fetch",
        (false, false) => "__b",
    };
    let code = &call.code;
    Ok(RunScripts {
        definition: format!(
            "async function ({params}) {{ \"use strict\";\n\
             {consts}\
             const __result = await (async function () {{\n{code}\n}})();\n\
             return __result;\n\
             }}"
        ),
        args,
        wants_db,
        wants_fetch,
    })
}

/// The compiled bodies one isolate holds, and the keys the isolate knows them
/// by.
///
/// Owned by the worker rather than by the isolate, because the point of it is
/// what the worker can leave **out** of a run's script: knowing that this
/// isolate has already compiled this body is what turns a run into
/// `__scInvoke(token, key, bindings)` with the source nowhere in it. A trigger
/// that fires a thousand times compiles once.
///
/// The key is a hash of the definition, and the definition is kept beside it so
/// that a hash *collision* costs a recompile rather than running the wrong body:
/// a key whose stored definition is not this one is a miss, and defining under
/// it replaces what the isolate had. That is what lets the key be cheap.
///
/// Bounded, because a server with many distinct bodies must not accumulate
/// compiled functions in the isolate for ever: past [`BODY_CACHE_CAPACITY`] the
/// least recently run body is dropped, here and — through `__scForget` — there.
#[cfg(feature = "eval")]
struct BodyCache {
    entries: HashMap<u64, CachedBody>,
    /// A logical clock: which entry was used last, without asking the operating
    /// system for the time on the hot path.
    clock: u64,
}

#[cfg(feature = "eval")]
struct CachedBody {
    definition: String,
    used: u64,
}

/// How many compiled bodies one isolate keeps. Generous next to the number of
/// triggers an installation has, and small next to the heap a run needs, so the
/// eviction path is the one this will almost never take.
#[cfg(feature = "eval")]
const BODY_CACHE_CAPACITY: usize = 256;

#[cfg(feature = "eval")]
impl BodyCache {
    fn new() -> BodyCache {
        BodyCache {
            entries: HashMap::new(),
            clock: 0,
        }
    }

    /// What the isolate will know this definition by.
    fn key(definition: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        definition.hash(&mut hasher);
        hasher.finish()
    }

    /// Whether the isolate already has *this* definition under `key` — which is
    /// whether the run's script can leave the source out.
    fn holds(&mut self, key: u64, definition: &str) -> bool {
        self.clock += 1;
        let now = self.clock;
        match self.entries.get_mut(&key) {
            Some(entry) if entry.definition == definition => {
                entry.used = now;
                true
            }
            _ => false,
        }
    }

    /// Record a definition the isolate has just compiled, and answer with the
    /// keys it should forget to make room for it.
    fn store(&mut self, key: u64, definition: String) -> Vec<u64> {
        self.clock += 1;
        let used = self.clock;
        self.entries.insert(key, CachedBody { definition, used });
        let mut evicted = Vec::new();
        while self.entries.len() > BODY_CACHE_CAPACITY {
            // Never the entry just stored: it has the highest clock of them all.
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            self.entries.remove(&oldest);
            evicted.push(oldest);
        }
        evicted
    }
}

/// One run's script: the definition when the isolate has not got it, and the
/// invocation either way.
///
/// The token and the bindings are what is left travelling per run — 32 hex
/// characters and this run's own values — because everything else is already
/// there.
#[cfg(feature = "eval")]
fn build_script(scripts: &RunScripts, token: &str, key: u64, held: bool) -> String {
    let RunScripts {
        definition,
        args,
        wants_db,
        wants_fetch,
    } = scripts;
    let mut script = String::new();
    if !held {
        script.push_str(&format!(
            "__scDefine(\"{key:016x}\", {wants_db}, {wants_fetch}, {definition});\n"
        ));
    }
    // The token is 32 hex characters this crate minted and the key is 16 this
    // one made; quoting them is belt and braces rather than escaping.
    script.push_str(&format!("__scInvoke(\"{token}\", \"{key:016x}\", {args});"));
    script
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
            .run(call("return await db.books.rows();"))
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
                r#"return await db.invoices
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
                   for await (const row of db.invoices.where({ paid: false }).iter(2)) {
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
                   const before = await db.invoices.count();
                   let first = null;
                   for await (const row of it) { first = row.id; break; }
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
                   for await (const row of db.invoices.limit(3).iter(2)) seen.push(row.id);
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
            "for await (const g of db.invoices.groupBy('paid').aggregate({ n: 'count()' }).iter()) {}",
        )
        .await;
        assert!(e.contains("aggregates"), "{e}");
        let e = refused("for await (const r of db.invoices.iter(0)) {}").await;
        assert!(e.contains("how many rows to read at a time"), "{e}");
        let e = refused("for await (const r of db.invoices.iter('lots')) {}").await;
        assert!(e.contains("how many rows to read at a time"), "{e}");
    }

    #[tokio::test]
    async fn the_two_spellings_of_a_filter_and_repeated_wheres_and() {
        let host = FakeHost::rows(json!([]));
        let rt = CodeRuntime::new();
        rt.run(with_host(
            r#"await db.books.where({ status: "draft" }).where('pages > 3').rows();
               await db.table("books").where('status === "draft"').rows();
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
            r#"await db.invoices.rows();
               await db.asUser().invoices.rows();
               await db.invoices.asUser().where({ paid: false }).rows();
               await db.invoices.where({ paid: false }).asUser().rows();
               await db.asUser().invoices.asAdmin().rows();
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
                r#"const a = await db.sql("select count(*) as n from books where pages > $1", [200]);
                   await db.sql("select 1", [], { asUser: true });
                   await db.asUser().sql("select 1");
                   await db.asUser().sql("select 1", null, { asUser: false });
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
            (r#"return await db.sql({ from: "books" });"#, "SQL text"),
            (
                r#"return await db.sql("select 1", { id: 1 });"#,
                "array of values",
            ),
            (
                r#"return await db.sql("select 1", [], { asuser: true });"#,
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
                     first:  await db.books.first(),
                     get:    await db.books.get(4),
                     exists: await db.books.where({ id: 4 }).exists(),
                     count:  await db.books.count(),
                     sum:    await db.books.sum("qty * price"),
                     insert: await db.books.insert({ title: "Orlando" }),
                     update: await db.books.where({ id: 3 }).update({ shelf: 3 }),
                     del:    await db.books.where({ id: 7 }).delete(),
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
                r#"return await db.books
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
                r#"return await db.books.aggregate({ n: "count()" }).rows();"#,
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
                r#"return await db.books.groupBy("author").rows();"#,
                "needs an .aggregate(",
            ),
            (
                r#"return await db.books.groupBy("author").count();"#,
                "answers one value",
            ),
            (
                r#"return await db.books.aggregate({ n: "count" }).rows();"#,
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
            "return await db.books.update({ shelf: 3 });",
            "return await db.books.delete();",
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
            .run(with_host("return await db.books.rows();", &*host))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("ownership formula does not grant"), "{err}");
        // Caught, the body carries on — §5's "try a delegated write and fall back".
        let out = rt
            .run(with_host(
                "try { await db.books.asUser().rows(); } catch (e) { return e.message; } return null;",
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
            "for (let i = 0; i < 100; i++) await db.books.rows(); return true;",
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
            tokio::spawn(
                async move { rt.run(with_host("return await db.a.rows();", &*host)).await },
            )
        };
        let two = {
            let (rt, host) = (Arc::clone(&rt), Arc::clone(&slow));
            tokio::spawn(
                async move { rt.run(with_host("return await db.b.rows();", &*host)).await },
            )
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
        // A slice is never longer than what is left of the run's own wall clock,
        // so a body given 100 ms is stopped at 100 ms — by the clock that ran out
        // first, and in that clock's own words.
        let mut c = call("while (true) {}");
        c.timeout = Some(Duration::from_millis(100));
        let started = Instant::now();
        let err = rt.run(c).await.unwrap_err().to_string();
        assert!(err.contains("exceeded its 100 ms time limit"), "{err}");
        assert!(
            started.elapsed() < Duration::from_millis(350),
            "it was not the watchdog that stopped it: {:?}",
            started.elapsed()
        );
        // The same worker serves the next run normally.
        assert_eq!(rt.run(call("return 1 + 1;")).await.unwrap(), json!(2));
    }

    #[tokio::test]
    async fn a_body_that_never_yields_is_stopped_at_the_slice_not_at_its_timeout() {
        // The second clock. This body has thirty seconds of wall clock and uses
        // none of it in the database, so nothing about its *timeout* is what
        // ought to stop it: what it is doing wrong is holding the isolate — and
        // every other body resident on it — without yielding.
        let rt = CodeRuntime::with_workers(1);
        let mut c = call("while (true) {}");
        c.timeout = Some(Duration::from_secs(30));
        let started = Instant::now();
        let err = rt.run(c).await.unwrap_err().to_string();
        assert!(err.contains("without awaiting anything"), "{err}");
        assert!(
            err.contains(&format!("{} ms", DEFAULT_JS_SLICE.as_millis())),
            "the slice is not named: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "it waited for the wall clock: {:?}",
            started.elapsed()
        );
        // And a body that spins *after* an await is the same body: the slice is a
        // fresh window at each resumption, not a budget the run spends.
        let host = FakeHost::rows(json!([]));
        let mut c = with_host("await db.books.rows(); while (true) {}", &*host);
        c.timeout = Some(Duration::from_secs(30));
        let err = rt.run(c).await.unwrap_err().to_string();
        assert!(err.contains("without awaiting anything"), "{err}");
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
            "for (let i = 0; i < 10; i++) await db.books.rows(); return true;",
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
        let mut c = with_host("await db.books.rows(); return true;", &*slow);
        c.timeout = Some(Duration::from_millis(1000));
        assert_eq!(rt.run(c).await.unwrap(), json!(true));
    }

    #[tokio::test]
    async fn the_globals_cannot_be_poisoned_for_the_next_run() {
        let host = FakeHost::rows(json!([{ "id": 1 }]));
        let rt = CodeRuntime::with_workers(1);
        // A body that tries to replace the op handle, the run wrapper, the `db`
        // factory and `db` itself — the last two of which are shared by every
        // body on this isolate now that neither is compiled per run.
        let out = rt
            .run(with_host(
                r#"let broke = [];
                   try { globalThis.__scDbCall = () => []; } catch (e) { broke.push("call"); }
                   try { delete globalThis.__scInvoke; } catch (e) { broke.push("invoke"); }
                   try { globalThis.__scMakeDb = () => ({}); } catch (e) { broke.push("makeDb"); }
                   try { Object.defineProperty(globalThis, "__scDbCall", { value: 1 }); }
                     catch (e) { broke.push("define"); }
                   globalThis.db = "poisoned";
                   db = "poisoned for this run only";
                   return broke;"#,
                &*host,
            ))
            .await
            .unwrap();
        assert_eq!(
            out,
            json!(["call", "invoke", "makeDb", "define"]),
            "strict mode throws"
        );
        // The next run on the same isolate gets its own `db` and a working op —
        // the handle is the factory's answer to *this* run's token, so what the
        // last body did to its own binding went with it.
        let out = rt
            .run(with_host("return await db.books.rows();", &*host))
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
        // The op handle exists — that is the one surface — but a body cannot
        // name its own run to it: the token is the `db` factory's argument and
        // stays in the handle's closure, so what a body holds is the handle.
        let out = rt
            .run(call("return typeof __scTok === 'undefined';"))
            .await
            .unwrap();
        assert_eq!(out, json!(true), "the run token is in the guest's scope");
        // The factory is a global like the op, so a body can ask it for a handle
        // over any token it likes — and get one that names nothing, which is
        // what the 128 random bits are for.
        let err = rt
            .run(call(
                r#"return await __scMakeDb("00000000000000000000000000000000").books.rows();"#,
            ))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("has already finished"), "{err}");
        // And the same by hand, at the op: a token that is not this run's names
        // nothing, because the table is keyed by those bits precisely so that a
        // body cannot reach another resident run's host by guessing at it.
        let host = FakeHost::rows(json!([{ "id": 1 }]));
        let err = rt
            .run(with_host(
                r#"return await __scDbCall("00000000000000000000000000000000",
                     { op: "select", table: "books" });"#,
                &*host,
            ))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("has already finished"), "{err}");
        assert!(host.plans().is_empty(), "nothing reached the host");
    }

    #[tokio::test]
    async fn a_returned_promise_is_awaited_rather_than_refused() {
        // The inversion this milestone turns on: the run wrapper used to refuse
        // a Promise because there was nothing in the sandbox to await it with.
        let rt = CodeRuntime::new();
        assert_eq!(
            rt.run(call("return (async () => 1)();")).await.unwrap(),
            json!(1)
        );
        // And `await` is legal at the top level of a body, because the body is
        // the inside of an async function.
        assert_eq!(
            rt.run(call("const n = await Promise.resolve(2); return n + 1;"))
                .await
                .unwrap(),
            json!(3)
        );
        // A rejection is the run's failure, with the thrown message.
        let err = rt
            .run(call("await Promise.reject(new Error('nope'));"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("nope"), "{err}");
    }

    #[tokio::test]
    async fn a_forgotten_await_is_one_named_error_rather_than_a_wrong_answer() {
        // Each of these is what a missing `await` used to look like: `{}` out of
        // JSON.stringify, `[object Promise]` out of a template string, and a bare
        // "is not iterable" TypeError out of a `for … of`. All three now name the
        // mistake, and name it at the point the body made it.
        let host = FakeHost::rows(json!([{ "id": 1 }]));
        let rt = CodeRuntime::new();
        for body in [
            "return JSON.stringify(db.books.rows());",
            "return `${db.books.count()}`;",
            "for (const r of db.books.rows()) {} return null;",
            "return db.books.count() + 1;",
        ] {
            let err = rt
                .run(with_host(body, &*host))
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("was not awaited"), "{body}: {err}");
        }
        // Returning one *is* awaiting it, though — `return db.books.rows()` from
        // an async body resolves to the rows, which is what its author meant.
        // The error is for the uses that would otherwise answer something else.
        assert_eq!(
            rt.run(with_host("return db.books.rows();", &*host))
                .await
                .unwrap(),
            json!([{ "id": 1 }])
        );
        // The chain itself is untouched: only a terminal answers a promise, so
        // `await` belongs at the end and nowhere inside.
        let out = rt
            .run(with_host(
                "return await db.books.where({ id: 1 }).orderBy('id').rows();",
                &*host,
            ))
            .await
            .unwrap();
        assert_eq!(out, json!([{ "id": 1 }]));
    }

    #[tokio::test]
    async fn promise_all_issues_its_queries_without_waiting_for_each() {
        // `Promise.all` is the spelling §1 promises works, and it is only
        // meaningful because a host call suspends rather than blocks: both plans
        // reach the host before either answer comes back.
        let host = FakeHost::new(|plan| {
            Ok(match plan["table"].as_str() {
                Some("books") => json!([{ "id": 1 }]),
                _ => json!([{ "id": 2 }]),
            })
        });
        let out = CodeRuntime::new()
            .run(with_host(
                r#"const [books, shelves] = await Promise.all([
                     db.books.rows(),
                     db.shelves.rows(),
                   ]);
                   return { books: books, shelves: shelves };"#,
                &*host,
            ))
            .await
            .unwrap();
        assert_eq!(out["books"], json!([{ "id": 1 }]));
        assert_eq!(out["shelves"], json!([{ "id": 2 }]));
        assert_eq!(host.plans().len(), 2);
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
    async fn an_expired_run_leaves_nothing_behind_for_the_next_one() {
        // A run abandoned at its wall clock is suspended *inside* the isolate,
        // and its continuation would otherwise resume during the next run's event
        // loop — reaching that run's host, spending that run's call budget, and
        // issuing queries in a stranger's name. So it is unwound before the
        // worker takes another job.
        let hung = Arc::new(FakeHost {
            plans: Mutex::new(Vec::new()),
            answer: Box::new(|_| Ok(json!([]))),
            delay: Some(Duration::from_secs(30)),
            calls: AtomicU32::new(0),
        });
        let rt = CodeRuntime::with_workers(1);
        let mut expiring = with_host(
            "await db.a.rows(); await db.a.rows(); return 'never';",
            &*hung,
        );
        expiring.timeout = Some(Duration::from_millis(150));
        let err = rt.run(expiring).await.unwrap_err().to_string();
        assert!(err.contains("time limit"), "{err}");

        // The next run on the same isolate is whole: its own host, and its own
        // budget, neither spent by what the last one left behind.
        let next = FakeHost::rows(json!([{ "id": 1 }]));
        let mut c = with_host(
            "let n = 0; for (let i = 0; i < 3; i++) n += (await db.b.rows()).length; return n;",
            &*next,
        );
        c.max_calls = 3;
        assert_eq!(rt.run(c).await.unwrap(), json!(3));
        assert_eq!(
            next.plans().len(),
            3,
            "the abandoned run spent none of this one's budget"
        );
        assert_eq!(
            hung.calls.load(Ordering::SeqCst),
            1,
            "and reached none of its host either"
        );
    }

    #[tokio::test]
    async fn a_body_suspended_on_a_promise_that_never_settles_loses_the_run_not_the_worker() {
        // The one shape the isolate's own two bounds cannot see: no JavaScript is
        // running, so the watchdog has nothing to terminate, and no host call is
        // in flight, so the deadline check is never reached. The **event loop**
        // sees it — it empties with the run's promise still pending — so the run
        // fails at once, by name, rather than holding the worker to its deadline.
        let rt = CodeRuntime::with_workers(1);
        let mut c = call("await new Promise(() => {}); return 1;");
        c.timeout = Some(Duration::from_secs(30));
        let started = Instant::now();
        let err = rt.run(c).await.unwrap_err().to_string();
        assert!(
            err.contains("awaited something that never happens"),
            "{err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "it waited it out"
        );
        // The same worker serves the next run normally.
        assert_eq!(rt.run(call("return 1 + 1;")).await.unwrap(), json!(2));
    }

    #[tokio::test]
    async fn a_run_no_longer_waits_for_a_worker_and_a_hung_call_still_ends_at_its_deadline() {
        // One worker, already serving a body whose host call will not come back
        // for half a minute. Before this milestone that worker was *held* — a run
        // cost a thread — and a second run could not start until the first
        // finished, which is the deadlock a body whose write fires a second
        // trigger fell into. Now the first run is a pending promise, so the
        // second is admitted at once and answers in microseconds.
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
                let mut c = with_host("return await db.a.rows();", &*host);
                c.timeout = Some(Duration::from_secs(2));
                rt.run(c).await
            })
        };
        // Let the first run reach its host call.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut queued = call("return 1;");
        queued.timeout = Some(Duration::from_millis(200));
        let started = Instant::now();
        assert_eq!(
            rt.run(queued).await.unwrap(),
            json!(1),
            "a run behind a suspended one is not a run behind a blocked thread"
        );
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "it waited for the resident run: {:?}",
            started.elapsed()
        );

        // And the run that *is* suspended is still bounded, rather than holding
        // its caller for as long as the database takes.
        let err = blocked
            .await
            .unwrap()
            .expect_err("a host call that never returns is not a run without a bound")
            .to_string();
        assert!(err.contains("time limit"), "{err}");
    }

    /// A host that answers after `delay`, counting how many calls are in flight
    /// at the same time and remembering the most there ever were.
    ///
    /// The number is the milestone: it is what tells "hundreds of runs served at
    /// once" apart from "hundreds of runs served one after another quickly".
    struct CountingHost {
        delay: Duration,
        live: Mutex<usize>,
        peak: AtomicUsize,
        answer: Json,
    }

    impl CountingHost {
        fn new(delay: Duration, answer: Json) -> Arc<CountingHost> {
            Arc::new(CountingHost {
                delay,
                live: Mutex::new(0),
                peak: AtomicUsize::new(0),
                answer,
            })
        }

        fn peak(&self) -> usize {
            self.peak.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl CodeHost for CountingHost {
        async fn call(&self, _request: Json) -> Result<Json> {
            {
                let mut live = self.live.lock().unwrap();
                *live += 1;
                self.peak.fetch_max(*live, Ordering::SeqCst);
            }
            tokio::time::sleep(self.delay).await;
            *self.live.lock().unwrap() -= 1;
            Ok(self.answer.clone())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn one_worker_serves_two_hundred_runs_at_once() {
        // The milestone, on a **single** isolate: 200 bodies, each a read and a
        // write, against a host that takes 50 ms per call. Serialised that is
        // 200 × 2 × 50 ms = 20 seconds; concurrent it is two round trips and the
        // isolate's own overhead. The peak in-flight count is the assertion that
        // matters — a fast wall clock could be a fast host, but a hundred calls
        // outstanding at one time can only be concurrency.
        const RUNS: usize = 200;
        let host = CountingHost::new(Duration::from_millis(50), json!([{ "id": 1 }]));
        let rt = Arc::new(CodeRuntime::with_workers(1));
        let started = Instant::now();
        let mut runs = Vec::new();
        for n in 0..RUNS {
            let (rt, host) = (Arc::clone(&rt), Arc::clone(&host));
            runs.push(tokio::spawn(async move {
                let mut c = with_host(
                    "const rows = await db.books.where({ id: 1 }).rows();
                     await db.log.insert({ n: rows.length });
                     return rows.length;",
                    &*host,
                );
                c.bindings.insert("which".into(), json!(n));
                c.timeout = Some(Duration::from_secs(20));
                rt.run(c).await
            }));
        }
        for run in runs {
            assert_eq!(run.await.unwrap().unwrap(), json!(1));
        }
        let elapsed = started.elapsed();
        assert!(
            host.peak() >= 100,
            "the runs were served one at a time: {} in flight at the peak",
            host.peak()
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "200 runs of two 50 ms calls took {elapsed:?}, which is the shape of a queue"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resident_runs_do_not_leak_into_each_other() {
        // Fifty runs on one isolate, each with its own bindings and its own
        // budget. The token is what keeps them apart: a run's `db` closes over
        // it, the host it reaches is the one its own table entry holds, and the
        // calls it spends come out of its own count.
        const RUNS: u32 = 50;
        let host = CountingHost::new(Duration::from_millis(20), json!([{ "id": 1 }]));
        let rt = Arc::new(CodeRuntime::with_workers(1));
        let mut runs = Vec::new();
        for n in 0..RUNS {
            let (rt, host) = (Arc::clone(&rt), Arc::clone(&host));
            runs.push(tokio::spawn(async move {
                // One run in five spends its whole budget in a loop; the rest
                // ask for exactly what they were given and must get it back.
                let greedy = n % 5 == 0;
                let code = if greedy {
                    "for (let i = 0; i < 100; i++) await db.books.rows(); return mine;"
                } else {
                    "const rows = await db.books.rows();
                     if (rows.length !== 1) throw new Error('wrong host');
                     return mine;"
                };
                let mut c = with_host(code, &*host);
                c.bindings.insert("mine".into(), json!(n));
                c.max_calls = 3;
                c.timeout = Some(Duration::from_secs(20));
                (n, greedy, rt.run(c).await)
            }));
        }
        for run in runs {
            let (n, greedy, outcome) = run.await.unwrap();
            if greedy {
                let err = outcome.unwrap_err().to_string();
                assert!(err.contains("more than 3 database calls"), "{n}: {err}");
            } else {
                assert_eq!(
                    outcome.unwrap(),
                    json!(n),
                    "run {n} answered another run's bindings"
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_body_whose_write_fires_a_second_body_completes_on_one_worker() {
        // The deadlock this milestone removes, and the test that would have
        // failed before it. The outer body's `.insert()` raises a table event; a
        // `run_js_code` trigger on that event is a *second* code body, and it
        // needs the runtime while the first is still holding its place. With one
        // worker and one run per worker that could never finish — the pool size
        // was `1 + max nesting depth × concurrency`, which is not a number
        // anyone can pick. With runs resident it is a pending promise inside a
        // pending promise, and one isolate is enough.
        struct NestingHost {
            rt: Arc<CodeRuntime>,
            depth: AtomicU32,
        }

        #[async_trait]
        impl CodeHost for NestingHost {
            async fn call(&self, request: Json) -> Result<Json> {
                if request["op"] == json!("insert") && self.depth.fetch_add(1, Ordering::SeqCst) < 2
                {
                    // The trigger the write fired: another code body, on the
                    // same pool, while this one is suspended waiting for us.
                    let inner = self.rt.run(CodeCall {
                        code: "return await db.audit.insert({ what: 'nested' });".to_owned(),
                        host: Some(self),
                        timeout: Some(Duration::from_secs(5)),
                        ..CodeCall::default()
                    });
                    inner.await?;
                }
                Ok(json!({ "id": 1 }))
            }
        }

        let rt = Arc::new(CodeRuntime::with_workers(1));
        let host = Arc::new(NestingHost {
            rt: Arc::clone(&rt),
            depth: AtomicU32::new(0),
        });
        let out = rt
            .run(with_host(
                "return await db.invoices.insert({ paid: false });",
                &*host,
            ))
            .await
            .unwrap();
        assert_eq!(out, json!({ "id": 1 }));
        assert_eq!(
            host.depth.load(Ordering::SeqCst),
            3,
            "the nested bodies did not all run"
        );
    }

    #[tokio::test]
    async fn a_finished_run_gives_its_place_back() {
        // The occupancy count is what the dispatcher chooses between workers by,
        // and what the admission bound is measured against. A run that ends the
        // ordinary way — a completion op, from inside a poll of the event loop —
        // has to decrement it just as a reaped or terminated one does, or the
        // count only ever grows and both of those decisions rot.
        let host = FakeHost::rows(json!([{ "id": 1 }]));
        let rt = CodeRuntime::with_workers(1);
        let place = || rt.workers[0].outstanding.load(Ordering::SeqCst);
        for _ in 0..5 {
            rt.run(with_host("return await db.books.rows();", &*host))
                .await
                .unwrap();
        }
        assert_eq!(place(), 0, "five ordinary runs");
        // A body that throws is a run that ended too.
        rt.run(with_host("throw new Error('no');", &*host))
            .await
            .unwrap_err();
        assert_eq!(place(), 0, "a body that threw");
        // So is one refused before its script was ever built.
        let mut bad = with_host("return 1;", &*host);
        bad.bindings.insert("db".into(), json!(1));
        rt.run(bad).await.unwrap_err();
        assert_eq!(place(), 0, "a run refused before it started");
        // And so is one the watchdog took out.
        let mut spinning = call("while (true) {}");
        spinning.timeout = Some(Duration::from_millis(100));
        rt.run(spinning).await.unwrap_err();
        assert_eq!(place(), 0, "a run that was terminated");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_freed_place_is_filled_while_its_neighbour_is_still_in_flight() {
        // Two places, one long body and ten short ones. A run finishes *inside* a
        // poll of the event loop, and that poll does not return while the long
        // body is still in flight — so unless finishing a run says so, the second
        // place would stay shut until the long body finished and emptied the loop,
        // and every short run would be queued behind a body it has nothing to do
        // with. Which is why the assertion is the interleaving and not the clock:
        // all ten shorts answer *while* the long one is still going.
        let host = CountingHost::new(Duration::from_millis(40), json!([]));
        let rt = Arc::new(CodeRuntime::with_workers_and_inflight(1, 2));
        let long = {
            let (rt, host) = (Arc::clone(&rt), Arc::clone(&host));
            tokio::spawn(async move {
                let mut c = with_host(
                    "for (let i = 0; i < 20; i++) await db.books.rows(); return 'long';",
                    &*host,
                );
                c.timeout = Some(Duration::from_secs(20));
                rt.run(c).await
            })
        };
        // Let the long body take one of the two places.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut shorts = Vec::new();
        for _ in 0..10 {
            let (rt, host) = (Arc::clone(&rt), Arc::clone(&host));
            shorts.push(tokio::spawn(async move {
                let mut c = with_host("return (await db.books.rows()).length;", &*host);
                c.timeout = Some(Duration::from_secs(20));
                rt.run(c).await
            }));
        }
        for short in shorts {
            assert_eq!(short.await.unwrap().unwrap(), json!(0));
        }
        assert!(
            !long.is_finished(),
            "the ten short runs waited for the long one to give the place back"
        );
        assert_eq!(long.await.unwrap().unwrap(), json!("long"));
        assert!(
            host.peak() <= 2,
            "the bound admitted more than it was given: {}",
            host.peak()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_occupancy_bound_queues_the_overflow_rather_than_refusing_it() {
        // A worker admits `max_inflight` runs; the rest wait in the channel, and
        // their wall clock is running while they do — which is why the queue is a
        // queue and not a second, hidden, unbounded resource.
        let host = CountingHost::new(Duration::from_millis(50), json!([]));
        let rt = Arc::new(CodeRuntime::with_workers_and_inflight(1, 4));
        let mut runs = Vec::new();
        for _ in 0..12 {
            let (rt, host) = (Arc::clone(&rt), Arc::clone(&host));
            runs.push(tokio::spawn(async move {
                let mut c = with_host("return (await db.books.rows()).length;", &*host);
                c.timeout = Some(Duration::from_secs(10));
                rt.run(c).await
            }));
        }
        for run in runs {
            assert_eq!(run.await.unwrap().unwrap(), json!(0));
        }
        assert!(
            host.peak() <= 4,
            "the bound admitted more than it was given: {}",
            host.peak()
        );
    }

    /// A host that records what it was asked to do and holds every call for
    /// `delay`, so that a run can be left provably suspended — with a write
    /// behind it — while something else happens on its isolate.
    struct WriteCounter {
        writes: AtomicU32,
        delay: Duration,
    }

    #[async_trait]
    impl CodeHost for WriteCounter {
        async fn call(&self, request: Json) -> Result<Json> {
            if request["op"] == json!("insert") {
                self.writes.fetch_add(1, Ordering::SeqCst);
            }
            tokio::time::sleep(self.delay).await;
            Ok(json!({ "id": 1 }))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_runaway_is_blamed_and_its_co_residents_are_never_re_run() {
        // The termination is the blunt instrument: it stops the isolate, not the
        // body that overran it. So three runs share one isolate and the
        // difference between them is the whole of this phase.
        //
        //   W  has written, and is suspended waiting for the host. Stopping it
        //      costs its caller an error; **re-running** it would cost a second
        //      row, so it is answered rather than retried.
        //   C  has made no host call at all — it is resident because its own
        //      completion is a microtask the next poll will drain — so there is
        //      provably nothing to repeat, and it goes back in the queue.
        //   B  is the body that never yields, and the one the message is for.
        let host = Arc::new(WriteCounter {
            writes: AtomicU32::new(0),
            delay: Duration::from_millis(800),
        });
        let rt = Arc::new(CodeRuntime::with_workers(1));

        let writer = {
            let (rt, host) = (Arc::clone(&rt), Arc::clone(&host));
            tokio::spawn(async move {
                let mut c = with_host("await db.log.insert({ n: 1 }); return 'W';", &*host);
                c.timeout = Some(Duration::from_secs(20));
                rt.run(c).await
            })
        };
        // Long enough for W to be resident and suspended in its insert.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Both submitted from one task, so both are in the worker's channel
        // before it wakes: it admits C, and admits B from the same burst without
        // a poll in between — which is why C is still resident, and still
        // callless, when B stops the isolate.
        let mut spinner = call("while (true) {}");
        spinner.timeout = Some(Duration::from_secs(20));
        let (innocent, guilty) = tokio::join!(rt.run(call("return 'C';")), rt.run(spinner));

        let blamed = guilty
            .expect_err("a body that never yields is not a body that ran")
            .to_string();
        assert!(
            blamed.contains("without awaiting anything"),
            "the runaway was not the one blamed: {blamed}"
        );
        assert_eq!(
            innocent.expect("a co-resident with no host call is owed another go"),
            json!("C"),
            "the callless co-resident was not re-run"
        );
        let bystander = writer
            .await
            .unwrap()
            .expect_err("the writer went down with the isolate")
            .to_string();
        assert!(
            bystander.contains("another code body on the same isolate"),
            "the writer was told it was its own fault: {bystander}"
        );
        assert!(
            bystander.contains("re-running it could repeat"),
            "the message does not say why it was not retried: {bystander}"
        );
        assert_eq!(
            host.writes.load(Ordering::SeqCst),
            1,
            "the body that had already written was run a second time"
        );
        // The isolate is whole afterwards.
        assert_eq!(rt.run(call("return 1 + 1;")).await.unwrap(), json!(2));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_body_that_loops_on_microtasks_loses_its_run_and_not_its_worker() {
        // The shape no op can see: a body that yields, but only to the microtask
        // queue, so nothing marks a resumption and nothing ever returns to Rust.
        // The mark left standing from the run's own admission is what keeps the
        // watchdog on it.
        let rt = CodeRuntime::with_workers(1);
        let mut c = call("for (;;) { await null; }");
        c.timeout = Some(Duration::from_secs(20));
        let started = Instant::now();
        let err = rt.run(c).await.unwrap_err().to_string();
        assert!(err.contains("without awaiting anything"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the worker was held to the wall clock: {:?}",
            started.elapsed()
        );
        assert_eq!(rt.run(call("return 1 + 1;")).await.unwrap(), json!(2));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_heap_bound_stops_admitting_rather_than_the_process() {
        // A body that keeps everything it reads, on an isolate given 24 MB to
        // keep it in. V8's own answer to a heap limit is to abort the process;
        // this is the answer instead — the near-heap-limit callback buys a grace
        // and stops admission, and a body that fills even the grace is stopped
        // the way any other runaway is.
        //
        // The body awaits between allocations, so its JS slice is fresh every
        // time round: what stops it can only be the heap.
        let host = FakeHost::rows(json!([{ "id": 1 }]));
        let rt = CodeRuntime::build(1, 8, 24 * 1024 * 1024);
        let mut hog = with_host(
            "const kept = [];
             for (let i = 0; i < 100; i++) {
               await db.books.rows();
               kept.push(new Array(200000).fill('x'));
             }
             return kept.length;",
            &*host,
        );
        hog.timeout = Some(Duration::from_secs(30));
        hog.max_calls = 1000;
        let err = rt.run(hog).await.unwrap_err().to_string();
        assert!(
            err.contains("more memory than the JavaScript engine has"),
            "{err}"
        );
        // And the isolate is still serving: the process is here to be asked.
        assert_eq!(
            rt.run(with_host("return (await db.books.rows()).length;", &*host))
                .await
                .unwrap(),
            json!(1)
        );
    }

    /// The body a run of `code` would be compiled from.
    fn definition_of(code: &str, bindings: &[(&str, Json)]) -> RunScripts {
        let mut run = CodeRun {
            code: code.to_owned(),
            bindings: BTreeMap::new(),
            host: None,
            fetch: None,
            timeout: Duration::from_secs(1),
            max_calls: 10,
            max_fetches: 10,
            started: None,
        };
        for (name, value) in bindings {
            run.bindings.insert((*name).to_owned(), value.clone());
        }
        build_run_scripts(&run).unwrap()
    }

    #[test]
    fn a_body_travels_only_on_a_miss() {
        // The hot path's whole claim: once the isolate has the body, what a run
        // sends is a token, a key and its own bindings — and the source is not
        // in it anywhere.
        let mut cache = BodyCache::new();
        let scripts = definition_of("return secret + 1;", &[("secret", json!(41))]);
        let key = BodyCache::key(&scripts.definition);
        assert!(
            !cache.holds(key, &scripts.definition),
            "nothing is warm yet"
        );

        let miss = build_script(&scripts, "aa", key, false);
        assert!(miss.contains("__scDefine"), "{miss}");
        assert!(miss.contains("return secret + 1;"), "{miss}");
        assert!(miss.contains("__scInvoke"), "{miss}");
        assert!(cache.store(key, scripts.definition.clone()).is_empty());

        assert!(
            cache.holds(key, &scripts.definition),
            "the isolate has it now"
        );
        let hit = build_script(&scripts, "bb", key, true);
        assert!(!hit.contains("__scDefine"), "{hit}");
        assert!(
            !hit.contains("return secret + 1;"),
            "the source travelled: {hit}"
        );
        // The bindings still do, because they are what differs per run.
        assert!(hit.contains("41"), "{hit}");
        assert!(hit.contains("bb"), "{hit}");

        // The *same* code with different binding names is a different body, and
        // gets its own key: the `const`s are part of what was compiled.
        let renamed = definition_of("return secret + 1;", &[("other", json!(41))]);
        assert!(!cache.holds(BodyCache::key(&renamed.definition), &renamed.definition));
    }

    #[test]
    fn a_key_collision_costs_a_compile_and_never_the_wrong_body() {
        // The key is a cheap hash, so it is the *definition* beside it that
        // decides a hit. Two sources under one key is a miss, and defining
        // replaces what the isolate had rather than shadowing it.
        let mut cache = BodyCache::new();
        let first = definition_of("return 1;", &[]);
        let key = BodyCache::key(&first.definition);
        cache.store(key, first.definition.clone());
        let second = definition_of("return 2;", &[]);
        assert!(
            !cache.holds(key, &second.definition),
            "a different body under the same key must not be served from it"
        );
        cache.store(key, second.definition.clone());
        assert!(cache.holds(key, &second.definition));
        assert!(!cache.holds(key, &first.definition));
    }

    #[test]
    fn the_cache_drops_the_least_recently_run_body() {
        // Bounded, because a server with many distinct bodies must not
        // accumulate compiled functions in an isolate for ever.
        let mut cache = BodyCache::new();
        let mut keys = Vec::new();
        for n in 0..BODY_CACHE_CAPACITY {
            let scripts = definition_of(&format!("return {n};"), &[]);
            let key = BodyCache::key(&scripts.definition);
            assert!(
                cache.store(key, scripts.definition).is_empty(),
                "no eviction yet"
            );
            keys.push(key);
        }
        // Touching the oldest is what makes the *second* oldest the one to go.
        let oldest = definition_of("return 0;", &[]);
        assert!(cache.holds(keys[0], &oldest.definition));

        let extra = definition_of("return 'one too many';", &[]);
        let evicted = cache.store(BodyCache::key(&extra.definition), extra.definition);
        assert_eq!(evicted, vec![keys[1]], "the least recently run body goes");
        assert!(
            cache.holds(keys[0], &oldest.definition),
            "the touched one stays"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_body_the_isolate_has_forgotten_is_compiled_again() {
        // The eviction path end to end: one body, then more distinct bodies than
        // the cache holds, then the first one again — which the isolate has
        // forgotten and must be sent afresh. A cache that lied here would be a
        // run that never answers.
        let rt = CodeRuntime::with_workers(1);
        assert_eq!(
            rt.run(call("return 'first';")).await.unwrap(),
            json!("first")
        );
        for n in 0..=BODY_CACHE_CAPACITY {
            assert_eq!(
                rt.run(call(&format!("return {n};"))).await.unwrap(),
                json!(n),
                "distinct body {n}"
            );
        }
        assert_eq!(
            rt.run(call("return 'first';")).await.unwrap(),
            json!("first")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn one_compiled_body_hands_each_run_its_own_db() {
        // Decision 5, now kept by the factory rather than by recompiling the
        // prelude: the body is compiled once, and `db` is an argument built per
        // run from that run's token. So a body that mutates its own `db` — the
        // only thing it can do to it — takes nothing with it into the next run
        // of the same compiled function, and each run reaches its own host.
        let mine = FakeHost::rows(json!([{ "id": 1 }]));
        let theirs = FakeHost::rows(json!([{ "id": 2 }, { "id": 3 }]));
        let rt = CodeRuntime::with_workers(1);
        let body = "const rows = await db.books.rows();
                    db = 'wrecked for this run';
                    return rows.length;";
        assert_eq!(rt.run(with_host(body, &*mine)).await.unwrap(), json!(1));
        // The same compiled body, a second run, a different host of its own.
        assert_eq!(rt.run(with_host(body, &*theirs)).await.unwrap(), json!(2));
        assert_eq!(mine.plans().len(), 1);
        assert_eq!(theirs.plans().len(), 1);
    }

    /// Phase 4's benchmark: how many code bodies **one isolate** runs per
    /// second.
    ///
    /// One body, run over and over against a host that answers at once, so what
    /// is timed is the runtime's own overhead — what a body costs to compile, to
    /// admit, and to carry through the bridge and back — rather than a database.
    /// It is what the [`BodyCache`] and `__scMakeDb` were measured with: about
    /// 2,000 runs a second before them and about 44,000 after, on the machine
    /// they were written on. The assertion is deliberately loose, because a
    /// benchmark that fails on a busy machine is a flaky test; the number it
    /// *prints* is what the CHANGELOG records.
    #[tokio::test(flavor = "multi_thread")]
    async fn throughput_of_one_isolate() {
        const RUNS: usize = 2000;
        const CONCURRENCY: usize = 32;
        let host = FakeHost::rows(json!([{ "id": 1, "title": "Dune" }]));
        let rt = CodeRuntime::with_workers(1);
        let body = r#"const rows = await db.books
                        .where({ id: payload.id })
                        .limit(1)
                        .rows();
                      return rows.length;"#;
        let one = |n: usize| {
            let mut c = with_host(body, &*host);
            c.bindings.insert("payload".into(), json!({ "id": n }));
            c.timeout = Some(Duration::from_secs(30));
            rt.run(c)
        };
        // Warm whatever there is to warm, so the number is the steady state.
        for n in 0..CONCURRENCY {
            assert_eq!(one(n).await.unwrap(), json!(1));
        }
        let started = Instant::now();
        for batch in 0..(RUNS / CONCURRENCY) {
            let outcomes = deno_core::futures::future::join_all(
                (0..CONCURRENCY).map(|n| one(batch * CONCURRENCY + n)),
            )
            .await;
            for outcome in outcomes {
                assert_eq!(outcome.unwrap(), json!(1));
            }
        }
        let elapsed = started.elapsed();
        let per_second = RUNS as f64 / elapsed.as_secs_f64();
        println!(
            "one isolate: {RUNS} runs in {elapsed:?} = {per_second:.0} runs/second \
             ({:.2} ms each)",
            elapsed.as_secs_f64() * 1000.0 / RUNS as f64
        );
        assert!(per_second > 50.0, "{per_second:.0} runs/second");
    }

    // -----------------------------------------------------------------------
    // `fetch`: the second host surface
    // -----------------------------------------------------------------------

    type Answered = Box<dyn Fn(&Json) -> Result<Json> + Send + Sync>;

    /// A network that records what it was asked to send and answers from a
    /// closure — [`FakeHost`]'s counterpart, and the proof that the fetch seam
    /// is JSON like the other one: no socket is opened anywhere in these tests.
    struct FakeNet {
        sent: Mutex<Vec<Json>>,
        answer: Answered,
        delay: Option<Duration>,
        /// Requests in flight, and the most there have ever been at once —
        /// which is how "these two went out together" is asserted.
        live: AtomicU32,
        peak: AtomicU32,
    }

    impl FakeNet {
        fn new(answer: impl Fn(&Json) -> Result<Json> + Send + Sync + 'static) -> Arc<FakeNet> {
            Arc::new(FakeNet {
                sent: Mutex::new(Vec::new()),
                answer: Box::new(answer),
                delay: None,
                live: AtomicU32::new(0),
                peak: AtomicU32::new(0),
            })
        }

        /// The everyday answer: 200, with this JSON as the body.
        fn ok(body: Json) -> Arc<FakeNet> {
            FakeNet::new(move |_| {
                Ok(json!({
                    "status": 200,
                    "status_text": "OK",
                    "url": "https://api.example.com/thing",
                    "redirected": false,
                    "headers": [["content-type", "application/json"]],
                    "text": body.to_string(),
                }))
            })
        }

        fn sent(&self) -> Vec<Json> {
            self.sent.lock().unwrap().clone()
        }

        fn peak(&self) -> u32 {
            self.peak.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl FetchHost for FakeNet {
        async fn fetch(&self, request: Json) -> Result<Json> {
            self.sent.lock().unwrap().push(request.clone());
            let now = self.live.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            self.live.fetch_sub(1, Ordering::SeqCst);
            (self.answer)(&request)
        }
    }

    /// A call with the network, and optionally the tables.
    fn with_net<'a>(code: &str, net: &'a dyn FetchHost) -> CodeCall<'a> {
        CodeCall {
            code: code.to_owned(),
            fetch: Some(net),
            ..CodeCall::default()
        }
    }

    #[tokio::test]
    async fn a_body_fetches_and_reads_the_response() {
        let net = FakeNet::ok(json!({ "id": 7, "name": "Ada" }));
        let rt = CodeRuntime::new();
        let out = rt
            .run(with_net(
                r#"const res = await fetch("https://api.example.com/thing");
                   return {
                     ok: res.ok,
                     status: res.status,
                     statusText: res.statusText,
                     url: res.url,
                     type: res.headers.get("content-type"),
                     missing: res.headers.get("x-nope"),
                     body: await res.json(),
                   };"#,
                &*net,
            ))
            .await
            .unwrap();
        assert_eq!(
            out,
            json!({
                "ok": true, "status": 200, "statusText": "OK",
                "url": "https://api.example.com/thing",
                "type": "application/json", "missing": Json::Null,
                "body": { "id": 7, "name": "Ada" },
            })
        );
        // And what went out is one plain JSON request: a GET, no body.
        let sent = net.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["url"], json!("https://api.example.com/thing"));
        assert_eq!(sent[0]["method"], json!("GET"));
        assert_eq!(sent[0]["body"], Json::Null);
    }

    #[tokio::test]
    async fn a_post_sends_the_headers_and_body_the_body_built() {
        let net = FakeNet::ok(json!({ "ok": true }));
        let rt = CodeRuntime::new();
        rt.run(with_net(
            r#"await fetch("https://api.example.com/hooks", {
                 method: "post",
                 headers: { "Authorization": "Bearer t0ken" },
                 body: { id: 1, title: "Orlando" },
               });
               const h = new Headers([["x-a", "1"]]);
               h.append("x-a", "2");
               h.set("content-type", "text/csv");
               await fetch("https://api.example.com/csv", { method: "PUT", headers: h, body: "a,b" });
               return null;"#,
            &*net,
        ))
        .await
        .unwrap();
        let sent = net.sent();
        // The method is upper-cased, an object body is JSON (which the web
        // would have sent as `[object Object]`), and the content type it
        // implies is filled in without overwriting one the body set.
        assert_eq!(sent[0]["method"], json!("POST"));
        assert_eq!(sent[0]["body"], json!(r#"{"id":1,"title":"Orlando"}"#));
        assert_eq!(sent[0]["body_base64"], json!(false));
        let headers = |plan: &Json| -> Vec<(String, String)> {
            plan["headers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|pair| {
                    (
                        pair[0].as_str().unwrap().to_owned(),
                        pair[1].as_str().unwrap().to_owned(),
                    )
                })
                .collect()
        };
        assert_eq!(
            headers(&sent[0]),
            vec![
                ("authorization".to_owned(), "Bearer t0ken".to_owned()),
                ("content-type".to_owned(), "application/json".to_owned()),
            ]
        );
        // A repeated header stays repeated on the wire; `set` replaced the one
        // the string body would otherwise have implied.
        assert_eq!(
            headers(&sent[1]),
            vec![
                ("x-a".to_owned(), "1".to_owned()),
                ("x-a".to_owned(), "2".to_owned()),
                ("content-type".to_owned(), "text/csv".to_owned()),
            ]
        );
        assert_eq!(sent[1]["body"], json!("a,b"));
    }

    #[tokio::test]
    async fn a_status_the_server_did_not_like_is_not_an_error() {
        // The web API's rule, and the one people are surprised by in the other
        // direction: only a transport failure rejects. A 404 is an answer.
        let net = FakeNet::new(|_| {
            Ok(json!({
                "status": 404, "status_text": "Not Found",
                "url": "https://api.example.com/gone", "redirected": true,
                "headers": [], "text": "no such thing",
            }))
        });
        let rt = CodeRuntime::new();
        let out = rt
            .run(with_net(
                r#"const res = await fetch("https://api.example.com/gone");
                   return { ok: res.ok, status: res.status, redirected: res.redirected,
                            text: await res.text() };"#,
                &*net,
            ))
            .await
            .unwrap();
        assert_eq!(
            out,
            json!({ "ok": false, "status": 404, "redirected": true, "text": "no such thing" })
        );
    }

    #[tokio::test]
    async fn a_transport_failure_is_a_type_error_the_body_can_catch() {
        let net = FakeNet::new(|_| Err(Error::msg("connection refused")));
        let rt = CodeRuntime::new();
        let out = rt
            .run(with_net(
                r#"try {
                     await fetch("https://nowhere.invalid/");
                     return "no throw";
                   } catch (e) {
                     return { name: e.constructor.name, message: e.message };
                   }"#,
                &*net,
            ))
            .await
            .unwrap();
        assert_eq!(out["name"], json!("TypeError"), "{out}");
        assert!(
            out["message"]
                .as_str()
                .unwrap()
                .contains("connection refused"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn a_body_without_the_network_cannot_name_fetch() {
        // The capability is the parameter: no fetch host, no `fetch` in scope —
        // a ReferenceError naming it, exactly as `db` is for a pure body.
        let rt = CodeRuntime::new();
        let err = rt
            .run(call(r#"return await fetch("https://example.com/");"#))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("fetch is not defined"), "{err}");
        // A body with tables but no network is the same: one capability does
        // not carry the other.
        let host = FakeHost::rows(json!([]));
        let err = rt
            .run(with_host(
                r#"await db.books.rows(); return typeof fetch;"#,
                &*host,
            ))
            .await
            .unwrap();
        assert_eq!(err, json!("undefined"));
    }

    #[tokio::test]
    async fn the_fetch_budget_is_counted_apart_from_the_database_one() {
        let net = FakeNet::ok(json!({}));
        let host = FakeHost::rows(json!([{ "id": 1 }]));
        let rt = CodeRuntime::new();
        let mut c = CodeCall {
            code: r#"let sent = 0;
                     try {
                       for (let i = 0; i < 10; i++) { await fetch("https://x.test/" + i); sent++; }
                     } catch (e) {
                       // The database is still there: the two budgets are two.
                       const rows = await db.books.rows();
                       return { sent: sent, rows: rows.length, why: e.message };
                     }
                     return { sent: sent };"#
                .to_owned(),
            host: Some(&*host),
            fetch: Some(&*net),
            ..CodeCall::default()
        };
        c.max_fetches = 3;
        let out = rt.run(c).await.unwrap();
        assert_eq!(out["sent"], json!(3), "{out}");
        assert_eq!(out["rows"], json!(1), "{out}");
        assert!(
            out["why"]
                .as_str()
                .unwrap()
                .contains("more than 3 fetch requests"),
            "{out}"
        );
        assert_eq!(net.sent().len(), 3);
    }

    #[tokio::test]
    async fn a_forgotten_await_on_a_fetch_says_so() {
        let net = FakeNet::ok(json!({ "id": 1 }));
        let rt = CodeRuntime::new();
        let err = rt
            .run(with_net(
                r#"return { res: fetch("https://api.example.com/thing") };"#,
                &*net,
            ))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("this fetch was not awaited"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_fetches_in_one_promise_all_go_out_together() {
        // The bridge is one channel serving both surfaces, so a body that asks
        // for a query and two requests at once gets all three in flight — the
        // parallelism it wrote, not three round trips in a row.
        let net = Arc::new(FakeNet {
            sent: Mutex::new(Vec::new()),
            answer: Box::new(|_| {
                Ok(json!({
                    "status": 200, "status_text": "OK", "url": "https://x.test/",
                    "redirected": false, "headers": [], "text": "{}",
                }))
            }),
            delay: Some(Duration::from_millis(50)),
            live: AtomicU32::new(0),
            peak: AtomicU32::new(0),
        });
        let rt = CodeRuntime::new();
        let mut c = with_net(
            r#"const [a, b] = await Promise.all([
                 fetch("https://x.test/a"),
                 fetch("https://x.test/b"),
               ]);
               return [a.status, b.status];"#,
            &*net,
        );
        c.timeout = Some(Duration::from_secs(10));
        let started = Instant::now();
        assert_eq!(rt.run(c).await.unwrap(), json!([200, 200]));
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(140),
            "the two requests were serialised: {elapsed:?}"
        );
        assert!(net.peak() >= 2, "never two at once");
    }

    #[tokio::test]
    async fn a_request_may_not_outlive_the_run_that_made_it() {
        // Whatever the body asks for, what reaches the host is bounded by what
        // is left of the run's own wall clock — so a `fetch` cannot hold the
        // trigger's caller past the timeout it was configured with.
        let net = FakeNet::ok(json!({}));
        let rt = CodeRuntime::new();
        let mut c = with_net(
            r#"await fetch("https://x.test/a", { timeout_ms: 60000 });
               await fetch("https://x.test/b");
               return null;"#,
            &*net,
        );
        c.timeout = Some(Duration::from_millis(900));
        rt.run(c).await.unwrap();
        let sent = net.sent();
        let asked = sent[0]["timeout_ms"].as_u64().unwrap();
        assert!(asked <= 900, "a minute was allowed through: {asked}");
        // The default is likewise what is left rather than the ten seconds a
        // request gets when there is room for them.
        assert!(sent[1]["timeout_ms"].as_u64().unwrap() <= 900);
    }

    #[tokio::test]
    async fn bytes_survive_the_seam_in_both_directions() {
        // Not text: the seam is JSON, so a body that is not valid UTF-8 travels
        // base64 — and neither codec is the isolate's, because there is no
        // `TextEncoder` in the sandbox to lend one.
        let net = FakeNet::new(|request| {
            assert_eq!(request["body_base64"], json!(true), "{request}");
            // Echo what was sent, as bytes.
            Ok(json!({
                "status": 200, "status_text": "OK", "url": "https://x.test/",
                "redirected": false, "headers": [],
                "base64": request["body"],
            }))
        });
        let rt = CodeRuntime::new();
        let out = rt
            .run(with_net(
                r#"const sent = new Uint8Array([0, 159, 146, 150, 255]);
                   const res = await fetch("https://x.test/", { method: "POST", body: sent });
                   const got = await res.bytes();
                   return Array.from(got);"#,
                &*net,
            ))
            .await
            .unwrap();
        assert_eq!(out, json!([0, 159, 146, 150, 255]));
        // A text body comes back as bytes too, encoded as UTF-8 by the guest.
        let net = FakeNet::ok(json!("héllo"));
        let out = rt
            .run(with_net(
                r#"const res = await fetch("https://x.test/");
                   return Array.from(new Uint8Array(await res.arrayBuffer()));"#,
                &*net,
            ))
            .await
            .unwrap();
        // `"héllo"` as JSON text: the quotes are part of it, and é is two bytes.
        assert_eq!(out, json!([34, 104, 195, 169, 108, 108, 111, 34]));
    }

    #[tokio::test]
    async fn the_options_a_browser_needs_are_ignored_and_a_typo_is_not() {
        let net = FakeNet::ok(json!({}));
        let rt = CodeRuntime::new();
        // What a browser needs and a server does not is accepted, so code that
        // carries it runs unchanged.
        rt.run(with_net(
            r#"await fetch("https://x.test/", { mode: "cors", credentials: "omit", cache: "no-store" });
               return null;"#,
            &*net,
        ))
        .await
        .unwrap();
        // Everything else is refused **by name**: a misspelled `header` that
        // silently sent nothing is the failure this exists to prevent.
        for (code, expected) in [
            (
                r#"await fetch("https://x.test/", { header: { a: "b" } });"#,
                "`header` is not an option",
            ),
            (
                r#"await fetch("https://x.test/", { signal: {} });"#,
                "no `signal`",
            ),
            (
                r#"await fetch("https://x.test/", { redirect: "manual" });"#,
                "only follows redirects",
            ),
            (
                r#"await fetch("https://x.test/", { method: "GET", body: "x" });"#,
                "cannot carry a body",
            ),
            (r#"await fetch("");"#, "absolute http(s) URL"),
            (
                r#"await fetch("https://x.test/", { method: "TRACE" });"#,
                "is not a method",
            ),
        ] {
            let err = rt.run(with_net(code, &*net)).await.unwrap_err().to_string();
            assert!(err.contains(expected), "{code}\n{err}");
        }
    }

    #[tokio::test]
    async fn a_response_body_is_read_once_and_cloned_to_read_twice() {
        let net = FakeNet::ok(json!({ "n": 1 }));
        let rt = CodeRuntime::new();
        let out = rt
            .run(with_net(
                r#"const res = await fetch("https://x.test/");
                   const copy = res.clone();
                   const first = await res.json();
                   let second = null;
                   try { await res.text(); } catch (e) { second = e.message; }
                   return { first: first, used: res.bodyUsed, second: second,
                            clone: await copy.text() };"#,
                &*net,
            ))
            .await
            .unwrap();
        assert_eq!(out["first"], json!({ "n": 1 }));
        assert_eq!(out["used"], json!(true));
        assert!(
            out["second"]
                .as_str()
                .unwrap()
                .contains("already been read"),
            "{out}"
        );
        assert_eq!(out["clone"], json!(r#"{"n":1}"#));
    }
}
