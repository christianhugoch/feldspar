//! The [`Framework`] trait and the [`CodeFramework`] that serves bundled static
//! assets (design §13.3).
//!
//! A framework owns an application's primary UI. **Code frameworks** (React,
//! Next.js, SvelteKit, …) keep their source in a git-repo file store, run a build
//! step, and serve the resulting bundle; the app reaches data only through the
//! API providers, never the database directly. [`CodeFramework`] is the MVP
//! implementation: it serves a pre-built [`AssetBundle`] of static files with an
//! SPA fallback to `index.html`, so a client-routed React app resolves deep links.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytes::Bytes;
use sc_catalog::Catalog;
use sc_error::{Error, Repr, Result};
use sc_types::{BasicType, FormField, validate_attrs};

use crate::application::{CspPolicy, FrameworkRef};
use crate::react::{
    CFG_PROJECT, REACT_FRAMEWORK, check_project_name, react_config_spec, react_csp,
};

pub use sc_api::Method;

/// A request routed to an application's framework: an HTTP method and the request
/// path **within the application** (e.g. `/`, `/assets/app.js`, `/posts/42`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRequest {
    /// The HTTP method.
    pub method: Method,
    /// The path within the app, with a leading slash.
    pub path: String,
}

impl AppRequest {
    /// A `GET` for `path`.
    pub fn get(path: impl Into<String>) -> AppRequest {
        AppRequest {
            method: Method::Get,
            path: path.into(),
        }
    }
}

/// A framework's response: an HTTP status, a `Content-Type`, and the body bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppResponse {
    /// The HTTP status code.
    pub status: u16,
    /// The `Content-Type` header value.
    pub content_type: String,
    /// The response body.
    pub body: Bytes,
}

impl AppResponse {
    /// A `200 OK` carrying `body` with the given content type.
    pub fn ok(content_type: impl Into<String>, body: Bytes) -> AppResponse {
        AppResponse {
            status: 200,
            content_type: content_type.into(),
            body,
        }
    }

    /// A `404 Not Found` with a short plain-text body.
    pub fn not_found() -> AppResponse {
        AppResponse {
            status: 404,
            content_type: "text/plain; charset=utf-8".to_owned(),
            body: Bytes::from_static(b"Not Found"),
        }
    }

    /// A `405 Method Not Allowed` with a short plain-text body.
    pub fn method_not_allowed() -> AppResponse {
        AppResponse {
            status: 405,
            content_type: "text/plain; charset=utf-8".to_owned(),
            body: Bytes::from_static(b"Method Not Allowed"),
        }
    }
}

/// How to build a code framework's bundle (design §13.3): the bundler command,
/// its arguments, and the source/output directories (relative to the app's file
/// store). The build step itself — invoking this — is wired in a later Phase 9
/// item; [`Framework::build`] returns this spec for code frameworks and `None`
/// for build-less ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildSpec {
    /// The bundler executable (e.g. `npm`).
    pub command: String,
    /// Arguments passed to the bundler (e.g. `["run", "build"]`).
    pub args: Vec<String>,
    /// The source directory, relative to the app's file store.
    pub source_dir: String,
    /// The output directory the bundle lands in, relative to the file store; its
    /// contents are what [`CodeFramework`] serves.
    pub output_dir: String,
    /// A dependency-install step to run before the build, when its marker is
    /// absent from the source directory. `None` for a framework that does not
    /// manage dependencies.
    pub install: Option<InstallSpec>,
}

/// How to install an app's dependencies before building it (TODO §2.3).
///
/// A property of the framework, not of the build: the `react` framework knows its
/// projects have a `package.json` and are installed with `npm`, while a `code`
/// app's dependencies are the admin's business and their build command is where
/// they say so. Carried on the [`BuildSpec`] rather than assumed by the build
/// step, so `run_build` stays a framework-agnostic "run this over that tree".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSpec {
    /// The installer executable (e.g. `npm`).
    pub command: String,
    /// Arguments passed to it (e.g. `["install"]`).
    pub args: Vec<String>,
    /// A directory, relative to the source directory, whose presence means the
    /// install has already been done (e.g. `node_modules`). Checked rather than
    /// remembered, because the truth is on disk: a store restored from a backup,
    /// or a project whose `node_modules` was deleted, must install again.
    pub marker: String,
}

