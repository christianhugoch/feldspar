//! Git-repository [`FileStore`] driver: a store whose contents are a **working
//! tree** cloned from a remote, with the repository operations an admin needs to
//! move changes in and out of it.
//!
//! ## What is new here, and what is not
//!
//! The bytes are on local disk, so everything [`LocalFileStore`] does — reading,
//! writing, listing, the traversal sandbox, xattr metadata — is exactly right
//! and is delegated to it unchanged. What a git store adds is *where the
//! directory comes from* and *what else can be done to it*:
//!
//! - **Where.** The admin supplies a URL, not a path. The clone lives in an
//!   OS-appropriate data directory ([`data_dir`]) that Saltcorn owns, because a
//!   directory Saltcorn created by cloning is Saltcorn's to place — unlike a
//!   `local` store's directory, which is the admin's own and must be named by
//!   them.
//! - **What else.** [`GitRepo`] carries `pull`, `push`, `commit_all` and
//!   `status`. These are not [`FileStore`] methods and deliberately so: the
//!   trait is the contract every backend answers, and three quarters of its
//!   implementations will never have a remote to push to. A caller that wants
//!   git operations asks for a git repository.
//!
//! ## Authentication: a generated deploy key
//!
//! Cloning a private repository over SSH needs a key the remote will accept, and
//! the flow that works for a hosted repository is the *deploy key*: generate a
//! keypair here, hand the admin the public half to paste into the repository's
//! settings, keep the private half. So [`generate_deploy_key`] exists and is
//! called **before** the store is saved — the admin must be able to install the
//! key at the remote before anything tries to clone, or the first clone fails
//! for a reason that has nothing to do with their configuration.
//!
//! The private key is written to [`key_dir`] with `0600` permissions on Unix and
//! is never returned by the API; only the public half is, which is the half that
//! is meant to be copied around.
//!
//! ## Every git invocation is non-interactive
//!
//! A server has no terminal, so a git subprocess that asks a question does not
//! get an answer — it hangs, holding a request open until something times out.
//! Every invocation therefore runs with `GIT_TERMINAL_PROMPT=0`, and every
//! invocation using a deploy key runs ssh with `BatchMode=yes`, so a missing or
//! rejected credential is an immediate error carrying git's own message rather
//! than a stall.

use async_trait::async_trait;
use bytes::Bytes;
use sc_error::{Context, Error, Result};
use sc_types::{Attrs, BasicType, FormField, Operation, OperationScope};
use std::path::{Path, PathBuf};
use tokio::process::Command;

use crate::def::{
    ATTR_CLONE_PATH, CFG_BRANCH, CFG_KEY_PATH, CFG_PUBLIC_KEY, CFG_URL, FileStoreDef, GIT_BACKEND,
};
use crate::local::LocalFileStore;
use crate::store::{Entry, FileMeta, FileStore};

/// Environment variable overriding the base directory clones and keys live
/// under, for tests and for an operator who wants them elsewhere.
///
/// Tests need this: they must not clone into the real user's data directory,
/// and a test that did would leave a repository behind on the developer's
/// machine and collide with the next run.
pub const DATA_DIR_ENV: &str = "SC_DATA_DIR";

/// The base directory Saltcorn keeps its own data in, chosen per operating
/// system.
///
/// Hand-rolled rather than taken from the `dirs` crate, which would be a
/// dependency in layer 5 for fifteen lines of `std::env::var`. The conventions
/// are stable and each is the platform's documented one:
///
/// | Platform | Directory |
/// |---|---|
/// | Windows | `%LOCALAPPDATA%\Saltcorn` |
/// | macOS | `~/Library/Application Support/Saltcorn` |
/// | other (XDG) | `$XDG_DATA_HOME/saltcorn`, else `~/.local/share/saltcorn` |
///
/// [`DATA_DIR_ENV`] overrides all of it. An environment with none of the
/// variables set — a daemon started with a scrubbed environment — is an error
/// rather than a fallback to the current directory, which would scatter clones
/// wherever the process happened to be started.
pub fn data_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var(DATA_DIR_ENV)
        && !dir.trim().is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    if cfg!(windows) {
        return std::env::var("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("Saltcorn"))
            .map_err(|_| {
                Error::config(
                    "cannot decide where to keep git clones: neither \
                     `SC_DATA_DIR` nor `LOCALAPPDATA` is set",
                )
            });
    }
    let home = std::env::var("HOME").map_err(|_| {
        Error::config("cannot decide where to keep git clones: neither `SC_DATA_DIR` nor `HOME` is set")
    })?;
    if cfg!(target_os = "macos") {
        return Ok(PathBuf::from(home).join("Library/Application Support/Saltcorn"));
    }
    match std::env::var("XDG_DATA_HOME") {
        Ok(xdg) if !xdg.trim().is_empty() => Ok(PathBuf::from(xdg).join("saltcorn")),
        _ => Ok(PathBuf::from(home).join(".local/share/saltcorn")),
    }
}

