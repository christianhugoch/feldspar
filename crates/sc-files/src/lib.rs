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
//!   [`FileStore::set_meta`]; for on-disk stores it is kept beside the bytes.
//! - **Paths are confined to the store root.** Absolute paths and `..`
//!   traversal are rejected before any I/O.

mod local;
mod store;

pub use local::LocalFileStore;
pub use store::{Entry, FileMeta, FileStore};

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    /// Create a `LocalFileStore` rooted at a fresh unique temp directory.
    fn temp_store() -> (std::path::PathBuf, LocalFileStore) {
        let base = std::env::temp_dir().join(format!(
            "sc-files-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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
        store.write("z.txt", Bytes::from_static(b"12345")).await.unwrap();
        store.write("a.txt", Bytes::from_static(b"x")).await.unwrap();
        store.write("sub/c.txt", Bytes::from_static(b"y")).await.unwrap();

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
        assert!(matches!(err, sc_error::Error::Invalid(_)));
        let err = store
            .write("a/../../b", Bytes::from_static(b"x"))
            .await
            .unwrap_err();
        assert!(matches!(err, sc_error::Error::Invalid(_)));
    }

    #[tokio::test]
    async fn meta_round_trips_and_defaults() {
        let (_base, store) = temp_store();
        store.write("doc.md", Bytes::from_static(b"# hi")).await.unwrap();

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

        // Metadata sidecar directory is hidden from listings.
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
        assert!(matches!(err, sc_error::Error::NotFound(_)));
    }

    #[tokio::test]
    async fn is_git_repo_detects_dot_git() {
        let (base, store) = temp_store();
        assert!(!store.is_git_repo());
        std::fs::create_dir(base.join(".git")).unwrap();
        assert!(store.is_git_repo());
    }
}