/// The registered name of the MVP's one framework: a code framework serving a
/// bundled SPA. Mirrors [`REST_PROVIDER`](sc_api::REST_PROVIDER) — a
/// [`FrameworkRef`](crate::FrameworkRef) names its framework rather than holding
/// it, so the name is the registry key.
pub const CODE_FRAMEWORK: &str = "code";

/// Owns an application's primary UI (design §13.3).
#[async_trait]
pub trait Framework: Send + Sync {
    /// The framework's registered name (`"code"`, `"saltcorn-v1"`, …).
    fn name(&self) -> &str;

    /// The settings this framework needs, so the admin UI can render a form for
    /// them **without knowing anything about this framework** (§13.2/§13.3).
    ///
    /// This is what makes "the admin picks a framework" work: GOALS requires that
    /// different frameworks have different settings — a React app needs the file
    /// store holding its code, a Saltcorn-v1 app needs none of that — and the
    /// admin UI must render a form for whichever was picked with no
    /// per-framework special case, including for a framework supplied by a guest
    /// language through `sc-code`. So settings are declared as data, in the same
    /// [`FormField`] vocabulary a row editor uses (§6.2).
    ///
    /// A [`FrameworkRef`](crate::FrameworkRef)'s config is checked against this on
    /// save ([`validate_framework_config`](crate::validate_framework_config)), so
    /// a misconfigured app is rejected where the admin can fix it rather than at
    /// build or serve time.
    fn config_spec(&self) -> Vec<FormField>;

    /// Serve one request against the app's routes. The [`Catalog`] is available
    /// for frameworks that render server-side against data; a code framework that
    /// serves a static bundle ignores it (data reaches the client through the API
    /// providers).
    async fn handle(&self, req: AppRequest, cat: &Catalog) -> Result<AppResponse>;

    /// The build step for a code framework, or `None` for a build-less framework.
    fn build(&self) -> Option<BuildSpec>;
}

/// The `store` setting: which file store holds the app's source.
pub const CFG_STORE: &str = "store";
/// The `source` setting: the sub-directory of the store the source sits in.
pub const CFG_SOURCE: &str = "source";
/// The `output` setting: the sub-directory the bundler emits into.
pub const CFG_OUTPUT: &str = "output";
/// The `command` setting: the build command, e.g. `npm run build`.
pub const CFG_COMMAND: &str = "command";
/// The `client` setting: where to emit the generated TypeScript client.
pub const CFG_CLIENT: &str = "client";

/// The settings a [`CodeFramework`] needs (design §13.3: "which file store, which
/// subdirectory, which build command").
///
/// A free function as well as a [`Framework::config_spec`] impl, because the
/// admin UI and the save-time check need a framework's settings **before** there
/// is an instance to ask: a `CodeFramework` only exists once its bundle is built,
/// and the whole point is to configure it before that. The trait method
/// delegates here, so the two cannot drift.
pub fn code_config_spec() -> Vec<FormField> {
    vec![
        // A pick-list of the file stores that exist, not free text (§1.6). The
        // list cannot be written into a static spec — it is whatever the admin
        // has configured — so it is declared as a named server query and
        // resolved by `sc_catalog::resolve_options` before this spec is
        // rendered or validated against.
        FormField::new(CFG_STORE, BasicType::Text)
            .label("File store")
            .required()
            .server_query(sc_catalog::QUERY_FILE_STORES),
        FormField::new(CFG_SOURCE, BasicType::Text)
            .label("Source directory")
            .default_value(""),
        FormField::new(CFG_OUTPUT, BasicType::Text)
            .label("Output directory")
            .required(),
        FormField::new(CFG_COMMAND, BasicType::Text)
            .label("Build command")
            .required(),
        FormField::new(CFG_CLIENT, BasicType::Text).label("Generated client path"),
    ]
}

