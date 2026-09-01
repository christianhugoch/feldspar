//! Terminating TLS in-process: pasted certificates and ACME (design §13.5).
//!
//! The server serves HTTPS itself, so a deployment needs no reverse proxy in
//! front of it — and the admin UI and every application get the same treatment,
//! because they are one listener with a `Host` router behind it. Certificates
//! come from one of two places, chosen by the `ssl_mode` setting:
//!
//! - **`custom`** — a chain and a key the admin pasted into the settings screen.
//!   They are parsed into a [`rustls::ServerConfig`] and that is the whole of it:
//!   no ACME traffic, nothing to renew.
//! - **`letsencrypt`** — [`rustls_acme`] obtains and renews certificates from an
//!   ACME CA, answering the **TLS-ALPN-01** challenge inside the handshake it is
//!   already terminating. That choice of challenge is why there is no `/.well-
//!   known/acme-challenge` route anywhere in the router: the validation never
//!   reaches HTTP, so there is nothing to keep in step and nothing an
//!   application's own routes could shadow.
//!
//! Both arrive at the listener as an `axum_server` acceptor, which is why this
//! crate serves through `axum-server` when TLS is on and through `axum::serve`
//! when it is off — one accept seam, two ways of filling it.
//!
//! **ALPN advertises `http/1.1` only, on purpose.** The admin UI's agent chat
//! and the IDE's language server are WebSockets, and a WebSocket over HTTP/2
//! needs RFC 8441's extended CONNECT, which axum's `ws` does not implement.
//! Advertising `h2` would therefore trade two working features for a
//! multiplexing win on an admin console. If HTTP/2 is wanted for an
//! application's API, that is a reverse proxy's job today.
//!
//! **The certificates are checked where they are pasted.** [`check_certificate`]
//! is what the settings endpoint calls before saving: a key that does not match
//! its chain fails in front of the admin, not at the next restart when nobody is
//! watching and the symptom is a server that will not bind.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls_acme::{AcmeConfig, AcmeState};
use sc_config::{AcmeCache, SslMode, SslSettings};
use sc_error::{Error, Result};

/// The shutdown handle of a running TLS server.
///
/// `axum_server::Handle` is generic over the kind of address its server is
/// bound to; every listener here is a TCP one, so this names that once instead
/// of spelling the parameter at each use.
pub type TlsHandle = axum_server::Handle<SocketAddr>;

/// The one ALPN protocol the server advertises. See the module docs for why
/// `h2` is not in this list.
const ALPN_HTTP11: &[u8] = b"http/1.1";

/// How long in-flight requests have to finish after a shutdown signal.
const GRACEFUL_SECONDS: u64 = 10;

/// How this server obtains the certificate it serves.
///
/// Built from the stored [`SslSettings`] plus the two things only the running
/// server knows: which domains it answers for, and which database the ACME
/// account and certificates are cached in.
#[derive(Clone)]
pub enum TlsSettings {
    /// Plain HTTP: no TLS listener, no redirect.
    Off,
    /// An admin-supplied certificate chain and private key, both PEM.
    Custom {
        /// The PEM chain: server certificate first, then intermediates.
        certificate: String,
        /// The PEM private key (PKCS#8, PKCS#1 or SEC1).
        private_key: String,
        /// The port TLS is served on.
        port: u16,
        /// Whether the plain-HTTP listener redirects here.
        redirect_http: bool,
    },
    /// Certificates obtained and renewed from an ACME CA.
    Acme {
        /// Every name the certificate must cover, in the order it is ordered in
        /// (which is part of the cache key — see [`sc_config::acme`]).
        domains: Vec<String>,
        /// The account contact, without the `mailto:` scheme.
        contact_email: String,
        /// The CA's directory URL.
        directory_url: String,
        /// Where the account key and the issued certificates are kept, so a
        /// renewal survives a restart and a second node does not order its own.
        cache: AcmeCache,
        /// The port TLS is served on. Reaching the ACME CA's TLS-ALPN-01
        /// validation means this must be **443** on a public address.
        port: u16,
        /// Whether the plain-HTTP listener redirects here.
        redirect_http: bool,
    },
}

