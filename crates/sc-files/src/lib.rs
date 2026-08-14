//! File stores (layer 5): the [`FileStore`] trait and its drivers.
//!
//! A file store is one named directory or object store (technical design
//! §14.1). This crate owns the *contract* — [`FileStore`] plus its descriptors
//! [`Entry`] and [`FileMeta`] — and the [`LocalFileStore`] driver backing a
//! store with a local directory.
//!
//! Design invariants encoded here as types rather than prose:
//!
//! - **Files have no database row** (design §9). Per-file metadata is
//!   [`FileMeta`], round-tripped via [`FileStore::get_meta`] /
//!   [`FileStore::set_meta`]; for on-disk stores it is kept beside the bytes in
//!   cross-platform extended attributes (see [`xattr`]).
//! - **Paths are confined to the store root.** Absolute paths and `..`
//!   traversal are rejected before any I/O.

mod access;
mod backend;
mod def;
mod git;
mod local;
mod reference;
mod search;
mod store;
pub mod xattr;

pub use access::{ROLE_PUBLIC, check_access, effective_min_role, filter_visible};
pub use backend::{
    OperationOutcome, backend_config_spec, backend_operations, connect_from_def, git_config_spec,
    local_config_spec, registered_backends, run_backend_operation, validate_file_store_config,
};
pub use def::{
    ATTR_CLONE_PATH, CFG_BRANCH, CFG_CREATE, CFG_DIR, CFG_KEY_PATH, CFG_PATH, CFG_PUBLIC_KEY,
    CFG_URL, FileStoreDef, FileStoreDefId, GIT_BACKEND, LOCAL_BACKEND,
};
pub use git::{
    ARG_BRANCH, ARG_CREATE, ARG_MESSAGE, CommitOutcome, DATA_DIR_ENV, DeployKey, GitChange,
    GitFileStore, GitOutput, GitRepo, GitStatus, OP_CHECKOUT, OP_CLONE, OP_COMMIT, OP_GENERATE_KEY,
    OP_PULL, OP_PUSH, OP_STATUS, clone_dir, clone_path, data_dir, generate_deploy_key,
    git_operations, key_dir, parse_change, record_clone_path, record_deploy_key,
};
pub use local::LocalFileStore;
pub use reference::{mime_for_path, validate_file_path};
pub use search::{
    DEFAULT_EXCLUDED_DIRS, DEFAULT_MAX_RESULTS, MAX_FILE_BYTES, MAX_FILES_SCANNED, MAX_LINE_CHARS,
    SearchHit, SearchOutcome, SearchQuery, glob_matches, search_store,
};
pub use store::{Entry, FileMeta, FileStore};

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    /// Create a `LocalFileStore` rooted at a fresh unique temp directory.
    ///
    /// The counter is what makes it *unique*: the timestamp alone can repeat
    /// between two tests starting in the same clock tick on different threads,
    /// and two tests sharing a store root fail on each other's files — rarely,
    /// and therefore confusingly.
    fn temp_store() -> (std::path::PathBuf, LocalFileStore) {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "sc-files-test-{}-{:?}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&base).unwrap();
        let store = LocalFileStore::new("test", &base).unwrap();
        (base, store)
    }

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let (_base, store) = temp_store();
        assert_eq!(store.name(), "test");
        store
            .write("a/b/hello.txt", Bytes::from_static(b"hello world"))
            .await
            .unwrap();
        let got = store.read("a/b/hello.txt").await.unwrap();
        assert_eq!(&got[..], b"hello world");
    }

    #[tokio::test]
    async fn list_returns_sorted_children_with_sizes() {
        let (_base, store) = temp_store();
        store
            .write("z.txt", Bytes::from_static(b"12345"))
            .await
            .unwrap();
        store
            .write("a.txt", Bytes::from_static(b"x"))
            .await
            .unwrap();
        store
            .write("sub/c.txt", Bytes::from_static(b"y"))
            .await
            .unwrap();

        let root = store.list("").await.unwrap();
        let names: Vec<_> = root.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "sub", "z.txt"]);

        let a = &root[0];
        assert!(!a.is_dir);
        assert_eq!(a.path, "a.txt");
        assert_eq!(a.size, Some(1));

        let sub = &root[1];
        assert!(sub.is_dir);
        assert_eq!(sub.size, None);

        let z = &root[2];
        assert_eq!(z.size, Some(5));

        // Listing a sub-directory yields paths relative to the root.
        let children = store.list("sub").await.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].path, "sub/c.txt");
    }

    #[tokio::test]
    async fn path_traversal_is_rejected() {
        let (_base, store) = temp_store();
        let err = store.read("../secret").await.unwrap_err();
        assert!(matches!(err.repr(), sc_error::Repr::Invalid(_)));
        let err = store
            .write("a/../../b", Bytes::from_static(b"x"))
            .await
            .unwrap_err();
        assert!(matches!(err.repr(), sc_error::Repr::Invalid(_)));
    }

    #[tokio::test]
    async fn meta_round_trips_and_defaults() {
        let (_base, store) = temp_store();
        store
            .write("doc.md", Bytes::from_static(b"# hi"))
            .await
            .unwrap();

        // No metadata set yet → defaults.
        let meta = store.get_meta("doc.md").await.unwrap();
        assert_eq!(meta, FileMeta::default());

        let mut m = FileMeta {
            min_role: Some(20),
            ..Default::default()
        };
        m.attributes.insert("mime".into(), "text/markdown".into());
        store.set_meta("doc.md", &m).await.unwrap();

        let got = store.get_meta("doc.md").await.unwrap();
        assert_eq!(got, m);

        // Metadata lives in an extended attribute, so it never shows as a file.
        let names: Vec<_> = store
            .list("")
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["doc.md"]);
    }

    #[tokio::test]
    async fn set_meta_requires_the_file_to_exist() {
        let (_base, store) = temp_store();
        let err = store
            .set_meta("missing.txt", &FileMeta::default())
            .await
            .unwrap_err();
        assert!(matches!(err.repr(), sc_error::Repr::NotFound(_)));
    }

    #[tokio::test]
    async fn is_git_repo_detects_dot_git() {
        let (base, store) = temp_store();
        assert!(!store.is_git_repo());
        std::fs::create_dir(base.join(".git")).unwrap();
        assert!(store.is_git_repo());
    }

    #[tokio::test]
    async fn mkdir_is_idempotent_but_refuses_to_shadow_a_file() {
        let (_base, store) = temp_store();

        store.mkdir("a/b/c").await.unwrap();
        let entries = store.list("a/b").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_dir);

        // Asking again for a directory that is already there is not an error —
        // the caller wanted one there and there is one.
        store.mkdir("a/b/c").await.unwrap();

        // But a file already occupying the name is a real conflict.
        store
            .write("a/file.txt", Bytes::from_static(b"x"))
            .await
            .unwrap();
        let err = store.mkdir("a/file.txt").await.unwrap_err();
        assert!(matches!(err.repr(), sc_error::Repr::Invalid(_)));
    }

    #[tokio::test]
    async fn delete_reports_whether_anything_was_there() {
        let (_base, store) = temp_store();
        store
            .write("dir/file.txt", Bytes::from_static(b"x"))
            .await
            .unwrap();

        assert!(store.delete("dir/file.txt").await.unwrap());
        // Already gone is `false`, not an error: a caller would otherwise have
        // to race an existence check against the delete.
        assert!(!store.delete("dir/file.txt").await.unwrap());

        // A directory goes with everything in it.
        store
            .write("dir/nested/deep.txt", Bytes::from_static(b"y"))
            .await
            .unwrap();
        assert!(store.delete("dir").await.unwrap());
        assert!(store.list("").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_refuses_the_store_root() {
        let (base, store) = temp_store();
        store
            .write("keep.txt", Bytes::from_static(b"x"))
            .await
            .unwrap();

        // Every spelling of "the root" is refused — a store's root *is* the
        // store, and removing it would leave a definition pointing at nothing.
        for root in ["", "/", "."] {
            let err = store.delete(root).await.unwrap_err();
            assert!(matches!(err.repr(), sc_error::Repr::Invalid(_)), "{root:?}");
        }
        assert!(base.is_dir());
        assert_eq!(store.list("").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rename_moves_within_the_store_and_never_clobbers() {
        let (_base, store) = temp_store();
        store
            .write("a/one.txt", Bytes::from_static(b"first"))
            .await
            .unwrap();
        store
            .write("b/two.txt", Bytes::from_static(b"second"))
            .await
            .unwrap();

        // Moving into a directory that does not exist yet creates it.
        store.rename("a/one.txt", "c/moved.txt").await.unwrap();
        assert_eq!(&store.read("c/moved.txt").await.unwrap()[..], b"first");
        assert!(store.list("a").await.unwrap().is_empty());

        // Refusing to overwrite is the important part: a silent replace on a
        // file manager's drag-and-drop is a lost file with no undo.
        let err = store.rename("b/two.txt", "c/moved.txt").await.unwrap_err();
        assert!(matches!(err.repr(), sc_error::Repr::Invalid(_)));
        assert_eq!(&store.read("c/moved.txt").await.unwrap()[..], b"first");
        assert_eq!(&store.read("b/two.txt").await.unwrap()[..], b"second");

        // A source that is not there is a not-found, not a silent success.
        let err = store.rename("nope.txt", "x.txt").await.unwrap_err();
        assert!(matches!(err.repr(), sc_error::Repr::NotFound(_)));

        // And traversal is rejected on both sides, so this cannot move data out
        // of the store.
        assert!(store.rename("b/two.txt", "../escaped.txt").await.is_err());
        assert!(store.rename("../secret", "here.txt").await.is_err());
    }

    /// The path-cumulative rule `FileMeta` has documented since the MVP, now
    /// actually enforced: a directory's restriction covers everything beneath it.
    #[tokio::test]
    async fn access_is_cumulative_along_the_path() {
        let (_base, store) = temp_store();
        store
            .write("public/open.txt", Bytes::from_static(b"x"))
            .await
            .unwrap();
        store
            .write("private/secret.txt", Bytes::from_static(b"y"))
            .await
            .unwrap();

        // Restrict the *directory* only; the file inside carries no rule.
        store.mkdir("private").await.unwrap();
        store
            .set_meta(
                "private",
                &FileMeta {
                    min_role: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // The file inherits the directory's restriction even though nothing was
        // set on the file. This is the whole point of the rule.
        assert_eq!(
            effective_min_role(&store, None, "private/secret.txt")
                .await
                .unwrap(),
            Some(1)
        );
        // An admin (role 1) clears it; a public caller (100) does not.
        assert!(
            check_access(&store, None, "private/secret.txt", 1)
                .await
                .is_ok()
        );
        assert!(
            check_access(&store, None, "private/secret.txt", 100)
                .await
                .is_err()
        );
        // An unrelated path is unaffected.
        assert!(
            check_access(&store, None, "public/open.txt", ROLE_PUBLIC)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_nested_rule_can_only_tighten_and_the_store_floor_applies_first() {
        let (_base, store) = temp_store();
        store
            .write("dir/file.txt", Bytes::from_static(b"x"))
            .await
            .unwrap();
        store.mkdir("dir").await.unwrap();

        // A permissive rule inside a restricted directory must NOT widen access:
        // otherwise restricting a folder would not restrict it, since anything
        // inside could opt back out.
        store
            .set_meta(
                "dir",
                &FileMeta {
                    min_role: Some(20),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        store
            .set_meta(
                "dir/file.txt",
                &FileMeta {
                    min_role: Some(100),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            effective_min_role(&store, None, "dir/file.txt")
                .await
                .unwrap(),
            Some(20),
            "the more restrictive rule on the path must win"
        );

        // The store-wide floor is simply the outermost entry on the path, and
        // composes the same way.
        assert_eq!(
            effective_min_role(&store, Some(10), "dir/file.txt")
                .await
                .unwrap(),
            Some(10)
        );
        // A laxer store floor does not loosen a tighter nested rule.
        assert_eq!(
            effective_min_role(&store, Some(80), "dir/file.txt")
                .await
                .unwrap(),
            Some(20)
        );
    }

    #[tokio::test]
    async fn listings_hide_entries_the_caller_may_not_open() {
        let (_base, store) = temp_store();
        store
            .write("open.txt", Bytes::from_static(b"x"))
            .await
            .unwrap();
        store
            .write("secret.txt", Bytes::from_static(b"y"))
            .await
            .unwrap();
        store
            .set_meta(
                "secret.txt",
                &FileMeta {
                    min_role: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let all = store.list("").await.unwrap();
        assert_eq!(all.len(), 2);

        // A listing must be filtered rather than merely refused: showing a name
        // the caller cannot open leaks exactly what the rule was set to hide.
        let visible = filter_visible(&store, None, all.clone(), ROLE_PUBLIC)
            .await
            .unwrap();
        let names: Vec<&str> = visible.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["open.txt"]);

        // An admin sees everything.
        let admin = filter_visible(&store, None, all, 1).await.unwrap();
        assert_eq!(admin.len(), 2);
    }

    // --- the store-side search ----------------------------------------------

    /// A small project to search: two source files, one binary, and a
    /// dependency directory that must not be walked.
    async fn searchable() -> (std::path::PathBuf, LocalFileStore) {
        let (base, store) = temp_store();
        for (path, bytes) in [
            ("src/app.ts", &b"export function todo() {}\n"[..]),
            ("src/deep/list.tsx", b"// TODO: paginate\nconst n = 1;\n"),
            ("readme.md", b"nothing to see\n"),
            ("node_modules/pkg/index.js", b"todo everywhere\n"),
        ] {
            store
                .write(path, Bytes::copy_from_slice(bytes))
                .await
                .unwrap();
        }
        // Not UTF-8: it has no lines, and "matched at line 3" of a PNG is noise.
        store
            .write(
                "logo.png",
                Bytes::from_static(&[0xff, 0xfe, b't', b'o', 0x00]),
            )
            .await
            .unwrap();
        (base, store)
    }

    #[tokio::test]
    async fn a_search_walks_the_tree_and_skips_what_it_should() {
        let (_base, store) = searchable().await;
        let found = search_store(&store, None, 1, &SearchQuery::literal("todo"))
            .await
            .unwrap();
        let mut paths: Vec<&str> = found.hits.iter().map(|h| h.path.as_str()).collect();
        paths.sort();
        // `node_modules` is not descended into and the binary is not scanned, so
        // a store with a dependency tree in it is still searchable.
        assert_eq!(paths, ["src/app.ts", "src/deep/list.tsx"]);
        assert!(!found.truncated);

        // Line and column are 1-based, and the line comes back whole.
        let hit = found
            .hits
            .iter()
            .find(|h| h.path == "src/app.ts")
            .expect("the source file");
        assert_eq!(hit.line, 1);
        assert_eq!(hit.column, 17);
        assert_eq!(hit.text, "export function todo() {}");
    }

    #[tokio::test]
    async fn a_search_can_be_narrowed_by_directory_and_by_glob() {
        let (_base, store) = searchable().await;

        let mut query = SearchQuery::literal("todo");
        query.dir = "src/deep".to_owned();
        let found = search_store(&store, None, 1, &query).await.unwrap();
        let paths: Vec<&str> = found.hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, ["src/deep/list.tsx"]);

        let mut query = SearchQuery::literal("todo");
        query.glob = Some("*.ts".to_owned());
        let found = search_store(&store, None, 1, &query).await.unwrap();
        let paths: Vec<&str> = found.hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, ["src/app.ts"]);
    }

    #[tokio::test]
    async fn a_search_cannot_report_a_line_the_caller_could_not_have_read() {
        let (_base, store) = searchable().await;
        store
            .set_meta(
                "src/deep",
                &FileMeta {
                    min_role: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // The restricted directory is not walked for a caller who could not open
        // it — otherwise its contents would leak one matching line at a time.
        let found = search_store(&store, None, ROLE_PUBLIC, &SearchQuery::literal("todo"))
            .await
            .unwrap();
        let paths: Vec<&str> = found.hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, ["src/app.ts"]);

        // The store's own floor composes the same way: above it, nothing at all.
        let found = search_store(&store, Some(1), ROLE_PUBLIC, &SearchQuery::literal("todo"))
            .await
            .unwrap();
        assert!(found.hits.is_empty());
    }

    #[tokio::test]
    async fn a_search_that_hit_its_ceiling_says_so() {
        let (_base, store) = temp_store();
        for n in 0..10 {
            store
                .write(&format!("f{n}.ts"), Bytes::from_static(b"needle\nneedle\n"))
                .await
                .unwrap();
        }
        let mut query = SearchQuery::literal("needle");
        query.max_results = 5;
        let found = search_store(&store, None, 1, &query).await.unwrap();
        assert_eq!(found.hits.len(), 5);
        assert!(found.truncated);
    }
}
