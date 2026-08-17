//! Security posture for the SPA: strict CSP, CSRF, and cookie conventions
//! (technical design §16 "Security posture").
//!
//! - **CSP.** A strict Content-Security-Policy with **no `unsafe-inline` script**
//!   — the React bundle carries no inline scripts or handlers, so `'self'` is
//!   enough for the directive that matters. Inline *styles* are allowed, for the
//!   one thing that needs them (the embedded Monaco editor's theme); see
//!   [`CONTENT_SECURITY_POLICY`]. It is applied as a response header via
//!   `tower-http`'s `set-header` layer (see [`crate::router`]).
//! - **CSRF.** The session cookie authenticates the SPA, so state-changing
//!   requests are protected with the **double-submit-cookie** pattern: the server
//!   hands the SPA a non-`HttpOnly` `sc_csrf` cookie, and every mutating request
//!   must echo it in the `x-csrf-token` header. A cross-site page can send the
//!   cookie but cannot read it to set the header, so the forgery fails.
//! - **Cookies.** `SameSite=Strict` on both cookies; the session cookie is
//!   `HttpOnly`; `Secure` is set behind TLS (see `ServerConfig::secure_cookies`).

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use uuid::Uuid;

/// Name of the session cookie (opaque token → [`SessionStore`](sc_auth::SessionStore)).
pub const SESSION_COOKIE: &str = "sc_session";
/// Name of the CSRF double-submit cookie (readable by the SPA), and the header a
/// mutating request must echo it in.
///
/// Both come from `sc-api`, which is where the wire contract is stated: the
/// server enforces the check here and the generated TypeScript client satisfies
/// it, and a second spelling of either name is exactly how those two stop
/// agreeing.
pub use sc_api::auth::{CSRF_COOKIE, CSRF_HEADER};

/// The strict Content-Security-Policy served with every response. No
/// `unsafe-inline` **script**: executable code loads only from the app's own
/// origin, so the React bundle is the sole executable source.
///
/// One relaxation, and it is about styles only: **`style-src 'unsafe-inline'`**,
/// because the admin UI embeds the Monaco editor for code settings (a
/// `run_js_code` body) and Monaco writes its theme — the token colours that *are*
/// the syntax highlighting — into a `<style>` element it creates at runtime. It
/// offers no nonce hook to sign that with, so under `style-src 'self'` the
/// element is blocked and the editor renders in one colour. The IDE's policy
/// below already makes the same allowance for the same reason.
///
/// What it costs is bounded by the directives that did not move: script sources
/// are still `'self'` with no `eval` and no `blob:`, and `default-src 'self'`
/// with `img-src 'self' data:` leaves injected CSS nowhere to send anything —
/// the classic CSS exfiltration channel is a remote `url()`, which is still
/// refused. It buys back an editor that highlights, completes and type-checks
/// what an admin is writing.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; \
script-src 'self'; \
style-src 'self' 'unsafe-inline'; \
img-src 'self' data:; \
font-src 'self'; \
connect-src 'self'; \
base-uri 'none'; \
form-action 'self'; \
frame-ancestors 'none'; \
object-src 'none'";

/// The Content-Security-Policy served with the **file-store IDE** under `/ide/`
/// (design §12.1), and with nothing else.
///
/// The admin SPA satisfies the strict policy above structurally, because React
/// escapes values and the bundle carries no inline anything. The IDE cannot: it is
/// VS Code, which computes styles at runtime and injects them, runs its editor,
/// textmate, search and extension-host code as workers created from blobs, and
/// hosts the worker extension host in a sandboxed iframe. Each relaxation below is
/// one of those facts, and no more than that:
///
/// - `style-src 'unsafe-inline'` — the workbench's own injected styles.
/// - `script-src 'unsafe-eval' blob:` — worker bootstrap code, and the
///   `WebAssembly` compilation textmate's oniguruma engine needs.
/// - `worker-src blob:` and `frame-src blob:` — the workers and the extension
///   host's iframe.
///
/// What it does **not** relax is where code may come from: `default-src 'self'`
/// stands, there is no `https:` or wildcard source, and `connect-src 'self'` keeps
/// the IDE talking to this server only (which is also what admits the same-origin
/// WebSocket a language server will need). An extension marketplace would need a
/// remote origin here, which is one more reason installing extensions is out of
/// scope.
///
/// It is served **per response** on the IDE's own route rather than as a layer, so
/// the strict policy remains the default for everything else: relaxing CSP for a
/// route must not be a way of relaxing it for the admin UI.
pub const IDE_CONTENT_SECURITY_POLICY: &str = "default-src 'self'; \
script-src 'self' 'unsafe-eval' blob:; \
style-src 'self' 'unsafe-inline'; \
img-src 'self' data: blob:; \
font-src 'self' data:; \
connect-src 'self' data: blob:; \
worker-src 'self' blob:; \
child-src 'self' blob:; \
frame-src 'self' blob:; \
base-uri 'none'; \
form-action 'self'; \
frame-ancestors 'none'; \
object-src 'none'";

/// A fresh, unguessable CSRF token (256 bits from two v4 UUIDs, hex-encoded).
pub(crate) fn new_csrf_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// Build a cookie with the shared security attributes.
pub(crate) fn build_cookie(
    name: &'static str,
    value: String,
    http_only: bool,
    secure: bool,
) -> Cookie<'static> {
    Cookie::build((name, value))
        .path("/")
        .same_site(SameSite::Strict)
        .http_only(http_only)
        .secure(secure)
        .build()
}

/// Whether a method may change server state (and so needs CSRF protection).
fn is_mutating(method: &Method) -> bool {
    !matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

/// CSRF middleware implementing the double-submit-cookie check.
///
/// On a mutating request the `x-csrf-token` header must equal the existing
/// `sc_csrf` cookie, else the request is rejected `403`. Every response ensures
/// the cookie is set (minting one on first contact) so the SPA can read it and
/// echo it on later mutations. `State<bool>` carries the `Secure` cookie flag.
pub(crate) async fn csrf_middleware(
    State(secure): State<bool>,
    jar: CookieJar,
    request: Request,
    next: Next,
) -> Response {
    let existing = jar.get(CSRF_COOKIE).map(|c| c.value().to_owned());

    if is_mutating(request.method()) {
        let header = request
            .headers()
            .get(CSRF_HEADER)
            .and_then(|v| v.to_str().ok());
        let valid = matches!((&existing, header), (Some(cookie), Some(hdr)) if cookie == hdr);
        if !valid {
            // Reject, but still hand out a token so a first-contact client can
            // read it and retry successfully.
            let jar = ensure_csrf_cookie(jar, existing, secure);
            return (StatusCode::FORBIDDEN, jar, "CSRF token missing or invalid").into_response();
        }
    }

    let response = next.run(request).await;
    let jar = ensure_csrf_cookie(jar, existing, secure);
    (jar, response).into_response()
}

/// Ensure the jar carries a CSRF cookie, minting one when absent.
fn ensure_csrf_cookie(jar: CookieJar, existing: Option<String>, secure: bool) -> CookieJar {
    match existing {
        Some(_) => jar,
        None => jar.add(build_cookie(CSRF_COOKIE, new_csrf_token(), false, secure)),
    }
}
