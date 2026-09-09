//! `_fd_runs`' **workflow** columns against a real Postgres (§10.3, phase 1.5).
//!
//! The agent half of this table is exercised by `agent_loop.rs`, which is where
//! the loop writes it. What is here is the four columns the durable engine
//! added — the version a run is pinned to, when it next wants the engine, and the
//! lease that says a node is working on it — plus the `waiting` state, because
//! all five are the queue's definition of "runnable" and a column that does not
//! round-trip would make that definition read the wrong runs.

use crate::common;

use chrono::{Duration, Utc};
use common::{Counter, catalog};
use sc_agent::{AgentLoop, Run, RunCaller, RunKind, RunState, load_run, save_run};
use sc_error::Result;
use sc_test_harness::TestDb;

#[tokio::test]
async fn a_workflow_runs_pinning_lease_and_wake_time_round_trip() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;
    let _ = Counter::new();

    let mut run = Run::new("approve_order", &RunCaller::system(), &AgentLoop::new(20));
    run.kind = RunKind::Workflow;
    run.state = RunState::Waiting;
    run.subject_version = Some(2);
    let wake = Utc::now() + Duration::seconds(30);
    run.wake_at = Some(wake);
    let lease = Utc::now() + Duration::seconds(5);
    run.lease_until = Some(lease);
    run.claimed_by = Some("node-a".to_owned());
    save_run(&catalog, &run).await?;

    let loaded = load_run(&catalog, run.id).await?.expect("by id");
    assert_eq!(loaded.kind, RunKind::Workflow);
    assert_eq!(loaded.state, RunState::Waiting);
    assert_eq!(loaded.subject_version, Some(2));
    assert_eq!(loaded.claimed_by.as_deref(), Some("node-a"));
    // Instants survive the round trip to the microsecond, which is Postgres's
    // own resolution — a nanosecond of the original is rounded away, and the
    // queue compares these at the second.
    let within_a_microsecond = |a: Option<chrono::DateTime<Utc>>, b: chrono::DateTime<Utc>| {
        (a.expect("stored") - b)
            .num_microseconds()
            .unwrap_or(1000)
            .abs()
            <= 1
    };
    assert!(within_a_microsecond(loaded.wake_at, wake));
    assert!(within_a_microsecond(loaded.lease_until, lease));
    // Waiting is **live**: the run has not finished, it is between steps.
    assert!(loaded.state.is_live());

    // Releasing the lease and finishing writes NULLs rather than leaving a
    // claim behind — a lease nobody holds is what lets another node pick a run
    // up, so "no claim" has to be storable.
    let mut done = loaded;
    done.lease_until = None;
    done.claimed_by = None;
    done.wake_at = None;
    done.state = RunState::Done;
    save_run(&catalog, &done).await?;
    let finished = load_run(&catalog, done.id).await?.expect("by id");
    assert_eq!(finished.lease_until, None);
    assert_eq!(finished.claimed_by, None);
    assert_eq!(finished.wake_at, None);
    assert!(!finished.state.is_live());
    Ok(())
}

#[tokio::test]
async fn an_agent_run_leaves_every_engine_column_null() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;

    let run = Run::new("librarian", &RunCaller::system(), &AgentLoop::new(20));
    save_run(&catalog, &run).await?;
    let loaded = load_run(&catalog, run.id).await?.expect("by id");
    // The loop is driven by whoever started it, so an agent run is pinned to
    // nothing, wants the engine at no time and is claimed by nobody.
    assert_eq!(loaded.subject_version, None);
    assert_eq!(loaded.wake_at, None);
    assert_eq!(loaded.lease_until, None);
    assert_eq!(loaded.claimed_by, None);
    assert_eq!(loaded.kind, RunKind::Agent);
    assert_eq!(loaded.subject, run.subject);
    assert_eq!(loaded.context, run.context);
    Ok(())
}
