//! [`RunWakeups`]: whether anything in this database wants the workflow engine,
//! and when — the process's answer to a question it would otherwise ask the
//! database every few seconds.
//!
//! The engine's queue is a query (`sc-workflow`'s [`queue`]), and a query that
//! runs every poll is a query that runs forever on a deployment with no runs at
//! all: a laptop, a Raspberry Pi and an idle server all paid a round trip every
//! five seconds to be told "nothing". The runs table is the **authority**; this
//! is a cache of one fact about it —
//!
//! ```text
//! the earliest instant at which some live run might want the engine
//! ```
//!
//! — held beside the table cache for the same reason the table cache is held
//! here: it is a fact about the connected database that several layers need and
//! only one of them can maintain.
//!
//! ## The invariant, and why it is one-sided
//!
//! **The cached instant is never later than the truth.** Everything follows from
//! that:
//!
//! - Too *early* costs one query, which finds nothing due, rescans, and goes
//!   quiet again. Self-correcting.
//! - Too *late* is a run that never wakes. Unacceptable.
//!
//! So [`note`](RunWakeups::note) only ever moves the instant **earlier**, a run
//! written with no `wake_at` (it finished, or it is waiting on a person) moves
//! nothing, and the only thing that may move it later is
//! [`scanned`](RunWakeups::scanned) — the database's own answer.
//!
//! ## What it does not know
//!
//! Another process that starts a run — a second node, a command-line trigger
//! against the same database — writes a row this cache never hears about, so the
//! trust window ([`due_by`](RunWakeups::due_by)'s `trust_for`) puts a floor under
//! how long a quiet process can stay wrong: one query per window instead of one
//! per poll. Cross-process invalidation is the bus's job, and it is the same seam
//! as `WorkQueue::wake` — when it exists, a `NOTIFY` calls [`note`] and the
//! window can grow.
//!
//! [`queue`]: https://docs.rs/sc-workflow
//! [`note`]: RunWakeups::note

use std::sync::RwLock;

use chrono::{DateTime, Duration, Utc};

/// What this process believes about runs that want the engine.
///
/// Cheap to consult (one `RwLock` read), safe to be wrong in one direction only
/// (see the module docs), and **not** a queue: it answers "is it worth asking the
/// database", never "which runs".
#[derive(Debug, Default)]
pub struct RunWakeups {
    state: RwLock<Wakeups>,
}

#[derive(Debug, Default)]
struct Wakeups {
    /// When the last scan was taken, `None` for "never asked" — which is what a
    /// freshly booted process is, and why its first poll always queries.
    scanned_at: Option<DateTime<Utc>>,
    /// The earliest instant a live run might want the engine; `None` beside a
    /// `scanned_at` means the database was asked and had nothing.
    earliest: Option<DateTime<Utc>>,
    /// Bumped by everything that makes a scan in flight out of date, so a scan
    /// that started before a run was written cannot overwrite what that run
    /// said. Without it the sequence "read the table, a run is inserted, store
    /// the read" would lose the insert and the run would sleep forever.
    generation: u64,
}

impl RunWakeups {
    /// A cache that knows nothing, which is the honest state at boot.
    pub fn new() -> RunWakeups {
        RunWakeups::default()
    }

    /// A run has been written that wants the engine at `at`.
    ///
    /// Only ever moves the cached instant earlier: the caller is telling us about
    /// one run, and a later instant than the one we hold says nothing about the
    /// run that instant came from.
    pub fn note(&self, at: DateTime<Utc>) {
        if let Ok(mut state) = self.state.write() {
            state.generation = state.generation.wrapping_add(1);
            if state.earliest.is_none_or(|held| at < held) {
                state.earliest = Some(at);
            }
        }
    }

    /// Forget everything: the next question goes to the database.
    ///
    /// For a caller that knows the cache may be wrong and cannot say how — a
    /// restored dump, a database reconnected under the process.
    pub fn forget(&self) {
        if let Ok(mut state) = self.state.write() {
            state.generation = state.generation.wrapping_add(1);
            state.scanned_at = None;
            state.earliest = None;
        }
    }

    /// Take the generation a scan is about to read the database at.
    ///
    /// Pass it back to [`scanned`](RunWakeups::scanned); anything that happened
    /// in between makes the scan's answer stale and it is dropped.
    pub fn begin_scan(&self) -> u64 {
        self.state.read().map(|state| state.generation).unwrap_or(0)
    }

