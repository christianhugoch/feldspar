//! Wiring a code framework's **build step** (design §13.3).
//!
//! A code framework's source lives in a git repository that is (a sub-directory
//! of) one of the application's file stores, and it has a build step: a bundler
//! is invoked over the source and emits a static bundle, which is what
//! [`CodeFramework`] serves. This module runs that step.
//!
//! The bundler is an external process handed a working directory, so it needs a
//! real on-disk tree — which is why the source store must be one with a
//! [`local_path`](sc_files::FileStore::local_path). An object-store-backed app is
//! rejected with a clear error rather than silently failing to build.
//!
//! Two layers, mirroring the `serve`/`handle` split in [`crate::framework`]:
//! [`run_build`] takes a plain directory and is exercisable without a database,
//! and [`build_app`] resolves the store through the [`Catalog`] and delegates to
//! it.

use std::path::{Path, PathBuf};

use bytes::Bytes;
use sc_api::{EndpointSet, generate_client};
use sc_catalog::{Attrs, Catalog, FileStoreId};
use sc_error::{Context, Error, Result};
use sc_types::FormField;
use serde_json::Value as Json;
use tokio::process::Command;

use crate::api::app_endpoints;
use crate::application::{Application, FrameworkRef};
use crate::framework::{
    AssetBundle, BuildSpec, CFG_CLIENT, CFG_COMMAND, CFG_OUTPUT, CFG_SOURCE, CFG_STORE,
    CODE_FRAMEWORK, CodeFramework, code_config_spec, validate_framework_config_structure,
};

/// How much of a failed build's output to quote in the error. A bundler can emit
/// a great deal on failure; the tail holds the actual error, and the whole of it
/// does not belong in an error message.
const OUTPUT_TAIL_BYTES: usize = 4096;

/// Where a code framework's source lives: the file store holding it, and the
/// build step to run over it (design §13.3).
///
/// The source and output sub-directories are carried by the [`BuildSpec`],
/// relative to the store root — so an app whose store is `apps` with a spec
/// sourced at `blog/web` builds from `<apps>/blog/web`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSource {
    /// The file store the app's source tree lives in. Expected to be a git repo
    /// (§13.3); [`BuildReport::git_repo`] reports whether it actually is.
    pub store: FileStoreId,
    /// The build step to run.
    pub build: BuildSpec,
    /// Where to emit the app's generated TypeScript client, relative to the
    /// store (e.g. `web/src/client.ts`). `None` for an app that does not consume
    /// a generated client.
    pub client_path: Option<String>,
}

impl AppSource {
    /// Source in file store `store`, built by `build`, with no generated client.
    pub fn new(store: FileStoreId, build: BuildSpec) -> AppSource {
        AppSource {
            store,
            build,
            client_path: None,
        }
    }

    /// Emit the generated TypeScript client at `path` (relative to the store)
    /// before building, returning `self` for chaining.
    pub fn with_client(mut self, path: impl Into<String>) -> AppSource {
        self.client_path = Some(path.into());
        self
    }
}

