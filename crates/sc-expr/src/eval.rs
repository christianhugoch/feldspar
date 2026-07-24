//! Reified evaluation: actually running a formula, in a real V8 isolate via
//! `deno_core` (TODO Phase 3).
//!
//! This is the fallback for formulas the symbolic translator refuses (method
//! calls, templates, truthiness of typed fields) and the reference
//! implementation the translator is tested against. It evaluates the
//! [normalised rendering](crate::normalise) of the shared AST — never the raw
//! source — so the semantics are the ones Phase 2 specified.
//!
//! # Architecture
//!
//! A `JsRuntime` is `!Send`, so [`DenoEvaluator`] owns a **dedicated thread**
//! holding the runtime, fed jobs over a channel; callers are async and await a
//! oneshot reply. The runtime is built with **no extensions and no ops** — the
//! sandbox has no I/O to reach (`Deno`, `fetch`, `require` do not exist; a
//! test asserts it). A watchdog thread terminates runaway evaluation through
//! the isolate's thread-safe handle after a timeout.
//!
//! # Fail closed
//!
//! A formula that throws, times out, or references something unbound returns
//! `Err` — and the §5 enforcement rule is that an `Err` **denies**. The
//! evaluator reports; the caller must never map an error to a grant.
//!
//! # Binding
//!
//! One evaluation binds, by name: every row field the formula reads, every
//! Ⱶ-join identifier (to its **prefetched** value — the evaluator does no
//! I/O; only the caller has a catalog), `user` (an object of the user's
//! fields, or `null`), and the five operation flags from the [`Operation`].
//! Values are embedded as JSON in the script text, so nothing crosses the JS
//! boundary except one script string and one boolean result.

use std::collections::BTreeMap;
#[cfg(feature = "eval")]
use std::sync::Arc;
#[cfg(feature = "eval")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "eval")]
use std::sync::mpsc;
#[cfg(feature = "eval")]
use std::time::{Duration, Instant};

use async_trait::async_trait;
#[cfg(feature = "eval")]
use sc_error::Error;
use sc_error::Result;
use sc_query::Value;

#[cfg(feature = "eval")]
use crate::analyze::OpFlag;
use crate::formula::Formula;
#[cfg(feature = "eval")]
use crate::normalise::{is_join_ident, render_js};
use crate::translate::Operation;

/// One formula evaluation: the formula, the operation, and the values in
/// scope. Owned data, sent across the evaluator's thread boundary.
#[derive(Debug, Clone)]
pub struct FormulaCall {
    /// The formula to evaluate.
    pub formula: Formula,
    /// The operation, deciding the five `_read`/`_write`/… flag bindings.
    pub op: Operation,
    /// The row's values by field name — including one entry per Ⱶ-join
    /// identifier the formula uses (`"publisherⱵname"` → the prefetched
    /// value), since the evaluator does no I/O.
    pub row: BTreeMap<String, Value>,
    /// The current user's fields, or `None` when nobody is logged in
    /// (`user` binds to `null`).
    pub user: Option<BTreeMap<String, Value>>,
}

/// The evaluator seam. `DenoEvaluator` is the implementation; the trait exists
/// so the formula machinery never names the engine — a lighter engine
/// (boa/quickjs) could sit behind it if V8's build weight ever matters.
///
/// **Contract for callers:** `Ok(bool)` is the formula's verdict; `Err` means
/// the formula could not be evaluated (throw, timeout, unbound variable) and
/// **must be treated as deny** — fail closed.
#[async_trait]
pub trait JsEvaluator: Send + Sync {
    /// Evaluate one formula against one row/user/operation, coercing the result
    /// to a boolean verdict (the ownership-formula use).
    async fn eval(&self, call: FormulaCall) -> Result<bool>;

    /// Evaluate one formula to its **value** (as JSON), for a non-stored
    /// calculated field (Phase 8). Same binding and normalisation as
    /// [`eval`](JsEvaluator::eval); the caller decodes the JSON into a column
    /// value. `Err` still means the formula could not be evaluated.
    async fn eval_value(&self, call: FormulaCall) -> Result<serde_json::Value>;
}

