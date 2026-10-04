//! `_fd_config` against real Postgres (design §9, §13.5).
//!
//! The claim under test is the one the table exists for: **a configuration
//! value is typed, and the type is checked where it is written**. A settings
//! table whose values are whatever the last caller happened to send is a table
//! that reports its mistakes at the next restart, in a log nobody is reading.
//!
//! So: a declared key round-trips as the type it declares; an undeclared key is
//! refused by name; a wrongly typed value is refused with the key in the
//! message and *nothing* written; a value outside a key's options is refused;
//! clearing a key returns it to its declared default; and a batch save is
//! checked whole before any of it lands.

use std::sync::Arc;

use sc_catalog::Catalog;
use sc_config::{
    ACME_CONTACT_EMAIL, MODE_CUSTOM, MODE_LETSENCRYPT, REDIRECT_HTTP_TO_HTTPS, SMTP_PORT,
    SSL_CERTIFICATE, SSL_MODE, SSL_PRIVATE_KEY, SslMode, all_config, bootstrap, config_value,
    delete_config, set_config, set_config_many, ssl_settings, stored_config, stray_config_keys,
};
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::Result;
use sc_query::{Expr, Insert, Statement};
use sc_types::Attrs;
use serde_json::json;

use sc_test_harness::TestDb;

/// A database with the configuration tables bootstrapped.
async fn fixture() -> Result<(Arc<Catalog>, TestDb)> {
    let db = TestDb::new().await?;
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let catalog = Arc::new(Catalog::init(driver as Arc<dyn DatabaseDriver>).await?);
    bootstrap(&catalog).await?;
    Ok((catalog, db))
}

#[tokio::test]
async fn a_declared_value_round_trips_as_its_declared_type() -> Result<()> {
    let (catalog, _db) = fixture().await?;

    // Nothing stored: the declared default is what the server acts on, and
    // `stored_config` still says "unset", which is the distinction a form needs.
    assert_eq!(stored_config(&catalog, SMTP_PORT).await?, None);
    assert_eq!(config_value(&catalog, SMTP_PORT).await?, json!(587));

    set_config(&catalog, SMTP_PORT, json!(2525)).await?;
    set_config(&catalog, SSL_MODE, json!(MODE_LETSENCRYPT)).await?;
    set_config(&catalog, REDIRECT_HTTP_TO_HTTPS, json!(false)).await?;

    // An int comes back an int and a bool a bool — the JSON column does not
    // flatten them to text on the way through the driver.
    assert_eq!(stored_config(&catalog, SMTP_PORT).await?, Some(json!(2525)));
    assert_eq!(
        stored_config(&catalog, REDIRECT_HTTP_TO_HTTPS).await?,
        Some(json!(false))
    );

    // A second write of the same key updates rather than duplicating.
    set_config(&catalog, SMTP_PORT, json!(2526)).await?;
    assert_eq!(config_value(&catalog, SMTP_PORT).await?, json!(2526));

    let settings = ssl_settings(&catalog).await?;
    assert_eq!(settings.mode, SslMode::LetsEncrypt);
    assert!(!settings.redirect_http);
    Ok(())
}

#[tokio::test]
async fn a_wrongly_typed_or_unknown_key_is_refused_and_writes_nothing() -> Result<()> {
    let (catalog, _db) = fixture().await?;

    let err = set_config(&catalog, SMTP_PORT, json!("eight thousand"))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains(SMTP_PORT), "{err}");
    assert_eq!(stored_config(&catalog, SMTP_PORT).await?, None);

    // An option a declaration does not offer is the same kind of mistake.
    let err = set_config(&catalog, SSL_MODE, json!("sometimes"))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains(MODE_LETSENCRYPT), "should list options: {err}");

    // And a key nobody declared is a typo, not a new setting.
    let err = set_config(&catalog, "ssl_moed", json!("off"))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("ssl_moed"), "{err}");
    assert!(err.contains(SSL_MODE), "should list known keys: {err}");
    Ok(())
}

