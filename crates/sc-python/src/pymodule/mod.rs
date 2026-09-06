//! **Python plugin modules**: a `pip`-installable distribution that supplies
//! actions, functions and table providers to this server (§2 of the API, §8,
//! §11; TODO phase 6).
//!
//! ```python
//! import saltcorn as sc
//!
//! sc.settings(sc.Field.string("api_key", label="API key", secret=True))
//!
//! @sc.action(config=[sc.Field.string("model", required=True)])
//! def score_lead(row, config, user):
//!     score = client.score(row["email"], model=config["model"])
//!     sc.db.leads.where(id=row["id"]).update(score=score)
//!     return {"score": score}
//! ```
//!
//! # One module table, two languages, and no screen that knows
//!
//! Everything here answers the **JavaScript** module host's types —
//! [`ModuleManifest`](sc_module::ModuleManifest),
//! [`LoadedModule`](sc_module::LoadedModule),
//! [`Action`](sc_action::Action), [`ModuleFnHost`](sc_expr::ModuleFnHost),
//! [`TableProviderHost`](sc_catalog::TableProviderHost) — because a module is a
//! module to an admin (§8). One `_sc_modules` table, one tab, one set of
//! endpoints, one action registry and one pair of catalog hosts; what the
//! language decides is which package manager installed the package and which
//! host loads it, and both of those are below every screen.
//!
//! That is why this lives in `sc-python` rather than beside the other host: the
//! two share their **types**, not their implementation, and the type they share
//! is `sc-module`'s. A Python module needs an interpreter, and the interpreter
//! is here.
//!
//! # The pieces
//!
//! - [`host`] — the calls: load, unload, run, call, and a provider's six.
//! - [`set`] — every stored Python module, loaded, with its issues.
//! - [`action`] — a module's action as an ordinary `Action`.
//! - [`functions`] — its functions, as the fifth host surface.
//! - [`providers`] — its table providers, as the catalog's seam.
//! - [`model_providers`] — its model providers, as `sc-model`'s seam.
//! - [`fields`] — its field declarations, in this system's own vocabulary.
//!
//! # What a plugin may reach, and what nobody pretends
//!
//! §10: **there is no sandbox.** A Python module runs with the server's
//! privileges, as its `pip install` already did. There is no import gate here —
//! the gate is for a code body, whose author typed it into a form — and no
//! permission set: `_sc_modules.permissions` stays a JavaScript column and the
//! Modules tab says so beside the Install button rather than showing an admin a
//! model that is not there.

pub mod action;
pub mod fields;
pub mod functions;
pub mod host;
pub mod model_providers;
pub mod providers;
pub mod set;

pub use action::PyModuleAction;
pub use functions::PyModuleFunctions;
pub use host::{DEFAULT_CALL_TIMEOUT, PyModuleHost};
pub use model_providers::PyModuleModelProviders;
pub use providers::PyModuleTableProviders;
pub use set::PyModuleSet;
