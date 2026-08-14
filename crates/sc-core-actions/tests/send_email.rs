//! The `send_email` action against a real database, a real V8 and a recording
//! transport (TODO "Email" Phase 4).
//!
//! The recording mailer is what makes these assertions worth making: the test
//! runs the trigger and then reads *the message that would have gone out* —
//! which addresses, which subject, which bodies — rather than asserting that a
//! call returned `Ok`. Everything a template can get wrong is in that message.
//!
//! What is asserted: a trigger on `orders` renders its recipient from a Ⱶ-join
//! path (so the address is prefetched, not `undefined`), its subject and its
//! bodies from the row, with the HTML body escaped and the text body not; a null
//! column renders as nothing; an MJML body is compiled into the table markup a
//! mail client lays out, *after* interpolation; the ways the configuration can be
//! wrong are refused while the admin is looking at the form; and an installation
//! with no SMTP settings fails with an error pointing at Settings → Email.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use sc_action::{ActionContext, Event, EventKind, Trigger, validate_trigger};
use sc_catalog::{Catalog, DataField, DataFieldKind, FieldId, TableId};
use sc_core_actions::builtin_actions;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_email::{Email, Mailer, RecordingMailer, SettingsMailer, parse_mailbox};
use sc_error::Result;
use sc_expr::{DenoEvaluator, JsEvaluator};
use sc_test_harness::TestDb;
use sc_types::{Attrs, BasicType, TypeRef};
use serde_json::{Value as Json, json};

fn int() -> TypeRef {
    TypeRef::Basic(BasicType::Int)
}

fn text() -> TypeRef {
    TypeRef::Basic(BasicType::Text)
}

/// `customers(id, email, name)` and `orders(id, total, note, customer → customers.id)`,
/// with one customer for the recipient to be joined from.
async fn shop(db: &TestDb) -> Result<Arc<Catalog>> {
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let cat = Catalog::init(driver as Arc<dyn DatabaseDriver>).await?;
    cat.create_table(
        "customers",
        &[
            DataField::plain("id", int()).required().primary_key(),
            DataField::plain("email", text()),
            DataField::plain("name", text()),
        ],
    )
    .await?;
    let mut customer = DataField::plain("customer", int());
    customer.kind = DataFieldKind::Key {
        target_table: TableId("customers".to_owned()),
        target_field: FieldId("id".to_owned()),
        summary_field: None,
    };
    cat.create_table(
        "orders",
        &[
            DataField::plain("id", int()).required().primary_key(),
            DataField::plain("total", int()),
            DataField::plain("note", text()),
            customer,
        ],
    )
    .await?;
    // The settings table `SettingsMailer` reads through, which a server
    // bootstraps at boot.
    sc_config::bootstrap_config(&cat).await?;
    db.pool()
        .get()
        .await
        .expect("client")
        .batch_execute("INSERT INTO customers VALUES (7, 'ada@example.com', 'Lovelace, Ada')")
        .await
        .expect("insert");
    Ok(Arc::new(cat))
}

/// The event a trigger on `orders` carries: the order, and the admin who caused
/// it. An `insert` here because that is the kind a table trigger has today; a
/// button running one against a row is Phase 5, and the action does not care —
/// what it reads is the event's `row`.
fn order_event() -> Event {
    Event::new(EventKind::Insert)
        .on("orders")
        .row(json!({ "id": 42, "total": 250, "note": "Tea & Coffee", "customer": 7 }))
        .caller(1, Some(json!({ "email": "admin@example.com" })))
}

