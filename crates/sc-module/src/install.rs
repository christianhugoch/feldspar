//! Installing and uninstalling a module's package, with `npm`.
//!
//! **npm is the installer** because a package's dependency tree is npm's
//! problem: `@saltcorn/mqtt` is a thin wrapper over `async-mqtt`, which has a
//! tree of its own, and resolving that tree is a package manager's whole job.
//! The alternative — fetching a tarball and unpacking it — installs the wrapper
//! and nothing it needs.
//!
//! The modules root is one npm project this crate owns ([`crate::paths`]), so an
//! install is `npm install <spec>` in it and the package lands at
//! `node_modules/<name>`. A **local directory** installs the same way, with
//! `--install-links`, which **copies** it in rather than symlinking it. That is
//! deliberate and it is the difference between working and not: npm does not
//! install a symlinked package's dependencies (they are the checkout's own
//! business), and — worse — it ignores the project's `overrides` for them, so a
//! linked v1 plugin both fails to find `async-mqtt` *and* downloads the whole of
//! v1's server. A copy is an ordinary node in the tree, and both problems go
//! away. The price is that editing the checkout changes nothing until it is
//! installed again, which is the "Install" button pressed a second time.
//!
//! **The `@saltcorn/*` dependencies are never downloaded.** A v1 plugin depends
//! on `@saltcorn/data` — v1's server, the program this one replaces, and some
//! 270 MB with its tree — which the host's `require` hook answers itself before
//! a line of it could be read. So the project depends on a local stub package of
//! the same name and declares an npm **override** redirecting the module's
//! dependency to it (see [`V1_API_PACKAGES`] and
//! [`Installer::write_overrides`]).
//!
//! **What the package is called is read from the package**, never from what the
//! admin typed: a specifier may carry a version (`@saltcorn/mqtt@0.2.0`), may be
//! a path, and may be a `github:` shorthand. The name is recovered by diffing
//! the project's `dependencies` across the install, which is the one answer that
//! is right for all three.
//!
//! A failure carries **npm's own output** (§16): "npm exited 1" is not a
//! diagnosis, while the registry 404, the unreachable proxy or the ENOSPC
//! underneath it tells the admin what to do.

use std::path::{Path, PathBuf};

use sc_error::{Context, Error, Result};
use serde_json::{Map, Value as Json};
use tokio::process::Command;

use crate::module::ModuleSource;

/// Whether `node` is on this server's PATH.
///
/// Asked by the Modules tab before an admin types anything, because "install
/// failed: could not run npm" after filling in a form is a worse way to learn
/// that a server has no Node toolchain than a sentence above the form.
pub async fn have_node() -> bool {
    version_of("node").await
}

/// Whether `npm` is on this server's PATH.
pub async fn have_npm() -> bool {
    version_of("npm").await
}

async fn version_of(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

/// The name of the npm project file this crate writes into the modules root.
const PACKAGE_JSON: &str = "package.json";

/// The directory holding the stub packages every `@saltcorn/*` dependency is
/// redirected to — one subdirectory per package, because npm resolves an
/// override to a package by name and a stub called something else is quietly
/// ignored.
const STUB_DIR: &str = "v1-api-stub";

/// The v1 packages a module declares a dependency on, and that must **not** be
/// downloaded.
///
/// A v1 plugin depends on `@saltcorn/data` because that is v1's server, and
/// npm would dutifully fetch it and its tree — hundreds of megabytes of the
/// program this one replaces, which the host's `require` hook intercepts before
/// a line of it is read ([`crate::host`]'s stubs). So the project declares an
/// **override** for each, pointing at a local stub package: npm resolves the
/// dependency, downloads nothing, and the module loads exactly as it would
/// have.
///
/// The list is the v1 monorepo's published packages. It is not exhaustive
/// forever — a module may depend on one nobody here has heard of — so
/// [`Installer::extend_overrides`] adds whatever an installed module actually
/// declares.
const V1_API_PACKAGES: &[&str] = &[
    "@saltcorn/data",
    "@saltcorn/markup",
    "@saltcorn/types",
    "@saltcorn/db-common",
    "@saltcorn/base-plugin",
    "@saltcorn/server",
    "@saltcorn/admin-models",
    "@saltcorn/plugins-loader",
    "@saltcorn/common-code",
    "@saltcorn/filemanager",
    "@saltcorn/sbadmin2",
];

/// One stub package's manifest: the overridden package's own name, a version
/// above anything a module could ask for, and no code — which is the whole
/// point, since nothing ever requires it (the host answers `@saltcorn/*`
/// itself).
fn stub_json(package: &str) -> String {
    format!(
        r#"{{
  "name": "{package}",
  "version": "999.0.0",
  "private": true,
  "description": "Stands in for the Saltcorn v1 API package {package}. The module host answers every @saltcorn/* require itself, so nothing here is ever loaded.",
  "main": "index.js"
}}
"#
    )
}