/// Resolve a code framework's [`AppSource`] (and its [`BuildSpec`]) **from a
/// stored application's framework config** (design §13.2/§13.3).
///
/// This is the join between "an app is a row the admin filled in" and "an app is
/// a thing that builds": before this, the store/sub-directories/build command
/// were hand-built in Rust at the call site, which is exactly the configuration
/// path §13.2 rules out. Now they come from [`FrameworkRef::config`], validated
/// against [`code_config_spec`].
///
/// The config's *structure* is validated first, so a missing or ill-typed
/// setting surfaces as the same [`Error::invalid`] naming the setting that a save
/// would have rejected — a stored app should never reach here invalid, but a
/// caller building a `FrameworkRef` in memory can. Whether the named store
/// actually exists is deliberately **not** re-checked here: that was settled on
/// save, and a build failing over it would report a configuration problem when
/// the real news is that the store cannot be resolved, which the build says
/// anyway and says better.
pub fn app_source_from_config(fw: &FrameworkRef) -> Result<AppSource> {
    if fw.name != CODE_FRAMEWORK {
        return Err(Error::config(format!(
            "framework `{}` has no build step; only `{CODE_FRAMEWORK}` builds from a file store",
            fw.name
        )));
    }
    validate_framework_config_structure(fw)?;

    let spec = code_config_spec();
    let store = required_setting(&spec, &fw.config, CFG_STORE)?;
    let output_dir = required_setting(&spec, &fw.config, CFG_OUTPUT)?;
    let command_line = required_setting(&spec, &fw.config, CFG_COMMAND)?;
    // Optional, and defaulted to the store root by the spec.
    let source_dir = setting(&spec, &fw.config, CFG_SOURCE)?.unwrap_or_default();
    let client = setting(&spec, &fw.config, CFG_CLIENT)?.filter(|p| !p.trim().is_empty());

    let (command, args) = split_command(&command_line)?;
    let source = AppSource::new(
        FileStoreId(store),
        BuildSpec {
            command,
            args,
            source_dir,
            output_dir,
        },
    );
    Ok(match client {
        Some(path) => source.with_client(path),
        None => source,
    })
}

/// A setting's resolved value (stored, else the spec's default) as a string.
///
/// `Ok(None)` means "not set and no default"; a non-string value is impossible
/// once the config has been validated against a spec whose fields are all text,
/// so it is reported rather than silently coerced.
fn setting(spec: &[FormField], config: &Attrs, name: &str) -> Result<Option<String>> {
    let field = spec
        .iter()
        .find(|f| f.name() == name)
        .ok_or_else(|| Error::config(format!("`{CODE_FRAMEWORK}` declares no `{name}` setting")))?;
    match field.resolve(config) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(Error::invalid(format!(
            "setting `{name}` should be text, got {other}"
        ))),
    }
}

/// A setting that must be present — [`Error::invalid`] naming it if it is not.
fn required_setting(spec: &[FormField], config: &Attrs, name: &str) -> Result<String> {
    setting(spec, config, name)?
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| Error::invalid(format!("setting `{name}` is required")))
}

/// Split a build command line into the executable and its arguments.
///
/// Whitespace-separated, with **no quoting or escaping**: `npm run build` is the
/// shape this is for, and an argument containing a space cannot be expressed. The
/// alternative — a separate list-of-arguments setting — needs a form control the
/// MVP does not have (§6.2's list options are post-MVP), and a real shell parse
/// would invite the quoting bugs that come with it. The bundler is spawned
/// directly, not through a shell, so this is a split rather than shell semantics:
/// no globbing, no pipes, no substitution.
fn split_command(line: &str) -> Result<(String, Vec<String>)> {
    let mut parts = line.split_whitespace().map(str::to_owned);
    let command = parts.next().ok_or_else(|| {
        Error::invalid(format!(
            "setting `{CFG_COMMAND}` is empty; it should be a build command such as `npm run build`"
        ))
    })?;
    Ok((command, parts.collect()))
}

/// The outcome of a successful build.
#[derive(Debug, Clone)]
pub struct BuildReport {
    /// The bundle loaded from the build's output directory — what the framework
    /// serves.
    pub bundle: AssetBundle,
    /// The absolute output directory the bundle was loaded from.
    pub output_dir: PathBuf,
    /// Whether the source store is a git repository. The design expects it to be
    /// (§13.3), but a build from a plain directory still works, so this is
    /// reported rather than enforced.
    pub git_repo: bool,
    /// What the bundler wrote to stdout, for surfacing in build logs.
    pub stdout: String,
    /// What the bundler wrote to stderr. Bundlers routinely report progress here
    /// on success, so this being non-empty does not mean the build failed.
    pub stderr: String,
    /// The path the generated TypeScript client was emitted to, when the build
    /// went through [`build_application`] and the app declares one.
    pub client_path: Option<String>,
}

