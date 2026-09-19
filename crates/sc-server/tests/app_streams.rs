//! Phase 8: an **application** observes a stream it exposes (TODO "Streams"
//! §10, task 8.4), driven through the assembled router against a real database.
//!
//! No broker: the provider is `sc_stream::testing::ScriptedProvider` behind the
//! production seam, exactly as `streams_admin_api` uses it, so everything under
//! test — the app's mount, the socket, the supervisor, the broadcast channel —
//! is the code that ships.
//!
//! The claim is the one §10 makes, and it has four parts:
//!
//! 1. **An app that exposes a stream can observe it** at
//!    `{mount}/streams/{name}/observe`, authenticated by the app's own session
//!    cookie, and gets the same frames the admin socket sends: `ready` with the
//!    element type, the ring replay, then live elements.
//! 2. **An app that does not expose it gets a 404** — not a 403 — for a stream
//!    the server is running perfectly well. The app's declaration is the whole
//!    of its surface, which is the rule its triggers already follow.
//! 3. **The stream's own `min_role` is the floor.** A logged-in user below it
//!    is refused *before* the upgrade, with a status, because that is the one
//!    refusal a browser can read.
//! 4. **The generated client has it**, typed: `observeStream_boiler` over an
//!    envelope whose `value` is this stream's declared keys.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderValue, Request, StatusCode, header};
use futures::StreamExt;
use sc_app::{ApiConfig, Application, AssetBundle, CodeFramework, FrameworkRef, StreamRef};
use sc_auth::{ROLE_ADMIN, Role, SessionStore, create_user, save_role};
use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::Result;
use sc_stream::testing::ScriptedProvider;
use sc_stream::{
    ElementField, ElementType, Stream, StreamConfig, StreamProvider, StreamRegistry, save_stream,
};
use sc_test_harness::TestDb;
use sc_types::BasicType;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tower::ServiceExt;

const BASE_DOMAIN: &str = "example.com";
const APP_HOST: &str = "blog.example.com";

const ADMIN: &str = "admin@example.com";
const EDITOR: &str = "editor@example.com";
const READER: &str = "reader@example.com";
const PASSWORD: &str = "correct-horse";

/// The floor the boiler is observable at: an editor may watch it, a reader may
/// not.
const ROLE_EDITOR: u8 = 80;
const ROLE_READER: u8 = 100;

/// What the scripted boiler publishes: one required key and one that may be
/// absent, so the generated envelope has one of each.
fn boiler_reading() -> ElementType {
    ElementType::json([
        ElementField::new("temperature", BasicType::Float).required(),
        ElementField::new("unit", BasicType::Text),
    ])
}

/// The app: it exposes one of the server's two streams.
fn blog_app() -> Application {
    Application::new("Blog", "blog", FrameworkRef::new("code"))
        .with_api(ApiConfig::new("rest", "/api"))
        .with_stream(StreamRef::new("boiler"))
}

/// A cookie-carrying client addressing one host.
struct Client {
    router: Router,
    host: String,
    cookies: HashMap<String, String>,
}

