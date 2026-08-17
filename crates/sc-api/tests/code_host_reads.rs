//! A code body's reads, end to end (the milestone "Tables in code", phase 2).
//!
//! The guest is not here — `sc-expr`'s own tests prove that a `db` chain becomes
//! a plan, and this crate's unit tests prove that a plan becomes a statement.
//! What is proved here is the half neither can: that the statement **answers**,
//! against a real database, in the shape a code body reads back.
//!
//! So each test sends the JSON a terminal would send and asserts the rows: a
//! joined column under its Ⱶ-path, a child aggregation under its alias, the two
//! spellings of a filter agreeing, an aggregate's one value, and the row cap
//! refusing rather than truncating — which is the failure this whole seam is
//! bounded against, since a body handed half a table's rows would compute a
//! wrong answer out of a right-looking one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use sc_api::code_host::{HostLimits, TableHost};
use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::Result;
use sc_expr::CodeHost;
use sc_test_harness::TestDb;
use serde_json::{Value as Json, json};

/// A small library: books with an author key, and reviews pointing back.
async fn setup(db: &TestDb) -> Result<Arc<Catalog>> {
    db.client()
        .await?
        .batch_execute(
            "CREATE TABLE authors (id bigint primary key, name text, country text);
             CREATE TABLE books (id bigint primary key, title text, pages bigint,
                 published date, author bigint references authors(id));
             CREATE TABLE reviews (id bigint primary key, book bigint references books(id),
                 stars bigint);
             INSERT INTO authors VALUES (1, 'Woolf', 'GB'), (2, 'Herbert', 'US');
             INSERT INTO books VALUES
                 (1, 'Orlando', 333, '1928-10-11', 1),
                 (2, 'The Waves', 297, '1931-10-08', 1),
                 (3, 'Dune', 412, '1965-08-01', 2);
             INSERT INTO reviews VALUES (1, 1, 5), (2, 1, 4), (3, 3, 3);",
        )
        .await
        .map_err(|e| sc_error::Error::database(e.to_string()))?;
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    Ok(Arc::new(
        Catalog::init(driver as Arc<dyn DatabaseDriver>).await?,
    ))
}

/// One plan, answered — what `__scDbCall(plan)` gets back.
async fn ask(host: &TableHost, plan: Json) -> Result<Json> {
    host.call(plan).await
}

#[tokio::test]
async fn a_read_joins_aggregates_orders_and_bounds_in_one_statement() -> Result<()> {
    let db = TestDb::new().await?;
    let host = TableHost::new(setup(&db).await?);

    // `db.books.where({ pages: { gt: 300 } })
    //     .select("id", "title", "authorⱵname", { reviews: "reviewsↃbook.length" })
    //     .orderBy("published", "desc").limit(2).rows()`
    let rows = ask(
        &host,
        json!({
            "op": "select",
            "table": "books",
            "where": { "pages": { "gt": 300 } },
            "select": [
                "id", "title", "authorⱵname",
                { "alias": "reviews", "formula": "reviewsↃbook.length" },
            ],
            "order": [ { "field": "published", "dir": "desc" } ],
            "limit": 2,
        }),
    )
    .await?;
    assert_eq!(
        rows,
        json!([
            { "id": 3, "title": "Dune", "authorⱵname": "Herbert", "reviews": 1 },
            { "id": 1, "title": "Orlando", "authorⱵname": "Woolf", "reviews": 2 },
        ]),
        "the join and the child count ride in the same read as the row"
    );
    Ok(())
}

#[tokio::test]
async fn the_two_spellings_of_a_filter_select_the_same_rows() -> Result<()> {
    let db = TestDb::new().await?;
    let host = TableHost::new(setup(&db).await?);

    let titles = |rows: &Json| -> Vec<String> {
        rows.as_array()
            .unwrap()
            .iter()
            .map(|r| r["title"].as_str().unwrap().to_owned())
            .collect()
    };

    // The object DSL every other surface speaks…
    let object = ask(
        &host,
        json!({
            "op": "select", "table": "books",
            "where": { "and": [ { "pages": { "gte": 300 } }, { "authorⱵcountry": "GB" } ] },
            "select": ["title"],
        }),
    )
    .await?;
    // …and the formula string, which is what `update_rows` already takes.
    let formula = ask(
        &host,
        json!({
            "op": "select", "table": "books",
            "where": { "formula": "pages >= 300 && authorⱵcountry === \"GB\"" },
            "select": ["title"],
        }),
    )
    .await?;
    assert_eq!(titles(&object), vec!["Orlando".to_owned()]);
    assert_eq!(titles(&object), titles(&formula));

    // A date filter binds a date, because the literal is coerced against the
    // column rather than against JSON.
    let before = ask(
        &host,
        json!({
            "op": "select", "table": "books",
            "where": { "published": { "lt": "1940-01-01" } },
            "select": ["title"], "order": [ { "field": "title" } ],
        }),
    )
    .await?;
    assert_eq!(titles(&before), vec!["Orlando", "The Waves"]);

    // A whole row is the REST wire shape: every column of the table, a Date as
    // its ISO string.
    let whole = ask(
        &host,
        json!({ "op": "select", "table": "books", "where": { "id": 1 } }),
    )
    .await?;
    assert_eq!(
        whole,
        json!([{
            "id": 1, "title": "Orlando", "pages": 333,
            "published": "1928-10-11", "author": 1,
        }])
    );
    Ok(())
}

