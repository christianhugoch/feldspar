//! `reload` against a **real Postgres**, and the observer it notifies (TODO
//! tasks 3.2 and 3.5).
//!
//! The diff itself is asserted without a database in `supervisor.rs`, over
//! `apply`. What is here is the half that only the real door has: `reload` is
//! what every writer of a stream calls afterwards — an admin's save, a delete,
//! a `SIGHUP`, a module change — and it is therefore the one place the
//! [`StreamObserver`] can be notified without every writer having to remember
//! to. Asserting that through a constructed `Vec<Stream>` would assert the part
//! that was never in doubt.
//!
//! The two facts worth a database:
//!
//! - **What is stored is what runs.** A saved row is subscribed to, a deleted
//!   one is hung up on, and a disabled one is stopped but still listed — read
//!   back through `list_streams`, so the enabled flag surviving a JSON column
//!   is part of what is being asserted rather than assumed.
//! - **The observer fires only when the set moved.** A `SIGHUP` reloads
//!   everything and mostly changes nothing, and re-projecting every application
//!   that exposes a stream is not free.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::Result;
use sc_stream::supervisor::{StreamConfig, StreamSupervisor};
use sc_stream::testing::ScriptedProvider;
use sc_stream::{
    ElementType, Stream, StreamObserver, StreamRegistry, StreamStatus, bootstrap_streams,
    delete_stream, save_stream,
};
use sc_test_harness::TestDb;
use sc_types::{BasicType, FormField, TypeRef};

/// A catalog over a per-test database with `_fd_streams` bootstrapped.
async fn setup(db: &TestDb) -> Result<Catalog> {
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let cat = Catalog::init(driver as Arc<dyn DatabaseDriver>).await?;
    bootstrap_streams(&cat).await?;
    Ok(cat)
}

/// A registry holding one scripted provider, and the provider itself so a test
/// can count what the supervisor did to it.
fn registry() -> (Arc<StreamRegistry>, Arc<ScriptedProvider>) {
    let provider = Arc::new(
        // It declares a `topic`, because these streams go through
        // `save_stream` — which validates the configuration against the
        // provider's own spec before it writes anything.
        ScriptedProvider::new("scripted", ElementType::text()).config(vec![FormField::new(
            "topic",
            TypeRef::Basic(BasicType::Text),
        )]),
    );
    let mut registry = StreamRegistry::new();
    registry
        .register(Arc::clone(&provider) as Arc<_>)
        .expect("a fresh registry takes its first provider");
    (Arc::new(registry), provider)
}

/// An observer that counts how many times it was told the set moved.
#[derive(Default)]
struct Counting {
    calls: AtomicUsize,
}

impl Counting {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl StreamObserver for Counting {
    fn streams_changed(&self, _catalog: &Catalog) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// An observer that cannot do its job — a mount that will not rebuild.
struct Refusing;

impl StreamObserver for Refusing {
    fn streams_changed(&self, _catalog: &Catalog) -> Result<()> {
        Err(sc_error::Error::msg("the mount could not be rebuilt"))
    }
}

#[tokio::test]
async fn what_is_stored_is_what_runs_and_the_observer_hears_about_each_change() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;
    let (registry, provider) = registry();
    let supervisor = Arc::new(StreamSupervisor::new(
        registry.clone(),
        StreamConfig::default(),
    ));
    let observer = Arc::new(Counting::default());
    supervisor.set_observer(Arc::clone(&observer) as Arc<_>);

    // Nothing stored: a reload is a no-op, and the observer is not woken for
    // it.
    assert!(!supervisor.reload(&catalog).await?);
    assert_eq!(observer.calls(), 0);

    let boiler = Stream::new("boiler", "scripted").config("topic", "house/boiler/#");
    save_stream(&catalog, &registry, &boiler).await?;

    assert!(supervisor.reload(&catalog).await?, "a new row starts");
    assert_eq!(observer.calls(), 1);
    assert_eq!(provider.subscribes(), 1);
    let running = supervisor.by_name("boiler").expect("subscribed");
    assert!(running.status().is_running());
    assert_eq!(
        running.element_type(),
        Some(&ElementType::text()),
        "the element type is resolved once and cached on the running stream (§5)"
    );

    // A second reload over an unchanged set does nothing, and says so. A
    // `SIGHUP` must not drop every broker session in the process.
    assert!(!supervisor.reload(&catalog).await?);
    assert_eq!(observer.calls(), 1);
    assert_eq!(provider.subscribes(), 1);

    // Switched off: hung up on, still listed.
    let mut off = boiler.clone();
    off.set_enabled(false);
    save_stream(&catalog, &registry, &off).await?;
    assert!(supervisor.reload(&catalog).await?);
    assert_eq!(observer.calls(), 2);
    assert_eq!(
        supervisor.by_name("boiler").map(|r| r.status()),
        Some(StreamStatus::Stopped)
    );

    // Switched back on: a new connection.
    save_stream(&catalog, &registry, &boiler).await?;
    assert!(supervisor.reload(&catalog).await?);
    assert_eq!(observer.calls(), 3);
    assert_eq!(provider.subscribes(), 2);

    // Deleted: hung up on and forgotten.
    assert!(delete_stream(&catalog, boiler.id, &[]).await?);
    assert!(supervisor.reload(&catalog).await?);
    assert_eq!(observer.calls(), 4);
    assert!(supervisor.streams().is_empty());

    Ok(())
}

#[tokio::test]
async fn a_reload_survives_an_observer_that_cannot_do_its_job() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;
    let (registry, provider) = registry();
    let supervisor = Arc::new(StreamSupervisor::new(
        registry.clone(),
        StreamConfig::default(),
    ));
    supervisor.set_observer(Arc::new(Refusing) as Arc<_>);

    save_stream(&catalog, &registry, &Stream::new("boiler", "scripted")).await?;

    // The stream *is* subscribed by the time the observer runs, so a failure
    // there is an application that keeps its previous mount — never a reload
    // that is reported as having failed, which would be the opposite of what
    // happened.
    assert!(supervisor.reload(&catalog).await?);
    assert_eq!(provider.subscribes(), 1);
    assert!(
        supervisor
            .by_name("boiler")
            .is_some_and(|r| r.status().is_running())
    );
    Ok(())
}
