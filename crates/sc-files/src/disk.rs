//! Where a store's files live on this machine, and removing them — the half of
//! **Clear all** that touches the disk rather than the database.
//!
//! Both backends keep a store in a directory: a local store *is* its `path`, and
//! a git store is a working copy at [`clone_path`]. Deleting a store's
//! definition leaves that directory where it was, which is the right default —
//! a local store usually points at files the admin had before Saltcorn — so
//! removing it is a separate, explicit act, asked for store by store.
//!
//! **Some directories are never removed**, whatever a store says: the root, the
//! home directory, the data directory and the directory the server was started
//! in, or any directory holding one of those. A local store rooted at `~` is a
//! perfectly good configuration, and "delete this store from disk" must not
//! turn it into the deletion of everything the account owns.

use std::path::{Path, PathBuf};

use sc_error::{Error, Result};

use crate::def::{CFG_KEY_PATH, CFG_PATH, FileStoreDef, GIT_BACKEND, LOCAL_BACKEND};
use crate::git::{clone_path, key_dir};
use crate::paths::data_dir;

/// The directory holding a store's files on this machine, or `None` for a store
/// with nowhere on disk to name (a backend that is not a directory, or a local
/// store with no `path`).
pub fn store_directory(def: &FileStoreDef) -> Result<Option<PathBuf>> {
    match def.backend.as_str() {
        LOCAL_BACKEND => Ok(def
            .setting(CFG_PATH)
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)),
        GIT_BACKEND => clone_path(def).map(Some),
        _ => Ok(None),
    }
}

/// Remove a store's files from disk: its directory and, for a git store, the
/// deploy key Saltcorn generated for it (a key the admin pointed at elsewhere
/// on the machine is theirs and stays).
///
/// Returns what was removed. A directory that is already gone is not an error —
/// the store's files are off the disk either way — and comes back empty.
pub fn remove_store_from_disk(def: &FileStoreDef) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    if let Some(dir) = store_directory(def)? {
        if dir.exists() {
            refuse_protected(&dir)?;
            std::fs::remove_dir_all(&dir)
                .map_err(|e| Error::file(format!("could not remove `{}`: {e}", dir.display())))?;
            removed.push(dir);
        }
    }
    if def.backend == GIT_BACKEND
        && let Some(key) = def
            .setting(CFG_KEY_PATH)
            .map(str::trim)
            .filter(|k| !k.is_empty())
    {
        let key = PathBuf::from(key);
        let generated = key_dir()
            .ok()
            .and_then(|d| d.canonicalize().ok())
            .zip(key.canonicalize().ok())
            .is_some_and(|(keys, key)| key.starts_with(keys));
        if generated {
            let public = PathBuf::from(format!("{}.pub", key.display()));
            for file in [key, public] {
                if file.exists() {
                    std::fs::remove_file(&file).map_err(|e| {
                        Error::file(format!("could not remove `{}`: {e}", file.display()))
                    })?;
                    removed.push(file);
                }
            }
        }
    }
    Ok(removed)
}

/// Refuse a directory that is, or contains, one this machine cannot do without.
fn refuse_protected(dir: &Path) -> Result<()> {
    if !dir.is_absolute() {
        return Err(Error::invalid(format!(
            "`{}` is not an absolute path, so it is not removed",
            dir.display()
        )));
    }
    let dir = dir
        .canonicalize()
        .map_err(|e| Error::file(format!("could not resolve `{}`: {e}", dir.display())))?;
    let mut protected: Vec<PathBuf> = vec![PathBuf::from("/")];
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        protected.push(PathBuf::from(home));
    }
    if let Ok(data) = data_dir() {
        protected.push(data);
    }
    if let Ok(cwd) = std::env::current_dir() {
        protected.push(cwd);
    }
    for p in protected {
        let p = p.canonicalize().unwrap_or(p);
        if p.starts_with(&dir) {
            return Err(Error::invalid(format!(
                "`{}` holds `{}`, so it is not removed",
                dir.display(),
                p.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(path: &Path) -> FileStoreDef {
        FileStoreDef::local("docs", path.to_string_lossy().into_owned())
    }

    #[test]
    fn a_local_store_is_removed_with_everything_in_it() {
        let dir = std::env::temp_dir().join(format!("sc-disk-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.txt"), "a").unwrap();
        let removed = remove_store_from_disk(&local(&dir)).unwrap();
        assert_eq!(removed, vec![dir.clone()]);
        assert!(!dir.exists());
        // Gone already is not a failure.
        assert!(remove_store_from_disk(&local(&dir)).unwrap().is_empty());
    }

    #[test]
    fn a_directory_holding_the_working_directory_is_refused() {
        let cwd = std::env::current_dir().unwrap();
        let parent = cwd.parent().unwrap().to_path_buf();
        let err = remove_store_from_disk(&local(&parent)).unwrap_err();
        assert!(err.to_string().contains("not removed"), "{err}");
        assert!(cwd.exists());
        let err = remove_store_from_disk(&local(Path::new("/"))).unwrap_err();
        assert!(err.to_string().contains("not removed"), "{err}");
    }
}
