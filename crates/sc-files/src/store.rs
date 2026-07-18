//! The [`FileStore`] contract and the descriptors it exchanges.
//!
//! A file store is one named directory or object store (technical design §14.1).
//! This module owns the *trait* and its value types; concrete backends such as
//! [`crate::LocalFileStore`] implement it. Files deliberately have **no per-file
//! database row** (design §9): per-file metadata ([`FileMeta`]) lives beside the
//! bytes — in extended attributes for on-disk stores — and is round-tripped
//! through [`FileStore::get_meta`] / [`FileStore::set_meta`].

use async_trait::async_trait;
use bytes::Bytes;
use sc_error::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One entry returned by [`FileStore::list`]: a file or sub-directory directly
/// inside the listed directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The final path component (no directory separators).
    pub name: String,
    /// The entry's path **relative to the store root**, using `/` separators.
    pub path: String,
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// Size in bytes for files; `None` for directories.
    pub size: Option<u64>,
}

/// Per-file metadata, stored out of band from the bytes (in xattrs for on-disk
/// stores) so that files need no database row (design §9/§14.1).
///
/// Access rules are **path-cumulative**: to reach a file a user must clear the
/// [`FileMeta::min_role`] of the file *and* of every directory on its path.
/// `role` follows the `sc-auth` convention (`1` = admin … `100` = public), so a
/// lower number is more restrictive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileMeta {
    /// Minimum role permitted to read this entry, if restricted. `None` leaves
    /// access to be decided by directories higher in the path.
    pub min_role: Option<u8>,
    /// Free-form per-file attributes (e.g. a declared MIME type, origin, or
    /// application-specific tags).
    pub attributes: BTreeMap<String, String>,
}

/// A named directory/object store (technical design §14.1).
///
/// Paths passed to every method are **relative to the store root** and use `/`
/// as the separator; drivers must reject absolute paths and any `..` traversal
/// that would escape the root.
#[async_trait]
pub trait FileStore: Send + Sync {
    /// The store's name (as connected to the catalog).
    fn name(&self) -> &str;

    /// Read a file's full contents.
    async fn read(&self, path: &str) -> Result<Bytes>;

    /// Write (creating or replacing) a file, creating parent directories as
    /// needed.
    async fn write(&self, path: &str, data: Bytes) -> Result<()>;

    /// List the direct children of a directory (`""` or `"/"` is the root).
    async fn list(&self, dir: &str) -> Result<Vec<Entry>>;

    /// Create a directory, and any missing parents.
    ///
    /// Creating one that already exists is **not** an error: the caller wanted a
    /// directory there and there is one. Creating one where a *file* already sits
    /// is, since that is a genuine conflict the caller must resolve.
    async fn mkdir(&self, path: &str) -> Result<()>;

    /// Delete a file, or a directory and everything in it.
    ///
    /// Returns whether anything was there to delete, so a caller can distinguish
    /// "removed" from "already gone" without a prior existence check — the check
    /// would race anyway. Deleting the store root is refused: a store's root is
    /// the store, and removing it would leave a definition pointing at nothing.
    async fn delete(&self, path: &str) -> Result<bool>;

    /// Move or rename a file or directory within the store.
    ///
    /// Both paths are store-relative and confined to the root, so this cannot be
    /// used to move data out of the store. Refuses to overwrite an existing
    /// destination: a rename that silently replaced a file would destroy data the
    /// caller never named.
    async fn rename(&self, from: &str, to: &str) -> Result<()>;

    /// Whether this store is (rooted at) a git repository.
    fn is_git_repo(&self) -> bool;

    /// The on-disk path of a store-relative path, for the backends that have
    /// one.
    ///
    /// Almost everything should go through [`read`](FileStore::read) /
    /// [`write`](FileStore::write) and stay backend-agnostic. This exists for the
    /// one job that cannot: running a code framework's **build step** (design
    /// §13.3), where an external bundler process is handed a working directory
    /// and reads/writes the tree itself. `Ok(None)` means the backend has no
    /// local path (an object store), and is the default — such a store cannot
    /// host a buildable app, and callers must say so rather than pretend
    /// otherwise. Traversal that would escape the store root is an error, exactly
    /// as it is for the byte-level methods.
    fn local_path(&self, _rel: &str) -> Result<Option<std::path::PathBuf>> {
        Ok(None)
    }

    /// Read a file's metadata, returning [`FileMeta::default`] when none has been
    /// set.
    async fn get_meta(&self, path: &str) -> Result<FileMeta>;

    /// Replace a file's metadata.
    async fn set_meta(&self, path: &str, meta: &FileMeta) -> Result<()>;
}
