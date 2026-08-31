//! What an idle engine costs: the queue's poll against a **counting** database
//! driver (§10.3, decision 5).
//!
//! The runnable set is a query, and before the catalog cached its answer it was
//! a query every five seconds forever — on a laptop, on a Pi, on a server with
//! no workflows in it at all. What is asserted here is the thing a benchmark
//! could not: not that the poll is fast, but that **it does not happen**.
//!
//! The driver is a real Postgres driver with a counter wrapped round it, so
//! every statement the queue would run is counted whether or not anybody thought
//! to look for it, and the clock is moved rather than waited on. No test here
//! sleeps.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sc_action::{EventKind, Trigger, TriggerBody, TriggerId};
use sc_agent::{Run, RunState, bootstrap_runs, save_run};
use sc_catalog::Catalog;
use sc_db::{
    DatabaseDriver, DbCapabilities, DescribedColumn, PhysicalTable, RowStream, SchemaChange,
    Transaction,
};
use sc_db_postgres::PgDriver;
use sc_error::Result;
use sc_query::{SqlDialect, Statement};
use sc_test_harness::TestDb;
use sc_workflow::machine::WorkflowRun;
use sc_workflow::queue::WorkQueue;
use sc_workflow::{
    DatabaseQueue, Step, StepKind, Workflow, bootstrap_run_traces, bootstrap_workflow_versions,
    new_run,
};

// ---------------------------------------------------------------------------
// The driver that counts.
// ---------------------------------------------------------------------------

/// A real driver with a tally of every statement that mentions the runs table.
///
/// Wrapping the driver rather than instrumenting the queue is the point: a
/// second query added to the poll one day is counted here without anybody
/// remembering to count it.
struct Counting {
    inner: Arc<dyn DatabaseDriver>,
    runs_queries: Arc<AtomicUsize>,
}

#[async_trait]
impl DatabaseDriver for Counting {
    async fn introspect(&self) -> Result<Vec<PhysicalTable>> {
        self.inner.introspect().await
    }

    async fn query(&self, stmt: &Statement) -> Result<RowStream> {
        if format!("{stmt:?}").contains(sc_agent::RUNS_TABLE) {
            self.runs_queries.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.query(stmt).await
    }

    async fn apply_schema(&self, change: &SchemaChange) -> Result<()> {
        self.inner.apply_schema(change).await
    }

    fn render_ddl(&self, change: &SchemaChange) -> Result<String> {
        self.inner.render_ddl(change)
    }

    async fn describe(&self, sql: &str, param_types: &[String]) -> Result<Vec<DescribedColumn>> {
        self.inner.describe(sql, param_types).await
    }

    async fn begin(&self) -> Result<Box<dyn Transaction>> {
        self.inner.begin().await
    }

    fn capabilities(&self) -> DbCapabilities {
        self.inner.capabilities()
    }

    fn dialect(&self) -> &dyn SqlDialect {
        self.inner.dialect()
    }
}

// ---------------------------------------------------------------------------
// The fixture.
// ---------------------------------------------------------------------------

/// A catalog with the workflow tables up, over a driver that counts.
async fn catalog(db: &TestDb) -> Result<(Arc<Catalog>, Arc<AtomicUsize>)> {
    let inner = Arc::new(PgDriver::from_pool(db.pool().clone())) as Arc<dyn DatabaseDriver>;
    let runs_queries = Arc::new(AtomicUsize::new(0));
    let counting = Arc::new(Counting {
        inner,
        runs_queries: Arc::clone(&runs_queries),
    }) as Arc<dyn DatabaseDriver>;
    let catalog = Catalog::init(counting).await?;
    bootstrap_workflow_versions(&catalog).await?;
    bootstrap_run_traces(&catalog).await?;
    bootstrap_runs(&catalog).await?;
    Ok((Arc::new(catalog), runs_queries))
}

fn queue(catalog: &Arc<Catalog>, rescan: Duration) -> DatabaseQueue {
    DatabaseQueue::new(
        Arc::clone(catalog),
        "node-a",
        std::time::Duration::from_millis(1),
    )
    .with_rescan(rescan)
}

/// A workflow run row that wants the engine at `wake_at`, written the way
/// everything writes one.
async fn waiting_run(catalog: &Catalog, wake_at: DateTime<Utc>) -> Result<Run> {
    let trigger = Trigger::with_body(
        TriggerId::new(),
        "sleeper",
        EventKind::Insert,
        TriggerBody::Workflow,
    )
    .on("orders");
    let workflow = Workflow::of(
        trigger.id,
        1,
        vec![Step::new(
            "one",
            StepKind::Action {
                action: "noop".to_owned(),
                configuration: Default::default(),
            },
        )],
    );
    let state = WorkflowRun::new(&workflow);
    let mut run = new_run(
        &trigger,
        1,
        &sc_action::Event::new(EventKind::Insert).on("orders"),
        vec!["sleeper".to_owned()],
        &state,
    );
    run.state = RunState::Waiting;
    run.wake_at = Some(wake_at);
    save_run(catalog, &run).await?;
    Ok(run)
}

/// Poll `times` times, a second apart, from `from` — and answer how many
/// statements against the runs table that cost.
async fn polls(
    queue: &DatabaseQueue,
    from: DateTime<Utc>,
    times: i64,
    counter: &AtomicUsize,
) -> Result<usize> {
    counter.store(0, Ordering::SeqCst);
    for tick in 0..times {
        let now = from + Duration::seconds(tick);
        assert!(
            queue
                .claim(now, now + Duration::seconds(60), 8)
                .await?
                .is_empty(),
            "nothing is due, so nothing may be claimed"
        );
    }
    Ok(counter.load(Ordering::SeqCst))
}

// ---------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_empty_database_is_asked_once_and_then_left_alone() -> Result<()> {
    let db = TestDb::new().await?;
    let (catalog, counter) = catalog(&db).await?;
    let queue = queue(&catalog, Duration::minutes(5));
    let now = Utc::now();