/// How long a single evaluation may run before the watchdog terminates it. A
/// formula is a pure expression over a handful of values; this is generous.
#[cfg(feature = "eval")]
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(250);

/// The aggregation prelude (Phase 7): the invented methods on `Array.prototype`
/// the curated chain uses, implementing the [semantics
/// table](../../../docs/AGG_EXPRS.md). Native `filter`/`map`/`some`/`every`/
/// `length`/`includes`/`join` are genuinely native and need nothing here.
///
/// Every method is non-enumerable (so `for…in`/`JSON` see nothing new), null
/// values are ignored, and the empty-relation defaults are `sum` `0`,
/// `avg`/`min`/`max`/`maxBy`/`minBy` `null`. `maxBy`/`minBy` break selector
/// ties by the child row's `id` — the host prefetch always exposes the primary
/// key under its own name, and the symbolic side's `ORDER BY key, pk` matches
/// (both are the production path for these; reified is the parity reference).
#[cfg(feature = "eval")]
const AGG_PRELUDE: &str = r#"
(() => {
  const A = Array.prototype;
  const def = (name, fn) =>
    Object.defineProperty(A, name, { value: fn, enumerable: false, writable: true, configurable: true });
  // Resolve a selector: absent → the element itself, a string → a field, a
  // function → its result. A null/undefined row yields null.
  const pick = (row, sel) => {
    if (sel === undefined || sel === null) return row;
    if (typeof sel === "function") return sel(row);
    return row == null ? null : row[sel];
  };
  // Non-null selector values.
  const vals = (arr, sel) => {
    const out = [];
    for (const row of arr) {
      const v = pick(row, sel);
      if (v !== null && v !== undefined) out.push(v);
    }
    return out;
  };
  def("sum", function (sel) {
    let t = 0;
    for (const v of vals(this, sel)) t += v;
    return t;
  });
  def("avg", function (sel) {
    const vs = vals(this, sel);
    if (vs.length === 0) return null;
    let t = 0;
    for (const v of vs) t += v;
    return t / vs.length;
  });
  def("min", function (sel) {
    const vs = vals(this, sel);
    if (vs.length === 0) return null;
    let m = vs[0];
    for (const v of vs) if (v < m) m = v;
    return m;
  });
  def("max", function (sel) {
    const vs = vals(this, sel);
    if (vs.length === 0) return null;
    let m = vs[0];
    for (const v of vs) if (v > m) m = v;
    return m;
  });
  def("distinct", function (sel) {
    const seen = new Set();
    const out = [];
    for (const v of vals(this, sel)) {
      if (!seen.has(v)) {
        seen.add(v);
        out.push(v);
      }
    }
    return out;
  });
  const by = (arr, sel, dir) => {
    let best = null, bestKey = null;
    for (const row of arr) {
      const k = pick(row, sel);
      if (k === null || k === undefined) continue;
      if (best === null) { best = row; bestKey = k; continue; }
      let cmp = 0;
      if (k > bestKey) cmp = 1; else if (k < bestKey) cmp = -1;
      else {
        const id = row == null ? null : row.id;
        const bid = best == null ? null : best.id;
        if (id > bid) cmp = 1; else if (id < bid) cmp = -1;
      }
      if (cmp === dir) { best = row; bestKey = k; }
    }
    return best;
  };
  def("maxBy", function (sel) { return by(this, sel, 1); });
  def("minBy", function (sel) { return by(this, sel, -1); });
})();
"#;

#[cfg(feature = "eval")]
enum Job {
    Eval(FormulaCall, tokio::sync::oneshot::Sender<Result<bool>>),
    /// Evaluate to the formula's **value** (as JSON), for calculated fields
    /// (Phase 8) — the same binding and normalisation, but the script returns
    /// `JSON.stringify(expr)` instead of `!!(expr)`.
    EvalValue(
        FormulaCall,
        tokio::sync::oneshot::Sender<Result<serde_json::Value>>,
    ),
    /// Raw script escape hatch for the watchdog test only: the formula
    /// language cannot express an infinite loop (no statements, no named
    /// recursion), which is a feature — but it leaves the timeout otherwise
    /// untestable.
    #[cfg(test)]
    Raw(String, tokio::sync::oneshot::Sender<Result<bool>>),
}