/// Where clones live: `<data dir>/git-stores`.
pub fn clone_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("git-stores"))
}

/// Where deploy keys live: `<data dir>/keys`.
pub fn key_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("keys"))
}

/// A store name reduced to something safe to use as a single path component.
///
/// A store's name is admin-supplied text and may hold anything — a slash, a
/// leading dot, a colon Windows will not take — while this becomes a directory
/// name. Every character outside `[A-Za-z0-9._-]` becomes `_`, and a name that
/// reduces to nothing (or to `.`/`..`) becomes `store`, since a clone directory
/// called `..` would be the parent of every clone.
fn path_safe(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    match mapped.trim_matches('.') {
        "" => "store".to_owned(),
        _ => mapped,
    }
}

/// The directory a store's clone lives in: its recorded
/// [`ATTR_CLONE_PATH`](crate::ATTR_CLONE_PATH) if it has one, else
/// `<clone dir>/<name>`.
///
/// The path is *recorded* on first clone rather than derived every time, so that
/// renaming a store does not orphan its working tree — a rename would otherwise
/// silently point the store at an empty directory and lose whatever was
/// uncommitted in the old one.
pub fn clone_path(def: &FileStoreDef) -> Result<PathBuf> {
    if let Some(recorded) = def
        .attributes
        .get(ATTR_CLONE_PATH)
        .and_then(serde_json::Value::as_str)
        && !recorded.trim().is_empty()
    {
        return Ok(PathBuf::from(recorded));
    }
    Ok(clone_dir()?.join(path_safe(&def.name)))
}

/// A generated SSH deploy key: the private half's location, and the public half
/// itself.
///
/// The private key is a *path* and the public key is its *contents*, which is
/// the asymmetry the two halves deserve. The public half is meant to be copied
/// into a repository's settings, so it travels; the private half must not leave
/// the machine, so what travels is only where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployKey {
    /// Absolute path of the private key file.
    pub private_key_path: PathBuf,
    /// The public key, in `authorized_keys` one-line form — what is pasted into
    /// the repository's deploy-key settings.
    pub public_key: String,
}

/// Generate an ed25519 SSH keypair for the store named `name`, returning the
/// public half.
///
/// Runs `ssh-keygen`, rather than generating the key in-process. The key must be
/// in OpenSSH's own on-disk format for `ssh` to use it, and `ssh-keygen` is
/// where that format is defined and is already present on any machine that can
/// clone over SSH — a machine that cannot run it could not use the key it
/// produced. An absent `ssh-keygen` is reported as such.
///
/// ed25519 with no passphrase: a passphrase-protected key is unusable by a
/// non-interactive server (see the module docs), and ed25519 is what every
/// hosted forge accepts and the shortest key to paste.
///
/// **An existing key of the same name is replaced.** Generating a deploy key is
/// an explicit act, and the alternative — refusing, or silently reusing the old
/// key — would leave the admin looking at a public key the remote has already
/// rejected with no way to get a new one.
pub async fn generate_deploy_key(name: &str) -> Result<DeployKey> {
    let dir = key_dir()?;
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("creating deploy-key directory {}", dir.display()))?;

    let private = dir.join(format!("{}.key", path_safe(name)));
    let public = private.with_extension("key.pub");
    // `ssh-keygen` refuses to overwrite without asking, and asking is a hang
    // (module docs), so clear the way first.
    let _ = tokio::fs::remove_file(&private).await;
    let _ = tokio::fs::remove_file(&public).await;

    let output = Command::new("ssh-keygen")
        .arg("-t")
        .arg("ed25519")
        .arg("-N")
        .arg("")
        .arg("-C")
        .arg(format!("saltcorn:{name}"))
        .arg("-f")
        .arg(&private)
        .output()
        .await
        .map_err(|e| {
            Error::config(format!(
                "could not run `ssh-keygen` to generate a deploy key ({e}); \
                 it is part of OpenSSH and must be installed to use the git backend"
            ))
        })?;
    if !output.status.success() {
        return Err(Error::invalid(format!(
            "`ssh-keygen` failed: {}",
            combined(&output.stdout, &output.stderr)
        )));
    }

    // 0600: an SSH private key readable by other users on the box is one `ssh`
    // will itself refuse to use, and rightly.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600))
            .await
            .with_context(|| format!("restricting permissions on {}", private.display()))?;
    }

    let public_key = tokio::fs::read_to_string(&public)
        .await
        .with_context(|| format!("reading generated public key {}", public.display()))?;

    Ok(DeployKey {
        private_key_path: private,
        public_key: public_key.trim().to_owned(),
    })
}

