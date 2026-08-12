//! The settings API, driven through the assembled router as an admin's browser
//! drives it (design §9, §13.5).
//!
//! `_sc_config`'s own tests cover the table. What can only be pinned down here,
//! at the HTTP boundary, is what the *screen* is handed and what a save does:
//!
//! - the response carries the **declarations** as well as the values, because a
//!   settings screen that knew what a setting meant would need changing every
//!   time one was added;
//! - a **secret never crosses the wire**: the private key reaches the browser as
//!   the sentinel, and handing that sentinel back leaves the stored key alone —
//!   the §11.1 rule an API key lives under, applied to a certificate's key;
//! - a save is refused **whole**, including for what the settings mean together:
//!   `custom` mode with no certificate, or a key that does not match its chain,
//!   is a message in front of the admin rather than a server that will not bind
//!   at the next restart;
//! - and settings are admin-only, like every other configuration endpoint.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sc_auth::SessionStore;
use sc_catalog::Catalog;
use sc_config::{
    ACME_CONTACT_EMAIL, HTTPS_PORT, MODE_CUSTOM, MODE_LETSENCRYPT, MODE_OFF, SSL_CERTIFICATE,
    SSL_MODE, SSL_PRIVATE_KEY, SslMode, ssl_settings, stored_config,
};
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_server::{AppMounts, CSRF_COOKIE, CSRF_HEADER, ServerConfig, admin_handlers, build_router};
use sc_test_harness::TestDb;
use sc_types::SECRET_SENTINEL;
use serde_json::{Value, json};
use tower::ServiceExt;

/// A cookie-jar-carrying client over the router (CSRF + session), as the other
/// admin-API tests use.
struct Client {
    router: Router,
    cookies: HashMap<String, String>,
}

impl Client {
    fn new(router: Router) -> Client {
        Client {
            router,
            cookies: HashMap::new(),
        }
    }

    async fn send(&mut self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if !self.cookies.is_empty() {
            let cookie_header = self
                .cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            builder = builder.header(header::COOKIE, cookie_header);
        }
        if method != "GET" && method != "HEAD" {
            if let Some(csrf) = self.cookies.get(CSRF_COOKIE) {
                builder = builder.header(CSRF_HEADER, csrf);
            }
        }
        let request = match body {
            Some(ref b) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(b).unwrap()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };

        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        for raw in response.headers().get_all(header::SET_COOKIE) {
            if let Ok(text) = raw.to_str() {
                let pair = text.split(';').next().unwrap_or("");
                if let Some((name, value)) = pair.split_once('=') {
                    if value.is_empty() {
                        self.cookies.remove(name);
                    } else {
                        self.cookies.insert(name.to_owned(), value.to_owned());
                    }
                }
            }
        }
        let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, value)
    }
}

/// A self-signed certificate for `localhost`, and its key.
fn self_signed() -> (String, String) {
    let key = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    (key.cert.pem(), key.key_pair.serialize_pem())
}

