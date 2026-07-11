//! Phase 5 integration test: login → session → logout against real Postgres.
//!
//! Bootstraps the `users` table, creates the first (admin) user, then exercises
//! the credential + session flow the server will wire to cookies in Phase 6:
//! wrong credentials are rejected, a correct login mints a session token that
//! resolves back to the user, and logout invalidates it.

use std::sync::Arc;

use sc_auth::{COL_EMAIL, ROLE_ADMIN, SessionStore, authenticate, bootstrap, create_first_user};
use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_query::Value;
use sc_test_harness::TestDb;

#[tokio::test]
async fn login_session_logout_round_trip() -> sc_error::Result<()> {
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
    let created = create_first_user(&catalog, "admin@example.com", "s3cret-pw").await?;

    // Wrong password and unknown email both fail without erroring.
    assert!(
        authenticate(&catalog, "admin@example.com", "wrong")
            .await?
            .is_none()
    );
    assert!(
        authenticate(&catalog, "nobody@example.com", "s3cret-pw")
            .await?
            .is_none()
    );

    // Correct credentials authenticate to the same user.
    let user = authenticate(&catalog, "admin@example.com", "s3cret-pw")
        .await?
        .expect("valid credentials authenticate");
    assert_eq!(user.id, created.id);
    assert_eq!(user.role, ROLE_ADMIN);
    assert_eq!(
        user.get(COL_EMAIL),
        Some(&Value::Text("admin@example.com".into()))
    );

    // A login mints a session token that resolves back to the user; logout ends it.
    let sessions = SessionStore::default();
    let token = sessions.login(user.clone())?;
    assert_eq!(sessions.user_for(&token)?, Some(user));

    assert!(sessions.logout(&token)?);
    assert_eq!(sessions.user_for(&token)?, None);

    Ok(())
}
