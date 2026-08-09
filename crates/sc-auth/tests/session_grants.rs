//! Integration test: one-time session grants (§7.2) against a real database.
//!
//! A grant is the only thing in the system that turns "I hold the database" into
//! "I hold a session", so the properties worth asserting are the ones that keep
//! it narrow: it works once, it stops working when it lapses, the secret is not
//! in the row, and the user it names is the user that comes back. The other half
//! — that the *server* redeems it and mints a real cookie — is asserted end to
//! end in `sc-cli`'s `auth_token` test, against a server on a real port.
//!
//! The lookups the command line resolves `--email`, `--admin` and `--role` with
//! are here too, since they are the other half of asking for a session without a
//! password.

use std::sync::Arc;

use chrono::{Duration, Utc};
use sc_auth::{
    COL_EMAIL, COL_GRANT_EXPIRES_AT, COL_GRANT_SECRET_HASH, GRANTS_TABLE, ROLE_ADMIN, Role,
    bootstrap, create_session_grant, create_user, first_user_with_role, load_user,
    load_user_by_email, redeem_session_grant, save_role,
};
use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::Result;
use sc_query::{Assignment, Expr, Select, Source, Statement, Update, Value};
use sc_test_harness::TestDb;

const ADMIN: &str = "admin@example.com";
const EDITOR: &str = "editor@example.com";
const OTHER_EDITOR: &str = "another-editor@example.com";
const PASSWORD: &str = "correct horse battery staple";
const EDITOR_ROLE: u8 = 40;

/// A catalog with the platform tables bootstrapped, over a database cleared of
/// the v1 template's stray `users` tables.
async fn catalog(db: &TestDb) -> Result<Catalog> {
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
    let catalog = Catalog::init(driver as Arc<dyn DatabaseDriver>).await?;
    bootstrap(&catalog).await?;
    save_role(&catalog, &Role::new(EDITOR_ROLE, "Editor")).await?;
    create_user(&catalog, ADMIN, PASSWORD, ROLE_ADMIN).await?;
    // Two editors, deliberately inserted in the order that is *not* alphabetical:
    // "the first user with this role" has to be an answer, not an accident of
    // insertion order.
    create_user(&catalog, EDITOR, PASSWORD, EDITOR_ROLE).await?;
    create_user(&catalog, OTHER_EDITOR, PASSWORD, EDITOR_ROLE).await?;
    Ok(catalog)
}

/// Every row of the grants table.
async fn grant_rows(cat: &Catalog) -> Result<Vec<sc_db::Row>> {
    cat.primary()
        .query(&Statement::from(Select::from(Source::table(GRANTS_TABLE))))
        .await?
        .try_collect()
        .await
}

