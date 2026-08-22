//! **Modules**: Saltcorn v1 JavaScript plugins, installed with npm and run on a
//! Deno worker in this process (design §15; TODO "Modules", "Modules
//! in-process").
//!
//! A module is an npm package exporting v1's plugin object — `{ actions,
//! configuration_workflow, viewtemplates, … }` — and this milestone reads two of
//! those keys: `actions`, which become [`Action`](sc_action::Action)s in the
//! registry the built-ins live in, and `configuration_workflow`, whose first
//! form is the module's own settings.
//!
//! ## Where a module runs
//!
//! A v1 plugin is a CommonJS Node package **whose dependencies are the point**:
//! `@saltcorn/mqtt` is a wrapper over `async-mqtt` (a TCP/TLS socket),
//! `@saltcorn/proxmox` a wrapper over `proxmox-api` (HTTPS). `sc-expr`'s
//! `CodeRuntime` is a bare V8 with four ops and no module loader — no `require`,
//! no `net`, no `fs` — so a module cannot run there.
//!
//! It runs on an implementation of Node instead. Two of them exist in this
//! crate while the milestone is in flight:
//!
//! - [`host`] — one long-lived `node` child process behind a newline-JSON pipe.
//!   What ships today, and what phase 2 deletes.
//! - [`deno`] — a `deno_runtime` worker thread **in this process**, on the same
//!   V8 the code pool already links, speaking the same protocol over a pipe that
//!   no longer crosses a process boundary. Behind the `deno-host` feature.
//!
//! The second exists because of what it makes possible rather than what it
//! saves: `node` stops being a runtime requirement of a Saltcorn server, and a
//! module's worker can be handed a permission set, which `node` has no way to
//! offer.
//!
//! ## The pieces
//!
//! - [`module`] — what a module is: the row, and where its package came from.
//! - [`store`] — `_sc_modules`, the row's schema and its lifecycle.
//! - [`paths`] — where packages are installed.
//! - [`install`] — npm, and what it turned out to have installed.
//! - [`bounds`] — the four bounds a module call is under, and the pool's size.
//! - [`host`] — the Node child process and the line protocol.
//! - [`deno`] — the in-process worker pool (feature `deno-host`).
//! - [`spec`] — v1's `configFields` translated into this system's `FormField`.
//! - [`action`] — a module's action as an `Action`.
//! - [`modules`] — the loaded set: every stored module, its actions, its issues.
//!
//! ## What is *not* here
//!
//! Every other entity type a v1 plugin can export. They are counted and reported
//! ([`ModuleManifest::unsupported`](host::ModuleManifest::unsupported)) so an
//! admin knows what they are not getting, and loading them is a later milestone.

pub mod action;
pub mod bounds;
#[cfg(feature = "deno-host")]
pub mod deno;
pub mod host;
pub mod install;
pub mod module;
pub mod modules;
pub mod paths;
pub mod spec;
pub mod store;

pub use action::ModuleAction;
pub use bounds::{
    DEFAULT_CALL_TIMEOUT, DEFAULT_MODULE_JS_SLICE, DEFAULT_MODULE_MAX_HEAP, DEFAULT_MODULE_WORKERS,
};
#[cfg(feature = "deno-host")]
pub use deno::{DenoModuleHost, PoolBounds};
pub use host::{ActionManifest, ModuleHost, ModuleManifest, UnsupportedEntity};
pub use install::{InstalledPackage, Installer, have_node, have_npm};
pub use module::{MODULE_SOURCES, Module, ModuleId, ModuleSource};
pub use modules::{LoadedModule, ModuleIssue, ModuleSet, redacted_configuration, unsupported_json};
pub use paths::default_modules_root;
pub use spec::config_fields_to_form_fields;
pub use store::{
    MODULES_TABLE, bootstrap_modules, delete_module, list_modules, load_module,
    load_module_by_name, require_module, save_module,
};
