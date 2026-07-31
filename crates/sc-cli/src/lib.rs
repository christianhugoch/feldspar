//! Library half of the `saltcorn` binary (layer 10).
//!
//! The binary ([`main`](../main/index.html)) stays thin; the reusable pieces —
//! parsing the database connection ([`DbConfig`]) and standing up a connected
//! [`Catalog`] ([`connect_catalog`]) — live here so integration tests can drive
//! the same boot path the CLI uses.

pub mod db;

use std::sync::Arc;

use sc_catalog::{Catalog, FileStoreConnections, connect_all_file_stores};
use sc_db::DatabaseDriver;
use sc_error::{Context, Error, Result};
use sc_files::{FileStoreDef, connect_from_def};

pub use db::DbConfig;

/// Connect to the primary database described by `db`, initialise the
/// [`Catalog`] from its live schema, and ensure the platform tables (`users`,
/// `_sc_applications`, `_sc_file_stores`, `_sc_tables`) exist.
///
/// This is the whole "bring the data layer up" step of `saltcorn serve`. The
/// first real connection happens inside [`Catalog::init`] (introspection), so a
/// database that is unreachable or misconfigured fails here — with the redacted
/// [`DbConfig::target`] in the message rather than a silent, half-booted server.
///
/// Every bootstrap is idempotent (each no-ops when its table already exists), so
/// this runs on every boot: a legacy database gains the tables on first serve,
/// and the admin UI can list/create applications without a migration step.
///
/// `_sc_tables` is bootstrapped here rather than lazily on first use because it
/// is an **overlay**: [`Catalog::reload`] consults it on every reload, and a
/// table that only appears once someone saves an overlay would mean the merge
/// silently does nothing on exactly the databases nobody has configured yet —
/// which is all of them, until they are.
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
    sc_app::bootstrap(&catalog)
        .await
        .context("ensuring the applications table exists")?;
    sc_catalog::bootstrap_file_stores(&catalog)
        .await
        .context("ensuring the file stores table exists")?;
    sc_catalog::bootstrap_table_meta(&catalog)
        .await
        .context("ensuring the table overlay table exists")?;
    sc_catalog::bootstrap_field_meta(&catalog)
        .await
        .context("ensuring the field overlay table exists")?;
    sc_llm::bootstrap_llm_providers(&catalog)
        .await
        .context("ensuring the LLM providers table exists")?;
    Ok(catalog)
}

/// Connect every **stored** file store, logging the outcome, and return the
/// report.
///
/// One store that fails to connect — a disk unmounted since it was defined — is
/// logged and skipped, never fatal. That is the same rule `mount_all` applies to
/// an application whose build fails (§13.2), and for the same reason: a server
/// that otherwise works should not refuse to boot over one broken store the
/// admin can repoint in the UI. The reason is recorded on the catalog, so the
/// admin API can still answer "why is this store not connected?" long after the
/// boot log has scrolled away.
pub async fn connect_stored_file_stores(catalog: &Catalog) -> Result<FileStoreConnections> {
    let report = connect_all_file_stores(catalog).await?;
    for name in &report.connected {
        eprintln!("saltcorn: connected file store `{name}`");
    }
    for (name, error) in &report.failed {
        eprintln!("saltcorn: file store `{name}` is defined but could not be connected: {error}");
    }
    Ok(report)
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

/// Connect each `NAME=PATH` spec as a local file store on `catalog`.
///
/// The store's name is the part before the first `=`; the rest is a local
/// directory path, which must already exist. A malformed spec or an unreadable
/// directory is an [`Error::Config`], so a typo fails at boot rather than
/// surfacing as a puzzling 404 later.
///
/// Each spec is turned into a [`FileStoreDef`] and connected through
/// [`connect_from_def`], rather than building a `LocalFileStore` here: that
/// function is meant to be *the* place a definition becomes an instance, and a
/// second construction path would be a second place for the two to drift — a
/// flag-connected store would quietly not behave like a stored one.
///
/// **How the flag coexists with stored stores (TODO §1.3, resolved).** These
/// definitions are *not* persisted: the flag stays an ephemeral,
/// process-lifetime convenience, which is how the tests use it and how a
/// developer points at a scratch directory without touching the database. It is
/// applied **after** the stored stores, and a name already connected is a
/// **startup error** rather than a silent override.
///
/// Refusing is the important part. `Catalog::connect_file_store` replaces on a
/// repeated name, so a clash would otherwise mean the flag silently shadowed a
/// store the admin had configured in the UI — the admin would edit a store, see
/// their change saved, and watch the server keep serving a different directory,
/// with nothing anywhere saying why. An error at boot costs one restart; that
/// costs an afternoon.
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
        if catalog.file_store(name)?.is_some() {
            return Err(Error::config(format!(
                "--file-store `{name}` clashes with a configured file store of the same name; \
                 rename one, or drop the flag and edit the store in the admin UI"
            )));
        }
        let store = connect_from_def(&FileStoreDef::local(name, path))
            .with_context(|| format!("connecting file store `{name}` at `{path}`"))?;
        catalog.connect_file_store(store)?;
    }
    Ok(())
}