/// Run an application's build step, resolving its source store through the
/// catalog.
///
/// Fails if the store is not connected, has no on-disk path, or the build itself
/// fails.
pub async fn build_app(cat: &Catalog, source: &AppSource) -> Result<BuildReport> {
    let store = cat.require_file_store(&source.store.0)?;
    let root = store.local_path("")?.ok_or_else(|| {
        Error::config(format!(
            "file store {:?} has no local path, so it cannot host a code framework's \
             build step; use a local store",
            source.store.0
        ))
    })?;
    let mut report = run_build(&source.build, &root).await?;
    report.git_repo = store.is_git_repo();
    Ok(report)
}

/// Emit an application's typed TypeScript client into its source tree, then
/// build it — the whole path from an [`Application`] to a servable bundle.
///
/// The client is generated from the app's own [`EndpointSet`](sc_api::EndpointSet)
/// (every provider it enables, projected — see [`app_endpoints`]) by the same
/// generator the admin SPA's client comes from (§13.1). It is written **before**
/// the bundler runs, because the app's source imports it: an app's endpoints
/// depend on which tables it declares, so unlike the admin's client it cannot be
/// a checked-in artifact and is regenerated on every build. An app that declares
/// no [`client_path`](AppSource::client_path) just builds.
pub async fn build_application(
    cat: &Catalog,
    app: &Application,
    source: &AppSource,
) -> Result<BuildReport> {
    let client_path = emit_client(cat, source, &app_endpoints(app, cat)?).await?;
    let mut report = build_app(cat, source).await?;
    report.client_path = client_path;
    Ok(report)
}

/// Write `endpoints` as a generated TypeScript client into the app's source
/// tree, at [`AppSource::client_path`].
///
/// Returns the path written, or `None` when the app declares no client path.
/// Written through the [`FileStore`](sc_files::FileStore), not the local
/// filesystem, so the source tree is reached the same way everything else
/// reaches it (and the store's own traversal sandboxing applies).
pub async fn emit_client(
    cat: &Catalog,
    source: &AppSource,
    endpoints: &EndpointSet,
) -> Result<Option<String>> {
    let Some(path) = &source.client_path else {
        return Ok(None);
    };
    let store = cat.require_file_store(&source.store.0)?;
    let client = generate_client(endpoints);
    store.write(path, Bytes::from(client.into_bytes())).await?;
    Ok(Some(path.clone()))
}

/// Build an application and return a [`CodeFramework`] serving the result, with
/// the [`BuildSpec`] attached.
///
/// This is the whole path from "source in a file store" to "framework that serves
/// the bundle".
pub async fn build_code_framework(
    cat: &Catalog,
    name: impl Into<String>,
    source: &AppSource,
) -> Result<CodeFramework> {
    let report = build_app(cat, source).await?;
    Ok(CodeFramework::new(name, report.bundle).with_build(source.build.clone()))
}