    // The first poll knows nothing, so it asks: the runnable set, and then —
    // because that was empty — when to bother next.
    counter.store(0, Ordering::SeqCst);
    assert!(
        queue
            .claim(now, now + Duration::seconds(60), 8)
            .await?
            .is_empty()
    );
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    assert!(catalog.run_wakeups().is_known());
    assert_eq!(catalog.run_wakeups().earliest(), None);

    // The next hundred polls cost nothing at all: that is a hundred round trips
    // an idle server no longer makes.
    let queries = polls(&queue, now + Duration::seconds(1), 100, &counter).await?;
    assert_eq!(
        queries, 0,
        "an idle engine must not ask the database once per poll"
    );

    // The trust window is the floor: once it has run out the database is asked
    // again — once — and the polls after that are free again.
    counter.store(0, Ordering::SeqCst);
    let after = now + Duration::minutes(5);
    assert!(
        queue
            .claim(after, after + Duration::seconds(60), 8)
            .await?
            .is_empty()
    );
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    assert_eq!(
        polls(&queue, after + Duration::seconds(1), 10, &counter).await?,
        0
    );
    Ok(())
}

#[tokio::test]
async fn a_run_that_wakes_tomorrow_costs_nothing_until_tomorrow() -> Result<()> {
    let db = TestDb::new().await?;
    let (catalog, counter) = catalog(&db).await?;
    let queue = queue(&catalog, Duration::days(365));
    let now = Utc::now();
    let tomorrow = now + Duration::days(1);
    waiting_run(&catalog, tomorrow).await?;

    // Writing the run told the catalog when it wants the engine, so the first
    // poll asks the database and learns the same thing from the authority.
    counter.store(0, Ordering::SeqCst);
    assert!(
        queue
            .claim(now, now + Duration::seconds(60), 8)
            .await?
            .is_empty()
    );
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    // The instant comes back at the column's precision rather than the clock's,
    // which is exactly why the cache is only ever allowed to be *early*.
    let earliest = catalog.run_wakeups().earliest().expect("an instant");
    assert!(
        (earliest - tomorrow).num_milliseconds().abs() <= 1,
        "{earliest} is not tomorrow ({tomorrow})"
    );

    // Every poll between now and then is free.
    let queries = polls(&queue, now + Duration::seconds(1), 200, &counter).await?;
    assert_eq!(queries, 0, "nothing is due for a day");

    // And when the day comes the run is claimed, because the cache only ever
    // said *when*, never *whether*.
    counter.store(0, Ordering::SeqCst);
    let claimed = queue
        .claim(tomorrow, tomorrow + Duration::seconds(60), 8)
        .await?;
    assert_eq!(claimed.len(), 1, "the deadline arrived");
    assert!(counter.load(Ordering::SeqCst) > 0);
    Ok(())
}

#[tokio::test]
async fn a_run_written_while_the_engine_is_quiet_is_still_picked_up() -> Result<()> {
    let db = TestDb::new().await?;
    let (catalog, counter) = catalog(&db).await?;
    let queue = queue(&catalog, Duration::days(365));
    let now = Utc::now();

    // Go quiet: the database is empty and the engine knows it.
    assert!(
        queue
            .claim(now, now + Duration::seconds(60), 8)
            .await?
            .is_empty()
    );
    let quiet = polls(&queue, now + Duration::seconds(1), 10, &counter).await?;
    assert_eq!(quiet, 0);

    // A trigger fires and a run is written — the write itself is what tells the
    // catalog, so the very next poll claims it with no rescan and no waiting for
    // the trust window.
    let run = waiting_run(&catalog, now + Duration::seconds(30)).await?;
    let claimed = queue
        .claim(now + Duration::seconds(30), now + Duration::seconds(90), 8)
        .await?;
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, run.id);
    Ok(())
}

#[tokio::test]
async fn another_process_is_noticed_when_the_trust_window_ends() -> Result<()> {
    let db = TestDb::new().await?;
    let (catalog, counter) = catalog(&db).await?;
    let queue = queue(&catalog, Duration::minutes(5));
    let now = Utc::now();

    assert!(
        queue
            .claim(now, now + Duration::seconds(60), 8)
            .await?
            .is_empty()
    );
    assert_eq!(
        polls(&queue, now + Duration::seconds(1), 10, &counter).await?,
        0
    );

    // A *second* process writes a run against the same database. Nothing tells
    // this one — there is no bus — so its cache is wrong, and the trust window
    // is the whole of what bounds how long it stays wrong.
    let second =
        Catalog::init(Arc::new(PgDriver::from_pool(db.pool().clone())) as Arc<dyn DatabaseDriver>)
            .await?;
    let run = waiting_run(&second, now).await?;

    // Inside the window: still quiet, still wrong.
    assert!(
        queue
            .claim(now + Duration::minutes(1), now + Duration::minutes(2), 8)
            .await?
            .is_empty()
    );

    // Once it ends the database is asked again, and the run is claimed.
    let claimed = queue
        .claim(now + Duration::minutes(5), now + Duration::minutes(6), 8)
        .await?;
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, run.id);
    Ok(())
}
