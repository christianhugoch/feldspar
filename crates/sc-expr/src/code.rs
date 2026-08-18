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
//! The wall clock is enforced in three places, because no one of them is enough:
//! the guest is refused a host call once it is spent, the worker reaps a resident
//! run whose deadline has passed while it was suspended, and the caller stops
//! waiting shortly after it (see `CALLER_GRACE`). Only the last covers a run that
//! holds its caller without ever being admitted; only the middle one covers a run
//! whose single query never comes back. A watchdog on an isolate that is not
//! running cannot see any of them.
//!
//! The watchdog itself is one instrument shared by every resident run, armed at
//! the earliest JS deadline any of them still has — which is why it is owned by
//! the run table rather than by a run. Pointing it at the body that actually
//! overran, and sparing that body's co-residents, is the next milestone's work.

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
/// send one plan and return a **promise** of its result.
///
/// This is JavaScript rather than something generated from Rust on purpose
/// (decision 4): Rust sees plans, so adding a chain method touches no Rust, and
/// the same plans will serve the other guest languages. It is emitted **inside**
/// each run's function scope (decision 5) — a body that assigns to `db` poisons
/// nothing, because the next run builds its own.
///
/// Only the terminals are asynchronous. The chain itself
/// (`db.invoices.where(…).orderBy(…)`) is pure and synchronous: it builds a plan
/// and touches nothing, so `await` belongs at the end of a chain and nowhere
/// inside it. Each terminal answers a `DbPromise` — see [`SETUP`] — and the
/// `.then()` that unwraps a reply preserves that class through species, so a
/// forgotten `await` is a named error wherever the chain ended.
#[cfg(feature = "eval")]
pub(crate) const DB_PRELUDE: &str = r#"
const db = (function () {
  // Every plan carries the run's own token: with many runs resident on one
  // isolate, the token is what tells the host *whose* call this is, and it is a
  // `const` of this run's scope rather than an index another body could guess.
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
/// # Rejections nobody awaited
///
/// A promise a body creates and discards must not fail *other* runs. Left to
/// `deno_core`'s default, an unhandled rejection halts the whole event loop —
/// which, with runs multiplexed, is every resident body punished for one body's
/// dropped `db` call. The run's own failure never comes this way (`__scRun`
/// attaches a rejection handler to the body's promise), so the handler here can
/// say "handled" and mean it.
#[cfg(feature = "eval")]
const SETUP: &str = r#"
(() => {
  const call = Deno.core.ops.op_sc_db;
  const done = Deno.core.ops.op_sc_done;
  const fail = Deno.core.ops.op_sc_fail;
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
      const reply = JSON.parse(answer);
      if (reply.error !== undefined) reject(new Error(reply.error));
      else resolve(reply.ok);
    }, reject);
  }));
  // What an admin should be shown: the stack when there is one, because a body
  // of any size wants the line, and the value itself when there is not.
  const describe = (e) => {
    if (e instanceof Error) return e.stack ? e.stack : String(e);
    try { return String(e); } catch (_) { return "the code threw a value it cannot describe"; }
  };
  // The run wrapper: start the body — an async function, so what comes back is
  // a promise — and report what it settles to through the completion ops. The
  // refusal of a returned Promise this used to carry has inverted: a promise is
  // what a body now answers with, and awaiting it is the point.
  fixed("__scRun", (token, body, bindings) => {
    let running;
    try {
      running = body(bindings);
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
    /// Wall clock: when this run may make no further host calls.
    deadline: Instant,
    /// What the deadline was, for the message.
    timeout: Duration,
    calls_left: u32,
    max_calls: u32,
    /// JS execution time still allowed. Host calls do not consume it.
    js_budget: Duration,
    /// When this run's JS window was armed, or `None` while it is suspended in
    /// a host call — the database's time is not the guest's.
    armed_at: Option<Instant>,
    /// Where the answer goes. The run's oneshot lives here rather than with the
    /// worker's loop, because the loop no longer waits for one run: a completion
    /// op finds the run by its token and answers whoever asked for it.
    reply: Option<tokio::sync::oneshot::Sender<Result<Json>>>,
}

#[cfg(feature = "eval")]
impl RunState {
    /// When this run's JS window runs out, or `None` while it is suspended.
    fn js_deadline(&self) -> Option<Instant> {
        self.armed_at.map(|at| at + self.js_budget)
    }

    /// Stop the clock on JS execution: a host call is the database's time, not
    /// the guest's, and reporting a slow query as "your code timed out" sends an
    /// admin to rewrite code that was never the problem.
    fn pause(&mut self) {
        if let Some(at) = self.armed_at.take() {
            self.js_budget = self.js_budget.saturating_sub(at.elapsed());
        }
    }

    /// Start it again, with whatever JS time was left.
    fn resume(&mut self) {
        self.armed_at = Some(Instant::now());
    }

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
/// The table also owns three things that are facts about the whole table rather
/// than about any run:
///
/// - the isolate's **watchdog**, which wants to be armed at the earliest JS
///   deadline any resident run still has (every mutation ends in
///   [`RunTable::rearm`]);
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
            watchdog,
            outstanding,
            freed,
        }
    }

    /// Point the one watchdog at the earliest JS deadline among the runs that
    /// are actually executing; disarm it when none of them is.
    fn rearm(&self) {
        match self.runs.values().filter_map(RunState::js_deadline).min() {
            Some(deadline) => self.watchdog.arm(deadline),
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

    /// Take every run out, for a failure that is the isolate's rather than any
    /// one run's.
    fn drain(&mut self) -> Vec<RunState> {
        let tokens: Vec<String> = self.runs.keys().cloned().collect();
        tokens.iter().filter_map(|t| self.take(t)).collect()
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
        run.pause();
        table.rearm();
        host
    };

    let outcome = host.call(plan).await;

    {
        let mut state = state.borrow_mut();
        if let Some(table) = state.try_borrow_mut::<RunTable>() {
            if let Some(run) = table.runs.get_mut(token) {
                run.resume();
            }
            table.rearm();
        }
    }
    match outcome {
        Ok(value) => serde_json::json!({ "ok": value }),
        Err(e) => refuse(e.to_string()),
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
deno_core::extension!(sc_db_ext, ops = [op_sc_db, op_sc_done, op_sc_fail]);

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

    /// Whether the watchdog has terminated the isolate since this was last
    /// cleared. Taken rather than read, because the worker acts on it exactly
    /// once: it cancels the termination, and the next JS to run must not be
    /// treated as the terminated one.
    fn took_fired(&self) -> bool {
        self.fired.swap(false, Ordering::SeqCst)
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
    /// Already defaulted and clamped, so the worker has no policy left to apply.
    timeout: Duration,
    max_calls: u32,
}

#[cfg(feature = "eval")]
struct CodeJob {
    run: CodeRun,
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
        let max_inflight = max_inflight.max(1);
        let mut pool = Vec::new();
        for n in 0..workers.max(1) {
            let (jobs, rx) = tokio::sync::mpsc::unbounded_channel::<CodeJob>();
            let outstanding = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&outstanding);
            std::thread::Builder::new()
                .name(format!("sc-code-{n}"))
                .spawn(move || worker_thread(rx, &counted, max_inflight))
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
                run: CodeRun {
                    code: call.code,
                    bindings: call.bindings,
                    host: proxy,
                    timeout,
                    max_calls: call.max_calls,
                },
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

        let Some((host, mut incoming)) = bridged else {
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
                Some(HostRequest { plan, reply }) = incoming.recv() => {
                    serving.push(async {
                        // A dropped receiver means the isolate stopped waiting
                        // for this one: the answer is simply not wanted.
                        let _ = reply.send(host.call(plan).await);
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
            ..Default::default()
        },
    );

    // The op handles and the run wrapper, then `Deno` goes away — see SETUP. A
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
#[cfg(feature = "eval")]
async fn serve(
    runtime: &mut deno_core::JsRuntime,
    op_state: &Rc<RefCell<OpState>>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<CodeJob>,
    max_inflight: usize,
    watchdog: &Watchdog,
    freed: &tokio::sync::Notify,
) {
    let mut closed = false;
    loop {
        // Admit whatever is already waiting, up to the occupancy bound.
        while !closed && resident(op_state) < max_inflight {
            match rx.try_recv() {
                Ok(job) => start_run(runtime, op_state, job, watchdog),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    closed = true;
                }
            }
        }
        if resident(op_state) == 0 {
            if closed {
                return; // The last CodeRuntime handle was dropped.
            }
            // Nothing to pump: park on the channel rather than poll an empty
            // event loop for ever.
            match rx.recv().await {
                Some(job) => start_run(runtime, op_state, job, watchdog),
                None => closed = true,
            }
            continue;
        }

        let admit = !closed && resident(op_state) < max_inflight;
        let due = next_deadline(op_state);
        let tick = {
            let pump = runtime.run_event_loop(deno_core::PollEventLoopOptions::default());
            tokio::pin!(pump);
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
        handle_terminated(runtime, op_state, watchdog);

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
            Tick::Job(Some(job)) => start_run(runtime, op_state, job, watchdog),
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
        .map_or_else(Vec::new, RunTable::drain)
}

/// If the watchdog terminated the isolate: cancel that, and fail everything that
/// was resident.
///
/// One instrument for the whole isolate is the honest gap in this phase — a body
/// that spins without yielding takes its co-residents with it. What must not
/// happen is worse: leaving the isolate terminated, so that the *next* run's
/// JavaScript is aborted in place of the guilty one's. Hence cancelling here,
/// after every way JavaScript can have run.
#[cfg(feature = "eval")]
fn handle_terminated(
    runtime: &mut deno_core::JsRuntime,
    op_state: &Rc<RefCell<OpState>>,
    watchdog: &Watchdog,
) {
    if !watchdog.took_fired() {
        return;
    }
    runtime.v8_isolate().cancel_terminate_execution();
    for mut run in drain_runs(op_state) {
        let ms = run.timeout.as_millis();
        run.answer(Err(Error::invalid(format!(
            "JavaScript code timed out after {ms} ms"
        ))));
    }
}

/// How often the loop looks again when it has nothing to look at: the floor under
/// the timer, so that a deadline already in the past cannot spin it.
#[cfg(feature = "eval")]
const TICK_FLOOR: Duration = Duration::from_millis(1);

/// When the loop next has something to do that is not an event: the earliest
/// wall-clock deadline among the resident runs, or the earliest JS deadline the
/// watchdog is armed at — whichever comes first.
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
                .flat_map(|run| [Some(run.deadline), run.js_deadline()])
                .flatten()
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
    watchdog: &Watchdog,
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
    let script = match build_code_script(&run, &token) {
        Ok(script) => script,
        Err(e) => {
            give_back(op_state);
            let _ = reply.send(Err(e));
            return;
        }
    };
    let started = Instant::now();
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
                host: run.host,
                deadline: started + run.timeout,
                timeout: run.timeout,
                calls_left: run.max_calls,
                max_calls: run.max_calls,
                js_budget: run.timeout,
                armed_at: Some(started),
                reply: Some(reply),
            },
        );
        table.rearm();
    }
    let outcome = runtime.execute_script("sc_code.js", script);
    // A terminated run is already answered by name, so this comes first.
    handle_terminated(runtime, op_state, watchdog);
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

/// Assemble the script for one code body: the run's **token** as a `const`, the
/// bindings as `const`s, the prelude (when there is a host) in the same scope,
/// the code as the body of a nested **async** function — so a top-level `return`
/// is legal, a top-level `await` is legal, and nothing it declares outlives the
/// run — and the whole thing handed to the fixed `__scRun` wrapper.
///
/// The token is bound *inside* the function, so it is this run's and no other's:
/// the prelude closes over it, and a body that reads it can only ask the
/// questions it was already allowed to ask.
///
/// The code itself is **not** escaped, and cannot be: it is the admin's own
/// JavaScript, spliced in as source. That is not a hole — the wrapper is no
/// privilege boundary, and the host re-validates every plan that comes back out
/// of it. What *is* escaped is every value, which rides in as JSON exactly as a
/// formula's bindings do.
#[cfg(feature = "eval")]
fn build_code_script(call: &CodeRun, token: &str) -> Result<String> {
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
    // The token is 32 hex characters this crate minted; quoting it is belt and
    // braces rather than escaping.
    let tok =
        serde_json::to_string(token).map_err(|e| Error::msg(format!("encode run token: {e}")))?;
    Ok(format!(
        "__scRun({tok}, async function (__b) {{ \"use strict\";\n\
         const __scTok = {tok};\n\
         {consts}{prelude}\n\
         const __result = await (async function () {{\n{code}\n}})();\n\
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
        // The op handle exists — that is the one surface — but it refuses a body
        // with no host rather than reaching anything.
        let err = rt
            .run(call(
                r#"return await __scDbCall(__scTok, { op: "select", table: "books" });"#,
            ))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("no database access"), "{err}");
        // And a token that is not this run's names nothing: the table is keyed
        // by 128 random bits precisely so that a body cannot reach another
        // resident run's host by guessing at it.
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
        // The inversion this milestone turns on: `__scRun` used to refuse a
        // Promise because there was nothing in the sandbox to await it with.
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
}
