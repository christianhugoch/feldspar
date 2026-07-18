//! Enforcing the **path-cumulative** access rule that [`FileMeta`] documents.
//!
//! [`FileMeta::min_role`] has been modelled since the MVP with the rule stated in
//! its own doc comment — *"to reach a file a user must clear the `min_role` of
//! the file **and** of every directory on its path"* — and nothing anywhere
//! enforced it. A stored rule that nothing checks is worse than no rule: an admin
//! sets a folder to admin-only, the UI shows it as restricted, and every file in
//! it stays readable. This module is that check.
//!
//! ## What "path-cumulative" means
//!
//! Roles follow the `sc-auth` convention: `1` is admin, `100` is public, and a
//! **lower number is more restrictive**. To reach `a/b/c.txt` a user must clear
//! the rule on the store, on `a`, on `a/b`, and on `c.txt` itself. The effective
//! floor is therefore the *most restrictive* (numerically smallest) rule on the
//! whole path, and a directory can only ever tighten access, never loosen it.
//!
//! That direction is the point. If a nested rule could widen access, then
//! restricting a folder would not actually restrict it — anything inside could
//! opt back out — and an admin could never reason about a tree from its root.
//!
//! ## Where the store's own floor fits
//!
//! A [`FileStoreDef::min_role`](crate::FileStoreDef) is the floor for the whole
//! store, applied before any per-file rule, and it composes the same way: it is
//! simply the outermost entry on the path.

use crate::store::{FileMeta, FileStore};
use sc_error::{Error, Result};

/// The least-restrictive role: everyone, including anonymous callers. Matches the
/// `sc-auth` scale where `100` is public.
pub const ROLE_PUBLIC: u8 = 100;

/// Combine two optional rules, keeping the more restrictive (smaller) one.
///
/// `None` means "this level says nothing", which must leave the inherited floor
/// untouched rather than reset it to public — otherwise a directory with no rule
/// of its own would silently undo its parent's.
fn tighten(current: Option<u8>, rule: Option<u8>) -> Option<u8> {
    match (current, rule) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Every prefix of `path` that carries a rule, from the store root inwards,
/// including `path` itself.
///
/// `a/b/c.txt` yields `a`, `a/b`, `a/b/c.txt` — each of which may carry its own
/// [`FileMeta`]. Empty segments and `.` are skipped so the caller's normalisation
/// quirks (leading, trailing or doubled slashes) cannot change the answer.
fn ancestors(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(segment);
        out.push(current.clone());
    }
    out
}

/// The effective minimum role required to reach `path` in `store`, given the
/// store-wide floor `store_min_role`.
///
/// `None` means nothing on the path restricts it. Metadata is read for each
/// ancestor in turn; a path component whose metadata cannot be read at all (it
/// does not exist yet, which is normal when writing a new file) contributes
/// nothing rather than failing the check — the existence of the target is the
/// caller's concern, not the access rule's.
pub async fn effective_min_role(
    store: &dyn FileStore,
    store_min_role: Option<u8>,
    path: &str,
) -> Result<Option<u8>> {
    let mut floor = store_min_role;
    for ancestor in ancestors(path) {
        // A missing component simply has no rule of its own. Reading metadata for
        // something that is not there is not an access failure, and treating it
        // as one would make writing a new file impossible.
        let meta = store.get_meta(&ancestor).await.unwrap_or_default();
        floor = tighten(floor, meta.min_role);
    }
    Ok(floor)
}

/// Check that `role` may reach `path`, returning an error if not.
///
/// Remember the scale is inverted: a user clears a rule when their role number is
/// **less than or equal to** the required one (admin `1` clears everything;
/// public `100` clears only unrestricted paths).
///
/// The error deliberately does not say which component of the path denied
/// access, nor whether the target exists. Naming the restricted folder would tell
/// a caller who cannot read a tree something about its shape, and distinguishing
/// "forbidden" from "not found" turns the endpoint into a probe for the existence
/// of files the caller may not see.
pub async fn check_access(
    store: &dyn FileStore,
    store_min_role: Option<u8>,
    path: &str,
    role: u8,
) -> Result<()> {
    match effective_min_role(store, store_min_role, path).await? {
        Some(required) if role > required => Err(Error::auth(format!(
            "not permitted to access {path:?} in file store {}",
            store.name()
        ))),
        _ => Ok(()),
    }
}

/// Filter a directory listing to the entries `role` may see.
///
/// A listing has to be filtered rather than merely refused: the directory itself
/// may be readable while individual children are not, and showing names the
/// caller cannot open leaks exactly what the rule was set to hide.
///
/// The parent's floor is passed in as `dir_floor` (already including the store's)
/// so this only consults each child's own metadata — the ancestors are common to
/// every entry and re-reading them per child would be one `get_meta` per level
/// per file.
pub async fn filter_visible(
    store: &dyn FileStore,
    dir_floor: Option<u8>,
    entries: Vec<crate::Entry>,
    role: u8,
) -> Result<Vec<crate::Entry>> {
    let mut visible = Vec::with_capacity(entries.len());
    for entry in entries {
        let meta: FileMeta = store.get_meta(&entry.path).await.unwrap_or_default();
        match tighten(dir_floor, meta.min_role) {
            Some(required) if role > required => continue,
            _ => visible.push(entry),
        }
    }
    Ok(visible)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestors_walks_from_the_root_inwards() {
        assert_eq!(ancestors("a/b/c.txt"), ["a", "a/b", "a/b/c.txt"]);
        assert_eq!(ancestors("only.txt"), ["only.txt"]);
        assert!(ancestors("").is_empty());
        // Normalisation quirks must not change which rules apply.
        assert_eq!(ancestors("/a//b/./c"), ["a", "a/b", "a/b/c"]);
    }

    #[test]
    fn tightening_keeps_the_most_restrictive_rule() {
        // Lower is more restrictive, so a nested rule can only narrow access.
        assert_eq!(tighten(Some(40), Some(20)), Some(20));
        assert_eq!(tighten(Some(20), Some(40)), Some(20));
        // A level with no rule leaves the inherited floor alone — it must not
        // reset it to public, or a folder's restriction would be undone by any
        // unmarked child.
        assert_eq!(tighten(Some(20), None), Some(20));
        assert_eq!(tighten(None, Some(20)), Some(20));
        assert_eq!(tighten(None, None), None);
    }
}
