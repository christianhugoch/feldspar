//! The four wirings a Deno worker needs to run a v1 CommonJS plugin, and the
//! worker built from them.
//!
//! Phase 0 measured these and they are as small as they look: a module loader
//! that is `FsModuleLoader` plus one branch, a `NodeRequireLoader` with three
//! methods, a `ByonmNpmResolver` over the modules root's own `node_modules`, and
//! one line of `sys_traits`. Nothing needed a workaround and nothing here is a
//! shim over node — it *is* node, as `deno_runtime` implements it.
//!
//! What is deliberately not here: a permission model. Every worker gets
//! [`PermissionsContainer::allow_all`] for now, which is what the sidecar
//! already is. The seam phase 3 narrows is
//! [`NodeRequireLoader::ensure_read_permission`], which is handed the module's
//! own container on every read.

use std::borrow::Cow;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::{FastString, FsModuleLoader, ModuleLoader, ModuleSpecifier};
use deno_error::JsErrorBox;
use deno_resolver::npm::{
    ByonmInNpmPackageChecker, ByonmNpmResolver, ByonmNpmResolverCreateOptions,
};
use deno_runtime::deno_fs::RealFs;
use deno_runtime::deno_io::{Stdio, StdioPipe};
use deno_runtime::deno_node::{NodeExtInitServices, NodeRequireLoader, NodeResolver};
use deno_runtime::deno_permissions::PermissionsContainer;
use deno_runtime::deno_web::{BlobStore, InMemoryBroadcastChannel};
use deno_runtime::permissions::RuntimePermissionDescriptorParser;
use deno_runtime::worker::{MainWorker, WorkerOptions, WorkerServiceOptions};
use deno_runtime::{BootstrapOptions, WorkerExecutionMode};
use node_resolver::cache::NodeResolutionSys;
use node_resolver::errors::PackageJsonLoadError;
use node_resolver::{DenoIsBuiltInNodeModuleChecker, PackageJsonResolver};
use sys_traits::impls::RealSys;

/// The real filesystem and the real environment: this is a server, not a test
/// harness for Deno.
type Sys = RealSys;

// §6: what `build.rs` produced. `RUNTIME_SNAPSHOT.bin` is the V8 startup blob;
// the two tables are the runtime's own extension sources that did not fit in it,
// already transpiled.
include!(concat!(env!("OUT_DIR"), "/residual_lazy.rs"));
static SNAPSHOT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/RUNTIME_SNAPSHOT.bin"));

// ---------------------------------------------------------------------------
// Wiring 1: the module loader
// ---------------------------------------------------------------------------

/// [`FsModuleLoader`] plus one branch.
///
/// A `node:` specifier is its own canonical form, and `deno_core`'s module map
/// answers it from the `lazy_loaded_esm` registry the `deno_node` extension
/// registered — so resolving it means handing back the URL unchanged and
/// nothing else. That is the whole of what `module-host.mjs`'s four `import`s
/// need; everything a *module* requires goes through [`ModuleRequireLoader`]
/// below rather than through here, because a v1 plugin is CommonJS.
struct HostModuleLoader(FsModuleLoader);

impl ModuleLoader for HostModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        kind: deno_core::ResolutionKind,
    ) -> deno_core::ModuleResolveResponse {
        if specifier.starts_with("node:") {
            return deno_core::url::Url::parse(specifier).map_err(JsErrorBox::from_err);
        }
        self.0.resolve(specifier, referrer, kind)
    }

    fn load(
        &self,
        specifier: &ModuleSpecifier,
        referrer: Option<&deno_core::ModuleLoadReferrer>,
        options: deno_core::ModuleLoadOptions,
    ) -> deno_core::ModuleLoadResponse {
        self.0.load(specifier, referrer, options)
    }
}

// ---------------------------------------------------------------------------
// Wiring 2: `NodeRequireLoader`
// ---------------------------------------------------------------------------

/// What `require()` reads a file with, and what decides whether that file is
/// CommonJS.
///
/// **This is where phase 3 goes.** `ensure_read_permission` is handed the
/// module's own [`PermissionsContainer`] on every read `require` performs, so a
/// module denied the filesystem is denied it here, once, rather than by hoping
/// each of its dependencies asks politely. It allows everything today, which is
/// exactly what the sidecar does.
#[derive(Debug)]
struct ModuleRequireLoader {
    pkg_json_resolver: Arc<PackageJsonResolver<Sys>>,
}

impl ModuleRequireLoader {
    /// node's own rule, and no more than it: the extension decides when it can,
    /// and otherwise the closest `package.json`'s `type` does — defaulting to
    /// CommonJS, because a v1 plugin is CommonJS and a package that means
    /// otherwise says so.
    fn maybe_cjs(&self, specifier: &deno_core::url::Url) -> Result<bool, PackageJsonLoadError> {
        let Ok(path) = deno_path_util::url_to_file_path(specifier) else {
            return Ok(false);
        };
        match path.extension().and_then(|e| e.to_str()) {
            Some("cjs" | "node") => Ok(true),
            Some("mjs" | "json") => Ok(false),
            _ => Ok(self
                .pkg_json_resolver
                .get_closest_package_json(&path)?
                .is_none_or(|pkg| pkg.typ != "module")),
        }
    }
}