impl Client {
    async fn send(&mut self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, &self.host);
        if !self.cookies.is_empty() {
            let jar = self
                .cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            builder = builder.header(header::COOKIE, jar);
        }
        if method != "GET"
            && let Some(csrf) = self.cookies.get(sc_server::CSRF_COOKIE)
        {
            builder = builder.header(sc_server::CSRF_HEADER, csrf);
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
            if let Ok(text) = raw.to_str()
                && let Some((name, value)) = text.split(';').next().unwrap_or("").split_once('=')
            {
                if value.is_empty() {
                    self.cookies.remove(name);
                } else {
                    self.cookies.insert(name.to_owned(), value.to_owned());
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

    /// Log in through the **app's own** login, as its code would.
    async fn login(&mut self, email: &str) {
        self.send("GET", "/api/whoami", None).await;
        let (status, body) = self
            .send(
                "POST",
                "/api/login",
                Some(json!({ "email": email, "password": PASSWORD })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    fn session(&self) -> Option<&String> {
        self.cookies.get(sc_server::SESSION_COOKIE)
    }
}

struct Server {
    router: Router,
    catalog: Arc<Catalog>,
    /// The same router on a real port: a socket test cannot go through
    /// `oneshot`, because its subject is what happens after the upgrade.
    addr: std::net::SocketAddr,
    _db: TestDb,
}

impl Server {
    fn app_client(&self) -> Client {
        Client {
            router: self.router.clone(),
            host: APP_HOST.to_owned(),
            cookies: HashMap::new(),
        }
    }

    /// Open an application's observe socket, carrying `session` if given.
    ///
    /// The `Host` header is the app's subdomain, because that is how an
    /// application is resolved — the socket is the app's, not the admin's, and
    /// nothing but the host says so.
    async fn observe(
        &self,
        path: &str,
        session: Option<&str>,
    ) -> std::result::Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        WsError,
    > {
        let mut request = format!("ws://{}{path}", self.addr)
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(header::HOST, HeaderValue::from_static(APP_HOST));
        if let Some(token) = session {
            request.headers_mut().insert(
                header::COOKIE,
                HeaderValue::from_str(&format!("{}={token}", sc_server::SESSION_COOKIE)).unwrap(),
            );
        }
        tokio_tungstenite::connect_async(request)
            .await
            .map(|(stream, _)| stream)
    }
}

/// The scripted boiler: it publishes on a timer fast enough for a test and slow
/// enough that a connection is not racing a thousand elements.
fn boiler_provider() -> ScriptedProvider {
    ScriptedProvider::new("scripted", boiler_reading())
        .json_elements([
            json!({ "temperature": 31.2, "unit": "C" }),
            json!({ "temperature": 31.4, "unit": "C" }),
            json!({ "temperature": 31.6 }),
        ])
        .every(Duration::from_millis(100))
}

async fn setup() -> Result<Server> {
    let db = TestDb::new().await?;
    db.client()
        .await?
        .batch_execute(
            "DO $$ DECLARE r record; BEGIN \
               FOR r IN SELECT table_schema FROM information_schema.tables \
               WHERE table_name = 'users' AND table_type = 'BASE TABLE' LOOP \
                 EXECUTE format('DROP TABLE IF EXISTS %I.users CASCADE', r.table_schema); \
               END LOOP; END $$;",
        )
        .await
        .map_err(|e| sc_error::Error::database(e.to_string()))?;

    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let catalog = Arc::new(Catalog::init(driver as Arc<dyn DatabaseDriver>).await?);
    sc_auth::bootstrap(&catalog).await?;
    sc_app::bootstrap(&catalog).await?;
    save_role(&catalog, &Role::new(ROLE_EDITOR, "Editor")).await?;
    create_user(&catalog, ADMIN, PASSWORD, ROLE_ADMIN).await?;
    create_user(&catalog, EDITOR, PASSWORD, ROLE_EDITOR).await?;
    create_user(&catalog, READER, PASSWORD, ROLE_READER).await?;

    let agents = sc_server::install_agents(&catalog).await?;
    let models = sc_server::install_models(&catalog, sc_model::DEFAULT_MAX_ROWS).await?;
    let triggers = sc_server::install_triggers(
        &catalog,
        sc_server::default_js_evaluator(),
        &agents,
        &models,
    )
    .await?;

    let mut registry = StreamRegistry::new();
    registry.register(Arc::new(boiler_provider()) as Arc<dyn StreamProvider>)?;
    let registry = Arc::new(registry);
    let streams = sc_server::install_streams_with(
        &catalog,
        &triggers,
        registry.clone(),
        StreamConfig::default(),
    )
    .await?;

    // Two streams, identical but for their names: one the app exposes, one it
    // does not. The second is what makes "a 404 rather than a 403" a claim
    // about the app's declaration rather than about the stream's existence.
    for name in ["boiler", "meter"] {
        let stream = Stream::new(name, "scripted")
            .description("a scripted flow")
            .min_role(ROLE_EDITOR);
        save_stream(&catalog, &registry, &stream).await?;
    }
    streams.reload(&catalog).await?;

    let apps = Arc::new(
        sc_server::AppMounts::new(catalog.clone())
            .with_triggers(triggers)
            .with_models(models)
            .with_streams(streams),
    );
    let framework = Arc::new(CodeFramework::new(
        "code",
        AssetBundle::new().with("index.html", "<!doctype html>"),
    ));
    apps.mount(sc_server::MountedApp::new_with(
        blog_app(),
        framework,
        &catalog,
        apps.evaluator(),
        apps.triggers(),
    )?)?;

    let config = sc_server::ServerConfig {
        base_domain: Some(BASE_DOMAIN.to_owned()),
        ..sc_server::ServerConfig::default()
    };
    let router = sc_server::build_router_with_apps(
        &sc_api::admin_endpoints(),
        sc_server::admin_handlers(catalog.clone(), apps.clone()),
        Arc::new(SessionStore::default()),
        &config,
        apps,
    )?;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = router.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, served).await;
    });

    Ok(Server {
        router,
        catalog,
        addr,
        _db: db,
    })
}

/// The next JSON frame, or a panic saying none arrived.
async fn next_frame(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(20), socket.next())
            .await
            .expect("a frame arrives within the timeout")
            .expect("the socket is still open")
            .expect("the frame is readable");
        match message {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("expected a text frame, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn an_exposed_stream_is_observed_over_the_apps_own_socket() -> Result<()> {
    let server = setup().await?;
    let mut app = server.app_client();
    app.login(EDITOR).await;
    let session = app.session().expect("the app login set a session").clone();

    let mut socket = server
        .observe("/api/streams/boiler/observe", Some(&session))
        .await
        .expect("the handshake succeeds for an exposed stream and a permitted user");

    // `ready` first, carrying what the elements will be — the same frame the
    // admin socket opens with, because it is the same socket.
    let ready = next_frame(&mut socket).await;
    assert_eq!(ready["type"], json!("ready"));
    assert_eq!(ready["stream"], json!("boiler"));
    assert_eq!(ready["element_type"]["kind"], json!("json"));
    assert_eq!(
        ready["element_type"]["keys"][0]["name"],
        json!("temperature")
    );

    // Then elements, in the §4 envelope. Some of them may be the ring's replay
    // (this stream has been publishing since the server booted), which is
    // exactly what `replayed` said — either way the shape is the contract.
    let element = next_frame(&mut socket).await;
    assert_eq!(element["type"], json!("element"));
    let envelope = &element["envelope"];
    assert_eq!(envelope["stream"], json!("boiler"));
    assert!(
        envelope["value"]["temperature"].is_number(),
        "the declared key arrives as its declared type: {envelope}"
    );
    assert!(
        envelope["received_at"].is_string(),
        "an envelope says when this server saw it: {envelope}"
    );
    Ok(())
}

#[tokio::test]
async fn a_stream_the_app_does_not_expose_is_not_there() -> Result<()> {
    let server = setup().await?;
    let mut app = server.app_client();
    app.login(EDITOR).await;
    let session = app.session().unwrap().clone();

    // `meter` is running on this server and this user's role would permit it.
    // The app did not name it, so there is nothing at that path — a 404, not a
    // 403, which is the answer an unexposed trigger already gives.
    let error = server
        .observe("/api/streams/meter/observe", Some(&session))
        .await
        .expect_err("an unexposed stream has no socket");
    assert!(
        matches!(&error, WsError::Http(response) if response.status() == StatusCode::NOT_FOUND),
        "expected a 404 before the upgrade, got {error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn the_streams_own_min_role_is_the_floor() -> Result<()> {
    let server = setup().await?;

    // Nobody at all: refused *before* the upgrade with a status, because a
    // browser cannot read the body of a failed handshake.
    let anonymous = server
        .observe("/api/streams/boiler/observe", None)
        .await
        .expect_err("an unauthenticated socket is refused");
    assert!(
        matches!(&anonymous, WsError::Http(r) if r.status() == StatusCode::UNAUTHORIZED),
        "expected a 401, got {anonymous:?}"
    );

    // Logged in, but below the stream's floor.
    let mut reader = server.app_client();
    reader.login(READER).await;
    let session = reader.session().unwrap().clone();
    let refused = server
        .observe("/api/streams/boiler/observe", Some(&session))
        .await
        .expect_err("a user below the stream's min_role is refused");
    assert!(
        matches!(&refused, WsError::Http(r) if r.status() == StatusCode::FORBIDDEN),
        "expected a 403, got {refused:?}"
    );
    Ok(())
}

#[tokio::test]
async fn the_path_is_a_socket_and_says_so_to_an_ordinary_get() -> Result<()> {
    let server = setup().await?;
    let mut app = server.app_client();
    app.login(EDITOR).await;
    let (status, body) = app.send("GET", "/api/streams/boiler/observe", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("WebSocket"),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn the_generated_client_observes_what_the_app_exposes() -> Result<()> {
    let server = setup().await?;
    let app = blog_app();
    // Resolved against the stored rows and the installed provider registry,
    // which is what an app's build does.
    let streams = sc_app::app_streams(&app, &server.catalog).await?;
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].stream.name, "boiler");

    let exports = sc_app::stream_exports(&app, &streams);
    let ts = sc_api::generate_client_with_streams(&sc_api::EndpointSet::new(), &exports);
    // The method the app's own code calls, on the path the server serves.
    assert!(ts.contains("observeStream_boiler("), "{ts}");
    assert!(ts.contains("/api/streams/boiler/observe"), "{ts}");
    // …typed from the element type: a required key is not nullable, one that
    // may be absent is (§4).
    assert!(ts.contains("temperature: number"), "{ts}");
    assert!(ts.contains("unit: string | null"), "{ts}");
    // And a stream this app does not expose is not in its client at all.
    assert!(!ts.contains("meter"), "{ts}");
    Ok(())
}

/// An app that names a stream the server no longer has does not quietly
/// generate a smaller client: it says which app and which stream, exactly as an
/// app naming a missing trigger does.
#[tokio::test]
async fn a_stream_that_is_gone_is_named_rather_than_skipped() -> Result<()> {
    let server = setup().await?;
    let app = Application::new("Ghost", "ghost", FrameworkRef::new("code"))
        .with_api(ApiConfig::new("rest", "/api"))
        .with_stream(StreamRef::new("nothing_here"));
    let error = sc_app::app_streams(&app, &server.catalog)
        .await
        .expect_err("a stream that does not exist is refused");
    let message = error.to_string();
    assert!(message.contains("Ghost"), "{message}");
    assert!(message.contains("nothing_here"), "{message}");
    Ok(())
}