/// Where a package's stub lives under [`STUB_DIR`]: its name with the scope's
/// slash flattened, so one directory holds them all.
fn stub_subdir(package: &str) -> String {
    package.trim_start_matches('@').replace('/', "-")
}

/// What an installed package turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPackage {
    /// The package's own name, from its `package.json`.
    pub name: String,
    /// The version installed, from the same place.
    pub version: String,
    /// npm's output, for the admin who wants to see what happened.
    pub log: String,
}

/// The npm project the server installs modules into.
#[derive(Debug, Clone)]
pub struct Installer {
    root: PathBuf,
}

impl Installer {
    /// An installer over `root`. The directory need not exist yet.
    pub fn new(root: impl Into<PathBuf>) -> Installer {
        Installer { root: root.into() }
    }

    /// The modules root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where a package by this name is installed, whether or not it is.
    pub fn package_dir(&self, name: &str) -> PathBuf {
        let mut dir = self.root.join("node_modules");
        // A scoped name (`@saltcorn/mqtt`) is two path components, which is how
        // npm lays it out; splitting on `/` is what makes that true here too.
        for part in name.split('/') {
            dir = dir.join(part);
        }
        dir
    }

    /// Create the modules root, its project file and the v1 API stub if they are
    /// not there, and make sure the project overrides every known v1 package.
    ///
    /// Idempotent, and called before every install: a root an operator deleted
    /// between two installs should be rebuilt rather than reported, and an
    /// override this version added since the project was written should reach an
    /// existing root too.
    pub async fn ensure_project(&self) -> Result<()> {
        tokio::fs::create_dir_all(&self.root)
            .await
            .with_context(|| format!("creating the modules directory {}", self.root.display()))?;

        let mut project = self.project().await?;
        if !project.contains_key("name") {
            project.insert("name".into(), Json::from("saltcorn-modules"));
            project.insert("version".into(), Json::from("1.0.0"));
            project.insert("private".into(), Json::from(true));
            project.insert(
                "description".into(),
                Json::from(
                    "Saltcorn modules installed by this server. Managed by Saltcorn; edit \
                     through Settings → Modules.",
                ),
            );
            project.insert("dependencies".into(), Json::Object(Map::new()));
        }
        self.write_overrides(&mut project, V1_API_PACKAGES.iter().copied())
            .await?;
        Ok(())
    }