/// How the reply for a finished job is sent — a bool verdict or a JSON value.
#[cfg(feature = "eval")]
enum Pending {
    Bool(tokio::sync::oneshot::Sender<Result<bool>>),
    Value(tokio::sync::oneshot::Sender<Result<serde_json::Value>>),
}

#[cfg(feature = "eval")]
/// The `deno_core`-backed [`JsEvaluator`]: one V8 isolate on one thread,
/// serving evaluations serially. Cheap to share (`Arc` inside); dropping the
/// last handle shuts the thread down.
pub struct DenoEvaluator {
    tx: mpsc::Sender<Job>,
}

#[cfg(feature = "eval")]
impl DenoEvaluator {
    /// Start the evaluator thread with the default timeout.
    pub fn new() -> DenoEvaluator {
        DenoEvaluator::with_timeout(DEFAULT_TIMEOUT)
    }

    /// Start the evaluator thread with an explicit per-evaluation timeout.
    pub fn with_timeout(timeout: Duration) -> DenoEvaluator {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("sc-expr-js-eval".into())
            .spawn(move || runtime_thread(rx, timeout))
            // Thread spawning fails only on resource exhaustion at process
            // level; there is no useful recovery, and every later eval would
            // error on a closed channel anyway.
            .ok();
        DenoEvaluator { tx }
    }

    #[cfg(test)]
    async fn eval_raw(&self, script: String) -> Result<bool> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(Job::Raw(script, reply_tx))
            .map_err(|_| Error::msg("formula evaluator thread is gone"))?;
        reply_rx
            .await
            .map_err(|_| Error::msg("formula evaluator dropped the reply"))?
    }
}

#[cfg(feature = "eval")]
impl Default for DenoEvaluator {
    fn default() -> Self {
        DenoEvaluator::new()
    }
}

#[cfg(feature = "eval")]
#[async_trait]
impl JsEvaluator for DenoEvaluator {
    async fn eval(&self, call: FormulaCall) -> Result<bool> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(Job::Eval(call, reply_tx))
            .map_err(|_| Error::msg("formula evaluator thread is gone"))?;
        reply_rx
            .await
            .map_err(|_| Error::msg("formula evaluator dropped the reply"))?
    }

    async fn eval_value(&self, call: FormulaCall) -> Result<serde_json::Value> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(Job::EvalValue(call, reply_tx))
            .map_err(|_| Error::msg("formula evaluator thread is gone"))?;
        reply_rx
            .await
            .map_err(|_| Error::msg("formula evaluator dropped the reply"))?
    }
}