/// Run `spec` with `root` as the file-store root: invoke the bundler in the
/// source directory and load the bundle from the output directory.
///
/// Catalog-free so it is testable without a database; [`build_app`] wraps it.
/// [`BuildReport::git_repo`] is always `false` here — only the store knows.
pub async fn run_build(spec: &BuildSpec, root: &Path) -> Result<BuildReport> {
    let source_dir = resolve_under(root, &spec.source_dir)?;
    if !source_dir.is_dir() {
        return Err(Error::config(format!(
            "build source directory {} does not exist",
            source_dir.display()
        )));
    }

    let output = Command::new(&spec.command)
        .args(&spec.args)
        .current_dir(&source_dir)
        .output()
        .await
        .with_context(|| {
            format!(
                "launching build command `{}` in {}",
                command_line(spec),
                source_dir.display()
            )
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if !output.status.success() {
        // The bundler's own diagnostics are the only useful part of this error,
        // so carry them rather than just the exit code (principle 5).
        return Err(Error::config(format!(
            "build command `{}` failed in {} with {}\n{}",
            command_line(spec),
            source_dir.display(),
            output.status,
            tail(&stderr, &stdout)
        )));
    }

    let output_dir = resolve_under(root, &spec.output_dir)?;
    if !output_dir.is_dir() {
        return Err(Error::config(format!(
            "build command `{}` succeeded but produced no output directory {}",
            command_line(spec),
            output_dir.display()
        )));
    }

    let bundle = AssetBundle::from_dir(&output_dir)?;
    if bundle.is_empty() {
        return Err(Error::config(format!(
            "build command `{}` produced an empty output directory {}",
            command_line(spec),
            output_dir.display()
        )));
    }

    Ok(BuildReport {
        bundle,
        output_dir,
        git_repo: false,
        stdout,
        stderr,
        client_path: None,
    })
}

/// Join a store-relative path onto `root`, rejecting anything that escapes it.
///
/// The same sandboxing the file store applies to reads and writes: a build spec
/// is admin-supplied configuration, and it does not get to reach outside the
/// app's store.
fn resolve_under(root: &Path, rel: &str) -> Result<PathBuf> {
    let mut out = root.to_path_buf();
    for segment in rel.split(['/', '\\']) {
        match segment {
            "" | "." => continue,
            ".." => {
                return Err(Error::invalid(format!(
                    "build path {rel:?} escapes the file store root"
                )));
            }
            seg => out.push(seg),
        }
    }
    Ok(out)
}

/// Render a spec's command line for diagnostics, e.g. `npm run build`.
fn command_line(spec: &BuildSpec) -> String {
    let mut line = spec.command.clone();
    for arg in &spec.args {
        line.push(' ');
        line.push_str(arg);
    }
    line
}

/// The tail of a failed build's output: stderr when it said anything, else
/// stdout — bundlers differ on which stream they fail to.
fn tail<'a>(stderr: &'a str, stdout: &'a str) -> &'a str {
    let text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    let text = text.trim_end();
    if text.len() <= OUTPUT_TAIL_BYTES {
        return text;
    }
    // Cut at a char boundary so the tail stays valid UTF-8.
    let mut start = text.len() - OUTPUT_TAIL_BYTES;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framework::AppRequest;

    /// A scratch directory removed when the guard drops, so a failing assertion
    /// does not leave a tree behind.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            // Unique per test: the pid alone collides across tests in a binary.
            let unique = format!(
                "sc-app-build-{}-{tag}-{:?}",
                std::process::id(),
                std::thread::current().id()
            );
            let dir = std::env::temp_dir().join(unique);
            std::fs::remove_dir_all(&dir).ok();
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    /// A stand-in bundler: a shell script that writes a bundle into `$1`. Real
    /// bundlers (vite, webpack) are exactly this from the build step's point of
    /// view — a process that emits files — and depending on a Node toolchain
    /// here would make the Rust test suite need one.
    fn write_fake_bundler(source: &Path, script: &str) {
        let path = source.join("build.sh");
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn spec(args: &[&str]) -> BuildSpec {
        BuildSpec {
            command: "sh".to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            source_dir: "web".to_owned(),
            output_dir: "web/dist".to_owned(),
        }
    }

    /// A fully-populated `code` framework config — what the admin UI's form for
    /// `code_config_spec` would post back.
    fn code_config() -> FrameworkRef {
        FrameworkRef::new(CODE_FRAMEWORK)
            .with(CFG_STORE, "apps")
            .with(CFG_SOURCE, "web")
            .with(CFG_OUTPUT, "web/dist")
            .with(CFG_COMMAND, "npm run build")
    }

    #[test]
    fn a_valid_config_resolves_to_a_build_spec() {
        let source = app_source_from_config(&code_config()).expect("valid config");

        assert_eq!(source.store, FileStoreId("apps".to_owned()));
        // The build command is split into the executable and its arguments.
        assert_eq!(source.build.command, "npm");
        assert_eq!(source.build.args, ["run", "build"]);
        assert_eq!(source.build.source_dir, "web");
        assert_eq!(source.build.output_dir, "web/dist");
        // `client` is optional: not set means no generated client.
        assert_eq!(source.client_path, None);

        // Set, it comes through.
        let with_client = code_config().with(CFG_CLIENT, "web/src/client.ts");
        let source = app_source_from_config(&with_client).expect("valid config");
        assert_eq!(source.client_path.as_deref(), Some("web/src/client.ts"));
    }

    #[test]
    fn the_source_directory_defaults_to_the_store_root() {
        // `source` is the one optional directory: an app whose source sits at the
        // store root should not have to say so.
        let mut config = code_config();
        config.config.remove(CFG_SOURCE);
        let source = app_source_from_config(&config).expect("valid config");
        assert_eq!(source.build.source_dir, "");
    }

    #[test]
    fn a_missing_setting_is_an_invalid_error_naming_it() {
        for missing in [CFG_STORE, CFG_OUTPUT, CFG_COMMAND] {
            let mut config = code_config();
            config.config.remove(missing);
            let err = app_source_from_config(&config)
                .expect_err("should reject a config missing a required setting")
                .to_string();
            assert!(err.contains(missing), "should name `{missing}`: {err}");
            assert!(err.contains("required"), "{err}");
            // And name the framework, so an app with extra frameworks is
            // unambiguous.
            assert!(err.contains(CODE_FRAMEWORK), "{err}");
        }
    }

    #[test]
    fn an_ill_typed_setting_is_an_invalid_error_naming_it() {
        // The admin UI renders `store` as a text control, so a number here means
        // something posted the wrong shape — better said than coerced.
        let config = FrameworkRef::new(CODE_FRAMEWORK)
            .with(CFG_STORE, 42)
            .with(CFG_OUTPUT, "web/dist")
            .with(CFG_COMMAND, "npm run build");
        let err = app_source_from_config(&config)
            .expect_err("should reject an ill-typed setting")
            .to_string();
        assert!(err.contains(CFG_STORE), "{err}");
        assert!(err.contains("text"), "should say what it wanted: {err}");
        assert!(err.contains("a number"), "should say what it got: {err}");
    }

    #[test]
    fn an_unknown_setting_is_rejected_rather_than_ignored() {
        let config = code_config().with("sorce", "web");
        let err = app_source_from_config(&config)
            .expect_err("should reject an unknown setting")
            .to_string();
        assert!(err.contains("sorce"), "{err}");
    }

    #[test]
    fn an_empty_build_command_is_rejected() {
        // Whitespace-only passes the type check but is not a command; the error
        // says what one looks like.
        let config = code_config().with(CFG_COMMAND, "   ");
        let err = app_source_from_config(&config)
            .expect_err("should reject a blank command")
            .to_string();
        assert!(err.contains(CFG_COMMAND), "{err}");
    }

    #[test]
    fn only_a_code_framework_resolves_a_build_step() {
        let err = app_source_from_config(&FrameworkRef::new("saltcorn-v1"))
            .expect_err("a build-less framework has no source to resolve")
            .to_string();
        assert!(err.contains("saltcorn-v1"), "{err}");
    }

    #[test]
    fn a_single_word_command_has_no_arguments() {
        let config = code_config().with(CFG_COMMAND, "build.sh");
        let source = app_source_from_config(&config).expect("valid config");
        assert_eq!(source.build.command, "build.sh");
        assert!(source.build.args.is_empty());
    }

    /// A source tree whose "bundler" emits an index.html + a JS asset into dist.
    fn good_source(root: &Path) {
        let web = root.join("web");
        std::fs::create_dir_all(&web).unwrap();
        write_fake_bundler(
            &web,
            "#!/bin/sh\n\
             set -e\n\
             mkdir -p dist/assets\n\
             printf '<!doctype html><div id=root>' > dist/index.html\n\
             printf 'console.log(1)' > dist/assets/app.js\n\
             echo built\n",
        );
    }

    #[tokio::test]
    async fn build_invokes_the_bundler_and_loads_its_output() {
        let tmp = TempDir::new("ok");
        good_source(tmp.path());

        let report = run_build(&spec(&["build.sh"]), tmp.path()).await.unwrap();

        // The bundler ran in the source dir and its output was picked up.
        assert_eq!(report.output_dir, tmp.path().join("web").join("dist"));
        assert_eq!(report.bundle.len(), 2);
        assert!(report.bundle.get("index.html").is_some());
        assert_eq!(
            report.bundle.get("assets/app.js").unwrap().content_type,
            "text/javascript; charset=utf-8"
        );
        assert!(report.stdout.contains("built"));
    }

    #[tokio::test]
    async fn built_bundle_is_served_by_a_code_framework() {
        let tmp = TempDir::new("serve");
        good_source(tmp.path());

        let report = run_build(&spec(&["build.sh"]), tmp.path()).await.unwrap();
        let fw = CodeFramework::new("code", report.bundle);

        // The freshly built index.html is served at `/`...
        let root = fw.serve(&AppRequest::get("/"));
        assert_eq!(root.status, 200);
        assert_eq!(&root.body[..], b"<!doctype html><div id=root>");

        // ...its assets at their own paths...
        assert_eq!(fw.serve(&AppRequest::get("/assets/app.js")).status, 200);

        // ...and a client-routed deep link falls back to the SPA entry point,
        // because `from_dir` adopted index.html as the fallback.
        let deep = fw.serve(&AppRequest::get("/posts/42"));
        assert_eq!(deep.status, 200);
        assert_eq!(&deep.body[..], b"<!doctype html><div id=root>");
    }

    #[tokio::test]
    async fn a_failing_build_reports_the_bundler_diagnostics() {
        let tmp = TempDir::new("fail");
        let web = tmp.path().join("web");
        std::fs::create_dir_all(&web).unwrap();
        write_fake_bundler(
            &web,
            "#!/bin/sh\necho 'TS2304: Cannot find name foo' >&2\nexit 2\n",
        );

        let err = run_build(&spec(&["build.sh"]), tmp.path())
            .await
            .unwrap_err();
        let msg = err.to_string();
        // No silent failures: the bundler's own error reaches the caller.
        assert!(msg.contains("TS2304: Cannot find name foo"), "{msg}");
        assert!(msg.contains("build.sh"), "{msg}");
    }

    #[tokio::test]
    async fn a_build_producing_no_output_is_an_error() {
        let tmp = TempDir::new("noout");
        let web = tmp.path().join("web");
        std::fs::create_dir_all(&web).unwrap();
        // Exits 0 but writes no dist/ — a success status is not proof of a bundle.
        write_fake_bundler(&web, "#!/bin/sh\nexit 0\n");

        let err = run_build(&spec(&["build.sh"]), tmp.path())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("produced no output directory"));
    }

    #[tokio::test]
    async fn a_missing_source_directory_is_an_error() {
        let tmp = TempDir::new("nosrc");
        let err = run_build(&spec(&["build.sh"]), tmp.path())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[tokio::test]
    async fn a_missing_bundler_is_an_error_not_a_panic() {
        let tmp = TempDir::new("nocmd");
        std::fs::create_dir_all(tmp.path().join("web")).unwrap();
        let mut spec = spec(&["build.sh"]);
        spec.command = "definitely-not-a-real-bundler".to_owned();

        let err = run_build(&spec, tmp.path()).await.unwrap_err();
        assert!(err.to_string().contains("launching build command"));
    }

    #[test]
    fn build_paths_cannot_escape_the_file_store() {
        let root = Path::new("/srv/store");
        // Ordinary relative paths resolve under the root.
        assert_eq!(
            resolve_under(root, "web/dist").unwrap(),
            Path::new("/srv/store/web/dist")
        );
        // `..` is refused rather than resolved, as it is for reads/writes.
        assert!(resolve_under(root, "../../etc").is_err());
        assert!(resolve_under(root, "web/../../etc").is_err());
        // An empty path is the root itself.
        assert_eq!(resolve_under(root, "").unwrap(), root);
    }
}