    /// Write the stub package that stands in for `package`.
    async fn write_stub(&self, package: &str) -> Result<()> {
        let dir = self.root.join(STUB_DIR).join(stub_subdir(package));
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("creating {}", dir.display()))?;
        tokio::fs::write(dir.join(PACKAGE_JSON), stub_json(package))
            .await
            .with_context(|| format!("writing {}", dir.join(PACKAGE_JSON).display()))?;
        tokio::fs::write(dir.join("index.js"), STUB_INDEX)
            .await
            .with_context(|| format!("writing {}", dir.join("index.js").display()))?;
        Ok(())
    }

    /// Redirect each of `packages` to the stub, and write the project file back
    /// if that changed anything. Returns whether it did.
    ///
    /// Two entries per package, not one. The project takes the stub on as a
    /// **direct dependency** at its `file:` path, and the override is the
    /// *reference* `$<package>` — npm's spelling of "whatever the project
    /// itself depends on". Writing the `file:` path straight into `overrides`
    /// is the obvious thing and npm cannot do it: resolving a `file:` override
    /// for a dependency of a package installed with `--install-links` makes npm
    /// look for a manifest at `<the dependent>/0/package.json` and abort the
    /// install with ENOENT (npm 11.12). The reference costs nothing — the stub
    /// is installed into the modules root either way, which is where the host
    /// resolves `@saltcorn/*` from — and it is resolved before npm has a path
    /// to mangle.
    async fn write_overrides<'a>(
        &self,
        project: &mut Map<String, Json>,
        packages: impl Iterator<Item = &'a str>,
    ) -> Result<bool> {
        let mut overrides = match project.get("overrides") {
            Some(Json::Object(existing)) => existing.clone(),
            _ => Map::new(),
        };
        let mut dependencies = match project.get("dependencies") {
            Some(Json::Object(existing)) => existing.clone(),
            _ => Map::new(),
        };
        let mut changed = false;
        for package in packages {
            // **Absolute**, because npm resolves a relative `file:` dependency
            // against the project's own directory: right here today, wrong the
            // moment anything reads the file from somewhere else. An absolute
            // path is also self-healing — a modules root that moved rewrites
            // itself on the next install, because the spec no longer matches.
            let stub = Json::from(format!(
                "file:{}",
                self.root
                    .join(STUB_DIR)
                    .join(stub_subdir(package))
                    .display()
            ));
            let reference = Json::from(format!("${package}"));
            if overrides.get(package) != Some(&reference)
                || dependencies.get(package) != Some(&stub)
            {
                self.write_stub(package).await?;
                overrides.insert(package.to_owned(), reference);
                dependencies.insert(package.to_owned(), stub);
                changed = true;
            }
        }
        if !changed {
            return Ok(false);
        }
        project.insert("dependencies".into(), Json::Object(dependencies));
        project.insert("overrides".into(), Json::Object(overrides));
        let path = self.root.join(PACKAGE_JSON);
        let text = serde_json::to_string_pretty(&Json::Object(project.clone()))
            .map_err(|e| Error::msg(format!("writing the modules project file: {e}")))?;
        tokio::fs::write(&path, text)
            .await
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(true)
    }

    /// The project file as an object, or an empty one if there is none.
    async fn project(&self) -> Result<Map<String, Json>> {
        let path = self.root.join(PACKAGE_JSON);
        let Ok(text) = tokio::fs::read_to_string(&path).await else {
            return Ok(Map::new());
        };
        match serde_json::from_str::<Json>(&text) {
            Ok(Json::Object(object)) => Ok(object),
            _ => Err(Error::config(format!(
                "{} is not a readable npm project file; move it aside and reinstall the modules",
                path.display()
            ))),
        }
    }

    /// Add an override for every `@saltcorn/*` package the installed module
    /// declares and the project does not already redirect, reifying the tree
    /// afterwards so anything already fetched is pruned.
    ///
    /// The dynamic half of [`V1_API_PACKAGES`]: the list covers what v1
    /// published, and this covers what a module actually asked for.
    async fn extend_overrides(&self, name: &str) -> Result<String> {
        let declared = self.package_dependencies(name).await?;
        let saltcorn: Vec<String> = declared
            .keys()
            .filter(|dep| dep.starts_with("@saltcorn/"))
            .cloned()
            .collect();
        if saltcorn.is_empty() {
            return Ok(String::new());
        }
        let mut project = self.project().await?;
        if !self
            .write_overrides(&mut project, saltcorn.iter().map(String::as_str))
            .await?
        {
            return Ok(String::new());
        }
        self.npm(&install_args("")).await
    }

    /// An installed package's declared dependencies.
    async fn package_dependencies(&self, name: &str) -> Result<Map<String, Json>> {
        let manifest = self.package_dir(name).join(PACKAGE_JSON);
        let Ok(text) = tokio::fs::read_to_string(&manifest).await else {
            return Ok(Map::new());
        };
        let json: Json = serde_json::from_str(&text)
            .with_context(|| format!("reading {}", manifest.display()))?;
        Ok(match json.get("dependencies") {
            Some(Json::Object(deps)) => deps.clone(),
            _ => Map::new(),
        })
    }

    /// Install the package `location` names, and report what it turned out to
    /// be.
    ///
    /// `source` decides only how `location` is interpreted: a registry
    /// specifier is passed to npm as it stands, and a local directory is
    /// canonicalised first — npm records the path it is given, and a relative
    /// one would be resolved against the modules root rather than against
    /// wherever the admin was standing.
    pub async fn install(&self, source: ModuleSource, location: &str) -> Result<InstalledPackage> {
        self.ensure_project().await?;

        let spec = match source {
            ModuleSource::Npm => {
                let spec = location.trim();
                if spec.is_empty() {
                    return Err(Error::invalid("a module needs an npm package name"));
                }
                spec.to_owned()
            }
            ModuleSource::Local => {
                let path = PathBuf::from(location.trim());
                let path = tokio::fs::canonicalize(&path).await.map_err(|e| {
                    Error::invalid(format!(
                        "no directory at {} to install a module from: {e}",
                        path.display()
                    ))
                })?;
                if !path.join(PACKAGE_JSON).is_file() {
                    return Err(Error::invalid(format!(
                        "{} is not an npm package: it has no {PACKAGE_JSON}",
                        path.display()
                    )));
                }
                path.display().to_string()
            }
        };

        let before = self.dependencies().await?;
        let mut log = self.npm(&install_args(&spec)).await?;
        let after = self.dependencies().await?;

        let name = self.installed_name(&before, &after, source, location)?;
        log.push_str(&self.extend_overrides(&name).await?);
        let version = self.installed_version(&name).await?;
        Ok(InstalledPackage { name, version, log })
    }

    /// Remove a package from the project.
    ///
    /// Best effort by design: the row is what makes a module *exist* to this
    /// server, so a package npm declines to remove must not keep the module
    /// installed. The caller deletes the row either way and this reports what
    /// npm said.
    pub async fn uninstall(&self, name: &str) -> Result<String> {
        if !self.root.join(PACKAGE_JSON).is_file() {
            // Nothing was ever installed here; there is nothing to remove and
            // no npm project to run in.
            return Ok(String::new());
        }
        self.npm(&["uninstall", "--no-audit", "--no-fund", name])
            .await
    }

    /// The version of an installed package, from its own `package.json`.
    pub async fn installed_version(&self, name: &str) -> Result<String> {
        let manifest = self.package_dir(name).join(PACKAGE_JSON);
        let text = tokio::fs::read_to_string(&manifest).await.map_err(|e| {
            Error::config(format!(
                "the module `{name}` was installed but no package is at {}: {e}",
                manifest.display()
            ))
        })?;
        let json: Json = serde_json::from_str(&text)
            .with_context(|| format!("reading {}", manifest.display()))?;
        Ok(json
            .get("version")
            .and_then(Json::as_str)
            .unwrap_or("0.0.0")
            .to_owned())
    }

    /// Whether a package by this name is on disk.
    pub fn is_installed(&self, name: &str) -> bool {
        self.package_dir(name).join(PACKAGE_JSON).is_file()
    }

    /// Run npm in the modules root, returning its combined output.
    async fn npm(&self, args: &[&str]) -> Result<String> {
        let line = format!("npm {}", args.join(" "));
        let output = Command::new("npm")
            .args(args)
            .current_dir(&self.root)
            .output()
            .await
            .map_err(|e| {
                Error::config(format!(
                    "could not run `{line}` in {}: {e}. Installing a module needs Node.js and \
                     npm on this server's PATH",
                    self.root.display()
                ))
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            // npm's own diagnostics are the only useful part of this error
            // (§16), so carry them rather than the exit code alone.
            return Err(Error::config(format!(
                "`{line}` failed in {} with {}\n{}",
                self.root.display(),
                output.status,
                tail(&stderr, &stdout)
            )));
        }
        Ok(tail(&stdout, &stderr))
    }

    /// The project's declared dependencies, by name.
    async fn dependencies(&self) -> Result<Map<String, Json>> {
        let project = self.root.join(PACKAGE_JSON);
        let Ok(text) = tokio::fs::read_to_string(&project).await else {
            return Ok(Map::new());
        };
        let json: Json = serde_json::from_str(&text)
            .with_context(|| format!("reading {}", project.display()))?;
        Ok(match json.get("dependencies") {
            Some(Json::Object(deps)) => deps.clone(),
            _ => Map::new(),
        })
    }

    /// Which package the install actually added.
    ///
    /// The dependency that appeared (or whose specifier changed) is the answer,
    /// because npm writes exactly one entry per `npm install <spec>` and writes
    /// it under the package's **own** name. A reinstall of something already
    /// present at the same specifier changes nothing, and then the name is
    /// recovered from the source: a local package says its name in its
    /// `package.json`, and a registry specifier is its name plus an optional
    /// `@version`.
    fn installed_name(
        &self,
        before: &Map<String, Json>,
        after: &Map<String, Json>,
        source: ModuleSource,
        location: &str,
    ) -> Result<String> {
        let mut changed: Vec<&String> = after
            .iter()
            .filter(|(name, spec)| before.get(*name) != Some(*spec))
            .map(|(name, _)| name)
            .collect();
        if changed.len() == 1 {
            return Ok(changed.remove(0).clone());
        }

        match source {
            ModuleSource::Local => local_package_name(Path::new(location.trim())),
            ModuleSource::Npm => npm_spec_name(location.trim()),
        }
    }
}