/// The outcome of one git invocation: whether it succeeded, and everything it
/// said.
///
/// stdout and stderr are kept together because git splits them along lines that
/// are of no use to a reader — `git push` reports success on stderr — and the
/// admin UI shows this verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    /// Whether git exited zero.
    pub success: bool,
    /// stdout and stderr, interleaved in that order and trimmed.
    pub output: String,
}

/// What a working tree currently is: which branch, what is uncommitted, and how
/// it stands against its upstream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitStatus {
    /// Whether the directory holds a clone at all. Everything below is empty
    /// when it does not.
    pub cloned: bool,
    /// The checked-out branch, or empty on a detached head or an empty
    /// repository.
    pub branch: String,
    /// One `git status --porcelain` line per changed path.
    pub changes: Vec<String>,
    /// The last commit, as `<short hash> <subject> (<author>, <when>)`; empty in
    /// a repository with no commits yet.
    pub last_commit: String,
    /// Commits the working tree has that its upstream does not.
    pub ahead: u32,
    /// Commits the upstream has that the working tree does not.
    pub behind: u32,
}

impl GitStatus {
    /// Whether there is nothing to commit.
    pub fn clean(&self) -> bool {
        self.changes.is_empty()
    }
}

/// The outcome of a commit: whether one was actually made, and git's output.
///
/// "Nothing to commit" is **not** an error and is reported as `committed:
/// false`. Pressing Commit on an unchanged tree is a question ("is there
/// anything to save?"), and answering it with a failure would make the admin
/// hunt for a mistake they did not make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitOutcome {
    /// Whether a commit was created.
    pub committed: bool,
    /// git's own output.
    pub output: String,
}

/// A git working tree Saltcorn manages: where it is, where it came from, and
/// which key reaches the remote.
///
/// Separate from [`GitFileStore`] because the operations are needed when there
/// is **no** connected store: a store that has not been cloned yet has nothing
/// to connect, and cloning it is exactly what fixes that. So this is built from
/// a [`FileStoreDef`] — inert data — and never requires an instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRepo {
    root: PathBuf,
    url: String,
    branch: Option<String>,
    key_path: Option<PathBuf>,
}