impl NodeRequireLoader for ModuleRequireLoader {
    fn ensure_read_permission<'a>(
        &self,
        _permissions: &mut PermissionsContainer,
        path: Cow<'a, Path>,
    ) -> Result<Cow<'a, Path>, JsErrorBox> {
        Ok(path)
    }

    fn load_text_file_lossy(&self, path: &Path) -> Result<FastString, JsErrorBox> {
        let text = std::fs::read_to_string(path).map_err(JsErrorBox::from_err)?;
        Ok(FastString::from(text))
    }

    fn is_maybe_cjs(&self, specifier: &deno_core::url::Url) -> Result<bool, PackageJsonLoadError> {
        self.maybe_cjs(specifier)
    }

    fn is_maybe_cjs_from_require(
        &self,
        specifier: &deno_core::url::Url,
    ) -> Result<bool, PackageJsonLoadError> {
        self.maybe_cjs(specifier)
    }
}

// ---------------------------------------------------------------------------
// Wirings 3 and 4: byonm, and the system
// ---------------------------------------------------------------------------

/// Everything `deno_node` needs, built over one modules root.
///
/// **byonm — "bring your own `node_modules`"** — is what keeps this milestone to
/// one layer: npm is still the installer, with the same `--install-links`, the
/// same `@saltcorn/*` overrides and the same stub packages, and the directory on
/// disk after this change is the directory that was there before it. There is no
/// Deno npm cache, no lockfile and no registry client anywhere in the server;
/// `require("async-mqtt")` resolves out of `<modules root>/node_modules` and
/// nowhere else.
fn node_services(
    root: &Path,
) -> NodeExtInitServices<ByonmInNpmPackageChecker, ByonmNpmResolver<Sys>, Sys> {
    let sys = RealSys;
    let pkg_json_resolver = Arc::new(PackageJsonResolver::new(sys.clone(), None));
    let resolution_sys = NodeResolutionSys::new(sys.clone(), None);
    let npm_resolver = ByonmNpmResolver::new(ByonmNpmResolverCreateOptions {
        root_node_modules_dir: Some(root.join("node_modules")),
        search_stop_dir: None,
        sys: resolution_sys.clone(),
        pkg_json_resolver: pkg_json_resolver.clone(),
    });
    let node_resolver = Arc::new(NodeResolver::new(
        ByonmInNpmPackageChecker,
        DenoIsBuiltInNodeModuleChecker,
        npm_resolver,
        pkg_json_resolver.clone(),
        resolution_sys,
        Default::default(),
    ));
    NodeExtInitServices {
        node_require_loader: Rc::new(ModuleRequireLoader {
            pkg_json_resolver: pkg_json_resolver.clone(),
        }),
        node_resolver,
        pkg_json_resolver,
        sys,
    }
}

// ---------------------------------------------------------------------------
// The worker
// ---------------------------------------------------------------------------

/// Build one module-host worker over `root`, with `host` as its main module and
/// the two ends of the host protocol as its stdio.
///
/// Must be called from inside a tokio context: `deno_core` registers the isolate
/// against whatever runtime is current when it is created, and an isolate whose
/// delayed foreground tasks belong to no runtime aborts the process. The worker
/// thread's own current-thread runtime is that context, which is also what
/// drives the worker's event loop — the same arrangement `sc_expr`'s
/// `build_isolate` documents for the code pool.
///
/// `stderr` is inherited rather than piped: it is the modules' own logging, and
/// giving it its own pipe is phase 2's business, where `console.log` goes to
/// `sc-log` directly instead.
pub(super) fn build_worker(
    root: &Path,
    host: &ModuleSpecifier,
    stdin: std::fs::File,
    stdout: std::fs::File,
    max_heap: usize,
) -> MainWorker {
    let parser = Arc::new(RuntimePermissionDescriptorParser::new(RealSys));
    let services = WorkerServiceOptions {
        blob_store: Arc::new(BlobStore::default()),
        broadcast_channel: InMemoryBroadcastChannel::default(),
        deno_rt_native_addon_loader: None,
        feature_checker: Arc::new(deno_runtime::FeatureChecker::default()),
        fs: Arc::new(RealFs),
        module_loader: Rc::new(HostModuleLoader(FsModuleLoader)),
        node_services: Some(node_services(root)),
        npm_process_state_provider: None,
        // Phase 3 replaces this with a container built from the module's own
        // `_sc_modules` row. Until then a module has what it has in the sidecar,
        // which is everything.
        permissions: PermissionsContainer::allow_all(parser),
        root_cert_store_provider: None,
        fetch_dns_resolver: Default::default(),
        shared_array_buffer_store: None,
        compiled_wasm_module_store: None,
        v8_code_cache: None,
        bundle_provider: None,
    };
    let options = WorkerOptions {
        startup_snapshot: Some(SNAPSHOT),
        residual_lazy_js_sources: RESIDUAL_LAZY_JS,
        residual_lazy_esm_sources: RESIDUAL_LAZY_ESM,
        // The heap this worker's modules share. Without a limit the isolate is
        // bounded only by the machine, and the failure mode of that is the
        // *server's* process rather than one module's worker.
        create_params: Some(deno_core::v8::CreateParams::default().heap_limits(0, max_heap)),
        bootstrap: BootstrapOptions {
            // The one bit the node layer reads: there **is** a `node_modules`
            // directory, so `require` resolves out of it rather than out of a
            // Deno npm cache that does not exist. Without this, every module's
            // first `require` of a dependency fails.
            has_node_modules_dir: true,
            mode: WorkerExecutionMode::Run,
            ..Default::default()
        },
        stdio: Stdio {
            stdin: StdioPipe::file(stdin),
            stdout: StdioPipe::file(stdout),
            stderr: StdioPipe::inherit(),
        },
        ..Default::default()
    };
    MainWorker::bootstrap_from_options(host, services, options)
}