/// The npm command every install runs, with `spec` appended when there is one
/// (an empty `spec` reifies the tree as it stands).
///
/// `--install-links` is the load-bearing one: it makes a local directory a
/// **copy** in the tree rather than a symlink, which is what gets its
/// dependencies installed and the project's `@saltcorn/*` overrides honoured.
fn install_args(spec: &str) -> Vec<&str> {
    let mut args = vec![
        "install",
        "--install-links",
        "--omit=dev",
        "--no-audit",
        "--no-fund",
    ];
    if !spec.is_empty() {
        args.push(spec);
    }
    args
}

/// The stub package's `index.js`. Never loaded — see [`stub_json`].
const STUB_INDEX: &str = "// The Saltcorn v1 API is answered by the module host itself (see\n                          // `module-host.mjs`); this package exists only so npm has something\n                          // to resolve a module's `@saltcorn/*` dependency to.\n                          module.exports = {};\n";

/// The `name` in a local directory's `package.json`.
fn local_package_name(dir: &Path) -> Result<String> {
    let manifest = dir.join(PACKAGE_JSON);
    let text = std::fs::read_to_string(&manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let json: Json =
        serde_json::from_str(&text).with_context(|| format!("reading {}", manifest.display()))?;
    json.get("name")
        .and_then(Json::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            Error::invalid(format!(
                "{} declares no package name, so there is nothing to install it as",
                manifest.display()
            ))
        })
}