#[tokio::test]
async fn a_grant_is_redeemed_once_and_never_again() -> Result<()> {
    let db = TestDb::new().await?;
    let cat = catalog(&db).await?;
    let admin = load_user_by_email(&cat, ADMIN).await?.expect("the admin");

    let grant = create_session_grant(&cat, admin.id).await?;
    assert_eq!(grant_rows(&cat).await?.len(), 1);

    let redeemed = redeem_session_grant(&cat, &grant)
        .await?
        .expect("a fresh grant names its user");
    assert_eq!(redeemed.id, admin.id);
    assert_eq!(redeemed.role, ROLE_ADMIN);
    // The user comes back whole — the email a caller prints is on it.
    assert_eq!(
        redeemed.get(COL_EMAIL),
        Some(&Value::Text(ADMIN.to_owned()))
    );

    // Redemption consumed the row, so the same string is now worth nothing.
    assert!(grant_rows(&cat).await?.is_empty());
    assert!(redeem_session_grant(&cat, &grant).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn the_secret_is_not_in_the_row_and_a_wrong_one_is_refused() -> Result<()> {
    let db = TestDb::new().await?;
    let cat = catalog(&db).await?;
    let admin = load_user_by_email(&cat, ADMIN).await?.expect("the admin");

    let grant = create_session_grant(&cat, admin.id).await?;
    let (id, secret) = grant.split_once('.').expect("id.secret");

    // What is stored is an argon2id hash, so a database dump is not a live
    // credential — the secret is nowhere in the row.
    let rows = grant_rows(&cat).await?;
    let stored = match rows[0].get(COL_GRANT_SECRET_HASH) {
        Some(Value::Text(hash)) => hash.clone(),
        other => panic!("the hash should be text, got {other:?}"),
    };
    assert!(stored.starts_with("$argon2id$"), "{stored}");
    assert!(!stored.contains(secret), "the secret must not be stored");

    // The right id with the wrong secret is refused — and, because redemption
    // consumes the row first, it also burns the grant: one guess, not many.
    let wrong = format!("{id}.{}", "0".repeat(secret.len()));
    assert!(redeem_session_grant(&cat, &wrong).await?.is_none());
    assert!(grant_rows(&cat).await?.is_empty());
    assert!(redeem_session_grant(&cat, &grant).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn a_lapsed_grant_is_no_grant_and_is_swept_by_the_next_one() -> Result<()> {
    let db = TestDb::new().await?;
    let cat = catalog(&db).await?;
    let admin = load_user_by_email(&cat, ADMIN).await?.expect("the admin");

    // Age it rather than waiting two minutes for it: the row is the clock.
    let grant = create_session_grant(&cat, admin.id).await?;
    let expire = Update::new(
        GRANTS_TABLE,
        vec![Assignment::new(
            COL_GRANT_EXPIRES_AT,
            Expr::Lit(Value::Timestamp(Utc::now() - Duration::seconds(1))),
        )],
    );
    cat.primary()
        .query(&Statement::from(expire))
        .await?
        .try_collect()
        .await?;
    assert!(redeem_session_grant(&cat, &grant).await?.is_none());

    // Housekeeping: a grant nobody redeemed is cleaned up by the next mint,
    // which is the only writer this table has.
    let expired = create_session_grant(&cat, admin.id).await?;
    let expire_all = Update::new(
        GRANTS_TABLE,
        vec![Assignment::new(
            COL_GRANT_EXPIRES_AT,
            Expr::Lit(Value::Timestamp(Utc::now() - Duration::seconds(1))),
        )],
    );
    cat.primary()
        .query(&Statement::from(expire_all))
        .await?
        .try_collect()
        .await?;
    let fresh = create_session_grant(&cat, admin.id).await?;
    let rows = grant_rows(&cat).await?;
    assert_eq!(rows.len(), 1, "the lapsed row should have been swept");
    assert!(redeem_session_grant(&cat, &expired).await?.is_none());
    assert!(redeem_session_grant(&cat, &fresh).await?.is_some());
    Ok(())
}

#[tokio::test]
async fn a_malformed_grant_is_refused_without_touching_anything() -> Result<()> {
    let db = TestDb::new().await?;
    let cat = catalog(&db).await?;
    let admin = load_user_by_email(&cat, ADMIN).await?.expect("the admin");
    let live = create_session_grant(&cat, admin.id).await?;

    for nonsense in [
        "",
        "no-separator",
        "not-a-uuid.secret",
        "00000000-0000-4000-8000-000000000000.wrong",
    ] {
        assert!(
            redeem_session_grant(&cat, nonsense).await?.is_none(),
            "{nonsense} should mint nothing"
        );
    }
    // None of that consumed the outstanding grant.
    assert!(redeem_session_grant(&cat, &live).await?.is_some());
    Ok(())
}

#[tokio::test]
async fn the_lookups_behind_email_admin_and_role() -> Result<()> {
    let db = TestDb::new().await?;
    let cat = catalog(&db).await?;

    let admin = load_user_by_email(&cat, ADMIN).await?.expect("by email");
    assert_eq!(admin.role, ROLE_ADMIN);
    // The email is matched exactly, and whitespace around it is a shell's, not
    // part of the identifier.
    assert!(load_user_by_email(&cat, "  ").await?.is_none());
    assert!(
        load_user_by_email(&cat, "nobody@example.com")
            .await?
            .is_none()
    );

    // `--admin` is the first user holding role 1.
    assert_eq!(
        first_user_with_role(&cat, ROLE_ADMIN).await?.map(|u| u.id),
        Some(admin.id)
    );

    // `--role Editor`: two users hold it, and the answer is the lowest email —
    // stable, so a screenshot script does not sign in as somebody else tomorrow.
    let editor = first_user_with_role(&cat, EDITOR_ROLE)
        .await?
        .expect("an editor");
    assert_eq!(
        editor.get(COL_EMAIL),
        Some(&Value::Text(OTHER_EDITOR.to_owned())),
        "the first editor by email, not by insertion order"
    );

    // A role nobody holds resolves to nobody, which is what the command turns
    // into "no user has the role `X`".
    save_role(&cat, &Role::new(60, "Nobody")).await?;
    assert!(first_user_with_role(&cat, 60).await?.is_none());

    // And a user is loadable by the id a grant carries.
    assert_eq!(
        load_user(&cat, admin.id).await?.map(|u| u.id),
        Some(admin.id)
    );
    Ok(())
}
