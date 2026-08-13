//! Configuration values: `_sc_config`, and what may be in it (layer 5).
//!
//! Saltcorn's settings are **rows**, not a file: an admin edits them in the
//! admin UI, and every node against the same database sees the same answer. The
//! design's table catalogue (§9) describes `_sc_config` as "per-key value-type
//! restriction; values stored as JSON", and that is exactly this crate's shape:
//!
//! - [`defs`] declares every key as a [`FormField`](sc_types::FormField) — the
//!   same vocabulary a file store's backend or an LLM provider declares its
//!   settings in (§6.2) — so a key has a type, a label, a default, an optional
//!   set of allowed values and a `secret` flag, and the admin UI renders the
//!   settings screen without knowing what any particular setting means.
//! - [`store`] is the table: writing checks the value against its declaration,
//!   so `https_port = "yes"` is refused where the admin can fix it rather than
//!   at the next restart.
//! - [`ssl`] is the first section of settings — how this server obtains the
//!   certificates it serves HTTPS with (§13.5) — and [`acme`] is where the ACME
//!   account and the issued certificates are cached, in the database so a
//!   renewal survives a restart and a second node does not order its own.
//!
//! The TLS *machinery* is a layer up, in `sc-server`, which is where the
//! listener and the rustls stack are. This crate says what was configured; it
//! does not serve anything.

pub mod acme;
pub mod defs;
pub mod ssl;
pub mod store;

pub use acme::{ACME_CACHE_TABLE, AcmeCache, bootstrap_acme_cache};
pub use defs::{
    BACKUP_INCLUDE, ConfigDef, ConfigSection, config_sections, config_spec, definition,
    internal_defs, known_keys,
};
pub use ssl::{
    ACME_CONTACT_EMAIL, ACME_DIRECTORY_URL, HTTPS_PORT, LETSENCRYPT_PRODUCTION,
    LETSENCRYPT_STAGING, MODE_CUSTOM, MODE_LETSENCRYPT, MODE_OFF, REDIRECT_HTTP_TO_HTTPS,
    SSL_CERTIFICATE, SSL_EXTRA_DOMAINS, SSL_MODE, SSL_PRIVATE_KEY, SslMode, SslSettings,
    parse_domains, ssl_settings, ssl_settings_from,
};
pub use store::{
    CONFIG_TABLE, all_config, bootstrap_config, config_value, delete_config, set_config,
    set_config_many, stored_config, stray_config_keys,
};

/// Ensure every table this crate owns exists: the configuration values and the
/// ACME cache.
///
/// Idempotent, and called once at startup — the same contract every other
/// bootstrap has.
pub async fn bootstrap(catalog: &sc_catalog::Catalog) -> sc_error::Result<()> {
    bootstrap_config(catalog).await?;
    bootstrap_acme_cache(catalog).await?;
    Ok(())
}
