//! The stream-change seam: how a saved, disabled or deleted stream becomes
//! something the rest of the process can react to (TODO task 3.5; §2's third
//! seam).
//!
//! `sc-action`'s `TriggerObserver`'s exact sibling, for its
//! exact reason. A **mounted application** projects the streams it exposes at
//! mount time (§10) — a socket route per exposed stream, each carrying that
//! stream's `min_role`, and a generated client typed from its element type. So
//! a stream created, renamed, re-roled or deleted underneath it leaves the
//! mount describing a stream set that is no longer the one there is, until a
//! restart.
//!
//! The fix that does not work is having each admin handler call the mount
//! registry itself: it holds only for as long as *every* writer is a handler,
//! and this tree already has writers that are not — a `SIGHUP` reload, a module
//! change that brings a provider back, and in time an agent. So the
//! notification lives where the change takes effect —
//! [`StreamSupervisor::reload`](crate::StreamSupervisor::reload), the one thing
//! every writer calls afterwards — behind a seam the mount registry installs
//! once at boot. A process with no observer (the CLI, a test) simply changes
//! its streams unobserved.
//!
//! ## Only when the set actually moved
//!
//! `reload` is called after every save, every delete and every `SIGHUP`, and
//! most of those change nothing: the diff finds the same rows it had. The
//! observer is notified only when the diff *did* something, because
//! re-projecting every application that exposes a stream is not free and a
//! `SIGHUP` that changed nothing must not cost it.

use sc_catalog::Catalog;
use sc_error::Result;

/// What it means to observe changes to the stream set. `sc-server`'s mount
/// registry is the implementation; the supervisor holds it as `dyn`.
///
/// No "what changed" argument, for
/// `sc-action`'s `TriggerObserver`'s reason: every consumer
/// re-projects every app that exposes any stream anyway, because a **rename**
/// changes two names at once and an app naming either is affected. An enum here
/// would be a distinction nobody branches on.
pub trait StreamObserver: Send + Sync {
    /// React to a stream set that has already been reloaded.
    ///
    /// Called **after** the new set is live and the subscriptions have been
    /// started and stopped, so an observer reads it from the supervisor (or the
    /// catalog) simply by asking. An `Err` means the *reaction* failed, never
    /// the change: the supervisor reports it and keeps the new set, because
    /// pretending the stream did not save — while its broker session is
    /// demonstrably open — would be the one answer that is certainly wrong.
    fn streams_changed(&self, catalog: &Catalog) -> Result<()>;
}