impl std::fmt::Debug for TlsSettings {
    /// Prints what is configured, never the private key. A `Debug` that dumps
    /// key material is a key in a log file.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TlsSettings::Off => f.write_str("TlsSettings::Off"),
            TlsSettings::Custom { port, .. } => f
                .debug_struct("TlsSettings::Custom")
                .field("port", port)
                .field("certificate", &"<pem>")
                .field("private_key", &"<redacted>")
                .finish(),
            TlsSettings::Acme {
                domains,
                contact_email,
                directory_url,
                port,
                ..
            } => f
                .debug_struct("TlsSettings::Acme")
                .field("domains", domains)
                .field("contact_email", contact_email)
                .field("directory_url", directory_url)
                .field("port", port)
                .finish(),
        }
    }
}

impl TlsSettings {
    /// Whether a TLS listener is served at all.
    pub fn enabled(&self) -> bool {
        !matches!(self, TlsSettings::Off)
    }

    /// The port TLS is served on, if any.
    pub fn port(&self) -> Option<u16> {
        match self {
            TlsSettings::Off => None,
            TlsSettings::Custom { port, .. } | TlsSettings::Acme { port, .. } => Some(*port),
        }
    }

    /// Whether the plain-HTTP listener answers with a redirect instead of the
    /// application.
    pub fn redirect_http(&self) -> bool {
        match self {
            TlsSettings::Off => false,
            TlsSettings::Custom { redirect_http, .. } | TlsSettings::Acme { redirect_http, .. } => {
                *redirect_http
            }
        }
    }

    /// Build the serving plan from the stored settings.
    ///
    /// `domains` is what the ACME order will cover — see [`tls_domains`] — and
    /// `cache` is where its results are kept. Both are ignored outside
    /// `letsencrypt` mode, and both are **required** in it: an order with no
    /// names is not an order, and a client with no cache would order a fresh
    /// certificate on every boot, which is how a deployment meets Let's
    /// Encrypt's rate limits.
    pub fn from_ssl(
        settings: &SslSettings,
        domains: Vec<String>,
        cache: Option<AcmeCache>,
    ) -> Result<TlsSettings> {
        settings.check()?;
        match settings.mode {
            SslMode::Off => Ok(TlsSettings::Off),
            SslMode::Custom => {
                check_certificate(&settings.certificate, &settings.private_key)?;
                Ok(TlsSettings::Custom {
                    certificate: settings.certificate.clone(),
                    private_key: settings.private_key.clone(),
                    port: settings.https_port,
                    redirect_http: settings.redirect_http,
                })
            }
            SslMode::LetsEncrypt => {
                if domains.is_empty() {
                    return Err(Error::config(
                        "TLS is set to `letsencrypt` but this server has no domain to certify: \
                         set a base domain (`--base-domain`) or list the names in \
                         `ssl_extra_domains`",
                    ));
                }
                let cache = cache.ok_or_else(|| {
                    Error::config("ACME needs a database to cache its account and certificates in")
                })?;
                Ok(TlsSettings::Acme {
                    domains,
                    contact_email: settings.contact_email.clone(),
                    directory_url: settings.directory_url.clone(),
                    cache,
                    port: settings.https_port,
                    redirect_http: settings.redirect_http,
                })
            }
        }
    }
}

/// Every name this server should hold a certificate for.
///
/// The admin does not list the obvious ones: the base domain is where the admin
/// UI is, and every application is a subdomain of it (§13.2), so both are
/// derived from what the server is already configured with. `extra` is for the
/// names that are neither — an apex the deployment also answers to, a vanity
/// domain pointed at one application.
///
/// De-duplicated and lower-cased, because a SAN list is a set.
pub fn tls_domains(
    base_domain: Option<&str>,
    subdomains: &[String],
    extra: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |name: String| {
        let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    };
    if let Some(base) = base_domain.map(str::trim).filter(|b| !b.is_empty()) {
        push(base.to_owned());
        for subdomain in subdomains {
            push(format!("{subdomain}.{base}"));
        }
    }
    for name in extra {
        push(name.clone());
    }
    out
}