#[tokio::test]
async fn the_scalar_terminals_answer_one_value_each() -> Result<()> {
    let db = TestDb::new().await?;
    let host = TableHost::new(setup(&db).await?);

    // `.count()`, `.sum(f)` and `.max(formula)` are one plan each, and the alias
    // the prelude uses for a scalar terminal is `value`.
    let count = ask(
        &host,
        json!({
            "op": "aggregate", "table": "books",
            "where": { "author": 1 },
            "aggregate": [ { "alias": "value", "fn": "count", "arg": null } ],
        }),
    )
    .await?;
    assert_eq!(count, json!({ "value": 2 }));

    let sum = ask(
        &host,
        json!({
            "op": "aggregate", "table": "books",
            "aggregate": [ { "alias": "value", "fn": "sum", "arg": "pages" } ],
        }),
    )
    .await?;
    // A string, and deliberately: Postgres sums a `bigint` as `numeric`, and a
    // Decimal crosses this seam exactly as it crosses the REST wire — as text,
    // because a JSON number is a float and an exact total that quietly stopped
    // being exact is the worst kind of wrong.
    assert_eq!(sum, json!({ "value": "1042" }));

    // The argument may be a formula, resolved by the same translator a
    // projection's is.
    let max = ask(
        &host,
        json!({
            "op": "aggregate", "table": "books",
            "aggregate": [ { "alias": "value", "fn": "max", "arg": "pages * 2" } ],
        }),
    )
    .await?;
    assert_eq!(max, json!({ "value": 824 }));

    // A `sum` over no rows is 0 and an `avg` over no rows is null — the empty
    // relation semantics of the shared builder, not this module's invention.
    let empty = ask(
        &host,
        json!({
            "op": "aggregate", "table": "books",
            "where": { "author": 99 },
            "aggregate": [
                { "alias": "total", "fn": "sum", "arg": "pages" },
                { "alias": "mean", "fn": "avg", "arg": "pages" },
            ],
        }),
    )
    .await?;
    assert_eq!(empty, json!({ "total": "0", "mean": Json::Null }));
    Ok(())
}

#[tokio::test]
async fn the_row_cap_refuses_rather_than_truncating() -> Result<()> {
    let db = TestDb::new().await?;
    let host = TableHost::new(setup(&db).await?).with_limits(HostLimits {
        max_rows: 2,
        max_calls: 10,
    });

    // Two rows fit under a cap of two…
    let fits = ask(
        &host,
        json!({ "op": "select", "table": "books", "where": { "author": 1 } }),
    )
    .await?;
    assert_eq!(fits.as_array().unwrap().len(), 2);

    // …and three do not. The error says what to do about it, and no rows are
    // answered at all — a body that got two of three would be wrong quietly.
    let err = ask(&host, json!({ "op": "select", "table": "books" }))
        .await
        .expect_err("the cap refuses");
    let message = err.to_string();
    assert!(
        message.contains("more than the 2 rows") && message.contains(".limit()"),
        "{message}"
    );

    // A bound above the cap is refused where the author can see it, before any
    // statement runs.
    let err = ask(
        &host,
        json!({ "op": "select", "table": "books", "limit": 500 }),
    )
    .await
    .expect_err("the cap refuses");
    assert!(err.to_string().contains("2 rows at once"), "{err}");
    Ok(())
}

#[tokio::test]
async fn a_get_by_key_is_one_row_or_none_and_an_unknown_name_is_refused() -> Result<()> {
    let db = TestDb::new().await?;
    let host = TableHost::new(setup(&db).await?);

    let found = ask(
        &host,
        json!({ "op": "select", "table": "books", "pk": 2, "limit": 1, "select": ["title"] }),
    )
    .await?;
    assert_eq!(found, json!([{ "title": "The Waves" }]));

    let missing = ask(
        &host,
        json!({ "op": "select", "table": "books", "pk": 99, "limit": 1 }),
    )
    .await?;
    assert_eq!(missing, json!([]), "`.get()` answers null on no rows");

    // Every name in a plan is the catalog's to confirm, and a wrong one is
    // refused naming it rather than reaching the database.
    for plan in [
        json!({ "op": "select", "table": "nope" }),
        json!({ "op": "select", "table": "books", "select": ["nope"] }),
        json!({ "op": "select", "table": "books", "where": { "nope": 1 } }),
    ] {
        let err = ask(&host, plan.clone()).await.expect_err("refused");
        assert!(err.to_string().contains("nope"), "{plan}: {err}");
    }
    Ok(())
}