impl GitRepo {
    /// Build the repository a definition describes.
    ///
    /// Fails only on a definition that is not a git store or has no URL — both
    /// of which [`validate_file_store_config`](crate::validate_file_store_config)
    /// rejects on save, so reaching either here means a caller went around the
    /// registry.
    pub fn from_def(def: &FileStoreDef) -> Result<GitRepo> {
        if def.backend != GIT_BACKEND {
            return Err(Error::invalid(format!(
                "file store `{}` is a `{}` store, not a git repository",
                def.name, def.backend
            )));
        }
        let url = def
            .setting(CFG_URL)
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "git file store `{}` has no `{CFG_URL}` setting",
                    def.name
                ))
            })?
            .to_owned();
        Ok(GitRepo {
            root: clone_path(def)?,
            url,
            branch: def
                .setting(CFG_BRANCH)
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .map(str::to_owned),
            key_path: def
                .setting(CFG_KEY_PATH)
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
        })
    }

    /// The working tree's directory — where the clone is or will be.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The remote URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Whether the directory holds a clone. A `.git` *file* counts as well as a
    /// directory, since that is what a worktree or submodule link looks like.
    pub fn is_cloned(&self) -> bool {
        self.root.join(".git").exists()
    }

    /// Clone the remote into the working-tree directory, unless it is already
    /// cloned — in which case this does nothing and says so.
    ///
    /// Idempotent on purpose: it runs on every save of a git store, and a save
    /// that re-cloned would throw away uncommitted work. Changing the URL of an
    /// already-cloned store therefore does *not* re-clone it; the admin who
    /// wants a different repository makes a different store, which is also the
    /// only reading under which their existing working tree is safe.
    ///
    /// A clone into a directory that exists but is not empty fails, with git's
    /// message. That is the right answer rather than something to work around:
    /// the contents are someone's, and they were not put there by this store.
    pub async fn ensure_cloned(&self) -> Result<GitOutput> {
        if self.is_cloned() {
            return Ok(GitOutput {
                success: true,
                output: format!("already cloned in {}", self.root.display()),
            });
        }
        let parent = self.root.parent().unwrap_or(&self.root).to_owned();
        tokio::fs::create_dir_all(&parent)
            .await
            .with_context(|| format!("creating clone directory {}", parent.display()))?;

        let mut args: Vec<String> = vec!["clone".to_owned()];
        if let Some(branch) = &self.branch {
            args.push("--branch".to_owned());
            args.push(branch.clone());
        }
        args.push(self.url.clone());
        args.push(self.root.to_string_lossy().into_owned());

        // Run from the parent: the target does not exist yet, so it cannot be
        // the working directory.
        let out = self.run(&parent, &args).await?;
        if !out.success {
            return Err(Error::invalid(format!(
                "cloning {} failed: {}",
                self.url, out.output
            )));
        }
        Ok(out)
    }

    /// `git pull` — fetch the remote and merge it into the working tree.
    ///
    /// `--no-edit` because a merge commit would otherwise open an editor, and an
    /// editor on a server is the hang the module docs describe.
    pub async fn pull(&self) -> Result<GitOutput> {
        self.checked(&["pull", "--no-edit"], "pull").await
    }

    /// `git push` the checked-out branch to `origin`.
    ///
    /// `--set-upstream` so that the first push of a branch created locally
    /// works rather than failing with "no upstream", which is a state the admin
    /// cannot fix from this UI.
    pub async fn push(&self) -> Result<GitOutput> {
        let branch = self.current_branch().await?;
        if branch.is_empty() {
            return Err(Error::invalid(
                "cannot push: the working tree is not on a branch",
            ));
        }
        self.checked(&["push", "--set-upstream", "origin", &branch], "push")
            .await
    }

    /// Stage **everything** and commit it with `message`.
    ///
    /// Commits all changes — including files the admin created outside the
    /// editor and deletions — because that is what the button says and because
    /// a per-file staging UI is a git client, which this is not.
    ///
    /// An identity is supplied for the commit only when the repository has none
    /// configured, so an admin's own `user.name`/`user.email` wins where they
    /// have set one and a bare machine can still commit where they have not.
    pub async fn commit_all(&self, message: &str) -> Result<CommitOutcome> {
        let message = message.trim();
        if message.is_empty() {
            return Err(Error::invalid("a commit needs a message"));
        }
        self.require_cloned()?;

        let staged = self.run(&self.root, &["add", "-A"]).await?;
        if !staged.success {
            return Err(Error::invalid(format!(
                "staging changes failed: {}",
                staged.output
            )));
        }

        let mut args: Vec<String> = Vec::new();
        if !self.has_identity().await? {
            args.extend(
                [
                    "-c",
                    "user.name=Saltcorn",
                    "-c",
                    "user.email=saltcorn@localhost",
                ]
                .map(str::to_owned),
            );
        }
        args.extend(["commit".to_owned(), "-m".to_owned(), message.to_owned()]);

        let out = self.run(&self.root, &args).await?;
        if out.success {
            return Ok(CommitOutcome {
                committed: true,
                output: out.output,
            });
        }
        // A commit with nothing staged exits non-zero and says so. That is a
        // no-op, not a failure — see `CommitOutcome`.
        if out.output.contains("nothing to commit")
            || out.output.contains("no changes added to commit")
        {
            return Ok(CommitOutcome {
                committed: false,
                output: out.output,
            });
        }
        Err(Error::invalid(format!("commit failed: {}", out.output)))
    }

    /// What the working tree currently is (see [`GitStatus`]).
    ///
    /// Every sub-command here is allowed to fail: an empty repository has no
    /// `HEAD` to name, a branch with no upstream has nothing to count against,
    /// and neither is an error — they are states this reports rather than
    /// refuses. Only a directory that is not a clone at all short-circuits.
    pub async fn status(&self) -> Result<GitStatus> {
        if !self.is_cloned() {
            return Ok(GitStatus::default());
        }
        let branch = self.current_branch().await?;
        let changes = self
            .run(&self.root, &["status", "--porcelain"])
            .await?
            .output
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_owned)
            .collect();
        let last_commit = self
            .run(
                &self.root,
                &["log", "-1", "--pretty=%h %s (%an, %ar)"],
            )
            .await?;
        let (ahead, behind) = self.tracking().await?;
        Ok(GitStatus {
            cloned: true,
            branch,
            changes,
            last_commit: if last_commit.success {
                last_commit.output
            } else {
                String::new()
            },
            ahead,
            behind,
        })
    }

    /// The checked-out branch, or empty when there is none (detached head, or a
    /// repository with no commits).
    async fn current_branch(&self) -> Result<String> {
        self.require_cloned()?;
        let out = self
            .run(&self.root, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await?;
        if !out.success || out.output == "HEAD" {
            return Ok(String::new());
        }
        Ok(out.output)
    }

    /// How far the branch is ahead of and behind its upstream, or `(0, 0)` when
    /// it has no upstream to compare against.
    async fn tracking(&self) -> Result<(u32, u32)> {
        let out = self
            .run(
                &self.root,
                &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
            )
            .await?;
        if !out.success {
            return Ok((0, 0));
        }
        let mut parts = out.output.split_whitespace();
        let ahead = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let behind = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        Ok((ahead, behind))
    }

    /// Whether the repository resolves a committer identity (from its own
    /// config, the user's, or the environment).
    async fn has_identity(&self) -> Result<bool> {
        Ok(self
            .run(&self.root, &["config", "--get", "user.email"])
            .await?
            .success)
    }

    /// Run a sub-command in the working tree and turn a non-zero exit into an
    /// error naming the operation.
    async fn checked(&self, args: &[&str], what: &str) -> Result<GitOutput> {
        self.require_cloned()?;
        let out = self.run(&self.root, args).await?;
        if !out.success {
            return Err(Error::invalid(format!("git {what} failed: {}", out.output)));
        }
        Ok(out)
    }

    /// The error for an operation on a store that has never been cloned, which
    /// says what to do about it rather than letting git report a directory that
    /// is not a repository.
    fn require_cloned(&self) -> Result<()> {
        if self.is_cloned() {
            return Ok(());
        }
        Err(Error::invalid(format!(
            "{} is not a git clone yet; clone the repository first",
            self.root.display()
        )))
    }

    /// Run `git` with this repository's credentials, non-interactively.
    async fn run<S: AsRef<std::ffi::OsStr>>(&self, dir: &Path, args: &[S]) -> Result<GitOutput> {
        let mut cmd = Command::new("git");
        cmd.current_dir(dir).args(args);
        // No terminal, so no question git asks can be answered (module docs).
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        if let Some(key) = &self.key_path {
            cmd.env("GIT_SSH_COMMAND", ssh_command(key));
        }
        let output = cmd.output().await.map_err(|e| {
            Error::config(format!(
                "could not run `git` ({e}); it must be installed to use the git backend"
            ))
        })?;
        Ok(GitOutput {
            success: output.status.success(),
            output: combined(&output.stdout, &output.stderr),
        })
    }
}

