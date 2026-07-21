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
//! Two code frameworks are registered (§13.3). `code` is the generic one — any
//! bundler, any layout, stated as five settings. [`react`](crate::react) is the
//! opinionated one: it asks for the file store and a project directory and
//! *derives* the rest, and (from §2.3) the server scaffolds the project itself.
//! It is not a second serving implementation — a built React app is a static
//! bundle with an SPA fallback, so it mounts as a [`CodeFramework`] under its own
//! name, and [`app_source_from_config`] resolves both to the same [`AppSource`].
//!
//! An [`Application`] is pure data; [`app_providers`]/[`app_endpoints`] are the
//! wiring that resolves it into running API providers and the single endpoint set
//! they project. [`app_client`] generates the app's typed TypeScript client from
//! that set — the same generator the admin SPA uses — and [`build_application`]
//! emits it into the app's source tree before invoking the bundler.
//!
//! Applications are **created in the admin UI, not in Rust** (§13.2), so an app
//! is defined by its `_sc_applications` row and nothing else — there is nothing
//! to introspect one from, which is why this is the one stored-metadata table
//! the MVP needs. [`bootstrap`] creates the table (idempotently, on any database
//! including one that has never seen Saltcorn) and [`save_application`] /
//! [`load_application`] / [`list_applications`] / [`delete_application`] are the
//! row ⇄ [`Application`] path. Saving is not building or mounting: an app that is
//! saved but unbuilt is a normal state.

mod api;
mod application;
mod applications;
mod build;
mod framework;
mod react;
mod scaffold;
mod store;

pub use api::{app_client, app_endpoints, app_providers, app_tables};
pub use application::{ApiConfig, AppId, Application, CspPolicy, FrameworkRef, StaticDir};
pub use applications::{
    APPLICATIONS_TABLE, COL_APIS, COL_ATTRIBUTES, COL_CSP, COL_DESCRIPTION, COL_EXTRA_FRAMEWORKS,
    COL_FILE_STORES, COL_FRAMEWORK, COL_ID, COL_NAME, COL_STATIC_DIRS, COL_SUBDOMAIN, COL_TABLES,
    bootstrap,
};
pub use build::{
    AppSource, BuildReport, app_source_from_config, build_app, build_application,
    build_code_framework, emit_client, run_build,
};
pub use framework::{
    AppRequest, AppResponse, Asset, AssetBundle, BuildSpec, CFG_CLIENT, CFG_COMMAND, CFG_OUTPUT,
    CFG_SOURCE, CFG_STORE, CODE_FRAMEWORK, CodeFramework, Framework, InstallSpec, Method,
    code_config_spec, framework_config_spec, framework_default_csp, registered_frameworks,
    validate_framework_config, validate_framework_config_structure,
};
pub use react::{
    CFG_PROJECT, REACT_BUILD_ARGS, REACT_BUILD_COMMAND, REACT_CLIENT_FILE, REACT_FRAMEWORK,
    REACT_OUTPUT_SUBDIR, REACT_RUNTIME_SUBDIR, check_project_name, react_build_spec,
    react_client_path, react_config_spec, react_csp, react_runtime_dir, valid_project_name,
};
pub use scaffold::{
    GeneratedFile, ScaffoldReport, emit_react_runtime, require_scaffoldable, scaffold_app,
};
pub use store::{
    applications_using_file_store, delete_application, list_applications, load_application,
    load_application_by_subdomain, save_application,
};