#[cfg(feature = "eval")]
/// The dedicated thread: owns the `JsRuntime`, its watchdog, and the job loop.
fn runtime_thread(rx: mpsc::Receiver<Job>, timeout: Duration) {
    let mut runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions::default());

    // A default runtime still carries `Deno.core` (deno_core's own plumbing —
    // not I/O, but not formula business either). Remove the global outright:
    // the sandbox test asserts `typeof Deno === 'undefined'`, and the simplest
    // way for that to be true is for it to actually be true.
    if runtime
        .execute_script("sc_setup.js", "delete globalThis.Deno;")
        .is_err()
    {
        // A failed setup leaves `Deno.core` visible but harmless; evaluation
        // still works, so serve rather than die. The sandbox test would flag
        // it loudly on any platform where this happens.
    }

    // The aggregation prelude (Phase 7): the invented `Array.prototype` methods
    // the curated chain relies on, implementing the semantics table. A failure
    // here means aggregation formulas throw (deny) rather than silently doing
    // the wrong thing; ordinary formulas are unaffected.
    if runtime.execute_script("sc_agg.js", AGG_PRELUDE).is_err() {
        // As above: serve rather than die; a parity test would catch a real
        // regression on any platform where the prelude failed to install.
    }

    // The watchdog: armed per evaluation with a deadline; on expiry it
    // terminates JS execution through the isolate's thread-safe handle (the
    // only safe cross-thread operation on an isolate).
    let isolate_handle = runtime.v8_isolate().thread_safe_handle();
    let timed_out = Arc::new(AtomicBool::new(false));
    let (watchdog_tx, watchdog_rx) = mpsc::channel::<WatchdogMsg>();
    {
        let timed_out = Arc::clone(&timed_out);
        std::thread::Builder::new()
            .name("sc-expr-js-watchdog".into())
            .spawn(move || watchdog_thread(watchdog_rx, isolate_handle, timed_out))
            .ok();
    }

    while let Ok(job) = rx.recv() {
        // Build the script (with the right result wrapper) and remember how to
        // reply. A build error goes straight back on the matching channel.
        let (script, pending) = match job {
            Job::Eval(call, reply) => match build_script(&call, false) {
                Ok(script) => (script, Pending::Bool(reply)),
                Err(e) => {
                    let _ = reply.send(Err(e));
                    continue;
                }
            },
            Job::EvalValue(call, reply) => match build_script(&call, true) {
                Ok(script) => (script, Pending::Value(reply)),
                Err(e) => {
                    let _ = reply.send(Err(e));
                    continue;
                }
            },
            #[cfg(test)]
            Job::Raw(script, reply) => (script, Pending::Bool(reply)),
        };

        timed_out.store(false, Ordering::SeqCst);
        let _ = watchdog_tx.send(WatchdogMsg::Arm(Instant::now() + timeout));
        let outcome = runtime.execute_script("sc_formula.js", script);
        let _ = watchdog_tx.send(WatchdogMsg::Disarm);

        match outcome {
            Ok(global) => {
                deno_core::scope!(scope, &mut runtime);
                let local = deno_core::v8::Local::new(scope, global);
                match pending {
                    // A `!!(…)` script yields a boolean.
                    Pending::Bool(reply) => {
                        let _ = reply.send(Ok(local.is_true()));
                    }
                    // A `JSON.stringify(…)` script yields a JSON string (or
                    // `undefined`, which reads as null).
                    Pending::Value(reply) => {
                        let value = if local.is_string() {
                            let text = local.to_rust_string_lossy(scope);
                            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null)
                        } else {
                            serde_json::Value::Null
                        };
                        let _ = reply.send(Ok(value));
                    }
                }
            }
            Err(e) => {
                let err = if timed_out.load(Ordering::SeqCst) {
                    // Termination poisons the isolate until cancelled; restore
                    // it so the next evaluation runs clean.
                    runtime.v8_isolate().cancel_terminate_execution();
                    Error::invalid(format!("formula evaluation timed out after {timeout:?}"))
                } else {
                    Error::invalid(format!("formula evaluation failed: {e}"))
                };
                match pending {
                    Pending::Bool(reply) => {
                        let _ = reply.send(Err(err));
                    }
                    Pending::Value(reply) => {
                        let _ = reply.send(Err(err));
                    }
                }
            }
        }
    }
    // rx closed: last DenoEvaluator handle dropped. watchdog_tx drops here,
    // which ends the watchdog thread's loop too.
}

#[cfg(feature = "eval")]
enum WatchdogMsg {
    Arm(Instant),
    Disarm,
}

