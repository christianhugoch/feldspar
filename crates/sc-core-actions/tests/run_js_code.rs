//! The `run_js_code` action against the real V8 engine (TODO Phase 3).
//!
//! Pinned against a `DenoEvaluator` rather than a stub, because everything that
//! can be wrong here is inside the isolate: whether statements and a `return`
//! actually run, what the event binds and what it deliberately does not, what a
//! throw or a runaway loop does to the caller — and, the point of the action's
//! bound, that the code **cannot reach the host**. A mock evaluator would assert
//! those against an isolate that never existed.
//!
//! The catalog is real too (one table, one row), so the sandbox assertion is made
//! in a process that genuinely has a database to reach.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use sc_action::{ActionContext, Event, EventKind, Trigger, validate_trigger};
use sc_catalog::Catalog;
use sc_core_actions::builtin_actions;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::{Error, Result};
use sc_expr::{DenoEvaluator, JsEvaluator};
use sc_test_harness::TestDb;
use sc_types::Attrs;

use serde_json::{Value as Json, json};

/// One table with one row in it: something for the sandbox to fail to reach.
const SCHEMA: &str = "CREATE TABLE books (id bigint primary key, title text, pages bigint);\
                      INSERT INTO books VALUES (7, 'A Book', 100);";

async fn setup(db: &TestDb) -> Result<Catalog> {
    db.client()
        .await?
        .batch_execute(SCHEMA)
        .await
        .map_err(|e| Error::database(e.to_string()))?;
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    Catalog::init(driver as Arc<dyn DatabaseDriver>).await
}

/// The event most tests fire for: an insert of a book by a known admin.
fn book_insert() -> Event {
    Event::new(EventKind::Insert)
        .on("books")
        .row(json!({ "id": 7, "title": "A Book", "pages": 100 }))
        .caller(1, Some(json!({ "email": "admin@example.com", "id": 3 })))
}

fn config(code: &str) -> Attrs {
    [("code".to_owned(), json!(code))].into_iter().collect()
}

/// Run `run_js_code` through the registry — the path a firing trigger takes.
async fn run(catalog: &Catalog, event: &Event, code: &str) -> Result<Json> {
    run_with(catalog, event, code, &Arc::new(DenoEvaluator::new())).await
}

async fn run_with(
    catalog: &Catalog,
    event: &Event,
    code: &str,
    engine: &Arc<DenoEvaluator>,
) -> Result<Json> {
    let engine: Arc<dyn JsEvaluator> = engine.clone();
    let registry = builtin_actions()?;
    let action = registry.require("run_js_code")?.clone();
    let cfg = config(code);
    let mut ctx = ActionContext::new(catalog, event, &cfg, "compute").with_evaluator(&engine);
    action.run(&mut ctx).await
}

#[tokio::test]
async fn code_computes_a_result_from_the_event() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;

    // Statements, a loop, a local declaration and a `return` — everything a
    // formula (one expression) cannot be, which is what this action is for.
    let result = run(
        &catalog,
        &book_insert(),
        "const words = row.title.split(' ');\n\
         let initials = '';\n\
         for (const word of words) { initials += word[0]; }\n\
         return { initials, per_page: row.pages / words.length, by: user.email };",
    )
    .await?;
    assert_eq!(
        result,
        // 100 / 2 is 50, and JSON says so: a JS number that is integral comes
        // back as an integer, not as `50.0`.
        json!({ "initials": "AB", "per_page": 50, "by": "admin@example.com" })
    );

    // The result is whatever JSON the body returns — including a scalar, and
    // including nothing at all (an action that only had an effect).
    assert_eq!(
        run(&catalog, &book_insert(), "return row.id * 6;").await?,
        json!(42)
    );
    assert_eq!(
        run(&catalog, &book_insert(), "const x = 1;").await?,
        Json::Null
    );
    Ok(())
}

#[tokio::test]
async fn the_event_binds_what_it_has_and_nothing_else() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;

    // On an insert `old` is in scope *and null* — a value the code can branch on,
    // which is the whole reason presence and nullity are different things here.
    assert_eq!(
        run(&catalog, &book_insert(), "return old === null;").await?,
        json!(true)
    );
    // On an update it is the row that was replaced.
    let update = Event::new(EventKind::Update)
        .on("books")
        .row(json!({ "id": 7, "title": "A Book", "pages": 120 }))
        .old_row(json!({ "id": 7, "title": "A Book", "pages": 100 }));
    assert_eq!(
        run(&catalog, &update, "return row.pages - old.pages;").await?,
        json!(20)
    );

    // A `none` trigger: the payload it was called with is in scope, and `user` is
    // null when nobody is logged in.
    let called = Event::new(EventKind::None).payload(json!({ "n": 5 }));
    assert_eq!(
        run(
            &catalog,
            &called,
            "return { double: payload.n * 2, who: user };"
        )
        .await?,
        json!({ "double": 10, "who": Json::Null })
    );
    // …and naming a row it does not have is an error naming `row`, not the
    // silent `undefined` that would make the trigger quietly do the wrong thing.
    let err = run(&catalog, &called, "return row.id;").await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("compute") && msg.contains("`code`"), "{msg}");
    assert!(msg.contains("row"), "{msg}");
    Ok(())
}

