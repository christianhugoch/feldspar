//! Application model, Framework trait, routing (layer 8; design §13.2–§13.3).
//!
//! An [`Application`] is v2's unit of multi-tenancy: multiple apps share one data
//! layer, each seeing only its declared subset of tables and file stores, served
//! on its own subdomain by one primary [`Framework`] with a strict [`CspPolicy`]
//! by default. A [`Framework`] owns an app's UI; [`CodeFramework`] is the MVP
//! implementation, serving a pre-built [`AssetBundle`] of static files (a bundled
//! React/Svelte/… SPA) with an SPA fallback so client-routed deep links resolve.
//!
//! A code framework's source lives in a git repository inside one of the app's
//! file stores and has a **build step**: [`build_code_framework`] invokes the
//! bundler over that source and hands back a [`CodeFramework`] serving the built
//! bundle.
//!
//! API providers and subdomain routing land in later Phase 9 items.

mod application;
mod build;
mod framework;

pub use application::{ApiConfig, AppId, Application, CspPolicy, FrameworkRef};
pub use build::{AppSource, BuildReport, build_app, build_code_framework, run_build};
pub use framework::{
    AppRequest, AppResponse, Asset, AssetBundle, BuildSpec, CodeFramework, Framework, Method,
};
