//! Local-directory [`FileStore`] driver.
//!
//! Backs a named store with a directory on the local filesystem. All I/O is
//! confined to the store root: paths are sanitised so absolute paths and `..`
//! traversal are rejected before touching disk.
//!
//! ## Metadata
//!
//! Per-file [`FileMeta`] is stored in a single extended attribute
//! ([`META_ATTR`]) on the file itself, via the cross-platform [`crate::xattr`]
//! layer (POSIX xattrs on Unix, NTFS Alternate Data Streams on Windows). Files
//! therefore need no database row and no sidecar (design §9).

use async_trait::async_trait;
use bytes::Bytes;
use sc_error::{Context, Error, Result};
use std::path::{Path, PathBuf};

use crate::store::{Entry, FileMeta, FileStat, FileStore};

/// A modification time as RFC 3339 in UTC, which is how every other timestamp
/// crosses this server's wire.
fn rfc3339(time: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Name of the extended attribute holding a file's serialised [`FileMeta`]. On
/// Unix the effective key is `user.saltcorn.meta` (the `user.` namespace is
/// applied by the xattr layer).
const META_ATTR: &str = "saltcorn.meta";

/// A [`FileStore`] backed by a directory on the local filesystem.
#[derive(Debug, Clone)]
pub struct LocalFileStore {
    name: String,
    root: PathBuf,
}

impl LocalFileStore {
    /// Connect a local store named `name` rooted at `root`.
    ///
    /// The root must be an existing directory; the path is canonicalised so that
    /// later traversal checks compare against a fully-resolved root.
    pub fn new(name: impl Into<String>, root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let canonical = std::fs::canonicalize(root)
            .with_context(|| format!("file store root {}", root.display()))?;
        if !canonical.is_dir() {
            return Err(Error::config(format!(
                "file store root {} is not a directory",
                canonical.display()
            )));
        }
        Ok(Self {
            name: name.into(),
            root: canonical,
        })
    }

    /// Resolve a store-relative path to an absolute path inside the root,
    /// rejecting anything that would escape it.
    fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let mut out = self.root.clone();
        for segment in rel.split('/') {
            match segment {
                // Empty (leading/trailing/double slash) and "." are no-ops.
                "" | "." => continue,
                ".." => {
                    return Err(Error::invalid(format!(
                        "path {rel:?} escapes the file store root"
                    )));
                }
                seg => out.push(seg),
            }
        }
        Ok(out)
    }

    /// Turn a filesystem error into a domain error, keeping **"it is not there"**
    /// distinct from "it broke".
    ///
    /// This is the difference between a 404 and a 500 at the API (§16), and it is
    /// not cosmetic: asking whether a path exists by trying to read it is the
    /// normal thing for a client to do — the IDE's filesystem (§12.1) stats a
    /// path before writing it, and looks for optional files like
    /// `.vscode/settings.json` that are usually absent. A missing file reported as
    /// a system failure makes every one of those an error the admin is shown.
    fn io<T>(&self, result: std::io::Result<T>, path: &str, doing: &str) -> Result<T> {
        match result {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::not_found(format!(
                "{path:?} does not exist in store {}",
                self.name
            ))),
            other => other.with_context(|| format!("{doing} {path:?} in store {}", self.name)),
        }
    }
}

#[async_trait]
impl FileStore for LocalFileStore {
    fn name(&self) -> &str {
        &self.name
    }

    async fn read(&self, path: &str) -> Result<Bytes> {
        let abs = self.resolve(path)?;
        let data = self.io(tokio::fs::read(&abs).await, path, "reading")?;
        Ok(Bytes::from(data))
    }