    /// The database's own answer, as of `at`: the earliest instant any live run
    /// wants the engine, or `None` if none does.
    ///
    /// Ignored if anything was noted since `scan` was taken, because the scan
    /// cannot have seen it.
    pub fn scanned(&self, scan: u64, at: DateTime<Utc>, earliest: Option<DateTime<Utc>>) {
        if let Ok(mut state) = self.state.write()
            && state.generation == scan
        {
            state.scanned_at = Some(at);
            state.earliest = earliest;
        }
    }

    /// Is it worth asking the database at `now`, given that a scan is trusted for
    /// `trust_for`?
    ///
    /// True when nothing is known, when what is known is older than the window,
    /// or when the instant we hold has arrived. A poisoned lock answers true:
    /// asking the database one extra time is the safe reading of "this cache is
    /// broken".
    pub fn due_by(&self, now: DateTime<Utc>, trust_for: Duration) -> bool {
        let Ok(state) = self.state.read() else {
            return true;
        };
        let Some(scanned_at) = state.scanned_at else {
            return true;
        };
        let age = now.signed_duration_since(scanned_at);
        // A clock that went backwards (a test's manual one, an NTP correction)
        // is a cache of unknown age, not a fresh one.
        if age >= trust_for || age < Duration::zero() {
            return true;
        }
        state.earliest.is_some_and(|at| at <= now)
    }

    /// The earliest instant this process believes a run wants the engine.
    ///
    /// `None` means either "nothing does" or "nobody has asked" —
    /// [`is_known`](RunWakeups::is_known) tells those apart. For an admin screen
    /// and for tests; the engine asks [`due_by`](RunWakeups::due_by).
    pub fn earliest(&self) -> Option<DateTime<Utc>> {
        self.state.read().ok().and_then(|state| state.earliest)
    }

    /// Whether the database has been asked at all since the last
    /// [`forget`](RunWakeups::forget).
    pub fn is_known(&self) -> bool {
        self.state
            .read()
            .is_ok_and(|state| state.scanned_at.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).expect("a timestamp")
    }

    #[test]
    fn a_cache_that_knows_nothing_always_says_ask() {
        let wakeups = RunWakeups::new();
        assert!(!wakeups.is_known());
        assert!(wakeups.due_by(t(0), Duration::minutes(5)));
    }

    #[test]
    fn an_empty_scan_makes_the_process_quiet() {
        let wakeups = RunWakeups::new();
        let scan = wakeups.begin_scan();
        wakeups.scanned(scan, t(0), None);
        assert!(wakeups.is_known());
        assert!(!wakeups.due_by(t(1), Duration::minutes(5)));
        // ...until the trust window runs out, which is the floor under how long
        // another process's run can go unnoticed.
        assert!(wakeups.due_by(t(300), Duration::minutes(5)));
    }

    #[test]
    fn a_scanned_instant_is_quiet_until_it_arrives() {
        let wakeups = RunWakeups::new();
        let scan = wakeups.begin_scan();
        wakeups.scanned(scan, t(0), Some(t(60)));
        assert!(!wakeups.due_by(t(59), Duration::minutes(5)));
        assert!(wakeups.due_by(t(60), Duration::minutes(5)));
    }

    #[test]
    fn a_note_only_moves_the_instant_earlier() {
        let wakeups = RunWakeups::new();
        let scan = wakeups.begin_scan();
        wakeups.scanned(scan, t(0), Some(t(60)));
        wakeups.note(t(90));
        assert_eq!(wakeups.earliest(), Some(t(60)));
        wakeups.note(t(10));
        assert_eq!(wakeups.earliest(), Some(t(10)));
        assert!(wakeups.due_by(t(10), Duration::minutes(5)));
    }

    #[test]
    fn a_note_during_a_scan_is_not_lost() {
        let wakeups = RunWakeups::new();
        let scan = wakeups.begin_scan();
        // The run is written while the scan's query is in flight, so the scan
        // could not have seen it. Its answer — "nothing wants the engine" —
        // would put this run to sleep for good.
        wakeups.note(t(30));
        wakeups.scanned(scan, t(0), None);
        assert_eq!(wakeups.earliest(), Some(t(30)));
        assert!(wakeups.due_by(t(30), Duration::minutes(5)));
    }

    #[test]
    fn a_backwards_clock_is_a_cache_of_unknown_age() {
        let wakeups = RunWakeups::new();
        let scan = wakeups.begin_scan();
        wakeups.scanned(scan, t(600), None);
        assert!(wakeups.due_by(t(0), Duration::minutes(5)));
    }

    #[test]
    fn forgetting_goes_back_to_asking() {
        let wakeups = RunWakeups::new();
        let scan = wakeups.begin_scan();
        wakeups.scanned(scan, t(0), None);
        wakeups.forget();
        assert!(!wakeups.is_known());
        assert!(wakeups.due_by(t(1), Duration::minutes(5)));
    }
}
