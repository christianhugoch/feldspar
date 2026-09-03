//! Phase 1 of the Python code adapter, asserted: the pipeline, the conversions,
//! the exception hierarchy, the four bounds and the concurrency claim.
//!
//! No database. Everything a body reaches here is a fake host in this file, so
//! what is under test is the runtime rather than the plans — phases 2 and 3 test
//! those against a real one.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use sc_error::{Error, Result};
use sc_expr::{CodeAdapter, CodeCall, CodeHost};
use sc_python::{PythonEnv, PythonRuntime, PythonState};
use serde_json::{Value as Json, json};

// ---------------------------------------------------------------------------
// Fake hosts
// ---------------------------------------------------------------------------

/// Answers every plan with `{"echo": <plan>}` after `delay`, and counts what it
/// was asked. The delay is what a query's latency is standing in for, and it is
/// where the GIL had better not be held.
struct EchoHost {
    delay: Duration,
    calls: AtomicUsize,
}

impl EchoHost {
    fn new(delay_ms: u64) -> Arc<EchoHost> {
        Arc::new(EchoHost {
            delay: Duration::from_millis(delay_ms),
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl CodeHost for EchoHost {
    async fn call(&self, request: Json) -> Result<Json> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        Ok(json!({ "echo": request }))
    }
}

/// Refuses everything, the way an ownership formula refuses a delegated write.
struct RefusingHost;

#[async_trait]
impl CodeHost for RefusingHost {
    async fn call(&self, _request: Json) -> Result<Json> {
        Err(Error::invalid("the ownership rule refused this write"))
    }
}

/// A host whose answer is **another Python run** — a body's `trigger(…)` whose
/// action is itself a Python body, in miniature.
struct NestingHost {
    runtime: Arc<PythonRuntime>,
}

#[async_trait]
impl CodeHost for NestingHost {
    async fn call(&self, request: Json) -> Result<Json> {
        let code = request
            .get("code")
            .and_then(Json::as_str)
            .unwrap_or("return None")
            .to_owned();
        self.runtime
            .run(CodeCall {
                code,
                ..CodeCall::default()
            })
            .await
    }
}

fn call<'a>(code: &str, host: &'a dyn CodeHost) -> CodeCall<'a> {
    CodeCall {
        code: code.to_owned(),
        host: Some(host),
        ..CodeCall::default()
    }
}

// ---------------------------------------------------------------------------
// The body pipeline
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pure_body_returns_a_value_and_return_is_legal_at_the_top_level() {
    let runtime = PythonRuntime::new();
    let out = runtime
        .run(CodeCall {
            code: "rows = [1, 2, 3]\ntotal = sum(rows)\nreturn {\"total\": total}".to_owned(),
            ..CodeCall::default()
        })
        .await
        .expect("a pure body needs no host");
    assert_eq!(out, json!({ "total": 6 }));
    // Which is §7's third state, and the version is what the screen shows.
    assert!(
        matches!(runtime.state(), PythonState::Running { .. }),
        "the interpreter should be up once a body has run: {:?}",
        runtime.state()
    );
    assert_eq!(runtime.stuck(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_body_with_no_return_answers_null() {
    let runtime = PythonRuntime::new();
    let out = runtime
        .run(CodeCall {
            code: "x = 1 + 1".to_owned(),
            ..CodeCall::default()
        })
        .await
        .unwrap();
    assert_eq!(out, Json::Null);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_triple_quoted_string_survives_the_wrap() {
    // The reason the body is moved into a `FunctionDef` through the AST rather
    // than re-indented into one: re-indentation would rewrite this string.
    let runtime = PythonRuntime::new();
    let out = runtime
        .run(CodeCall {
            code: "s = \"\"\"one\n  two\nthree\"\"\"\nreturn s.splitlines()".to_owned(),
            ..CodeCall::default()
        })
        .await
        .unwrap();
    assert_eq!(out, json!(["one", "  two", "three"]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_syntax_error_names_the_authors_own_line() {
    let runtime = PythonRuntime::new();
    let error = runtime
        .run(CodeCall {
            code: "a = 1\nb = 2\nc = a +* b\n".to_owned(),
            ..CodeCall::default()
        })
        .await
        .expect_err("that is not Python");
    let said = error.to_string();
    assert!(said.contains("syntax error"), "{said}");
    assert!(said.contains("line 3"), "the author's own line: {said}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_exception_is_reported_with_its_own_line_and_the_authors_frames() {
    let runtime = PythonRuntime::new();
    let error = runtime
        .run(CodeCall {
            // Starting with a comment on purpose: the wrapper node spans the
            // author's statements, and a body whose first statement is not on
            // line 1 is the case that gets that wrong.
            code: "# chase the overdue invoices\na = 1\n\ndef helper(x):\n    return x / 0\n\nreturn helper(a)\n"
                .to_owned(),
            ..CodeCall::default()
        })
        .await
        .expect_err("dividing by zero");
    let said = error.to_string();
    assert!(said.contains("ZeroDivisionError"), "{said}");
    // Both of the author's frames, at their own line numbers — the call at 7
    // and the division at 5 — and the wrapper named as what it is.
    assert!(said.contains("line 7, in <body>"), "{said}");
    assert!(said.contains("line 5, in helper"), "{said}");
    // `linecache` was fed the source, so the line is beside the number rather
    // than blank.
    assert!(said.contains("return x / 0"), "{said}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bindings_are_scope_and_what_is_not_bound_is_a_name_error() {
    let runtime = PythonRuntime::new();
    let mut bindings = std::collections::BTreeMap::new();
    bindings.insert("row".to_owned(), json!({ "id": 7, "paid": false }));
    bindings.insert("user".to_owned(), Json::Null);
    let out = runtime
        .run(CodeCall {
            code: "return [row[\"id\"], row[\"paid\"], user]".to_owned(),
            bindings: bindings.clone(),
            ..CodeCall::default()
        })
        .await
        .unwrap();
    assert_eq!(out, json!([7, false, null]));

    // Presence is scope: `old` was not bound, so naming it is a `NameError`
    // rather than a silent `None`.
    let error = runtime
        .run(CodeCall {
            code: "return old".to_owned(),
            bindings,
            ..CodeCall::default()
        })
        .await
        .expect_err("`old` is not in scope here");
    let said = error.to_string();
    assert!(said.contains("NameError"), "{said}");
    assert!(said.contains("old"), "{said}");
}

// ---------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_five_extra_types_convert_and_a_set_does_not() {
    let runtime = PythonRuntime::new();
    let out = runtime
        .run(CodeCall {
            code: "\
import datetime, decimal, uuid
return {
    \"at\": datetime.datetime(2026, 9, 2, 14, 30),
    \"on\": datetime.date(2026, 9, 2),
    \"time\": datetime.time(14, 30),
    \"amount\": decimal.Decimal(\"12.50\"),
    \"id\": uuid.UUID(\"0b7f2b3e-1a2c-4d5e-8f90-1a2b3c4d5e6f\"),
    \"nested\": [True, None, 1.5, {\"k\": \"v\"}],
}"
            .to_owned(),
            ..CodeCall::default()
        })
        .await
        .unwrap();
    assert_eq!(out["at"], json!("2026-09-02T14:30:00"));
    assert_eq!(out["on"], json!("2026-09-02"));
    assert_eq!(out["time"], json!("14:30:00"));
    assert_eq!(out["amount"], json!(12.5));
    assert_eq!(out["id"], json!("0b7f2b3e-1a2c-4d5e-8f90-1a2b3c4d5e6f"));
    assert_eq!(out["nested"], json!([true, null, 1.5, { "k": "v" }]));

    // And a value with no JSON form is an error that names the **type** and the
    // **path to it**, rather than a `null` somebody finds three screens later.
    let error = runtime
        .run(CodeCall {
            code: "return {\"items\": [1, {2, 3}]}".to_owned(),
            ..CodeCall::default()
        })
        .await
        .expect_err("a set has no JSON form");
    let said = error.to_string();
    assert!(said.contains("`set`"), "the type: {said}");
    assert!(said.contains("result[\"items\"][1]"), "the path: {said}");
}

// ---------------------------------------------------------------------------
// The seam
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_call_crosses_and_a_refusal_is_catchable_at_the_call_site() {
    let host = EchoHost::new(0);
    let runtime = PythonRuntime::new();
    let out = runtime
        .run(call(
            "answer = __sc_db({\"op\": \"count\", \"table\": \"books\"})\nreturn answer[\"echo\"][\"table\"]",
            host.as_ref(),
        ))
        .await
        .unwrap();
    assert_eq!(out, json!("books"));
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);

    // A refusal arrives as `saltcorn.DbError` at the call site, so a body may
    // try a delegated write and fall back.
    let refusing = RefusingHost;
    let out = runtime
        .run(call(
            "import __sc\ntry:\n    __sc.__sc_db({\"op\": \"insert\"})\nexcept __sc.DbError as e:\n    return {\"refused\": str(e)}\nreturn {\"refused\": None}",
            &refusing,
        ))
        .await
        .unwrap();
    assert!(
        out["refused"]
            .as_str()
            .unwrap_or_default()
            .contains("the ownership rule refused this write"),
        "{out}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_surface_this_run_was_not_given_is_not_bound_at_all() {
    let runtime = PythonRuntime::new();
    let error = runtime
        .run(CodeCall {
            code: "return __sc_db({\"op\": \"count\"})".to_owned(),
            ..CodeCall::default()
        })
        .await
        .expect_err("a pure body has no database");
    let said = error.to_string();
    assert!(said.contains("NameError"), "{said}");
    assert!(said.contains("__sc_db"), "{said}");

    // And reached the long way round, through the module, it refuses by saying
    // what the run has rather than by failing somewhere else later.
    let error = runtime
        .run(CodeCall {
            code: "import __sc\nreturn __sc.__sc_db({\"op\": \"count\"})".to_owned(),
            ..CodeCall::default()
        })
        .await
        .expect_err("still no database");
    assert!(error.to_string().contains("no database access"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_call_budget_is_the_javascript_one_and_says_what_it_is_for() {
    let host = EchoHost::new(0);
    let runtime = PythonRuntime::new();
    let error = runtime
        .run(CodeCall {
            max_calls: 3,
            ..call(
                "for i in range(10):\n    __sc_db({\"i\": i})\nreturn \"done\"",
                host.as_ref(),
            )
        })
        .await
        .expect_err("four is more than three");
    let said = error.to_string();
    assert!(said.contains("more than 3 database calls"), "{said}");
    assert_eq!(
        host.calls.load(Ordering::SeqCst),
        3,
        "the fourth never left"
    );
}

// ---------------------------------------------------------------------------
// The concurrency claim — phase 0.2, turned into a test that stays
// ---------------------------------------------------------------------------

/// Eight runs blocked in a host call while eight more start, execute and finish.
///
/// This is the assertion the whole design rests on. If the bridge stopped
/// releasing the GIL nothing here would fail to *work* — it would merely become
/// serial, which is the failure a test has to be watching for, so the numbers
/// below are deliberately far apart: the blocked runs hold their thread for
/// 600 ms each, and eight of them serialised would be 4.8 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runs_blocked_in_a_host_call_do_not_block_anybody() {
    let host = EchoHost::new(600);
    let runtime = PythonRuntime::new();
    let started = Instant::now();

    let blocked = (0..8).map(|_| async {
        runtime
            .run(call(
                "return __sc_db({\"op\": \"slow\"})[\"echo\"][\"op\"]",
                host.as_ref(),
            ))
            .await
            .map(|out| (out, started.elapsed()))
    });
    // Started once the eight above are parked in their host call, and doing real
    // Python — which is what they cannot do if the blocked runs hold the GIL.
    let short = (0..8).map(|_| async {
        tokio::time::sleep(Duration::from_millis(80)).await;
        runtime
            .run(CodeCall {
                code: "total = 0\nfor i in range(20000):\n    total += i\nreturn total".to_owned(),
                ..CodeCall::default()
            })
            .await
            .map(|out| (out, started.elapsed()))
    });

    let (blocked, short) = futures::future::join(
        futures::future::join_all(blocked),
        futures::future::join_all(short),
    )
    .await;

    for outcome in &short {
        let (value, at) = outcome.as_ref().expect("a short run");
        assert_eq!(*value, json!(199_990_000));
        assert!(
            *at < Duration::from_millis(500),
            "a short run finished at {at:?}, which means it waited for the blocked ones"
        );
    }
    for outcome in &blocked {
        let (value, at) = outcome.as_ref().expect("a blocked run");
        assert_eq!(*value, json!("slow"));
        assert!(
            *at < Duration::from_millis(2000),
            "eight 600 ms host calls took {at:?}; serialised they would take 4.8 s"
        );
    }
    assert_eq!(host.calls.load(Ordering::SeqCst), 8);
    assert_eq!(runtime.stuck(), 0);
    // The thread cache is the other half of §1: eight resident runs are eight
    // threads, not sixteen, because the short runs reused them.
    assert!(
        runtime.threads() <= 16,
        "{} threads for sixteen runs, at most eight of them resident at once",
        runtime.threads()
    );
}

// ---------------------------------------------------------------------------
// The four bounds
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_runaway_loop_is_stopped_and_leaves_the_interpreter_unharmed() {
    let host = EchoHost::new(400);
    let runtime = PythonRuntime::new();

    // The runaway, and a co-resident run in a host call beside it. The point of
    // `SetAsyncExc` over V8's `terminate_execution` is that it names **one
    // thread**: the neighbour must come back with its answer.
    let runaway = runtime.run(CodeCall {
        code: "while True:\n    pass".to_owned(),
        timeout: Some(Duration::from_millis(300)),
        ..CodeCall::default()
    });
    let neighbour = runtime.run(call(
        "return __sc_db({\"op\": \"neighbour\"})[\"echo\"][\"op\"]",
        host.as_ref(),
    ));
    let (runaway, neighbour) = futures::future::join(runaway, neighbour).await;

    let said = runaway
        .expect_err("a `while True` has to be stopped")
        .to_string();
    assert!(said.contains("exceeded its 300 ms time limit"), "{said}");
    assert_eq!(
        neighbour.expect("the neighbour is unharmed"),
        json!("neighbour")
    );

    // The thread came back, so nothing is quarantined and the interpreter is
    // still the same interpreter.
    assert_eq!(runtime.stuck(), 0);
    let after = runtime
        .run(CodeCall {
            code: "return 1 + 1".to_owned(),
            ..CodeCall::default()
        })
        .await
        .expect("the interpreter is still there");
    assert_eq!(after, json!(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_deadline_is_not_an_ordinary_exception() {
    // `Timeout` derives from `BaseException` on purpose: a bare `except
    // Exception:` in somebody's retry loop must not swallow the run's deadline.
    let runtime = PythonRuntime::new();
    let error = runtime
        .run(CodeCall {
            code:
                "try:\n    while True:\n        pass\nexcept Exception:\n    return \"swallowed\""
                    .to_owned(),
            timeout: Some(Duration::from_millis(300)),
            ..CodeCall::default()
        })
        .await
        .expect_err("the deadline is not catchable that way");
    assert!(
        error.to_string().contains("exceeded its 300 ms time limit"),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_body_past_its_deadline_cannot_reach_a_host() {
    // §4's first instrument, and the one that matters most: whatever else is or
    // is not stoppable, a body past its deadline cannot write anything.
    let host = EchoHost::new(0);
    let runtime = PythonRuntime::new();
    let error = runtime
        .run(CodeCall {
            timeout: Some(Duration::from_millis(400)),
            ..call(
                "import time\n__sc_db({\"first\": True})\ntime.sleep(0.5)\n__sc_db({\"second\": True})\nreturn \"done\"",
                host.as_ref(),
            )
        })
        .await
        .expect_err("the second call is past the deadline");
    assert!(
        error.to_string().contains("exceeded its 400 ms time limit"),
        "{error}"
    );
    assert_eq!(
        host.calls.load(Ordering::SeqCst),
        1,
        "only the call made before the deadline reached the host"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_thread_that_cannot_be_stopped_is_quarantined_and_comes_back() {
    // What `SetAsyncExc` cannot reach: a thread inside a C call. `time.sleep` is
    // the cheap version of `numpy.linalg.inv` on a large matrix, and it fails
    // the same way — no bytecode boundary, so the exception is queued and never
    // delivered until the call returns.
    let runtime = PythonRuntime::with_bounds(4, 1);
    let error = runtime
        .run(CodeCall {
            code: "import time\ntime.sleep(1.5)\nreturn \"never\"".to_owned(),
            timeout: Some(Duration::from_millis(200)),
            ..CodeCall::default()
        })
        .await
        .expect_err("the caller stops waiting");
    assert!(
        error.to_string().contains("exceeded its 200 ms time limit"),
        "{error}"
    );
    assert_eq!(runtime.stuck(), 1, "the thread is counted as stuck");

    // At `--python-max-stuck` the runtime refuses new runs by name rather than
    // accumulating threads that will never come back.
    let refused = runtime
        .run(CodeCall {
            code: "return 1".to_owned(),
            ..CodeCall::default()
        })
        .await
        .expect_err("one stuck thread is this runtime's limit");
    let said = refused.to_string();
    assert!(said.contains("never returned"), "{said}");
    assert!(said.contains("restarted"), "{said}");

    // And this one does come back, so it stops being counted — a quarantine is
    // a report, not a grave.
    let waited = Instant::now();
    while runtime.stuck() > 0 && waited.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(runtime.stuck(), 0, "the sleeping thread returned");
    let out = runtime
        .run(CodeCall {
            code: "return 1".to_owned(),
            ..CodeCall::default()
        })
        .await
        .expect("runs are admitted again");
    assert_eq!(out, json!(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_nested_run_is_admitted_past_a_full_admission_bound() {
    // §6: the parent is holding the only slot while it waits for the child. A
    // runtime that made the child queue for it would deadlock, and only under
    // load — which is why the bound is one here rather than thirty-two.
    let runtime = Arc::new(PythonRuntime::with_bounds(1, 8));
    let host = NestingHost {
        runtime: Arc::clone(&runtime),
    };
    let out = runtime
        .run(CodeCall {
            timeout: Some(Duration::from_millis(3000)),
            ..call(
                "inner = __sc_db({\"code\": \"return 6 * 7\"})\nreturn {\"inner\": inner}",
                &host,
            )
        })
        .await
        .expect("the nested run is admitted past the bound its parent holds");
    assert_eq!(out, json!({ "inner": 42 }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_different_bodies_compile_at_once() {
    // A regression test with a specific deadlock behind it. The compiled-body
    // cache is a `Mutex` taken by a thread that is holding the GIL, and CPython
    // hands the GIL over between bytecodes — so a thread that compiled with the
    // lock held could lose the GIL mid-compile while a second thread, holding
    // the GIL, blocked on the lock. Neither could then proceed, and it took two
    // *different* bodies compiling at once to happen at all.
    let runtime = PythonRuntime::new();
    let bodies: Vec<String> = (0..16)
        .map(|n| format!("# body {n}\nvalues = [i * {n} for i in range(10)]\nreturn sum(values)"))
        .collect();
    let runs = bodies.iter().map(|code| {
        runtime.run(CodeCall {
            code: code.clone(),
            ..CodeCall::default()
        })
    });
    let outcomes = futures::future::join_all(runs).await;
    for (n, outcome) in outcomes.into_iter().enumerate() {
        assert_eq!(outcome.unwrap(), json!(45 * n));
    }
}

// ---------------------------------------------------------------------------
// The adapter
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_adapter_is_reached_by_its_language_name() {
    let runtime = PythonRuntime::new();
    let adapter: &dyn CodeAdapter = &runtime;
    assert_eq!(adapter.language(), "python");
    let out = adapter
        .run_code(CodeCall {
            code: "return \"through the trait\"".to_owned(),
            ..CodeCall::default()
        })
        .await
        .unwrap();
    assert_eq!(out, json!("through the trait"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_body_is_compiled_once_however_often_it_is_fired() {
    // The cache is per content key, and 35× is what it buys (phase 0.5). What is
    // asserted here is the consequence rather than the timing: a body fired a
    // hundred times answers the same thing every time and starts no more threads
    // than it has concurrent runs.
    let runtime = PythonRuntime::new();
    for _ in 0..100 {
        let out = runtime
            .run(CodeCall {
                code: "return sum(range(10))".to_owned(),
                ..CodeCall::default()
            })
            .await
            .unwrap();
        assert_eq!(out, json!(45));
    }
    assert_eq!(runtime.threads(), 1, "one at a time is one thread, reused");
}

// ---------------------------------------------------------------------------
// The flag (phase 2.5)
// ---------------------------------------------------------------------------

/// `--python off` on a binary that *has* an interpreter: every entry point
/// answers with the flag, and nothing starts.
///
/// The distinction this pins is the one an admin needs. "Built without Python"
/// is a rebuild, "off" is a restart, and "not initialised" is neither — so they
/// are three different sentences and three different states, rather than one
/// silence to be guessed at.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_process_told_not_to_start_an_interpreter_says_so_and_does_not() {
    let off = PythonRuntime::new().with_enabled(false);
    assert_eq!(off.state(), PythonState::Off);

    let said = off
        .run(CodeCall {
            code: "return 1".to_owned(),
            ..CodeCall::default()
        })
        .await
        .expect_err("a run must not start what the flag turned off")
        .to_string();
    assert!(said.contains("--python off"), "{said}");
    // The remedy, which is a restart rather than a rebuild.
    assert!(said.contains("--python auto"), "{said}");
    assert!(
        off.initialise()
            .expect_err("nor may an eager start")
            .to_string()
            .contains("--python off")
    );
    assert_eq!(off.threads(), 0, "nothing ran");

    // And the flag is this runtime's, not the process's: a runtime that was not
    // turned off is unaffected by one that was.
    let on = PythonRuntime::new();
    assert_ne!(on.state(), PythonState::Off);
    assert_eq!(
        on.run(CodeCall {
            code: "return 1".to_owned(),
            ..CodeCall::default()
        })
        .await
        .unwrap(),
        json!(1)
    );
}

/// The environment is recorded where the flags set it, so `--python-dir` and
/// `--python-bin` are one setting rather than one invented per caller. What it
/// is *used* for — the virtual environment, `pip`, and the ABI check between the
/// two interpreters — is phase 5.
#[test]
fn the_environment_flags_are_held_where_the_installer_will_read_them() {
    let plain = PythonRuntime::new();
    assert_eq!(plain.env(), &PythonEnv::default());
    assert!(plain.env().dir.is_none() && plain.env().bin.is_none());

    let configured = PythonRuntime::new().with_env(PythonEnv {
        dir: Some(std::path::PathBuf::from("/srv/python")),
        bin: Some(std::path::PathBuf::from("/usr/bin/python3.12")),
    });
    assert_eq!(
        configured.env().dir.as_deref(),
        Some(std::path::Path::new("/srv/python"))
    );
    assert_eq!(
        configured.env().bin.as_deref(),
        Some(std::path::Path::new("/usr/bin/python3.12"))
    );
}

/// The embedded interpreter does not import the **host's** packages.
///
/// It inherits the `sys.path` of the interpreter it was linked against, which on
/// an ordinary Linux box means the user's `site-packages` and up to three
/// `dist-packages` directories — phase 0.4 watched the spike import the system's
/// `numpy` from one without being asked. Left alone, what a body may import
/// would depend on what the operator had `apt install`ed, so the gate's
/// amendment to §9 is that this is **isolated** rather than prepended to.
///
/// The standard library stays, because it is the interpreter's own and is where
/// the import gate (phase 4.1) does its work. So does **this server's own**
/// environment, which since phase 5 has a default rather than existing only
/// where `--python-dir` named one: the whole point of it is that a body can
/// import what an admin installed, and the path it sits at is the one this
/// process would install into.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hosts_installed_packages_are_not_on_the_path() {
    let out = PythonRuntime::new()
        .run(CodeCall {
            code: "import sys\nimport json\nreturn {\"path\": sys.path, \"json\": json.dumps([1])}"
                .to_owned(),
            ..CodeCall::default()
        })
        .await
        .unwrap();
    let path: Vec<String> = out["path"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|entry| entry.as_str().unwrap_or_default().to_owned())
        .collect();
    // Everything this server would install into is its own; anything else that
    // holds packages is the host's.
    let ours = PythonEnv::default()
        .directory()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    for entry in &path {
        if !ours.is_empty() && entry.starts_with(&ours) {
            continue;
        }
        assert!(
            !entry.ends_with("site-packages") && !entry.ends_with("dist-packages"),
            "the host's packages are on the path: {path:?}"
        );
        // Nor the directory the server happened to be started in.
        assert!(!entry.is_empty() && entry != ".", "{path:?}");
    }
    // And the server's own environment *is* there, which is what makes an
    // installed Python module importable at all (§9).
    if !ours.is_empty() {
        assert!(
            path.iter().any(|entry| entry.starts_with(&ours)),
            "this server's own environment is not on the path: {path:?}"
        );
    }
    // And the standard library is still there, which is what the body needs and
    // what the import gate is about.
    assert!(
        path.iter().any(|entry| entry.contains("python")),
        "the standard library went with them: {path:?}"
    );
    assert_eq!(out["json"], json!("[1]"));
}