/// The `GIT_SSH_COMMAND` that makes git use one specific key and ask nothing.
///
/// - `IdentitiesOnly=yes` — otherwise ssh offers every key an agent holds
///   before the one we named, and a forge that accepts one of those would use
///   the wrong identity while a forge that rejects them may hit its auth-attempt
///   limit before reaching ours.
/// - `BatchMode=yes` — never prompt (module docs).
/// - `StrictHostKeyChecking=accept-new` — a first clone from a host that is not
///   in `known_hosts` is the normal case here, and with `BatchMode` the strict
///   default would fail it; `accept-new` still refuses a host whose key has
///   *changed*, which is the case that matters.
fn ssh_command(key: &Path) -> String {
    format!(
        "ssh -i {} -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new",
        shell_quote(&key.to_string_lossy())
    )
}

/// Single-quote a path for the shell-like splitting git applies to
/// `GIT_SSH_COMMAND`, so a key path containing a space still names one argument.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// stdout then stderr, lossily decoded and trimmed.
fn combined(stdout: &[u8], stderr: &[u8]) -> String {
    let out = String::from_utf8_lossy(stdout);
    let err = String::from_utf8_lossy(stderr);
    let mut joined = String::new();
    for part in [out.trim(), err.trim()] {
        if part.is_empty() {
            continue;
        }
        if !joined.is_empty() {
            joined.push('\n');
        }
        joined.push_str(part);
    }
    joined
}

/// A [`FileStore`] whose directory is a git working tree.
///
/// Every byte-level operation is [`LocalFileStore`]'s — the files are on local
/// disk and the traversal sandbox, the xattr metadata and the listing behaviour
/// should be identical, not merely similar. What this adds is the [`GitRepo`]
/// behind it and an unconditional [`is_git_repo`](FileStore::is_git_repo), so a
/// caller that needs to know (an app scaffold deciding whether to `git init`,
/// design §13.3) gets the right answer without inspecting the disk.
#[derive(Debug, Clone)]
pub struct GitFileStore {
    inner: LocalFileStore,
    repo: GitRepo,
}

impl GitFileStore {
    /// Connect a git store named `name` over an existing clone.
    ///
    /// The directory must already be a clone: connecting is not the place to
    /// reach the network. [`GitRepo::ensure_cloned`] is, and the server calls it
    /// when the admin saves the store.
    pub fn new(name: impl Into<String>, repo: GitRepo) -> Result<GitFileStore> {
        let name = name.into();
        if !repo.is_cloned() {
            return Err(Error::not_found(format!(
                "git file store `{name}` has not been cloned into {} yet",
                repo.root().display()
            )));
        }
        let inner = LocalFileStore::new(name, repo.root())?;
        Ok(GitFileStore { inner, repo })
    }