#[tokio::test]
async fn clearing_a_key_returns_it_to_its_default() -> Result<()> {
    let (catalog, _db) = fixture().await?;

    set_config(&catalog, SMTP_PORT, json!(2525)).await?;
    assert!(delete_config(&catalog, SMTP_PORT).await?);
    assert_eq!(config_value(&catalog, SMTP_PORT).await?, json!(587));
    // Deleting what is not there is not an error, it is just false.
    assert!(!delete_config(&catalog, SMTP_PORT).await?);

    // A null does the same thing through the write path, which is how a form
    // clears a box.
    set_config(&catalog, SMTP_PORT, json!(2525)).await?;
    set_config(&catalog, SMTP_PORT, json!(null)).await?;
    assert_eq!(stored_config(&catalog, SMTP_PORT).await?, None);
    Ok(())
}

/// A settings form's Save is one act: a bad value in it must not leave half the
/// screen applied — the mode switched to `custom` with no certificate to serve.
#[tokio::test]
async fn a_batch_save_is_checked_whole_before_any_of_it_lands() -> Result<()> {
    let (catalog, _db) = fixture().await?;

    let mut values = Attrs::new();
    values.insert(SSL_MODE.to_owned(), json!(MODE_CUSTOM));
    values.insert(
        SSL_CERTIFICATE.to_owned(),
        json!("-----BEGIN CERTIFICATE-----"),
    );
    values.insert(SMTP_PORT.to_owned(), json!("not a port"));

    let err = set_config_many(&catalog, &values).await.unwrap_err();
    assert!(err.to_string().contains(SMTP_PORT), "{err}");
    assert_eq!(stored_config(&catalog, SSL_MODE).await?, None);
    assert_eq!(stored_config(&catalog, SSL_CERTIFICATE).await?, None);

    values.insert(SMTP_PORT.to_owned(), json!(2525));
    set_config_many(&catalog, &values).await?;
    let stored = all_config(&catalog).await?;
    assert_eq!(stored.get(SSL_MODE), Some(&json!(MODE_CUSTOM)));
    assert_eq!(stored.get(SMTP_PORT), Some(&json!(2525)));
    // A key that was never set still reports its default in the same bag.
    assert_eq!(stored.get(ACME_CONTACT_EMAIL), None);
    assert_eq!(stored.get(REDIRECT_HTTP_TO_HTTPS), Some(&json!(true)));
    Ok(())
}

/// A row for a key no declaration describes is not readable as a setting, and
/// not invisible either: it is reportable, and deletable.
#[tokio::test]
async fn a_stray_row_is_reported_rather_than_read() -> Result<()> {
    let (catalog, _db) = fixture().await?;

    set_config(&catalog, SSL_MODE, json!(MODE_CUSTOM)).await?;
    // Written behind the store's back, which is the only way it can happen: an
    // older release's key, or a hand-edited row.
    let insert = Insert::row(
        sc_config::CONFIG_TABLE,
        vec!["key".to_owned(), "value".to_owned()],
        vec![Expr::lit("legacy_setting"), Expr::lit(json!("x"))],
    );
    catalog
        .primary()
        .query(&Statement::from(insert))
        .await?
        .try_collect()
        .await?;

    assert_eq!(stray_config_keys(&catalog).await?, ["legacy_setting"]);
    // It does not disturb the settings that *are* declared.
    let all = all_config(&catalog).await?;
    assert_eq!(all.get(SSL_MODE), Some(&json!(MODE_CUSTOM)));
    assert!(all.get("legacy_setting").is_none());

    assert!(delete_config(&catalog, "legacy_setting").await?);
    assert!(stray_config_keys(&catalog).await?.is_empty());
    Ok(())
}

/// The private key is stored as written — the redaction is what the API does on
/// the way out, not what the table does on the way in. A key mangled at rest is
/// a certificate that cannot be served.
#[tokio::test]
async fn a_secret_setting_is_stored_verbatim() -> Result<()> {
    let (catalog, _db) = fixture().await?;

    let pem = "-----BEGIN PRIVATE KEY-----\nMIIBVQ==\n-----END PRIVATE KEY-----\n";
    set_config(&catalog, SSL_PRIVATE_KEY, json!(pem)).await?;
    assert_eq!(
        stored_config(&catalog, SSL_PRIVATE_KEY).await?,
        Some(json!(pem))
    );
    Ok(())
}