    async fn write(&self, path: &str, data: Bytes) -> Result<()> {
        let abs = self.resolve(path)?;
        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating parent dirs for {path:?}"))?;
        }
        tokio::fs::write(&abs, &data)
            .await
            .with_context(|| format!("writing {path:?} to store {}", self.name))?;
        Ok(())
    }

    async fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        let abs = self.resolve(dir)?;
        let mut read_dir = self.io(tokio::fs::read_dir(&abs).await, dir, "listing")?;
        // Normalise the directory prefix so child paths are `<dir>/<name>` with
        // no leading, trailing, or doubled separators.
        let prefix = dir.trim_matches('/');
        let mut entries = Vec::new();
        while let Some(dirent) = read_dir
            .next_entry()
            .await
            .with_context(|| format!("reading dir entry in {dir:?}"))?
        {
            let name = dirent.file_name().to_string_lossy().into_owned();
            let meta = dirent
                .metadata()
                .await
                .with_context(|| format!("stat {name:?} in {dir:?}"))?;
            let is_dir = meta.is_dir();
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            entries.push(Entry {
                name,
                path,
                is_dir,
                size: if is_dir { None } else { Some(meta.len()) },
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    async fn mkdir(&self, path: &str) -> Result<()> {
        let abs = self.resolve(path)?;
        // A file already sitting there is a real conflict — `create_dir_all`
        // would report an opaque OS error, so name it.
        if abs.is_file() {
            return Err(Error::invalid(format!(
                "cannot create directory {path:?} in store {}: a file already exists there",
                self.name
            )));
        }
        // Idempotent by construction: `create_dir_all` succeeds when the
        // directory is already there.
        tokio::fs::create_dir_all(&abs)
            .await
            .with_context(|| format!("creating directory {path:?} in store {}", self.name))?;
        Ok(())
    }

    async fn delete(&self, path: &str) -> Result<bool> {
        let abs = self.resolve(path)?;
        // `resolve` collapses "", "/" and "." to the root, so this also catches
        // an attempt to delete the whole store by passing an empty path.
        if abs == self.root {
            return Err(Error::invalid(format!(
                "refusing to delete the root of file store {}",
                self.name
            )));
        }
        let meta = match tokio::fs::symlink_metadata(&abs).await {
            Ok(meta) => meta,
            // Already gone: report it rather than erroring, so a caller need not
            // race an existence check against the delete.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => {
                return Err(Error::from(e))
                    .with_context(|| format!("stat {path:?} in store {}", self.name));
            }
        };
        if meta.is_dir() {
            tokio::fs::remove_dir_all(&abs)
                .await
                .with_context(|| format!("deleting directory {path:?} in store {}", self.name))?;
        } else {
            tokio::fs::remove_file(&abs)
                .await
                .with_context(|| format!("deleting {path:?} in store {}", self.name))?;
        }
        Ok(true)
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        let src = self.resolve(from)?;
        let dst = self.resolve(to)?;
        if src == self.root || dst == self.root {
            return Err(Error::invalid(format!(
                "refusing to rename the root of file store {}",
                self.name
            )));
        }
        if !tokio::fs::try_exists(&src)
            .await
            .with_context(|| format!("checking {from:?} exists"))?
        {
            return Err(Error::not_found(format!(
                "{from:?} does not exist in store {}",
                self.name
            )));
        }
        // Refuse to clobber. `tokio::fs::rename` would silently replace the
        // destination, destroying data the caller never named — and on a file
        // manager's drag-and-drop that is a lost file with no undo.
        if tokio::fs::try_exists(&dst)
            .await
            .with_context(|| format!("checking {to:?} exists"))?
        {
            return Err(Error::invalid(format!(
                "cannot rename {from:?} to {to:?} in store {}: the destination already exists",
                self.name
            )));
        }
        if let Some(parent) = dst.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating parent dirs for {to:?}"))?;
        }
        tokio::fs::rename(&src, &dst)
            .await
            .with_context(|| format!("renaming {from:?} to {to:?} in store {}", self.name))?;
        Ok(())
    }

    fn is_git_repo(&self) -> bool {
        // A `.git` directory (normal repo) or file (worktree/submodule link).
        self.root.join(".git").exists()
    }

    fn local_path(&self, rel: &str) -> Result<Option<PathBuf>> {
        // A local store is all local path; `resolve` applies the same traversal
        // sandboxing the byte-level methods get.
        self.resolve(rel).map(Some)
    }

    /// The entry's own facts, from one `stat` call rather than the default
    /// implementation's listing of its parent — which on a directory of ten
    /// thousand files is ten thousand entries built to answer one question.
    ///
    /// Follows symlinks, as [`read`](FileStore::read) does: what a caller asks
    /// about is the file they would read.
    async fn stat(&self, path: &str) -> Result<Option<FileStat>> {
        let abs = self.resolve(path)?;
        let meta = match tokio::fs::metadata(&abs).await {
            Ok(meta) => meta,
            // Nothing there is an answer, not a failure — and so is a path whose
            // parent is a file, which the OS reports as `NotADirectory`.
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    || e.kind() == std::io::ErrorKind::NotADirectory =>
            {
                return Ok(None);
            }
            Err(e) => {
                return Err(Error::from(e))
                    .with_context(|| format!("stat {path:?} in store {}", self.name));
            }
        };
        let is_dir = meta.is_dir();
        Ok(Some(FileStat {
            size: if is_dir { 0 } else { meta.len() },
            is_dir,
            modified: meta.modified().ok().map(rfc3339),
        }))
    }

    async fn get_meta(&self, path: &str) -> Result<FileMeta> {
        let abs = self.resolve(path)?;
        match crate::xattr::get(&abs, META_ATTR).await? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing metadata for {path:?}")),
            // The attribute has never been set: default metadata.
            None => Ok(FileMeta::default()),
        }
    }

    async fn set_meta(&self, path: &str, meta: &FileMeta) -> Result<()> {
        // The target file must exist before it can carry metadata.
        let abs = self.resolve(path)?;
        if !tokio::fs::try_exists(&abs)
            .await
            .with_context(|| format!("checking {path:?} exists"))?
        {
            return Err(Error::not_found(format!(
                "cannot set metadata: {path:?} does not exist in store {}",
                self.name
            )));
        }
        let json = serde_json::to_vec(meta)
            .with_context(|| format!("serialising metadata for {path:?}"))?;
        crate::xattr::set(&abs, META_ATTR, json).await?;
        Ok(())
    }
}