/// Install the process-wide rustls crypto provider.
///
/// Two providers reach this binary — `aws-lc-rs` here and `ring` through other
/// dependencies — and rustls refuses to guess between them, failing at the first
/// handshake rather than at startup. Naming one at boot is what turns that into
/// a decision. Idempotent: a second call (another test in the same process) is a
/// no-op.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// Parse a PEM chain and key into a rustls server configuration.
///
/// Every failure names what is wrong with the *paste* — no certificate in it, no
/// key in it, a key that does not go with the chain — because that is what the
/// admin has in front of them.
pub fn certificate_config(certificate: &str, private_key: &str) -> Result<ServerConfig> {
    install_crypto_provider();

    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut certificate.trim().as_bytes())
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::invalid(format!("the certificate is not valid PEM: {e}")))?;
    if certs.is_empty() {
        return Err(Error::invalid(
            "no certificate found: the chain should start with a \
             `-----BEGIN CERTIFICATE-----` block",
        ));
    }

    let key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut private_key.trim().as_bytes())
            .map_err(|e| Error::invalid(format!("the private key is not valid PEM: {e}")))?
            .ok_or_else(|| {
                Error::invalid(
                    "no private key found: expected a PKCS#8 (`BEGIN PRIVATE KEY`), \
                     PKCS#1 (`BEGIN RSA PRIVATE KEY`) or SEC1 (`BEGIN EC PRIVATE KEY`) block",
                )
            })?;

    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        // rustls checks here that the key matches the leaf certificate, which is
        // the mistake a paste actually makes: two files, copied one at a time.
        .map_err(|e| Error::invalid(format!("certificate and private key do not match: {e}")))?;
    config.alpn_protocols = vec![ALPN_HTTP11.to_vec()];
    Ok(config)
}

/// Check a pasted certificate and key without keeping the result — what the
/// settings endpoint calls before it saves.
pub fn check_certificate(certificate: &str, private_key: &str) -> Result<()> {
    certificate_config(certificate, private_key).map(|_| ())
}

/// Serve `app` over TLS on an already-bound listener until `handle` is told to
/// shut down.
///
/// The listener is bound by the caller so a port that cannot be bound is
/// reported by whoever chose it, with its address in the message, before
/// anything else is started.
pub async fn serve_https(
    listener: std::net::TcpListener,
    app: Router,
    tls: &TlsSettings,
    handle: TlsHandle,
) -> Result<()> {
    listener
        .set_nonblocking(true)
        .map_err(|e| Error::msg(format!("preparing the TLS listener: {e}")))?;

    match tls {
        TlsSettings::Off => Err(Error::config("serve_https called with TLS off")),
        TlsSettings::Custom {
            certificate,
            private_key,
            ..
        } => {
            let config = certificate_config(certificate, private_key)?;
            let acceptor = RustlsAcceptor::new(RustlsConfig::from_config(Arc::new(config)));
            axum_server::from_tcp(listener)
                .map_err(|e| Error::msg(format!("serving TLS: {e}")))?
                .acceptor(acceptor)
                .handle(handle)
                .serve(app.into_make_service())
                .await
                .map_err(|e| Error::msg(format!("TLS server error: {e}")))
        }
        TlsSettings::Acme {
            domains,
            contact_email,
            directory_url,
            cache,
            ..
        } => {
            install_crypto_provider();
            let state = acme_state(domains, contact_email, directory_url, cache.clone());
            let mut server_config = ServerConfig::builder()
                .with_no_client_auth()
                .with_cert_resolver(state.resolver());
            server_config.alpn_protocols = vec![ALPN_HTTP11.to_vec()];
            let acceptor = state.axum_acceptor(Arc::new(server_config));
            // The state is a stream of ACME events, and it only makes progress
            // while something polls it: this task *is* the certificate
            // provisioning and renewal. Its outcomes are logged rather than
            // fatal — a CA that is briefly unreachable must not take the server
            // with it, and a cached certificate keeps serving meanwhile.
            tokio::spawn(drive_acme(state));
            axum_server::from_tcp(listener)
                .map_err(|e| Error::msg(format!("serving TLS: {e}")))?
                .acceptor(acceptor)
                .handle(handle)
                .serve(app.into_make_service())
                .await
                .map_err(|e| Error::msg(format!("TLS server error: {e}")))
        }
    }
}