#[cfg(feature = "eval")]
fn watchdog_thread(
    rx: mpsc::Receiver<WatchdogMsg>,
    isolate: deno_core::v8::IsolateHandle,
    timed_out: Arc<AtomicBool>,
) {
    while let Ok(msg) = rx.recv() {
        let WatchdogMsg::Arm(deadline) = msg else {
            continue; // A stray Disarm; nothing armed.
        };
        let wait = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(WatchdogMsg::Disarm) => {}
            Ok(WatchdogMsg::Arm(_)) => {} // Cannot happen: evals are serial.
            Err(mpsc::RecvTimeoutError::Timeout) => {
                timed_out.store(true, Ordering::SeqCst);
                isolate.terminate_execution();
                // Wait for the evaluation to acknowledge before re-arming.
                if rx.recv().is_err() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(feature = "eval")]
/// Assemble the self-contained script for one call: every free variable bound
/// as a `const` from a JSON bindings object, the normalised expression, and a
/// `!!` truthiness coercion (or, in `value_mode`, `JSON.stringify`). JSON is (in
/// a V8 this modern) a syntactic subset of JS, so embedding `serde_json`'s
/// output as the argument literal is safe — no value ever touches string
/// concatenation un-escaped.
fn build_script(call: &FormulaCall, value_mode: bool) -> Result<String> {
    let free = call.formula.free_vars();
    let mut bindings = serde_json::Map::new();
    let mut consts = String::new();
    for ident in &free.idents {
        let value = binding_for(call, ident)?;
        let Some(value) = value else {
            continue; // A global (Math, …): let the real one show through.
        };
        // `const x = __b["x"];` — the name was parsed as an identifier, so it
        // is one; the key lookup quotes via JSON escaping.
        let key = serde_json::to_string(ident)
            .map_err(|e| Error::msg(format!("encode binding name: {e}")))?;
        consts.push_str(&format!("const {ident} = __b[{key}];\n"));
        bindings.insert(ident.clone(), value);
    }
    let args = serde_json::to_string(&serde_json::Value::Object(bindings))
        .map_err(|e| Error::msg(format!("encode bindings: {e}")))?;
    let expr = render_js(call.formula.ast());
    // Value mode returns the JSON text of the result (calc fields); bool mode
    // returns the `!!` verdict (ownership). `JSON.stringify(undefined)` is
    // `undefined`, which the reader maps to null.
    let ret = if value_mode {
        format!("JSON.stringify({expr})")
    } else {
        format!("!!({expr})")
    };
    Ok(format!(
        "(function(__b) {{ \"use strict\";\n{consts}return {ret};\n}})({args})"
    ))
}


#[cfg(feature = "eval")]
/// The JSON value an identifier binds to, or `None` for a whitelisted global.
/// An identifier with no binding and no global is an error — a caller bug (it
/// should have validated and prefetched), reported rather than bound to
/// `undefined`, which would silently diverge from SQL's `NULL`.
fn binding_for(call: &FormulaCall, ident: &str) -> Result<Option<serde_json::Value>> {
    if let Some(flag) = OpFlag::from_ident(ident) {
        return Ok(Some(serde_json::Value::Bool(flag_value(call.op, flag))));
    }
    if ident == "user" {
        return Ok(Some(match &call.user {
            None => serde_json::Value::Null,
            Some(fields) => serde_json::Value::Object(
                fields
                    .iter()
                    .map(|(k, v)| (k.clone(), value_to_json(v)))
                    .collect(),
            ),
        }));
    }
    if let Some(v) = call.row.get(ident) {
        return Ok(Some(value_to_json(v)));
    }
    if crate::analyze::GLOBALS.contains(&ident) {
        return Ok(None);
    }
    // A join or relation identifier the caller failed to prefetch is the
    // likeliest way here; name it precisely.
    let what = if crate::agg::is_relation_ident(ident) {
        "relation was not prefetched"
    } else if is_join_ident(ident) {
        "join value was not prefetched"
    } else {
        "no value was bound"
    };
    Err(Error::msg(format!("formula evaluation: `{ident}`: {what}")))
}

#[cfg(feature = "eval")]
/// Mirror of the symbolic side's flag folding ([`Operation`] decides each
/// flag), so both evaluators see identical flag values by construction.
fn flag_value(op: Operation, flag: OpFlag) -> bool {
    match flag {
        OpFlag::Read => op == Operation::Read,
        OpFlag::Insert => op == Operation::Insert,
        OpFlag::Update => op == Operation::Update,
        OpFlag::Delete => op == Operation::Delete,
        OpFlag::Write => op != Operation::Read,
    }
}

#[cfg(feature = "eval")]
/// A SQL [`Value`] as the JSON (hence JS) value the formula sees. The mapping
/// is chosen to line up with the symbolic side: text-like values (UUIDs,
/// dates, times) become strings — which is also what their SQL comparisons
/// compare — and numbers become JS numbers.
///
/// Lossy corners, deliberate and documented: an `i64` beyond 2^53 loses
/// precision (JS numbers are f64); a non-finite float and a `Decimal` beyond
/// f64 become `null`; `Bytes` has no JS literal and becomes `null` (a formula
/// over a bytea column is not a supported thing).
fn value_to_json(v: &Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Int(i) => J::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f).map_or(J::Null, J::Number),
        Value::Text(s) => J::String(s.clone()),
        Value::Uuid(u) => J::String(u.to_string()),
        Value::Date(d) => J::String(d.to_string()),
        Value::Time(t) => J::String(t.to_string()),
        Value::Timestamp(ts) => J::String(ts.to_rfc3339()),
        Value::Decimal(d) => {
            let f: f64 = (*d).try_into().unwrap_or(f64::NAN);
            serde_json::Number::from_f64(f).map_or(J::Null, J::Number)
        }
        Value::Json(j) => j.clone(),
        Value::Bytes(_) => J::Null,
    }
}