fn config(entries: &[(&str, Json)]) -> Attrs {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

/// Run `send_email` through the registry — the path a firing trigger takes —
/// against `mailer`.
async fn run(
    catalog: &Catalog,
    event: &Event,
    config: &Attrs,
    mailer: &Arc<dyn Mailer>,
) -> Result<Json> {
    let engine: Arc<dyn JsEvaluator> = Arc::new(DenoEvaluator::new());
    let registry = builtin_actions()?;
    let action = registry.require("send_email")?.clone();
    let mut ctx = ActionContext::new(catalog, event, config, "receipt")
        .with_evaluator(&engine)
        .with_mailer(mailer);
    action.run(&mut ctx).await
}

/// A recorder that claims the installation's from-address, so the message a test
/// reads is the message the SMTP transport would have built.
fn recording_mailer() -> (Arc<RecordingMailer>, Arc<dyn Mailer>) {
    let recorder = Arc::new(RecordingMailer::sending_as(
        parse_mailbox("Saltcorn <saltcorn@example.com>").unwrap(),
    ));
    let mailer: Arc<dyn Mailer> = Arc::clone(&recorder) as Arc<dyn Mailer>;
    (recorder, mailer)
}

#[tokio::test]
async fn a_trigger_sends_the_rows_own_values_to_the_address_it_joins_to() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = shop(&db).await?;
    let (recorder, mailer) = recording_mailer();

    let cfg = config(&[
        // Nothing in the event holds the address: this only renders if the
        // Ⱶ-path was prefetched.
        ("to", json!("{{ customerⱵemail }}")),
        ("cc", json!("audit@example.com")),
        ("subject", json!("Receipt for order {{ id }}")),
        (
            "html",
            json!("<p>Hello {{ customerⱵname }}, you owe {{ total * 2 }}. {{ note }}</p>"),
        ),
        ("text", json!("Order {{ id }}: {{ note }}")),
    ]);
    let result = run(&catalog, &order_event(), &cfg, &mailer).await?;

    let sent: Email = recorder.only()?;
    assert_eq!(sent.to.len(), 1);
    assert_eq!(sent.to[0].address, "ada@example.com");
    assert_eq!(sent.cc[0].address, "audit@example.com");
    // No `from` of its own, so the message is sent as the transport's identity.
    assert_eq!(sent.from.address, "saltcorn@example.com");
    // The subject is text: `&` stays `&`, and the id is the row's own.
    assert_eq!(sent.subject, "Receipt for order 42");
    // The HTML body escapes what it interpolates, and an expression is an
    // expression — the same language a calculated field is written in.
    assert_eq!(
        sent.html.as_deref(),
        Some("<p>Hello Lovelace, Ada, you owe 500. Tea &amp; Coffee</p>")
    );
    // The text body does not escape: a plain-text receipt full of `&amp;` is
    // exactly what decision 3 exists to prevent.
    assert_eq!(sent.text.as_deref(), Some("Order 42: Tea & Coffee"));

    // The action's result is what was sent, so a button can say so.
    assert_eq!(
        result,
        json!({
            "to": ["ada@example.com"],
            "cc": ["audit@example.com"],
            "bcc": [],
            "subject": "Receipt for order 42",
        })
    );
    Ok(())
}

#[tokio::test]
async fn a_null_column_renders_as_nothing_and_a_from_override_is_parsed() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = shop(&db).await?;
    let (recorder, mailer) = recording_mailer();

    let event = Event::new(EventKind::Insert)
        .on("orders")
        .row(json!({ "id": 43, "total": 10, "note": null, "customer": 7 }));
    let cfg = config(&[
        ("to", json!("{{ customerⱵemail }}")),
        ("from", json!("Orders <orders@example.com>")),
        ("subject", json!("Order {{ id }}")),
        ("text", json!("Note: [{{ note }}]")),
    ]);
    run(&catalog, &event, &cfg, &mailer).await?;

    let sent = recorder.only()?;
    // A null column is data, not a mistake (decision 4).
    assert_eq!(sent.text.as_deref(), Some("Note: []"));
    // The configured `from` wins over the transport's own address.
    assert_eq!(sent.from.address, "orders@example.com");
    assert_eq!(sent.from.name.as_deref(), Some("Orders"));
    Ok(())
}

/// MJML: the body is written as the markup an email designer writes, and what
/// leaves is the markup a mail client lays out.
#[tokio::test]
async fn an_mjml_body_is_interpolated_and_then_compiled() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = shop(&db).await?;
    let (recorder, mailer) = recording_mailer();

    let cfg = config(&[
        ("to", json!("{{ customerⱵemail }}")),
        ("subject", json!("Receipt for order {{ id }}")),
        (
            "html",
            json!(
                "<mjml><mj-body><mj-section><mj-column>\
                 <mj-text>Order {{ id }} — {{ note }}</mj-text>\
                 <mj-button href=\"https://example.com/orders/{{ id }}\">View</mj-button>\
                 </mj-column></mj-section></mj-body></mjml>"
            ),
        ),
        ("mjml", json!(true)),
    ]);
    run(&catalog, &order_event(), &cfg, &mailer).await?;

    let html = recorder.only()?.html.expect("an HTML body");
    // What was sent is compiled HTML, not the MJML source.
    assert!(!html.contains("<mj-section"), "{html}");
    assert!(html.contains("<table"), "{html}");
    // Interpolation happened **first**, so the tokens are gone and their values
    // are inside the generated markup — escaped by the HTML rule on the way in.
    assert!(html.contains("Order 42 — Tea &amp; Coffee"), "{html}");
    assert!(html.contains("https://example.com/orders/42"), "{html}");

    // Without the flag the same source is sent as-is: MJML is a property of the
    // body the admin declared, never a guess from what it looks like.
    let plain = config(&[
        ("to", json!("ada@example.com")),
        ("subject", json!("x")),
        ("html", json!("<mjml><mj-body></mj-body></mjml>")),
    ]);
    let (as_written, mailer) = recording_mailer();
    run(&catalog, &order_event(), &plain, &mailer).await?;
    assert_eq!(
        as_written.only()?.html.as_deref(),
        Some("<mjml><mj-body></mj-body></mjml>")
    );
    Ok(())
}