/// The ACME client for these domains, cached in the database.
fn acme_state(
    domains: &[String],
    contact_email: &str,
    directory_url: &str,
    cache: AcmeCache,
) -> AcmeState<Error, Error> {
    let mut config = AcmeConfig::new(domains)
        .directory(directory_url)
        .cache(DatabaseCache(cache));
    if !contact_email.trim().is_empty() {
        config = config.contact_push(format!("mailto:{}", contact_email.trim()));
    }
    config.state()
}

/// Poll the ACME state forever, reporting what it does.
///
/// Every line here is an operator's answer to "is my certificate coming?", which
/// is otherwise unanswerable from outside: the handshake either works or does
/// not, and the reason lives in the CA's response.
async fn drive_acme(mut state: AcmeState<Error, Error>) {
    use futures::StreamExt;
    while let Some(event) = state.next().await {
        match event {
            Ok(ok) => eprintln!("feldspar: acme: {ok:?}"),
            Err(err) => eprintln!("feldspar: acme error: {err}"),
        }
    }
}

/// The database-backed ACME cache, in the shape `rustls-acme` asks for.
///
/// The storage itself is [`sc_config::AcmeCache`] — a layer down, where the
/// tables are. This is only the adapter, which is why it is here: naming
/// `rustls_acme`'s traits is this crate's business, not the configuration
/// crate's (§2).
struct DatabaseCache(AcmeCache);

#[async_trait::async_trait]
impl rustls_acme::CertCache for DatabaseCache {
    type EC = Error;

    async fn load_cert(
        &self,
        domains: &[String],
        directory_url: &str,
    ) -> std::result::Result<Option<Vec<u8>>, Error> {
        self.0.load_cert(domains, directory_url).await
    }

    async fn store_cert(
        &self,
        domains: &[String],
        directory_url: &str,
        cert: &[u8],
    ) -> std::result::Result<(), Error> {
        self.0.store_cert(domains, directory_url, cert).await
    }
}

#[async_trait::async_trait]
impl rustls_acme::AccountCache for DatabaseCache {
    type EA = Error;

    async fn load_account(
        &self,
        contact: &[String],
        directory_url: &str,
    ) -> std::result::Result<Option<Vec<u8>>, Error> {
        self.0.load_account(contact, directory_url).await
    }

    async fn store_account(
        &self,
        contact: &[String],
        directory_url: &str,
        account: &[u8],
    ) -> std::result::Result<(), Error> {
        self.0.store_account(contact, directory_url, account).await
    }
}

/// The plain-HTTP router served when TLS is on and redirects are wanted: every
/// request is answered with a permanent redirect to the same URL over HTTPS.
///
/// **308, not 301**: a 301 lets a client turn a `POST` into a `GET`, which for
/// an API request means a write silently becoming a read.
pub fn redirect_router(https_port: u16) -> Router {
    Router::new()
        .fallback(move |request: Request| async move { redirect_response(&request, https_port) })
}

/// The redirect for one request, or 400 when there is no host to redirect to.
fn redirect_response(request: &Request, https_port: u16) -> Response {
    let Some(host) = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
    else {
        return (StatusCode::BAD_REQUEST, "no Host header to redirect").into_response();
    };
    // The `Host` carries the *plain-HTTP* port, which is not the port the
    // redirect goes to; the hostname is the only part worth keeping.
    let hostname = host.split(':').next().unwrap_or(host);
    let target = if https_port == 443 {
        format!("https://{hostname}{}", path_and_query(request.uri()))
    } else {
        format!(
            "https://{hostname}:{https_port}{}",
            path_and_query(request.uri())
        )
    };
    match HeaderValue::from_str(&target) {
        Ok(location) => (
            StatusCode::PERMANENT_REDIRECT,
            [(header::LOCATION, location)],
        )
            .into_response(),
        Err(_) => (StatusCode::BAD_REQUEST, "invalid Host header").into_response(),
    }
}