#[cfg(feature = "eval")]
#[cfg(test)]
mod tests {
    use super::*;

    fn call(src: &str, op: Operation) -> FormulaCall {
        FormulaCall {
            formula: Formula::parse(src).unwrap(),
            op,
            row: BTreeMap::new(),
            user: None,
        }
    }

    fn with_row(mut c: FormulaCall, fields: &[(&str, Value)]) -> FormulaCall {
        c.row = fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        c
    }

    fn with_user(mut c: FormulaCall, fields: &[(&str, Value)]) -> FormulaCall {
        c.user = Some(
            fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        );
        c
    }

    async fn eval(c: FormulaCall) -> Result<bool> {
        DenoEvaluator::new().eval(c).await
    }

    #[tokio::test]
    async fn evaluates_ownership_against_a_row_and_user() {
        let owner = |v: &str| {
            with_row(
                call("owner === user.id", Operation::Read),
                &[("owner", Value::Text(v.into()))],
            )
        };
        let c = with_user(owner("u1"), &[("id", Value::Text("u1".into()))]);
        assert!(eval(c).await.unwrap());
        let c = with_user(owner("u2"), &[("id", Value::Text("u1".into()))]);
        assert!(!eval(c).await.unwrap());
        // No user: `user.id` is null under the normalised guard; a non-null
        // owner does not match.
        assert!(!eval(owner("u1")).await.unwrap());
    }

    #[tokio::test]
    async fn a_half_h_identifier_binds_in_v8() {
        // The in-crate half of the Phase 1 claim: V8 accepts `publisherⱵname`
        // as one identifier, and the prefetched binding reaches it.
        let c = with_row(
            call("publisherⱵname === 'ACME'", Operation::Read),
            &[("publisherⱵname", Value::Text("ACME".into()))],
        );
        assert!(eval(c).await.unwrap());
    }

    #[tokio::test]
    async fn operation_flags_bind_per_operation() {
        assert!(eval(call("_read", Operation::Read)).await.unwrap());
        assert!(!eval(call("_read", Operation::Update)).await.unwrap());
        assert!(eval(call("_write", Operation::Delete)).await.unwrap());
        assert!(!eval(call("_write", Operation::Read)).await.unwrap());
    }

    #[tokio::test]
    async fn truthiness_coerces_the_result() {
        // A non-boolean result is coerced by JS truthiness, as specified.
        let c = with_row(
            call("title", Operation::Read),
            &[("title", Value::Text("x".into()))],
        );
        assert!(eval(c).await.unwrap());
        let c = with_row(
            call("title", Operation::Read),
            &[("title", Value::Text(String::new()))],
        );
        assert!(!eval(c).await.unwrap());
        let c = with_row(call("title", Operation::Read), &[("title", Value::Null)]);
        assert!(!eval(c).await.unwrap());
    }