    /// The working tree behind the store, for the repository operations.
    pub fn repo(&self) -> &GitRepo {
        &self.repo
    }
}

#[async_trait]
impl FileStore for GitFileStore {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn read(&self, path: &str) -> Result<Bytes> {
        self.inner.read(path).await
    }

    async fn write(&self, path: &str, data: Bytes) -> Result<()> {
        self.inner.write(path, data).await
    }

    async fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        self.inner.list(dir).await
    }

    async fn mkdir(&self, path: &str) -> Result<()> {
        self.inner.mkdir(path).await
    }

    async fn delete(&self, path: &str) -> Result<bool> {
        self.inner.delete(path).await
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.inner.rename(from, to).await
    }

    /// Always true, by construction — this store *is* a clone.
    fn is_git_repo(&self) -> bool {
        true
    }

    fn local_path(&self, rel: &str) -> Result<Option<PathBuf>> {
        self.inner.local_path(rel)
    }

    async fn get_meta(&self, path: &str) -> Result<FileMeta> {
        self.inner.get_meta(path).await
    }

    async fn set_meta(&self, path: &str, meta: &FileMeta) -> Result<()> {
        self.inner.set_meta(path, meta).await
    }
}

/// The name of the operation that generates a deploy key — the one that runs
/// *before* the store is saved.
pub const OP_GENERATE_KEY: &str = "generate_deploy_key";
/// The name of the operation reporting the working tree's state.
pub const OP_STATUS: &str = "status";
/// The name of the operation that clones the remote.
pub const OP_CLONE: &str = "clone";
/// The name of the operation that fetches and merges the remote.
pub const OP_PULL: &str = "pull";
/// The name of the operation that pushes the branch.
pub const OP_PUSH: &str = "push";
/// The name of the operation that stages and commits everything.
pub const OP_COMMIT: &str = "commit";
/// The `message` argument of [`OP_COMMIT`].
pub const ARG_MESSAGE: &str = "message";

/// What the git backend offers beyond its settings (§6.2's [`Operation`]).
///
/// Declared as data for the same reason the settings are: the admin UI renders a
/// button per operation and a form per argument, and knows nothing about git. A
/// backend supplied by a plugin declares its own the same way and gets the same
/// buttons.
///
/// Note the scopes, which are the substance rather than bookkeeping. Generating
/// a deploy key is [`Configure`](OperationScope::Configure) — it must run before
/// the store is saved, because saving clones and cloning needs a key the remote
/// already accepts. Everything else is
/// [`Instance`](OperationScope::Instance): there is no working tree to pull
/// until there is a saved store.
pub fn git_operations() -> Vec<Operation> {
    vec![
        Operation::new(OP_GENERATE_KEY, OperationScope::Configure)
            .label("Generate deploy key")
            .description(
                "Creates an SSH key for this repository and fills in the settings below. \
                 Add the public key to the repository's deploy keys — with write access if \
                 you want to push — before saving, because saving clones the repository.",
            ),
        Operation::new(OP_STATUS, OperationScope::Instance)
            .label("Refresh status")
            .description("What the working copy looks like right now.")
            // The one an admin needs in front of them before deciding what to
            // do, so it runs on opening the store rather than on a press.
            .automatic(),
        Operation::new(OP_CLONE, OperationScope::Instance)
            .label("Clone")
            .description(
                "Clones the repository if it has not been cloned yet. \
                 An existing working copy is left untouched.",
            )
            // The clone *is* the creation of a git store: until it succeeds
            // there is no working copy, and therefore no store. Saying so is
            // what makes creating one transactional — a failed clone leaves no
            // row behind for the admin's corrected second attempt to collide
            // with. It remains a button too, since a working copy can be lost
            // (deleted, or a database restored onto a fresh machine) and
            // re-running it is the repair.
            .on_create(),
        Operation::new(OP_PULL, OperationScope::Instance)
            .label("Pull")
            .description("Fetches the remote and merges it into the working copy."),
        Operation::new(OP_PUSH, OperationScope::Instance)
            .label("Push")
            .description("Sends committed changes to the remote."),
        Operation::new(OP_COMMIT, OperationScope::Instance)
            .label("Commit all changes")
            .description("Stages everything in the working copy and commits it.")
            .input([
                FormField::new(ARG_MESSAGE, BasicType::Text)
                    .label("Commit message")
                    .required(),
            ]),
    ]
}

