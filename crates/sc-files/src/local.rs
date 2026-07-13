//! Local-directory [`FileStore`] driver.
//!
//! Backs a named store with a directory on the local filesystem. All I/O is
//! confined to the store root: paths are sanitised so absolute paths and `..`
//! traversal are rejected before touching disk.
//!
//! ## Metadata
//!
//! Per-file [`FileMeta`] is currently kept in a JSON **sidecar** under a hidden
//! `.scmeta/` directory that mirrors the file tree. This is a portable
//! placeholder: the next TODO item swaps the backend for cross-platform
//! extended attributes (Linux/macOS/Windows/FreeBSD) while keeping this trait
//! surface unchanged. The `.scmeta/` directory is never returned by
//! [`FileStore::list`].

use async_trait::async_trait;
use bytes::Bytes;
use sc_error::{Context, Error, Result};
use std::path::{Path, PathBuf};

use crate::store::{Entry, FileMeta, FileStore};

/// Hidden directory (relative to the store root) holding metadata sidecars.
const META_DIR: &str = ".scmeta";

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
                seg if seg == META_DIR => {
                    return Err(Error::invalid(format!(
                        "path {rel:?} touches the reserved {META_DIR} directory"
                    )));
                }
                seg => out.push(seg),
            }
        }
        Ok(out)
    }

    /// The sidecar path holding metadata for a store-relative file path.
    fn meta_path(&self, rel: &str) -> Result<PathBuf> {
        // Validate `rel` first (reuse the traversal checks), then map it under
        // the meta directory with a `.json` suffix.
        self.resolve(rel)?;
        let mut out = self.root.join(META_DIR);
        for segment in rel.split('/') {
            if matches!(segment, "" | ".") {
                continue;
            }
            out.push(segment);
        }
        let file_name = out
            .file_name()
            .context("metadata path has no file name")?
            .to_owned();
        out.set_file_name(format!(
            "{}.json",
            file_name.to_string_lossy()
        ));
        Ok(out)
    }
}

#[async_trait]
impl FileStore for LocalFileStore {
    fn name(&self) -> &str {
        &self.name
    }

    async fn read(&self, path: &str) -> Result<Bytes> {
        let abs = self.resolve(path)?;
        let data = tokio::fs::read(&abs)
            .await
            .with_context(|| format!("reading {path:?} from store {}", self.name))?;
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
        let mut read_dir = tokio::fs::read_dir(&abs)
            .await
            .with_context(|| format!("listing {dir:?} in store {}", self.name))?;
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
            // Hide the metadata sidecar directory from listings.
            if prefix.is_empty() && name == META_DIR {
                continue;
            }
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

    fn is_git_repo(&self) -> bool {
        // A `.git` directory (normal repo) or file (worktree/submodule link).
        self.root.join(".git").exists()
    }

    async fn get_meta(&self, path: &str) -> Result<FileMeta> {
        let meta_path = self.meta_path(path)?;
        match tokio::fs::read(&meta_path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing metadata for {path:?}")),
            // No sidecar yet: the file simply has default metadata.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FileMeta::default()),
            Err(e) => Err(Error::from(e))
                .with_context(|| format!("reading metadata for {path:?}")),
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
        let meta_path = self.meta_path(path)?;
        if let Some(parent) = meta_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating metadata dir for {path:?}"))?;
        }
        let json = serde_json::to_vec(meta)
            .with_context(|| format!("serialising metadata for {path:?}"))?;
        tokio::fs::write(&meta_path, &json)
            .await
            .with_context(|| format!("writing metadata for {path:?}"))?;
        Ok(())
    }
}