    #[tokio::test]
    async fn the_untranslatable_class_runs_here() {
        // A method call with an arrow — refused by the symbolic translator,
        // which is exactly what this evaluator exists for.
        let c = with_user(
            with_row(
                call("user.groups.some(g => g === dept)", Operation::Read),
                &[("dept", Value::Text("eng".into()))],
            ),
            &[("groups", Value::Json(serde_json::json!(["eng", "ops"])))],
        );
        assert!(eval(c).await.unwrap());
    }

    #[tokio::test]
    async fn a_throwing_formula_is_an_error_not_a_grant() {
        // `null.x` throws a TypeError; the verdict is Err, which callers must
        // treat as deny.
        let c = with_row(
            call("owner.name === 'x'", Operation::Read),
            &[("owner", Value::Null)],
        );
        let err = eval(c).await.unwrap_err();
        assert!(err.to_string().contains("evaluation failed"), "got: {err}");
    }

    #[tokio::test]
    async fn an_unbound_identifier_is_a_named_error() {
        // A join value the caller failed to prefetch must not silently become
        // `undefined`.
        let c = call("publisherⱵname === 'ACME'", Operation::Read);
        let err = eval(c).await.unwrap_err();
        assert!(
            err.to_string().contains("publisherⱵname")
                && err.to_string().contains("not prefetched"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn the_sandbox_has_no_io_surface() {
        // Two layers. The outer one: `Deno`, `fetch` &c are not in the formula
        // vocabulary at all — the binder refuses them before V8 ever runs.
        let err = eval(call("typeof Deno === 'undefined'", Operation::Read))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("`Deno`"), "got: {err}");
        // The inner one: even raw script (no binder) finds no I/O surface in
        // the isolate — the runtime is built with no extensions and no ops.
        let ev = DenoEvaluator::new();
        for probe in [
            "typeof Deno === 'undefined'",
            "typeof fetch === 'undefined'",
            "typeof require === 'undefined'",
            "typeof process === 'undefined'",
        ] {
            assert!(
                ev.eval_raw(format!("!!({probe})")).await.unwrap(),
                "sandbox leak: {probe}"
            );
        }
    }

    #[tokio::test]
    async fn a_runaway_script_is_terminated_and_the_isolate_recovers() {
        let ev = DenoEvaluator::with_timeout(Duration::from_millis(50));
        let err = ev.eval_raw("while (true) {}".into()).await.unwrap_err();
        assert!(err.to_string().contains("timed out"), "got: {err}");
        // The isolate serves the next evaluation normally.
        let (tx, rx) = tokio::sync::oneshot::channel();
        ev.tx
            .send(Job::Eval(call("_read", Operation::Read), tx))
            .unwrap();
        assert!(rx.await.unwrap().unwrap());
    }

    #[tokio::test]
    async fn injection_shaped_values_stay_values() {
        // A value that looks like code must remain a string: it rides in as
        // JSON, never concatenated into the expression.
        let payload = "\"; globalThis.pwned = 1; \"";
        let ev = DenoEvaluator::new();
        let c = with_row(
            call("title === 'x'", Operation::Read),
            &[("title", Value::Text(payload.into()))],
        );
        assert!(!ev.eval(c).await.unwrap());
        // Same isolate: had the payload escaped its string, the global would
        // exist now. (Raw probe — `globalThis` is not formula vocabulary.)
        assert!(
            ev.eval_raw("!!(typeof globalThis.pwned === 'undefined')".into())
                .await
                .unwrap()
        );
    }

    /// A relation identifier binds to a prefetched JSON array of child rows.
    fn with_relation(mut c: FormulaCall, ident: &str, rows: serde_json::Value) -> FormulaCall {
        c.row.insert(ident.to_string(), Value::Json(rows));
        c
    }

    #[tokio::test]
    async fn aggregation_prelude_implements_the_semantics_table() {
        let rel = "linesↃorder";
        let rows = serde_json::json!([
            {"id": 1, "qty": 2, "status": "shipped"},
            {"id": 2, "qty": 3, "status": "pending"},
            {"id": 3, "qty": null, "status": "shipped"},
        ]);
        let cases: &[(&str, bool)] = &[
            ("linesↃorder.length === 3", true),
            ("linesↃorder.sum(\"qty\") === 5", true), // null ignored
            ("linesↃorder.filter(r => r.status === \"shipped\").length === 2", true),
            ("linesↃorder.some(r => r.qty > 2)", true),
            ("linesↃorder.every(r => r.qty > 0)", false), // null qty fails
            ("linesↃorder.map(r => r.status).includes(\"pending\")", true),
            ("linesↃorder.distinct(\"status\").length === 2", true),
            ("linesↃorder.maxBy(\"qty\").id === 2", true),
        ];
        for (src, expect) in cases {
            let c = with_relation(call(src, Operation::Read), rel, rows.clone());
            assert_eq!(eval(c).await.unwrap(), *expect, "{src}");
        }
    }

    #[tokio::test]
    async fn aggregation_empty_relation_defaults() {
        let rel = "linesↃorder";
        let empty = serde_json::json!([]);
        for (src, expect) in [
            ("linesↃorder.length === 0", true),
            ("linesↃorder.sum(\"qty\") === 0", true),
            ("linesↃorder.avg(\"qty\") === null", true),
            ("linesↃorder.some(r => r.qty > 0)", false),
            ("linesↃorder.every(r => r.qty > 0)", true),
        ] {
            let c = with_relation(call(src, Operation::Read), rel, empty.clone());
            assert_eq!(eval(c).await.unwrap(), expect, "{src}");
        }
    }

    #[tokio::test]
    async fn maxby_member_access_is_null_safe_on_an_empty_relation() {
        // `.reviewer` on an empty `maxBy` must read null (optional chaining),
        // not throw — mirroring the symbolic `LIMIT 1` subquery.
        let c = with_relation(
            call("linesↃorder.maxBy(\"qty\").status === null", Operation::Read),
            "linesↃorder",
            serde_json::json!([]),
        );
        assert!(eval(c).await.unwrap());
    }

    #[tokio::test]
    async fn an_unprefetched_relation_is_a_named_error() {
        let c = call("linesↃorder.length > 0", Operation::Read);
        let err = eval(c).await.unwrap_err().to_string();
        assert!(err.contains("linesↃorder") && err.contains("not prefetched"), "got: {err}");
    }

    #[tokio::test]
    async fn eval_value_returns_the_computed_value_for_a_calc_field() {
        use serde_json::json;
        let ev = DenoEvaluator::new();
        // Arithmetic over two fields → a number.
        let c = with_row(
            call("pages * 2 + 1", Operation::Read),
            &[("pages", Value::Int(10))],
        );
        assert_eq!(ev.eval_value(c).await.unwrap(), json!(21));
        // A string expression.
        let c = with_row(
            call("title", Operation::Read),
            &[("title", Value::Text("hi".into()))],
        );
        assert_eq!(ev.eval_value(c).await.unwrap(), json!("hi"));
        // An aggregation value (relation prefetched as an array).
        let c = with_relation(
            call("linesↃorder.sum(\"qty\")", Operation::Read),
            "linesↃorder",
            json!([{ "id": 1, "qty": 2 }, { "id": 2, "qty": 3 }]),
        );
        assert_eq!(ev.eval_value(c).await.unwrap(), json!(5));
        // A null-valued expression reads back as JSON null.
        let c = with_row(call("owner", Operation::Read), &[("owner", Value::Null)]);
        assert_eq!(ev.eval_value(c).await.unwrap(), serde_json::Value::Null);
    }

    #[tokio::test]
    async fn user_binds_as_an_object_or_null() {
        let c = with_user(call("user !== null", Operation::Read), &[]);
        assert!(eval(c).await.unwrap());
        assert!(!eval(call("user !== null", Operation::Read)).await.unwrap());
        assert!(!eval(call("user && true", Operation::Read)).await.unwrap());
    }
}