/// Run one of [`git_operations`] against `def`, which may be an unsaved
/// definition the admin is still editing.
///
/// `def` is mutable because two of these configure as well as act: the key
/// generator writes the key path and public key into the settings, and a clone
/// records where it cloned to. The caller persists what changed.
///
/// Called through [`run_backend_operation`](crate::run_backend_operation), which
/// has already checked the operation exists and that its arguments match what it
/// declared — so a missing commit message never reaches here.
pub(crate) async fn run_git_operation(
    def: &mut FileStoreDef,
    operation: &str,
    input: &Attrs,
) -> Result<String> {
    if operation == OP_GENERATE_KEY {
        // The one operation that must work with no repository and no clone: it
        // is what makes reaching the repository possible in the first place.
        let key = generate_deploy_key(&def.name).await?;
        record_deploy_key(def, &key);
        return Ok(format!(
            "Generated a deploy key. Add this to the repository's deploy keys:\n\n{}",
            key.public_key
        ));
    }

    let repo = GitRepo::from_def(def)?;
    match operation {
        OP_STATUS => Ok(describe(&repo.status().await?, &repo)),
        OP_CLONE => {
            // Recorded before the clone runs, so the location survives a later
            // rename — which would otherwise re-derive a fresh directory and
            // abandon this working copy.
            record_clone_path(def, repo.root());
            let out = repo.ensure_cloned().await?;
            Ok(format!(
                "{}\n\n{}",
                out.output,
                describe(&repo.status().await?, &repo)
            ))
        }
        OP_PULL => Ok(repo.pull().await?.output),
        OP_PUSH => Ok(repo.push().await?.output),
        OP_COMMIT => {
            let message = input
                .get(ARG_MESSAGE)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let outcome = repo.commit_all(message).await?;
            Ok(if outcome.committed {
                outcome.output
            } else {
                // Not a failure — see `CommitOutcome`. Said plainly, because
                // git's own "nothing to commit, working tree clean" arrives
                // through an error path and would read as one.
                "Nothing to commit — the working copy has no changes.".to_owned()
            })
        }
        other => Err(Error::invalid(format!("unknown git operation `{other}`"))),
    }
}

/// A working tree's state as a few lines of prose for the admin.
///
/// Text rather than a structure, which is what keeps the admin UI free of git:
/// a screen that rendered branches and ahead/behind counts would be a screen
/// that knows what those are, and could not render a plugin backend's status at
/// all. The porcelain lines are git's own and are passed through unchanged.
fn describe(status: &GitStatus, repo: &GitRepo) -> String {
    if !status.cloned {
        return format!(
            "Not cloned yet. Cloning will put the working copy in {}.",
            repo.root().display()
        );
    }
    let mut lines = Vec::new();
    lines.push(match status.branch.as_str() {
        "" => "Not on a branch.".to_owned(),
        branch => format!("On branch {branch}."),
    });
    lines.push(match (status.ahead, status.behind) {
        (0, 0) => "Up to date with the remote.".to_owned(),
        (a, 0) => format!("{a} commit(s) to push."),
        (0, b) => format!("{b} commit(s) to pull."),
        (a, b) => format!("{a} commit(s) to push, {b} to pull."),
    });
    if status.clean() {
        lines.push("No uncommitted changes.".to_owned());
    } else {
        lines.push(format!("{} uncommitted change(s):", status.changes.len()));
        lines.extend(status.changes.iter().cloned());
    }
    if !status.last_commit.is_empty() {
        lines.push(format!("Last commit: {}", status.last_commit));
    }
    lines.push(format!("Working copy: {}", repo.root().display()));
    lines.join("\n")
}

/// Record where a store was cloned, so a later rename cannot orphan the working
/// tree (see [`clone_path`]).
pub fn record_clone_path(def: &mut FileStoreDef, path: &Path) {
    def.attributes.insert(
        ATTR_CLONE_PATH.to_owned(),
        serde_json::Value::String(path.to_string_lossy().into_owned()),
    );
}

