//! The ACME cache against real Postgres (design §13.5).
//!
//! Why this is a table and not a directory: a renewal has to survive a restart,
//! and a second node has to serve the certificate the first one ordered rather
//! than ordering its own — which is how a deployment that scales out walks into
//! a CA's rate limits. Both of those are claims about *storage*, so they are
//! asserted here.
//!
//! Two caches over one database stand in for two nodes, exactly as the session
//! store's tests do.

use std::sync::Arc;

use sc_catalog::Catalog;
use sc_config::{AcmeCache, LETSENCRYPT_PRODUCTION, LETSENCRYPT_STAGING, bootstrap};
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::Result;
use sc_test_harness::TestDb;

async fn fixture() -> Result<(Arc<Catalog>, TestDb)> {
    let db = TestDb::new().await?;
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let catalog = Arc::new(Catalog::init(driver as Arc<dyn DatabaseDriver>).await?);
    bootstrap(&catalog).await?;
    Ok((catalog, db))
}

#[tokio::test]
async fn a_certificate_cached_by_one_node_is_found_by_another() -> Result<()> {
    let (catalog, _db) = fixture().await?;
    let domains = vec!["example.com".to_owned(), "blog.example.com".to_owned()];
    // The bytes are opaque and not text — a private key is in there — so the
    // round trip is asserted over something that is not valid UTF-8.
    let pem: Vec<u8> = vec![0x00, 0xff, b'-', b'-', 0x10, b'K'];

    let node_a = AcmeCache::new(catalog.clone());
    let node_b = AcmeCache::new(catalog.clone());

    assert_eq!(
        node_b.load_cert(&domains, LETSENCRYPT_PRODUCTION).await?,
        None
    );
    node_a
        .store_cert(&domains, LETSENCRYPT_PRODUCTION, &pem)
        .await?;
    assert_eq!(
        node_b.load_cert(&domains, LETSENCRYPT_PRODUCTION).await?,
        Some(pem.clone())
    );

    // A renewal replaces rather than accumulating.
    let renewed = vec![0x01, 0x02];
    node_b
        .store_cert(&domains, LETSENCRYPT_PRODUCTION, &renewed)
        .await?;
    assert_eq!(
        node_a.load_cert(&domains, LETSENCRYPT_PRODUCTION).await?,
        Some(renewed)
    );
    Ok(())
}

/// The cache key is what the entry is *for*: pointing a deployment at the
/// staging directory, or adding a domain, must miss rather than serve the wrong
/// certificate.
#[tokio::test]
async fn an_entry_is_not_reused_for_a_different_order() -> Result<()> {
    let (catalog, _db) = fixture().await?;
    let cache = AcmeCache::new(catalog.clone());
    let one = vec!["example.com".to_owned()];
    let two = vec!["example.com".to_owned(), "www.example.com".to_owned()];

    cache
        .store_cert(&one, LETSENCRYPT_PRODUCTION, b"prod-one")
        .await?;
    assert_eq!(cache.load_cert(&two, LETSENCRYPT_PRODUCTION).await?, None);
    assert_eq!(cache.load_cert(&one, LETSENCRYPT_STAGING).await?, None);
    assert_eq!(
        cache.load_cert(&one, LETSENCRYPT_PRODUCTION).await?,
        Some(b"prod-one".to_vec())
    );

    // The account is keyed separately from the certificate, so one does not
    // answer for the other.
    let contacts = vec!["mailto:admin@example.com".to_owned()];
    assert_eq!(
        cache
            .load_account(&contacts, LETSENCRYPT_PRODUCTION)
            .await?,
        None
    );
    cache
        .store_account(&contacts, LETSENCRYPT_PRODUCTION, b"account-key")
        .await?;
    assert_eq!(
        cache
            .load_account(&contacts, LETSENCRYPT_PRODUCTION)
            .await?,
        Some(b"account-key".to_vec())
    );
    assert_eq!(
        cache.load_cert(&one, LETSENCRYPT_PRODUCTION).await?,
        Some(b"prod-one".to_vec())
    );
    Ok(())
}
