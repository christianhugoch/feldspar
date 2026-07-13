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

    /// Whether this store is (rooted at) a git repository.
    fn is_git_repo(&self) -> bool;

    /// Read a file's metadata, returning [`FileMeta::default`] when none has been
    /// set.
    async fn get_meta(&self, path: &str) -> Result<FileMeta>;

    /// Replace a file's metadata.
    async fn set_meta(&self, path: &str, meta: &FileMeta) -> Result<()>;
}