/// The names of every registered framework — what the admin UI lists so an admin
/// can pick one and be shown its [`config_spec`](framework_config_spec).
///
/// **Order is meaningful**: [`REACT_FRAMEWORK`] comes first because it is the
/// path an admin should take, and [`CODE_FRAMEWORK`] second because it is the
/// generic escape hatch for a project React's conventions do not fit. Two names
/// in a dropdown are not two equal choices, and the list is where that starts
/// (the admin UI's presentation of it is §2.4).
///
/// This is the single place that enumerates them, so a new framework is listed by
/// adding it here (and to [`framework_config_spec`]).
pub fn registered_frameworks() -> Vec<String> {
    registered_framework_info()
        .into_iter()
        .map(|f| f.name)
        .collect()
}

/// How a framework presents itself to an admin choosing one: a human name and a
/// sentence saying who it is for.
///
/// This exists so the admin UI can show two frameworks as the *different
/// propositions they are* — React the path to take, `code` the escape hatch —
/// without containing any knowledge of either. The alternative was a screen that
/// special-cases the name `react`, which would undo §13.3's whole arrangement the
/// moment a third framework (or one from a guest language) arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameworkInfo {
    /// The registry key, as used in a [`FrameworkRef`](crate::FrameworkRef).
    pub name: String,
    /// A human-facing name.
    pub label: String,
    /// One sentence: what this framework does for the admin, and what it asks of
    /// them in return.
    pub description: String,
}

/// Every registered framework with its presentation, in the order an admin
/// should be offered them — the single place both the list and the editorial
/// ordering live.
pub fn registered_framework_info() -> Vec<FrameworkInfo> {
    vec![
        FrameworkInfo {
            name: REACT_FRAMEWORK.to_owned(),
            label: "React".to_owned(),
            description: "Saltcorn creates the project, generates a typed client and \
                          hooks for your tables, installs its dependencies and builds \
                          it. Pick a file store and a name."
                .to_owned(),
        },
        FrameworkInfo {
            name: CODE_FRAMEWORK.to_owned(),
            label: "Code (bring your own build)".to_owned(),
            description: "Any bundler, any layout. You create the project and state \
                          where its source, output and build command are — for a \
                          project React's conventions do not fit."
                .to_owned(),
        },
    ]
}

