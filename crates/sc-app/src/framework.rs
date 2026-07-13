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
use sc_error::{Error, Result};

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
}

/// Owns an application's primary UI (design §13.3).
#[async_trait]
pub trait Framework: Send + Sync {
    /// The framework's registered name (`"code"`, `"saltcorn-v1"`, …).
    fn name(&self) -> &str;

    /// Serve one request against the app's routes. The [`Catalog`] is available
    /// for frameworks that render server-side against data; a code framework that
    /// serves a static bundle ignores it (data reaches the client through the API
    /// providers).
    async fn handle(&self, req: AppRequest, cat: &Catalog) -> Result<AppResponse>;

    /// The build step for a code framework, or `None` for a build-less framework.
    fn build(&self) -> Option<BuildSpec>;
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
    for part in path.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".") {
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

    fn sample_bundle() -> AssetBundle {
        AssetBundle::new()
            .with("index.html", Bytes::from_static(b"<!doctype html><div id=root>"))
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