/// Put a generated key into a definition's settings: the private key's path,
/// which is how git reaches the remote, and the public key, which the admin
/// pastes into the repository's deploy keys.
///
/// Both live in `config` rather than `attributes` because both are things the
/// admin can legitimately supply by hand — pointing a store at a key that
/// already exists on the machine is a perfectly good configuration, and one an
/// admin managing several stores against one repository will want.
pub fn record_deploy_key(def: &mut FileStoreDef, key: &DeployKey) {
    def.config.insert(
        CFG_KEY_PATH.to_owned(),
        serde_json::Value::String(key.private_key_path.to_string_lossy().into_owned()),
    );
    def.config.insert(
        CFG_PUBLIC_KEY.to_owned(),
        serde_json::Value::String(key.public_key.clone()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_store_name_becomes_one_safe_path_component() {
        assert_eq!(path_safe("docs"), "docs");
        assert_eq!(path_safe("my docs"), "my_docs");
        // The cases that would escape the clone directory or name it. A dot
        // survives (it is legal in a directory name) but the separator does
        // not, so what is left is one harmless component.
        assert_eq!(path_safe("../etc"), ".._etc");
        assert_eq!(path_safe(".."), "store");
        assert_eq!(path_safe(""), "store");
        assert!(!path_safe("a/b").contains('/'));
    }

    #[test]
    fn the_data_directory_honours_the_override() {
        // A test must never clone into the developer's real data directory.
        temp_env(|| {
            unsafe { std::env::set_var(DATA_DIR_ENV, "/tmp/sc-data") };
            assert_eq!(data_dir().unwrap(), PathBuf::from("/tmp/sc-data"));
            assert_eq!(clone_dir().unwrap(), PathBuf::from("/tmp/sc-data/git-stores"));
            assert_eq!(key_dir().unwrap(), PathBuf::from("/tmp/sc-data/keys"));
        });
    }

    #[test]
    fn a_clone_path_is_derived_from_the_name_until_one_is_recorded() {
        temp_env(|| {
            unsafe { std::env::set_var(DATA_DIR_ENV, "/tmp/sc-data") };
            let mut def = FileStoreDef::git("web app", "git@example.com:me/app.git");
            assert_eq!(
                clone_path(&def).unwrap(),
                PathBuf::from("/tmp/sc-data/git-stores/web_app")
            );

            // Once recorded, the recorded path wins — renaming the store must
            // not point it at a different, empty directory.
            record_clone_path(&mut def, Path::new("/srv/checkout"));
            def.name = "renamed".to_owned();
            assert_eq!(clone_path(&def).unwrap(), PathBuf::from("/srv/checkout"));
        });
    }

    #[test]
    fn a_repo_is_built_from_the_definitions_settings() {
        temp_env(|| {
            unsafe { std::env::set_var(DATA_DIR_ENV, "/tmp/sc-data") };
            let def = FileStoreDef::git("app", "git@example.com:me/app.git")
                .with(CFG_BRANCH, "main")
                .with(CFG_KEY_PATH, "/keys/app.key");
            let repo = GitRepo::from_def(&def).unwrap();
            assert_eq!(repo.url(), "git@example.com:me/app.git");
            assert_eq!(repo.branch.as_deref(), Some("main"));
            assert_eq!(repo.key_path, Some(PathBuf::from("/keys/app.key")));
            assert!(!repo.is_cloned());
        });
    }

    #[test]
    fn a_non_git_definition_is_not_a_repository() {
        let def = FileStoreDef::local("docs", "/srv/docs");
        let err = GitRepo::from_def(&def).unwrap_err().to_string();
        assert!(err.contains("docs"), "{err}");
        assert!(err.contains("local"), "{err}");
    }

    #[test]
    fn a_blank_url_is_no_url() {
        // Whitespace is not a URL; catching it here means the failure names the
        // setting rather than being git's "repository '   ' does not exist".
        let def = FileStoreDef::git("app", "   ");
        assert!(GitRepo::from_def(&def).is_err());
    }

    #[test]
    fn the_ssh_command_pins_one_key_and_never_prompts() {
        let cmd = ssh_command(Path::new("/keys/my key.key"));
        assert!(cmd.contains("'/keys/my key.key'"), "{cmd}");
        assert!(cmd.contains("IdentitiesOnly=yes"), "{cmd}");
        assert!(cmd.contains("BatchMode=yes"), "{cmd}");
        assert!(cmd.contains("StrictHostKeyChecking=accept-new"), "{cmd}");
    }

    #[test]
    fn output_keeps_both_streams() {
        assert_eq!(combined(b"out\n", b"err\n"), "out\nerr");
        assert_eq!(combined(b"", b"err"), "err");
        assert_eq!(combined(b"", b""), "");
    }

    #[test]
    fn a_status_with_no_changes_is_clean() {
        assert!(GitStatus::default().clean());
        let dirty = GitStatus {
            changes: vec![" M src/main.rs".to_owned()],
            ..GitStatus::default()
        };
        assert!(!dirty.clean());
    }

    /// Run `f` with the data-directory variable restored afterwards, under a
    /// lock: `set_var` is process-wide, so two of these running at once would
    /// each see the other's value.
    fn temp_env(f: impl FnOnce()) {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = std::env::var(DATA_DIR_ENV).ok();
        f();
        unsafe {
            match before {
                Some(v) => std::env::set_var(DATA_DIR_ENV, v),
                None => std::env::remove_var(DATA_DIR_ENV),
            }
        }
    }
}