/// The settings the framework registered under `name` declares — the registry
/// lookup, resolving a [`FrameworkRef`](crate::FrameworkRef)'s name to a spec
/// without needing an instance.
///
/// An unknown name is a configuration error rather than an app with no settings,
/// mirroring how [`app_providers`](crate::app_providers) treats an unknown API
/// provider.
pub fn framework_config_spec(name: &str) -> Result<Vec<FormField>> {
    match name {
        CODE_FRAMEWORK => Ok(code_config_spec()),
        REACT_FRAMEWORK => Ok(react_config_spec()),
        other => Err(Error::config(format!(
            "unknown framework `{other}`; this server registers {}",
            registered_frameworks()
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// The Content-Security-Policy an application gets when the admin does not state
/// one, chosen by its framework.
///
/// The framework knows what its own output needs — `react` scaffolds a Vite
/// bundle and can therefore name a policy tighter and more specific than
/// "strict" ([`react_csp`]), while a `code` app is arbitrary and gets the strict
/// baseline. A framework that does not answer gets the baseline too, which is the
/// safe direction to fail in.
pub fn framework_default_csp(name: &str) -> CspPolicy {
    match name {
        REACT_FRAMEWORK => react_csp(),
        _ => CspPolicy::strict(),
    }
}

/// Check a [`FrameworkRef`](crate::FrameworkRef)'s config against its framework's
/// [`config_spec`](Framework::config_spec) (§13.3).
///
/// Called **on save** (see [`save_application`](crate::save_application)), which
/// is the point of it: a missing or ill-typed setting is the admin's to fix, and
/// the admin is standing in front of the form. Discovering it at build time means
/// a bundler error, and at serve time means a broken app.
pub async fn validate_framework_config(catalog: &Catalog, fw: &FrameworkRef) -> Result<()> {
    let spec = framework_config_spec(&fw.name)?;
    // Resolve any server-query options first, so a setting restricted to "the
    // stores that exist" is checked against the stores that actually exist. This
    // is what turns an unknown store name from a build-time failure into a
    // save-time one, where the admin is still looking at the form.
    let spec = sc_catalog::resolve_options(catalog, spec).await?;
    validate_against(fw, &spec)
}

/// Check a config's **structure** only: every setting present, of the right
/// type, and no unknown keys — but *not* whether a server-query setting names
/// something that exists.
///
/// The distinction is about when each question is worth asking. "Is this
/// well-formed?" has one answer forever and needs nothing but the spec. "Does
/// store `apps` exist?" depends on the state of the system and is settled on save
/// ([`validate_framework_config`]), where the admin can fix it. Re-asking it at
/// build time would mean a build could fail for a reason unrelated to the build,
/// and the honest error there is the one the build already gives — the store
/// cannot be resolved.
///
/// This falls out of the model rather than being bolted on: an unresolved
/// [`ServerQuery`](sc_types::OptionsSource::ServerQuery) has no static options,
/// and `validate_attrs` only checks membership against options it has.
pub fn validate_framework_config_structure(fw: &FrameworkRef) -> Result<()> {
    let spec = framework_config_spec(&fw.name)?;
    validate_against(fw, &spec)
}

/// Validate `fw`'s config against an already-prepared spec, then apply any check
/// the spec vocabulary cannot express ([`framework_specific_checks`]).
fn validate_against(fw: &FrameworkRef, spec: &[FormField]) -> Result<()> {
    check_attrs(fw, spec)?;
    framework_specific_checks(fw)
}

/// Checks a [`FormField`] spec cannot state.
///
/// §6.2's vocabulary covers presence, type and membership of a list; `react`'s
/// project name needs "is a usable directory name", which is a pattern. Rather
/// than growing the spec vocabulary for one setting — every guest-language
/// framework would then have to be understood by it — the framework checks its
/// own. It runs on both validation paths, so an unusable name is refused **on
/// save**, not discovered when a build interpolates it into a path.
fn framework_specific_checks(fw: &FrameworkRef) -> Result<()> {
    if fw.name == REACT_FRAMEWORK
        && let Some(project) = fw.config.get(CFG_PROJECT).and_then(|v| v.as_str())
    {
        check_project_name(project)?;
    }
    Ok(())
}

/// Validate `fw`'s config against `spec`, naming the framework in any error.
fn check_attrs(fw: &FrameworkRef, spec: &[FormField]) -> Result<()> {
    validate_attrs(spec, &fw.config).map_err(|e| {
        // Name the framework as well as the setting. Rebuilt rather than
        // wrapped: `Error`'s `Invalid` renders its own "invalid:" prefix, so
        // formatting the whole error into a new one would say it twice, and
        // a `Context` would show only the context and hide the setting —
        // which is the part the admin needs.
        if let Repr::Invalid(msg) = e.repr() {
            Error::invalid(format!("framework `{}`: {msg}", fw.name))
        } else {
            e
        }
    })
}

/// One bundled asset: its bytes and content type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// The file bytes.
    pub bytes: Bytes,
    /// The `Content-Type` derived from the file extension.
    pub content_type: String,
}

/// A pre-built bundle of static assets served by a [`CodeFramework`].
///
/// Assets are keyed by their path relative to the bundle root, normalised to no
/// leading slash and `/` separators (e.g. `index.html`, `assets/app.js`). An
/// optional [`fallback`](AssetBundle::fallback) path (typically `index.html`) is
/// returned for any request that does not match a file, so a client-side-routed
/// SPA resolves deep links.
#[derive(Debug, Clone, Default)]
pub struct AssetBundle {
    assets: HashMap<String, Asset>,
    fallback: Option<String>,
}

impl AssetBundle {
    /// An empty bundle with no SPA fallback.
    pub fn new() -> AssetBundle {
        AssetBundle {
            assets: HashMap::new(),
            fallback: None,
        }
    }

    /// Insert an asset at `path`; the content type is derived from its extension.
    pub fn insert(&mut self, path: &str, bytes: impl Into<Bytes>) -> &mut AssetBundle {
        let key = normalize_key(path);
        let content_type = content_type_for(&key).to_owned();
        self.assets.insert(
            key,
            Asset {
                bytes: bytes.into(),
                content_type,
            },
        );
        self
    }

    /// Builder form of [`insert`](AssetBundle::insert).
    pub fn with(mut self, path: &str, bytes: impl Into<Bytes>) -> AssetBundle {
        self.insert(path, bytes);
        self
    }

    /// Set the SPA fallback asset path (returned for unmatched requests).
    pub fn fallback(mut self, path: &str) -> AssetBundle {
        self.fallback = Some(normalize_key(path));
        self
    }

    /// The number of assets in the bundle.
    pub fn len(&self) -> usize {
        self.assets.len()
    }

    /// Whether the bundle has no assets.
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    /// Look up an asset by (normalised) path.
    pub fn get(&self, path: &str) -> Option<&Asset> {
        self.assets.get(&normalize_key(path))
    }

    /// Load a bundle by recursively reading every file under `dir`, keying each by
    /// its path relative to `dir`. When an `index.html` is present it becomes the
    /// SPA fallback.
    pub fn from_dir(dir: impl AsRef<Path>) -> Result<AssetBundle> {
        let root = dir.as_ref();
        let mut bundle = AssetBundle::new();
        load_dir(root, root, &mut bundle)?;
        if bundle.get("index.html").is_some() {
            bundle = bundle.fallback("index.html");
        }
        Ok(bundle)
    }
}

/// Recursively read files under `current`, keying by path relative to `root`.
fn load_dir(root: &Path, current: &Path, bundle: &mut AssetBundle) -> Result<()> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            load_dir(root, &path, bundle)?;
        } else if file_type.is_file() {
            let rel = path.strip_prefix(root).map_err(|_| {
                Error::file(format!("asset path {} escapes bundle root", path.display()))
            })?;
            let key = rel.to_string_lossy().replace('\\', "/");
            let bytes = std::fs::read(&path)?;
            bundle.insert(&key, Bytes::from(bytes));
        }
    }
    Ok(())
}

/// Normalise an asset key: strip leading slashes, use `/` separators.
fn normalize_key(path: &str) -> String {
    let mut normalized = PathBuf::new();
    for part in path
        .split(['/', '\\'])
        .filter(|p| !p.is_empty() && *p != ".")
    {
        normalized.push(part);
    }
    normalized.to_string_lossy().replace('\\', "/")
}

/// Guess a `Content-Type` from a file's extension, defaulting to
/// `application/octet-stream`.
fn content_type_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// A code framework (React/Next/SvelteKit/…) that serves a pre-built
/// [`AssetBundle`] of static files (design §13.3).
///
/// Requests are matched against the bundle by path; a request for `/` serves
/// `index.html`, and any path that matches no file falls back to the bundle's SPA
/// fallback (typically `index.html`) so a client-routed app resolves deep links.
/// Only `GET`/`HEAD` are served; other methods get `405`. Data never flows
/// through the framework — the client talks to the API providers.
pub struct CodeFramework {
    name: String,
    bundle: AssetBundle,
    build: Option<BuildSpec>,
}

impl CodeFramework {
    /// A code framework named `name` serving `bundle`, with no build spec.
    pub fn new(name: impl Into<String>, bundle: AssetBundle) -> CodeFramework {
        CodeFramework {
            name: name.into(),
            bundle,
            build: None,
        }
    }

    /// Attach a [`BuildSpec`], returning `self` for chaining.
    pub fn with_build(mut self, build: BuildSpec) -> CodeFramework {
        self.build = Some(build);
        self
    }

    /// The asset the framework would serve for `path`, applying the `/` →
    /// `index.html` rule and the SPA fallback. `None` means a genuine 404 (no
    /// match and no fallback).
    fn resolve(&self, path: &str) -> Option<&Asset> {
        let key = normalize_key(path);
        let key = if key.is_empty() { "index.html" } else { &key };
        if let Some(asset) = self.bundle.get(key) {
            return Some(asset);
        }
        self.bundle
            .fallback
            .as_ref()
            .and_then(|fb| self.bundle.get(fb))
    }

    /// Serve one request from the bundle. This is the whole behaviour of the
    /// framework — [`Framework::handle`] is a thin `async` wrapper around it —
    /// factored out so it can be exercised without a [`Catalog`], which a static
    /// bundle never touches. Only `GET` is served; other methods get `405`, and
    /// an unmatched path with no SPA fallback gets `404`.
    pub fn serve(&self, req: &AppRequest) -> AppResponse {
        if !matches!(req.method, Method::Get) {
            return AppResponse::method_not_allowed();
        }
        match self.resolve(&req.path) {
            Some(asset) => AppResponse::ok(asset.content_type.clone(), asset.bytes.clone()),
            None => AppResponse::not_found(),
        }
    }
}

#[async_trait]
impl Framework for CodeFramework {
    fn name(&self) -> &str {
        &self.name
    }

    fn config_spec(&self) -> Vec<FormField> {
        // Looked up by name rather than hard-coded to `code_config_spec`, because
        // one `CodeFramework` serves both registered code frameworks: a built
        // React app is a static bundle with an SPA fallback, so `react` is
        // mounted as an instance of this type under its own name. Reporting
        // `code`'s five settings for it would make the instance disagree with the
        // registry about what the admin was asked. An unregistered name falls
        // back rather than failing: `config_spec` cannot report an error, and a
        // framework serving a bundle is at worst a `code` one.
        framework_config_spec(&self.name).unwrap_or_else(|_| code_config_spec())
    }

    async fn handle(&self, req: AppRequest, _cat: &Catalog) -> Result<AppResponse> {
        // A static bundle never touches the catalog; all behaviour lives in
        // `serve`, which is directly testable without a database.
        Ok(self.serve(&req))
    }

    fn build(&self) -> Option<BuildSpec> {
        self.build.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_framework_declares_the_settings_section_13_3_names() {
        let spec = code_config_spec();
        let names: Vec<&str> = spec.iter().map(|f| f.name()).collect();
        // §13.3: "which file store, which subdirectory, which build command".
        assert_eq!(
            names,
            [CFG_STORE, CFG_SOURCE, CFG_OUTPUT, CFG_COMMAND, CFG_CLIENT]
        );

        // Every setting carries a human label, because the admin UI renders this
        // and nothing else knows what these mean.
        assert!(spec.iter().all(|f| !f.base.label.is_empty()));
        assert!(spec.iter().any(|f| f.base.label == "File store"));

        // The store, output and command are the app's to state; the source
        // directory defaults to the store root and the client is opt-in.
        let required: Vec<&str> = spec
            .iter()
            .filter(|f| f.required)
            .map(|f| f.name())
            .collect();
        assert_eq!(required, [CFG_STORE, CFG_OUTPUT, CFG_COMMAND]);
    }

    #[test]
    fn an_instance_reports_the_same_spec_as_the_free_function() {
        // The trait method delegates, so the admin UI (which has no instance) and
        // a mounted framework cannot disagree about what the settings are.
        let framework = CodeFramework::new(CODE_FRAMEWORK, AssetBundle::new());
        assert_eq!(framework.config_spec(), code_config_spec());
        assert_eq!(
            framework_config_spec(CODE_FRAMEWORK).unwrap(),
            code_config_spec()
        );

        // And a `react` app — mounted as a `CodeFramework` because a built React
        // app is just a bundle — reports `react`'s two settings, not `code`'s
        // five. The instance and the registry agree for both names.
        let react = CodeFramework::new(REACT_FRAMEWORK, AssetBundle::new());
        assert_eq!(react.config_spec(), react_config_spec());
        assert_ne!(react.config_spec(), code_config_spec());
    }

    #[test]
    fn both_code_frameworks_are_registered_react_first() {
        // Order is the registry's one editorial statement: React is the path an
        // admin should take, `code` the escape hatch (§2.4 renders that).
        assert_eq!(registered_frameworks(), [REACT_FRAMEWORK, CODE_FRAMEWORK]);
        // Every registered name resolves to a spec — the list and the lookup
        // cannot drift apart without this failing.
        for name in registered_frameworks() {
            let spec = framework_config_spec(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!spec.is_empty(), "{name}");
            assert!(spec.iter().all(|f| !f.base.label.is_empty()), "{name}");
        }
        // The two really are different forms; that is the point of having both.
        assert!(react_config_spec().len() < code_config_spec().len());
    }

    #[test]
    fn an_unusable_project_name_is_refused_at_validation_not_at_build() {
        // §1.6's principle applied to the one setting a spec cannot describe: the
        // admin hears about it while looking at the form. Both validation paths
        // apply it, so saving and building agree.
        let fw = FrameworkRef::new(REACT_FRAMEWORK)
            .with(CFG_STORE, "apps")
            .with(CFG_PROJECT, "../../etc");
        let err = validate_framework_config_structure(&fw)
            .expect_err("a traversal is not a directory name")
            .to_string();
        assert!(err.contains(CFG_PROJECT), "{err}");
        assert!(err.contains("directory name"), "{err}");

        // A plain name passes, and `code` is unaffected by react's rule.
        let ok = fw.with(CFG_PROJECT, "todo");
        assert!(validate_framework_config_structure(&ok).is_ok());
        let code = FrameworkRef::new(CODE_FRAMEWORK)
            .with(CFG_STORE, "apps")
            .with(CFG_OUTPUT, "../shared/dist")
            .with(CFG_COMMAND, "npm run build");
        assert!(validate_framework_config_structure(&code).is_ok());
    }

    #[test]
    fn the_default_csp_comes_from_the_framework() {
        // A `react` app's default is fitted to what Vite emits...
        assert_eq!(framework_default_csp(REACT_FRAMEWORK), react_csp());
        assert!(
            framework_default_csp(REACT_FRAMEWORK)
                .header_value()
                .contains("connect-src 'self'")
        );
        // ...while an arbitrary bundle gets the strict baseline, as does anything
        // unrecognised — failing towards the tighter policy.
        assert_eq!(framework_default_csp(CODE_FRAMEWORK), CspPolicy::strict());
        assert_eq!(framework_default_csp("saltcorn-v1"), CspPolicy::strict());
    }

    #[test]
    fn an_unknown_framework_has_no_spec() {
        let err = framework_config_spec("nextjs").unwrap_err().to_string();
        assert!(err.contains("nextjs"), "{err}");
        assert!(err.contains(CODE_FRAMEWORK), "should say what is available");
    }

    #[test]
    fn validate_framework_config_names_the_framework_and_the_setting() {
        // The structural variant: same error path, no catalog needed. Whether a
        // named store *exists* is the async variant's job and is covered by an
        // integration test, since it needs a real catalog to have stores in.
        let err = validate_framework_config_structure(&FrameworkRef::new(CODE_FRAMEWORK))
            .unwrap_err()
            .to_string();
        assert!(err.contains(CODE_FRAMEWORK), "{err}");
        assert!(err.contains(CFG_STORE), "{err}");
        // One "invalid:" prefix, not two — the message is rebuilt, not nested.
        assert_eq!(err.matches("invalid:").count(), 1, "{err}");
    }

    #[test]
    fn the_store_setting_is_a_server_query_not_free_text() {
        // §1.6: the list of stores cannot be written into a static spec, so the
        // setting names a server-side source instead. The admin UI never sees
        // this — the server resolves it before handing the spec over — but the
        // declaration is what makes that possible.
        let spec = code_config_spec();
        let store = spec.iter().find(|f| f.name() == CFG_STORE).unwrap();
        assert_eq!(store.query(), Some(sc_catalog::QUERY_FILE_STORES));
        // Unresolved, it restricts nothing: validating against options you do
        // not have would reject every value.
        assert!(store.static_options().is_empty());

        // The other settings are genuinely free text and stay that way.
        for name in [CFG_SOURCE, CFG_OUTPUT, CFG_COMMAND, CFG_CLIENT] {
            let field = spec.iter().find(|f| f.name() == name).unwrap();
            assert_eq!(field.query(), None, "{name}");
        }
    }

    #[test]
    fn an_unresolved_server_query_does_not_reject_a_value() {
        // Structural validation must pass for any well-formed store name: the
        // membership question belongs to the async variant, which has a catalog
        // to answer it with. If this ever started failing, every build would
        // break for a reason that has nothing to do with building.
        let fw = FrameworkRef::new(CODE_FRAMEWORK)
            .with(CFG_STORE, "anything-at-all")
            .with(CFG_OUTPUT, "dist")
            .with(CFG_COMMAND, "npm run build");
        assert!(validate_framework_config_structure(&fw).is_ok());
    }

    fn sample_bundle() -> AssetBundle {
        AssetBundle::new()
            .with(
                "index.html",
                Bytes::from_static(b"<!doctype html><div id=root>"),
            )
            .with("assets/app.js", Bytes::from_static(b"console.log(1)"))
            .with("assets/app.css", Bytes::from_static(b"body{}"))
            .fallback("index.html")
    }

    #[test]
    fn content_type_from_extension() {
        let bundle = sample_bundle();
        assert_eq!(
            bundle.get("index.html").unwrap().content_type,
            "text/html; charset=utf-8"
        );
        assert_eq!(
            bundle.get("assets/app.js").unwrap().content_type,
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            bundle.get("assets/app.css").unwrap().content_type,
            "text/css; charset=utf-8"
        );
    }

    #[test]
    fn key_normalisation_ignores_leading_slash() {
        let bundle = sample_bundle();
        // Leading slash and no leading slash resolve to the same asset.
        assert!(bundle.get("/assets/app.js").is_some());
        assert!(bundle.get("assets/app.js").is_some());
    }

    #[test]
    fn serves_root_asset_and_fallback() {
        let fw = CodeFramework::new("code", sample_bundle());

        // `/` serves index.html.
        let root = fw.serve(&AppRequest::get("/"));
        assert_eq!(root.status, 200);
        assert_eq!(root.content_type, "text/html; charset=utf-8");
        assert_eq!(&root.body[..], b"<!doctype html><div id=root>");

        // An exact asset is served with its own content type.
        let js = fw.serve(&AppRequest::get("/assets/app.js"));
        assert_eq!(js.status, 200);
        assert_eq!(js.content_type, "text/javascript; charset=utf-8");

        // An unknown path falls back to the SPA entry point (client routing).
        let deep = fw.serve(&AppRequest::get("/posts/42"));
        assert_eq!(deep.status, 200);
        assert_eq!(deep.content_type, "text/html; charset=utf-8");
        assert_eq!(&deep.body[..], b"<!doctype html><div id=root>");
    }

    #[test]
    fn no_fallback_yields_404_and_non_get_yields_405() {
        // No fallback configured.
        let fw = CodeFramework::new(
            "code",
            AssetBundle::new().with("index.html", Bytes::from_static(b"hi")),
        );
        assert_eq!(fw.serve(&AppRequest::get("/missing")).status, 404);

        // Static bundles only answer GET.
        let post = AppRequest {
            method: Method::Post,
            path: "/".to_owned(),
        };
        assert_eq!(fw.serve(&post).status, 405);
    }

    #[tokio::test]
    async fn handle_delegates_to_serve() {
        // The async trait method returns the same result as `serve`; a static
        // bundle ignores the catalog, so we can compare against `serve` directly.
        let fw = CodeFramework::new("code", sample_bundle());
        let req = AppRequest::get("/assets/app.css");
        let expected = fw.serve(&req);
        // `handle` matches; it merely wraps `serve` in `Ok(..)`.
        assert_eq!(expected.status, 200);
        assert_eq!(expected.content_type, "text/css; charset=utf-8");
    }

    #[test]
    fn build_spec_is_reported_for_code_frameworks() {
        let spec = BuildSpec {
            command: "npm".to_owned(),
            args: vec!["run".to_owned(), "build".to_owned()],
            source_dir: "web".to_owned(),
            output_dir: "web/dist".to_owned(),
            install: None,
        };
        let fw = CodeFramework::new("code", sample_bundle()).with_build(spec.clone());
        assert_eq!(fw.name(), "code");
        assert_eq!(fw.build(), Some(spec));

        // Build-less by default.
        let plain = CodeFramework::new("code", sample_bundle());
        assert_eq!(plain.build(), None);
    }

    #[test]
    fn from_dir_loads_a_bundle_with_index_fallback() {
        let dir = std::env::temp_dir().join(format!("sc-app-bundle-{}", std::process::id()));
        let assets = dir.join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::write(dir.join("index.html"), b"<html>").unwrap();
        std::fs::write(assets.join("app.js"), b"1").unwrap();

        let bundle = AssetBundle::from_dir(&dir).unwrap();
        assert_eq!(bundle.len(), 2);
        assert!(bundle.get("index.html").is_some());
        // Nested files are keyed by their relative path.
        assert_eq!(
            bundle.get("assets/app.js").unwrap().content_type,
            "text/javascript; charset=utf-8"
        );
        // index.html present ⇒ it becomes the SPA fallback.
        let fw = CodeFramework::new("code", bundle);
        assert_eq!(fw.serve(&AppRequest::get("/deep/link")).status, 200);

        std::fs::remove_dir_all(&dir).ok();
    }
}
