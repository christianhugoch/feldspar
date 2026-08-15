//! The trigger-change seam: how a saved or deleted trigger becomes something the
//! rest of the process can react to.
//!
//! The exact sibling of [`SchemaObserver`](sc_catalog::SchemaObserver), for the
//! exact reason. A **mounted application** projects its exposed triggers as
//! endpoints at mount time (§10.2, §13.4), encoding each one's `min_role` as its
//! endpoint's auth requirement. So a trigger created, renamed, re-roled or
//! deleted underneath it leaves the mount describing a trigger set that is no
//! longer the one there is — until a restart.
//!
//! The admin handlers used to fix that by calling `AppMounts::refresh_triggers`
//! themselves, which worked only because every trigger change went through a
//! handler. Once an **agent** can save one (§11.3's `admin_copilot`), that is no
//! longer true. So the notification moves to where the change takes effect —
//! [`TriggerDispatcher::reload`](crate::TriggerDispatcher::reload), the one thing
//! every writer calls afterwards — behind a seam the mount registry installs
//! once at boot. A process with no observer (the CLI, a test) simply changes its
//! triggers unobserved.

use sc_catalog::Catalog;
use sc_error::Result;

/// What it means to observe changes to the trigger set. `sc-server`'s mount
/// registry is the implementation; the dispatcher holds it as `dyn`.
///
/// No "what changed" argument, unlike [`SchemaChanged`](sc_catalog::SchemaChanged):
/// every consumer re-projects every app that exposes any trigger anyway, because
/// a **rename** changes two names at once and an app naming either is affected.
/// An enum here would be a distinction nobody branches on.
pub trait TriggerObserver: Send + Sync {
    /// React to a trigger set that has already been reloaded.
    ///
    /// Called **after** the new set is live, so an observer reads it from the
    /// dispatcher (or the catalog) simply by asking. An `Err` means the
    /// *reaction* failed, never the change: the dispatcher reports it and keeps
    /// the new set, because pretending the trigger did not save would be the one
    /// answer that is certainly wrong.
    fn triggers_changed(&self, catalog: &Catalog) -> Result<()>;
}
