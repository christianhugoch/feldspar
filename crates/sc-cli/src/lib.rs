//! Library half of the `saltcorn` binary (layer 10).
//!
//! The binary ([`main`](../main/index.html)) stays thin; the reusable pieces —
//! parsing the database connection ([`DbConfig`]) and standing up a connected
//! [`Catalog`] ([`connect_catalog`]) — live here so integration tests can drive
//! the same boot path the CLI uses.

pub mod db;

use std::sync::Arc;

use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_error::{Context, Error, Result};
use sc_files::{FileStore, LocalFileStore};

pub use db::DbConfig;

/// Connect to the primary database described by `db`, initialise the
/// [`Catalog`] from its live schema, and ensure the `users` table exists.
///
/// This is the whole "bring the data layer up" step of `saltcorn serve`. The
/// first real connection happens inside [`Catalog::init`] (introspection), so a
/// database that is unreachable or misconfigured fails here — with the redacted
/// [`DbConfig::target`] in the message rather than a silent, half-booted server.
pub async fn connect_catalog(db: &DbConfig) -> Result<Arc<Catalog>> {
    let driver = db
        .connect()
        .await
        .with_context(|| format!("connecting to database {}", db.target()))?;
    let catalog = Catalog::init(Arc::new(driver) as Arc<dyn DatabaseDriver>)
        .await
        .with_context(|| format!("reading the schema of database {}", db.target()))?;
    let catalog = Arc::new(catalog);
    sc_auth::bootstrap(&catalog)
        .await
        .context("ensuring the users table exists")?;
    Ok(catalog)
}

/// Pull `--file-store NAME=PATH` flags (repeatable) out of `args`, returning the
/// parsed `NAME=PATH` specs and the arguments that were **not** consumed (for the
/// server config to parse). Like [`DbConfig::extract`], unknown flags pass through
/// so they still fail loudly in the server parser rather than here.
pub fn extract_file_stores<I, S>(args: I) -> Result<(Vec<String>, Vec<String>)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut specs = Vec::new();
    let mut rest = Vec::new();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        if arg.as_ref() == "--file-store" {
            let value = it
                .next()
                .map(|s| s.as_ref().to_owned())
                .ok_or_else(|| Error::config("--file-store requires a NAME=PATH value"))?;
            specs.push(value);
        } else {
            rest.push(arg.as_ref().to_owned());
        }
    }
    Ok((specs, rest))
}

/// Connect each `NAME=PATH` spec as a [`LocalFileStore`] on `catalog`.
///
/// The store's name is the part before the first `=`; the rest is a local
/// directory path (which must already exist — [`LocalFileStore::new`] canonicalises
/// it). A malformed spec or an unreadable directory is an [`Error::Config`], so a
/// typo fails at boot rather than surfacing as a puzzling 404 later.
pub fn connect_file_stores(catalog: &Catalog, specs: &[String]) -> Result<()> {
    for spec in specs {
        let (name, path) = spec.split_once('=').ok_or_else(|| {
            Error::config(format!("invalid --file-store `{spec}`: expected NAME=PATH"))
        })?;
        if name.is_empty() {
            return Err(Error::config(format!(
                "invalid --file-store `{spec}`: the name must not be empty"
            )));
        }
        let store = LocalFileStore::new(name, path)
            .with_context(|| format!("connecting file store `{name}` at `{path}`"))?;
        catalog.connect_file_store(Arc::new(store) as Arc<dyn FileStore>)?;
    }
    Ok(())
}
