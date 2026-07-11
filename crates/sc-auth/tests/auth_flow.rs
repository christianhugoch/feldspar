//! Phase 5 acceptance walk (the "Integration tests" TODO item): one end-to-end
//! story against real Postgres covering create-first-user, login success and
//! failure, logout, and session expiry.
//!
//! Per-feature integration tests live alongside this one (`first_user`, `login`,
//! `role_gate`); this file is the consolidated Phase 5 definition-of-done for the
//! users-and-auth story.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::Duration;
use sc_auth::{
    COL_EMAIL, ROLE_ADMIN, SessionStore, any_user_exists, authenticate, bootstrap,
    create_first_user,
};
use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_query::Value;
use sc_test_harness::TestDb;

#[tokio::test]
async fn phase5_auth_acceptance_walk() -> sc_error::Result<()> {
    let db = TestDb::new().await?;

    // The multi-tenant v1 test template carries `users` tables in several
    // schemas; drop them all so bootstrap creates the clean MVP table. No-op in
    // clean CI.
    db.client()
        .await?
        .batch_execute(
            "DO $$ DECLARE r record; BEGIN \
               FOR r IN SELECT table_schema FROM information_schema.tables \
               WHERE table_name = 'users' AND table_type = 'BASE TABLE' LOOP \
                 EXECUTE format('DROP TABLE IF EXISTS %I.users CASCADE', r.table_schema); \
               END LOOP; END $$",
        )
        .await
        .map_err(|e| sc_error::Error::database(e.to_string()))?;

    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let catalog = Catalog::init(driver.clone() as Arc<dyn DatabaseDriver>).await?;
    bootstrap(&catalog).await?;

    // --- create-first-user ----------------------------------------------------
    assert!(
        !any_user_exists(&catalog).await?,
        "no user before bootstrap"
    );
    let admin = create_first_user(&catalog, "admin@example.com", "s3cret-pw").await?;
    assert!(admin.is_admin());
    assert!(
        any_user_exists(&catalog).await?,
        "user exists after creation"
    );
    // The flow only creates the *first* user.
    assert!(
        create_first_user(&catalog, "second@example.com", "pw")
            .await
            .is_err(),
        "second create-first-user is refused"
    );

    // --- login failure --------------------------------------------------------
    assert!(
        authenticate(&catalog, "admin@example.com", "wrong")
            .await?
            .is_none(),
        "wrong password fails"
    );
    assert!(
        authenticate(&catalog, "nobody@example.com", "s3cret-pw")
            .await?
            .is_none(),
        "unknown email fails"
    );

    // --- login success --------------------------------------------------------
    let user = authenticate(&catalog, "admin@example.com", "s3cret-pw")
        .await?
        .expect("valid credentials authenticate");
    assert_eq!(user.id, admin.id);
    assert_eq!(user.role, ROLE_ADMIN);
    assert_eq!(
        user.get(COL_EMAIL),
        Some(&Value::Text("admin@example.com".into()))
    );

    // --- logout ---------------------------------------------------------------
    let sessions = SessionStore::default();
    let token = sessions.login(user.clone())?;
    assert_eq!(
        sessions.user_for(&token)?,
        Some(user.clone()),
        "session resolves before logout"
    );
    assert!(sessions.logout(&token)?, "logout removes the session");
    assert_eq!(
        sessions.user_for(&token)?,
        None,
        "session gone after logout"
    );

    // --- session expiry -------------------------------------------------------
    // A short-lived session resolves while fresh, then lapses after its TTL.
    let short = SessionStore::with_ttl(Duration::milliseconds(150));
    let expiring = short.login(user.clone())?;
    assert_eq!(
        short.user_for(&expiring)?,
        Some(user),
        "session valid within its TTL"
    );
    tokio::time::sleep(StdDuration::from_millis(300)).await;
    assert_eq!(
        short.user_for(&expiring)?,
        None,
        "session expired after its TTL"
    );

    Ok(())
}