#[tokio::test]
async fn the_ways_a_configuration_can_be_wrong_are_refused_on_save() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = shop(&db).await?;
    let registry = builtin_actions()?;

    /// One refusal: the configuration, and the words the message must contain.
    type Case<'a> = (&'a [(&'a str, Json)], &'a [&'a str]);

    let cases: &[Case<'_>] = &[
        // A token naming a field the table does not have — the failure that
        // would otherwise be an empty recipient at 3am.
        (
            &[
                ("to", json!("{{ customerⱵemial }}")),
                ("subject", json!("hi")),
                ("text", json!("hi")),
            ],
            &["to", "emial"],
        ),
        (
            &[
                ("to", json!("ada@example.com")),
                ("subject", json!("Order {{ ident }}")),
                ("text", json!("hi")),
            ],
            &["subject", "ident"],
        ),
        // No body at all, and no recipient at all: both named by the settings
        // that would fix them.
        (
            &[("to", json!("ada@example.com")), ("subject", json!("hi"))],
            &["body", "html", "text"],
        ),
        (
            &[("subject", json!("hi")), ("text", json!("hi"))],
            &["recipient", "to", "cc", "bcc"],
        ),
        // A static address that is not one. (One with a token in it cannot be
        // checked until a row is in hand — that failure names the address it got.)
        (
            &[
                ("to", json!("ada@example.com, not-an-address")),
                ("subject", json!("hi")),
                ("text", json!("hi")),
            ],
            &["to", "not-an-address"],
        ),
        (
            &[
                ("to", json!("ada@example.com")),
                ("from", json!("orders.example.com")),
                ("subject", json!("hi")),
                ("text", json!("hi")),
            ],
            &["from", "orders.example.com"],
        ),
        // MJML claimed for a body that is not there, and MJML that does not
        // compile: both are save-time errors, not Tuesday-night ones.
        (
            &[
                ("to", json!("ada@example.com")),
                ("subject", json!("hi")),
                ("text", json!("hi")),
                ("mjml", json!(true)),
            ],
            &["mjml", "html"],
        ),
        (
            &[
                ("to", json!("ada@example.com")),
                ("subject", json!("hi")),
                ("html", json!("<mjml><mj-body><mj-section></mjml>")),
                ("mjml", json!(true)),
            ],
            &["html", "MJML"],
        ),
    ];

    for (entries, expected) in cases {
        let trigger = Trigger::new("receipt", EventKind::Insert, "send_email")
            .on("orders")
            .configuration(config(entries));
        let err = validate_trigger(&catalog, &registry, &trigger)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("receipt"), "the trigger is named: {msg}");
        for fragment in *expected {
            assert!(msg.contains(fragment), "expected `{fragment}` in: {msg}");
        }
        assert_eq!(err.kind(), sc_error::ErrorKind::Application, "{msg}");
    }

    // The valid configuration the refusals are measured against — including an
    // MJML body, which is compiled on save because it has to compile at send.
    let ok = Trigger::new("receipt", EventKind::Insert, "send_email")
        .on("orders")
        .configuration(config(&[
            ("to", json!("{{ customerⱵemail }}")),
            ("bcc", json!("audit@example.com")),
            ("subject", json!("Receipt for order {{ id }}")),
            (
                "html",
                json!(
                    "<mjml><mj-body><mj-section><mj-column>\
                     <mj-text>{{ total }}</mj-text></mj-column></mj-section></mj-body></mjml>"
                ),
            ),
            ("mjml", json!(true)),
        ]));
    validate_trigger(&catalog, &registry, &ok).await?;
    Ok(())
}

/// An installation that never configured SMTP: the failure names the screen that
/// fixes it, and it happens before anything is rendered or connected to.
#[tokio::test]
async fn an_installation_with_no_email_settings_says_where_to_set_them() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = shop(&db).await?;
    let mailer: Arc<dyn Mailer> = Arc::new(SettingsMailer::new(Arc::clone(&catalog)));

    let cfg = config(&[
        ("to", json!("{{ customerⱵemail }}")),
        ("subject", json!("Receipt for order {{ id }}")),
        ("text", json!("Thank you")),
    ]);
    let msg = run(&catalog, &order_event(), &cfg, &mailer)
        .await
        .unwrap_err()
        .to_string();
    assert!(msg.contains("Settings"), "{msg}");
    assert!(msg.contains("Email"), "{msg}");
    assert!(msg.contains("receipt"), "the trigger is named: {msg}");

    // And a context with no transport at all — a unit test, the client
    // generator — says *that*, rather than quietly doing nothing.
    let engine: Arc<dyn JsEvaluator> = Arc::new(DenoEvaluator::new());
    let registry = builtin_actions()?;
    let action = registry.require("send_email")?.clone();
    let event = order_event();
    let mut ctx = ActionContext::new(&catalog, &event, &cfg, "receipt").with_evaluator(&engine);
    let msg = action.run(&mut ctx).await.unwrap_err().to_string();
    assert!(
        msg.contains("receipt") && msg.contains("transport"),
        "{msg}"
    );
    Ok(())
}