/// The part of a URI that survives the scheme change.
fn path_and_query(uri: &Uri) -> String {
    uri.path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned())
}

/// Ask a running TLS server to stop, giving in-flight requests
/// [`GRACEFUL_SECONDS`] to finish.
pub fn graceful_shutdown(handle: &TlsHandle) {
    handle.graceful_shutdown(Some(std::time::Duration::from_secs(GRACEFUL_SECONDS)));
}

/// The address TLS is served on: the bind address's interface, the settings'
/// port.
///
/// The interface is not a second setting on purpose — an operator who binds
/// `127.0.0.1` for a local trial should not have to remember that TLS has its
/// own idea of where to listen.
pub fn https_addr(http_addr: SocketAddr, port: u16) -> SocketAddr {
    SocketAddr::new(http_addr.ip(), port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sc_config::{SslMode, SslSettings};

    fn settings(mode: SslMode) -> SslSettings {
        SslSettings {
            mode,
            ..SslSettings::default()
        }
    }

    #[test]
    fn tls_off_is_the_default_and_serves_nothing_extra() {
        let plan = TlsSettings::from_ssl(&settings(SslMode::Off), vec![], None).unwrap();
        assert!(!plan.enabled());
        assert_eq!(plan.port(), None);
        assert!(!plan.redirect_http());
    }

    #[test]
    fn the_domain_list_is_derived_from_what_the_server_serves() {
        let apps = ["blog".to_owned(), "shop".to_owned()];
        let extra = ["vanity.example.net".to_owned(), "Example.com".to_owned()];
        assert_eq!(
            tls_domains(Some("example.com"), &apps, &extra),
            [
                "example.com",
                "blog.example.com",
                "shop.example.com",
                "vanity.example.net"
            ]
        );
        // No base domain: apps are not addressable, so only what was listed.
        assert_eq!(
            tls_domains(None, &apps, &extra),
            ["vanity.example.net", "example.com"]
        );
        assert!(tls_domains(None, &apps, &[]).is_empty());
    }

    /// ACME with nothing to certify is refused at boot rather than at the CA.
    #[test]
    fn acme_without_a_domain_or_a_cache_is_refused() {
        let mut acme = settings(SslMode::LetsEncrypt);
        acme.contact_email = "admin@example.com".to_owned();
        let err = TlsSettings::from_ssl(&acme, vec![], None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("domain"), "{err}");

        let err = TlsSettings::from_ssl(&acme, vec!["example.com".to_owned()], None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("cache"), "{err}");
    }

    #[test]
    fn a_paste_that_is_not_a_certificate_says_so() {
        let err = check_certificate("hello", "world").unwrap_err().to_string();
        assert!(err.contains("certificate"), "{err}");

        let err = check_certificate(
            "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
            "",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("private key"), "{err}");
    }

    #[test]
    fn a_redirect_keeps_the_path_and_the_method() {
        let request = Request::builder()
            .method("POST")
            .uri("/api/tables?limit=2")
            .header(header::HOST, "blog.example.com:8080")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = redirect_response(&request, 443);
        // 308 rather than 301: a redirected POST stays a POST.
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "https://blog.example.com/api/tables?limit=2"
        );

        // A non-default TLS port is spelled out; the HTTP port never is.
        let request = Request::builder()
            .uri("/")
            .header(header::HOST, "example.com:8080")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = redirect_response(&request, 8443);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "https://example.com:8443/"
        );
    }

    #[test]
    fn a_request_with_no_host_is_a_bad_request_not_a_panic() {
        let request = Request::builder()
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(
            redirect_response(&request, 443).status(),
            StatusCode::BAD_REQUEST
        );
    }
}