/// A router over a real database with the platform tables bootstrapped, and an
/// admin logged in.
async fn setup() -> sc_error::Result<(Client, Arc<Catalog>, TestDb)> {
    let db = TestDb::new().await?;
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
    let catalog = Arc::new(Catalog::init(driver as Arc<dyn DatabaseDriver>).await?);
    sc_auth::bootstrap(&catalog).await?;
    sc_config::bootstrap(&catalog).await?;

    let sessions = Arc::new(SessionStore::default());
    let apps = Arc::new(AppMounts::new(catalog.clone()));
    let router = build_router(
        &sc_api::admin_endpoints(),
        admin_handlers(catalog.clone(), apps),
        sessions,
        &ServerConfig::default(),
    )?;

    let mut client = Client::new(router);
    client.send("GET", "/api/auth/status", None).await;
    let (status, _) = client
        .send(
            "POST",
            "/api/first-user",
            Some(json!({ "email": "admin@example.com", "password": "hunter2pass" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    Ok((client, catalog, db))
}

#[tokio::test]
async fn the_screen_is_handed_the_declarations_and_the_values() -> sc_error::Result<()> {
    let (mut client, _catalog, _db) = setup().await?;

    let (status, body) = client.send("GET", "/api/settings", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let ssl = body["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == json!("ssl"))
        .expect("an SSL section");
    assert!(!ssl["label"].as_str().unwrap().is_empty());

    let mode = ssl["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == json!(SSL_MODE))
        .expect("the mode field");
    // Everything the screen needs to render a control it knows nothing about:
    // the type, the choices, the default, and the sentence under it.
    assert_eq!(mode["type"], json!("text"));
    assert_eq!(
        mode["options"],
        json!([MODE_OFF, MODE_LETSENCRYPT, MODE_CUSTOM])
    );
    assert_eq!(mode["default"], json!(MODE_OFF));
    assert!(!mode["help"].as_str().unwrap().is_empty());

    let key = ssl["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == json!(SSL_PRIVATE_KEY))
        .expect("the private key field");
    assert_eq!(key["secret"], json!(true));
    assert_eq!(key["multiline"], json!(true));

    // Nothing is stored yet, so the values are the declared defaults.
    assert_eq!(body["values"][SSL_MODE], json!(MODE_OFF));
    assert_eq!(body["values"][HTTPS_PORT], json!(443));
    Ok(())
}

#[tokio::test]
async fn a_save_stores_what_the_server_then_serves_with() -> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;
    let (certificate, private_key) = self_signed();

    let (status, saved) = client
        .send(
            "POST",
            "/api/settings",
            Some(json!({ "values": {
                SSL_MODE: MODE_CUSTOM,
                SSL_CERTIFICATE: certificate,
                SSL_PRIVATE_KEY: private_key,
                HTTPS_PORT: 8443,
            }})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    // The boot path reads exactly what was saved.
    let settings = ssl_settings(&catalog).await?;
    assert_eq!(settings.mode, SslMode::Custom);
    assert_eq!(settings.https_port, 8443);
    assert!(settings.certificate.contains("BEGIN CERTIFICATE"));
    // ...and it is servable, which is what the save promised.
    sc_server::TlsSettings::from_ssl(&settings, vec![], None)?;
    Ok(())
}

/// The §11.1 rule, applied to a certificate's key: it reaches the browser as the
/// sentinel, and a save that hands the sentinel back leaves the stored key
/// alone. Getting this wrong destroys a working certificate the next time
/// anybody edits an unrelated setting.
#[tokio::test]
async fn the_private_key_never_crosses_the_wire_and_a_save_does_not_destroy_it()
-> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;
    let (certificate, private_key) = self_signed();

    let (status, saved) = client
        .send(
            "POST",
            "/api/settings",
            Some(json!({ "values": {
                SSL_MODE: MODE_CUSTOM,
                SSL_CERTIFICATE: certificate,
                SSL_PRIVATE_KEY: private_key.clone(),
            }})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["values"][SSL_PRIVATE_KEY], json!(SECRET_SENTINEL));

    let (_, read) = client.send("GET", "/api/settings", None).await;
    assert_eq!(read["values"][SSL_PRIVATE_KEY], json!(SECRET_SENTINEL));
    // The stored value is the key itself — the redaction is the API's, not the
    // table's, or there would be nothing to serve with.
    assert_eq!(
        stored_config(&catalog, SSL_PRIVATE_KEY).await?,
        Some(json!(private_key))
    );

    // Editing another setting, with the sentinel handed back untouched.
    let (status, saved) = client
        .send(
            "POST",
            "/api/settings",
            Some(json!({ "values": {
                SSL_MODE: MODE_CUSTOM,
                SSL_CERTIFICATE: read["values"][SSL_CERTIFICATE].clone(),
                SSL_PRIVATE_KEY: SECRET_SENTINEL,
                HTTPS_PORT: 9443,
            }})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        stored_config(&catalog, SSL_PRIVATE_KEY).await?,
        Some(json!(private_key)),
        "the stored key must survive a save that never saw it"
    );
    assert_eq!(ssl_settings(&catalog).await?.https_port, 9443);
    Ok(())
}

#[tokio::test]
async fn a_configuration_that_cannot_serve_is_refused_whole() -> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;
    let (certificate, _key) = self_signed();
    let (_other_cert, other_key) = self_signed();

    // A mode with nothing behind it.
    let (status, body) = client
        .send(
            "POST",
            "/api/settings",
            Some(json!({ "values": { SSL_MODE: MODE_CUSTOM, HTTPS_PORT: 8443 }})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains(SSL_CERTIFICATE),
        "{body}"
    );
    // Nothing landed — not even the port, which was fine on its own.
    assert_eq!(stored_config(&catalog, HTTPS_PORT).await?, None);
    assert_eq!(stored_config(&catalog, SSL_MODE).await?, None);

    // A key that does not go with the chain: valid PEM, wrong pair.
    let (status, body) = client
        .send(
            "POST",
            "/api/settings",
            Some(json!({ "values": {
                SSL_MODE: MODE_CUSTOM,
                SSL_CERTIFICATE: certificate,
                SSL_PRIVATE_KEY: other_key,
            }})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(stored_config(&catalog, SSL_MODE).await?, None);

    // ACME without a contact address: the CA needs somewhere to send warnings.
    let (status, body) = client
        .send(
            "POST",
            "/api/settings",
            Some(json!({ "values": { SSL_MODE: MODE_LETSENCRYPT }})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains(ACME_CONTACT_EMAIL),
        "{body}"
    );

    // A value of the wrong type is refused by the declaration itself.
    let (status, body) = client
        .send(
            "POST",
            "/api/settings",
            Some(json!({ "values": { HTTPS_PORT: "eight thousand" }})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // And a key nobody declared is a typo, not a new setting.
    let (status, body) = client
        .send(
            "POST",
            "/api/settings",
            Some(json!({ "values": { "ssl_moed": MODE_OFF }})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    Ok(())
}

#[tokio::test]
async fn settings_are_admin_only() -> sc_error::Result<()> {
    let (client, _catalog, _db) = setup().await?;
    // A fresh client over the same router: no session, so no authority.
    let mut anonymous = Client::new(client.router.clone());
    anonymous.send("GET", "/api/auth/status", None).await;

    let (status, _) = anonymous.send("GET", "/api/settings", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = anonymous
        .send(
            "POST",
            "/api/settings",
            Some(json!({ "values": { SSL_MODE: MODE_OFF }})),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    Ok(())
}

/// Each setting is its own row, so a save that omits a key leaves it alone —
/// which is why the screen sends `null` for an emptied box. Without this the
/// only thing an admin could do to a setting is change it, never clear it.
#[tokio::test]
async fn a_null_clears_a_setting_back_to_its_default() -> sc_error::Result<()> {
    let (mut client, catalog, _db) = setup().await?;

    let (status, _) = client
        .send(
            "POST",
            "/api/settings",
            Some(json!({ "values": { HTTPS_PORT: 8443 }})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        stored_config(&catalog, HTTPS_PORT).await?,
        Some(json!(8443))
    );

    let (status, body) = client
        .send(
            "POST",
            "/api/settings",
            Some(json!({ "values": { HTTPS_PORT: Value::Null }})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(stored_config(&catalog, HTTPS_PORT).await?, None);
    // ...and what comes back is the declared default, which is what the screen
    // then shows in the box the admin just emptied.
    assert_eq!(body["values"][HTTPS_PORT], json!(443));
    assert_eq!(ssl_settings(&catalog).await?.https_port, 443);
    Ok(())
}