#[tokio::test]
async fn the_code_cannot_reach_the_host() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;

    // The bound this action ships with: no host API, so a body cannot read or
    // write the catalog, the network or the disk — even though this process has
    // all three, and a `books` table with a row in it. Catalog access from guest
    // code is `sc-code`'s milestone (§15); this is its seed, not a preview.
    let probes = [
        "Deno",
        "fetch",
        "require",
        "process",
        "XMLHttpRequest",
        "WebSocket",
        "globalThis.saltcorn",
        "globalThis.books",
    ];
    for probe in probes {
        let code = format!("return typeof {probe} === 'undefined';");
        assert_eq!(
            run(&catalog, &book_insert(), &code).await?,
            json!(true),
            "sandbox leak: {probe}"
        );
    }
    // What the code *can* see is exactly what the event bound: the row it was
    // handed, not the table it came from.
    assert_eq!(
        run(&catalog, &book_insert(), "return Object.keys(row);").await?,
        json!(["id", "title", "pages"])
    );
    Ok(())
}

#[tokio::test]
async fn a_failing_body_is_an_application_error_naming_the_trigger() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;

    for (code, expected) in [
        ("throw new Error('no good');", "no good"),
        ("return row.title.nope();", "not a function"),
        // Syntax is not checkable on save (there is no engine there), so a typo
        // surfaces here — as an error naming the trigger and the setting.
        ("retrun 1;", "SyntaxError"),
    ] {
        let err = run(&catalog, &book_insert(), code).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("compute") && msg.contains("`code`"), "{msg}");
        assert!(msg.contains(expected), "expected `{expected}` in: {msg}");
        // The admin's code failing is a fact about their configuration, not a
        // bug in the server.
        assert_eq!(err.kind(), sc_error::ErrorKind::Application, "{msg}");
    }
    Ok(())
}

#[tokio::test]
async fn a_runaway_body_is_terminated_and_the_engine_survives() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;
    // The code runtime's per-run timeout, which is the code pool's own and not
    // the formula watchdog's: a code body runs on its own isolates (decision 1),
    // so bounding it does not bound every ownership check in the process. The
    // production default is `DEFAULT_CODE_TIMEOUT`; the test wants it in
    // milliseconds so the assertion below is quick.
    let engine = Arc::new(DenoEvaluator::new().with_code_timeout(Duration::from_millis(100)));

    let started = Instant::now();
    let err = run_with(&catalog, &book_insert(), "while (true) {}", &engine)
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    let msg = err.to_string();
    assert!(
        msg.contains("timed out") && msg.contains("compute"),
        "{msg}"
    );
    // The caller is released, not held by the loop.
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");

    // And the isolate is usable afterwards: one trigger's mistake does not take
    // the process's only JavaScript engine with it.
    assert_eq!(
        run_with(&catalog, &book_insert(), "return row.id;", &engine).await?,
        json!(7)
    );
    Ok(())
}

#[tokio::test]
async fn a_body_with_no_code_in_it_is_refused_on_save() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;
    let registry = builtin_actions()?;

    let trigger = |cfg: Attrs| {
        Trigger::new("compute", EventKind::Insert, "run_js_code")
            .on("books")
            .configuration(cfg)
    };

    // Absent (the generic spec check) and blank (the action's own): both are a
    // trigger that would fire and do nothing.
    for cfg in [Attrs::new(), config("   ")] {
        let err = validate_trigger(&catalog, &registry, &trigger(cfg))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("compute") && msg.contains("code"), "{msg}");
        assert!(msg.contains("required"), "{msg}");
        assert_eq!(err.kind(), sc_error::ErrorKind::Application, "{msg}");
    }

    // A body that has code in it saves — including on an event with no row,
    // since only the engine can say what a body means.
    validate_trigger(&catalog, &registry, &trigger(config("return row.id;"))).await?;
    let login = Trigger::new("compute", EventKind::Login, "run_js_code")
        .configuration(config("return user.email;"));
    validate_trigger(&catalog, &registry, &login).await?;
    Ok(())
}

/// A trigger configured to run code, in a context with no JavaScript engine at
/// all (client generation, a test harness), must say so rather than skip.
#[tokio::test]
async fn without_an_engine_the_action_fails_by_name() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;
    let registry = builtin_actions()?;
    let action = registry.require("run_js_code")?.clone();
    let event = book_insert();
    let cfg = config("return 1;");
    let mut ctx = ActionContext::new(&catalog, &event, &cfg, "compute");

    let err = action.run(&mut ctx).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("compute") && msg.contains("JavaScript engine"),
        "{msg}"
    );
    Ok(())
}
