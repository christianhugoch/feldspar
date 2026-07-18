//! Cross-platform extended-attribute access (technical design §9/§14.1).
//!
//! Thin async wrappers over the [`fsquirrel`] crate, which stores attributes in
//! POSIX extended attributes (hardcoded `user.` namespace) on Unix —
//! Linux, macOS, FreeBSD, NetBSD, Android — and in **NTFS Alternate Data
//! Streams** on Windows. This is the layer that lets Saltcorn keep per-file
//! metadata beside the bytes with no database row (design §9).
//!
//! `fsquirrel`'s calls are blocking filesystem syscalls, so each wrapper hands
//! the work to tokio's blocking pool via [`tokio::task::spawn_blocking`].
//!
//! Attribute values do not survive leaving the filesystem (uploads, most
//! copies), which matches their role as local, regenerable metadata rather than
//! a source of truth.

use sc_error::{Context, Result};
use std::path::Path;

/// Read an extended attribute, returning `None` when the attribute is absent
/// (the file itself must exist).
pub async fn get(path: impl AsRef<Path>, name: &str) -> Result<Option<Vec<u8>>> {
    let path = path.as_ref().to_owned();
    let attr = name.to_owned();
    let label = name.to_owned();
    run(move || fsquirrel::get(&path, &attr))
        .await
        .with_context(|| format!("reading extended attribute {label:?}"))
}

/// Create or overwrite an extended attribute.
pub async fn set(path: impl AsRef<Path>, name: &str, value: Vec<u8>) -> Result<()> {
    let path = path.as_ref().to_owned();
    let attr = name.to_owned();
    let label = name.to_owned();
    run(move || fsquirrel::set(&path, &attr, value))
        .await
        .with_context(|| format!("writing extended attribute {label:?}"))
}

/// Remove an extended attribute. Removing an absent attribute is an error at the
/// OS level and is surfaced as such.
pub async fn remove(path: impl AsRef<Path>, name: &str) -> Result<()> {
    let path = path.as_ref().to_owned();
    let attr = name.to_owned();
    let label = name.to_owned();
    run(move || fsquirrel::remove(&path, &attr))
        .await
        .with_context(|| format!("removing extended attribute {label:?}"))
}

/// Run a blocking `fsquirrel` call on the blocking pool and flatten the
/// join/`io` results into one workspace [`Result`].
async fn run<T, F>(f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> std::io::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .context("extended-attribute task failed to join")?
        .map_err(sc_error::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp file the current process owns, on the default temp fs.
    fn temp_file() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "sc-xattr-test-{}-{:?}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&p, b"payload").unwrap();
        p
    }

    #[tokio::test]
    async fn get_set_remove_round_trip() {
        let path = temp_file();

        // Absent attribute reads back as None.
        assert_eq!(get(&path, "saltcorn.test").await.unwrap(), None);

        set(&path, "saltcorn.test", b"value-1".to_vec())
            .await
            .unwrap();
        assert_eq!(
            get(&path, "saltcorn.test").await.unwrap().as_deref(),
            Some(&b"value-1"[..])
        );

        // Overwrite.
        set(&path, "saltcorn.test", b"value-2".to_vec())
            .await
            .unwrap();
        assert_eq!(
            get(&path, "saltcorn.test").await.unwrap().as_deref(),
            Some(&b"value-2"[..])
        );

        remove(&path, "saltcorn.test").await.unwrap();
        assert_eq!(get(&path, "saltcorn.test").await.unwrap(), None);

        let _ = std::fs::remove_file(&path);
    }
}
