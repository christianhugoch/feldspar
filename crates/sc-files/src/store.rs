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
    /// When the entry was last modified, as RFC 3339, where the backend knows.
    ///
    /// `None` is the honest answer for a backend that does not record it —
    /// exactly as it is on [`FileStat`]. A listing carries it because the file
    /// manager shows a "modified" column, and asking for it per row would be one
    /// `stat` per entry over the wire.
    pub modified: Option<String>,
}

impl Entry {
    /// A file entry with no modification time — what a backend that cannot
    /// report one produces, and what an operation that has just written a path
    /// (rather than listed it) knows.
    pub fn file(name: impl Into<String>, path: impl Into<String>, size: Option<u64>) -> Entry {
        Entry {
            name: name.into(),
            path: path.into(),
            is_dir: false,
            size,
            modified: None,
        }
    }

    /// The same for a directory, which never has a size.
    pub fn dir(name: impl Into<String>, path: impl Into<String>) -> Entry {
        Entry {
            name: name.into(),
            path: path.into(),
            is_dir: true,
            size: None,
            modified: None,
        }
    }
}

/// What [`FileStore::stat`] answers: the facts about one entry that need no
/// bytes read.
///
/// Deliberately not [`Entry`]. An `Entry` is a *child of a listing* — it carries
/// the name and path the listing gave it — while this describes a path the
/// caller already named, and it carries the one fact a listing has no place for:
/// when the entry last changed. `size` is `0` for a directory, matching what a
/// listing reports for one (`None` there, since nothing sensible can be said).
///
/// The MIME type is **not** here: it is a function of the path, which the caller
/// already has ([`crate::mime_for_path`]), and a backend that guessed it from
/// the bytes would disagree with the `File` field rule that guesses it from the
/// name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStat {
    /// Size in bytes; `0` for a directory.
    pub size: u64,
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// When the entry was last modified, as RFC 3339, where the backend knows.
    /// `None` is an honest answer for a store whose backend does not record it.
    pub modified: Option<String>,
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
    /// Who created the entry, as the user's id. `None` for anything written
    /// before there was a user to record — a file put there by a code body, a
    /// scaffold, or by hand on the disk underneath.
    ///
    /// It is deliberately *not* an authority over anything: access is the
    /// path-cumulative [`min_role`](FileMeta::min_role) rule and nothing else.
    /// This is the "who put this here" a file manager shows in a column, and it
    /// survives a rewrite of the bytes — the owner is the creator, not the last
    /// writer.
    pub owner: Option<String>,
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

    /// The facts about one entry, or `None` when nothing is there.
    ///
    /// `None` rather than an error, because "is there a file here?" is a
    /// question a caller is entitled to ask and an error is not an answer to it:
    /// a code body writes `if (await file.exists())`, and every other way of
    /// asking would make a missing file a `catch`.
    ///
    /// The default implementation **lists the parent directory** and looks for
    /// the entry, which is correct for any backend that can list — so a store
    /// gains `exists()` without implementing anything. A backend with a cheaper
    /// answer (and a modification time to report) should override it, as
    /// [`crate::LocalFileStore`] does.
    async fn stat(&self, path: &str) -> Result<Option<FileStat>> {
        let trimmed = path.trim_matches('/');
        // The root is the store, and the store is there.
        if trimmed.is_empty() || trimmed == "." {
            return Ok(Some(FileStat {
                size: 0,
                is_dir: true,
                modified: None,
            }));
        }
        let (parent, name) = match trimmed.rsplit_once('/') {
            Some((parent, name)) => (parent, name),
            None => ("", trimmed),
        };
        // A parent that cannot be listed (it does not exist, or it is a file)
        // means the entry is not there either — which is what was asked.
        let Ok(entries) = self.list(parent).await else {
            return Ok(None);
        };
        Ok(entries
            .into_iter()
            .find(|entry| entry.name == name)
            .map(|entry| FileStat {
                size: entry.size.unwrap_or(0),
                is_dir: entry.is_dir,
                modified: entry.modified,
            }))
    }

    /// Read a file's metadata, returning [`FileMeta::default`] when none has been
    /// set.
    async fn get_meta(&self, path: &str) -> Result<FileMeta>;

    /// Replace a file's metadata.
    async fn set_meta(&self, path: &str, meta: &FileMeta) -> Result<()>;
}