/// The package name in a registry specifier: `@saltcorn/mqtt@0.2.0` is
/// `@saltcorn/mqtt`, `mqtt@1` is `mqtt`.
fn npm_spec_name(spec: &str) -> Result<String> {
    let (name, _version) = match spec.strip_prefix('@') {
        // A scoped name's own `@` is the first character, so the version
        // separator is the *next* one.
        Some(rest) => match rest.split_once('@') {
            Some((name, version)) => (format!("@{name}"), Some(version)),
            None => (spec.to_owned(), None),
        },
        None => match spec.split_once('@') {
            Some((name, version)) => (name.to_owned(), Some(version)),
            None => (spec.to_owned(), None),
        },
    };
    if name.is_empty() {
        return Err(Error::invalid(format!(
            "`{spec}` does not name an npm package"
        )));
    }
    Ok(name)
}

/// The tail of a command's output: stderr if it said anything, else stdout,
/// capped so an error message stays readable.
fn tail(first: &str, second: &str) -> String {
    let text = if first.trim().is_empty() {
        second
    } else {
        first
    };
    const MAX: usize = 4000;
    if text.len() <= MAX {
        return text.trim().to_owned();
    }
    let start = text.len() - MAX;
    // Do not split a UTF-8 sequence: walk forward to a boundary.
    let start = (start..text.len())
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(text.len());
    format!("…\n{}", text[start..].trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scoped_specifier_keeps_its_scope_and_loses_its_version() {
        assert_eq!(npm_spec_name("@saltcorn/mqtt").unwrap(), "@saltcorn/mqtt");
        assert_eq!(
            npm_spec_name("@saltcorn/mqtt@0.2.0").unwrap(),
            "@saltcorn/mqtt"
        );
        assert_eq!(npm_spec_name("mqtt").unwrap(), "mqtt");
        assert_eq!(npm_spec_name("mqtt@^4").unwrap(), "mqtt");
    }

    #[test]
    fn a_scoped_package_is_two_path_components_as_npm_lays_it_out() {
        let installer = Installer::new("/srv/modules");
        assert_eq!(
            installer.package_dir("@saltcorn/mqtt"),
            PathBuf::from("/srv/modules/node_modules/@saltcorn/mqtt")
        );
        assert_eq!(
            installer.package_dir("mqtt"),
            PathBuf::from("/srv/modules/node_modules/mqtt")
        );
    }

    #[test]
    fn the_added_dependency_is_the_package_that_was_installed() {
        let installer = Installer::new("/srv/modules");
        let mut before = Map::new();
        before.insert("left-pad".into(), Json::from("^1.3.0"));
        let mut after = before.clone();
        after.insert("@saltcorn/mqtt".into(), Json::from("^0.2.0"));
        let name = installer
            .installed_name(&before, &after, ModuleSource::Npm, "@saltcorn/mqtt")
            .unwrap();
        assert_eq!(name, "@saltcorn/mqtt");
    }

    #[test]
    fn a_reinstall_that_changes_nothing_falls_back_to_the_specifier() {
        let installer = Installer::new("/srv/modules");
        let mut deps = Map::new();
        deps.insert("@saltcorn/mqtt".into(), Json::from("^0.2.0"));
        let name = installer
            .installed_name(&deps, &deps, ModuleSource::Npm, "@saltcorn/mqtt@0.2.0")
            .unwrap();
        assert_eq!(name, "@saltcorn/mqtt");
    }

    #[test]
    fn the_output_tail_prefers_stderr_and_stays_readable() {
        assert_eq!(tail("boom", "fine"), "boom");
        assert_eq!(tail("   ", "fine"), "fine");
        let long = "x".repeat(9000);
        let cut = tail(&long, "");
        assert!(cut.len() < 4100, "{}", cut.len());
        assert!(cut.starts_with('…'));
    }
}
