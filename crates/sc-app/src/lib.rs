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
//! An [`Application`] is pure data; [`app_providers`]/[`app_endpoints`] are the
//! wiring that resolves it into running API providers and the single endpoint set
//! they project. [`app_client`] generates the app's typed TypeScript client from
//! that set — the same generator the admin SPA uses — and [`build_application`]
//! emits it into the app's source tree before invoking the bundler.
//!
//! Subdomain routing and serving an app from `sc-server` land in later Phase 9
//! items.

mod api;
mod application;
mod build;
mod framework;

pub use api::{app_client, app_endpoints, app_providers, app_tables};
pub use application::{ApiConfig, AppId, Application, CspPolicy, FrameworkRef};
pub use build::{
    AppSource, BuildReport, build_app, build_application, build_code_framework, emit_client,
    run_build,
};
pub use framework::{
    AppRequest, AppResponse, Asset, AssetBundle, BuildSpec, CodeFramework, Framework, Method,
};
