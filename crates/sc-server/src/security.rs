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

/// Whether this request authenticates by a bearer credential rather than by the
/// session cookie — and is therefore exempt from the CSRF check below.
///
/// **The exemption is written as a property of the request, not as a path**
/// (design §13.6). `/mcp` is the one route that has it today, and a
/// `path == "/mcp"` test here would be one refactor away from being wrong: it
/// would still exempt a route that had since started honouring cookies, and it
/// would not exempt the next bearer-authenticated route somebody adds.
///
/// It is safe for exactly one reason, and the reason is the whole of it: a page
/// a user visits **cannot set an `Authorization` header on a cross-origin
/// request** without a preflight this server does not answer. A forged
/// cross-site request therefore cannot carry one, so a request that does carry
/// one was not forged — which is the property the double-submit cookie is
/// standing in for everywhere else. Nothing is given up on the routes that do
/// honour cookies, because they ignore this header and are still checked.
fn is_bearer_authenticated(request: &Request) -> bool {
    crate::mcp::bearer_token(request.headers()).is_some()
}

/// The v1 spelling of the CSRF header: what `saltcorn.js` sends on every ajax
/// POST a Saltcorn UI view makes (`"CSRF-Token": _sc_globalCsrf`).
pub const V1_CSRF_HEADER: &str = "csrf-token";

/// The form field a server-rendered form carries the token in — v1's
/// `renderForm(form, req.csrfToken())` writes `<input name="_csrf">`.
pub const CSRF_FORM_FIELD: &str = "_csrf";

/// The largest form body the CSRF check reads to find [`CSRF_FORM_FIELD`]. A
/// form is fields, not files; an upload is not form-encoded and is never read
/// here.
const MAX_CSRF_FORM_BODY: usize = 2 * 1024 * 1024;

/// This browser's CSRF token, as the request's handler sees it: the cookie's
/// value, or the one minted for it on first contact — which the response then
/// sets, so a page rendered on first contact carries the token its cookie will
/// hold (TODO "Saltcorn UI" 6.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsrfToken(pub String);

/// CSRF middleware implementing the double-submit-cookie check.
///
/// On a mutating request the token must equal the existing `sc_csrf` cookie,
/// else the request is rejected `403`. The token may come in any of three
/// places, all the same check against the same cookie:
///
/// - the `x-csrf-token` header — the admin SPA and the generated client;
/// - the `csrf-token` header — v1's `saltcorn.js`, in a Saltcorn UI view;
/// - an `application/x-www-form-urlencoded` body's `_csrf` field — a Saltcorn UI
///   form submitted by the browser, which cannot set a header.
///
/// A cross-site page can make the browser send the cookie but cannot read it,
/// so it can put the token in none of them. Every response ensures the cookie is
/// set (minting one on first contact) and the handler is told the token in a
/// [`CsrfToken`] extension. `State<bool>` carries the `Secure` cookie flag.
///
/// **Bearer requests are exempt** (see [`is_bearer_authenticated`]).
pub(crate) async fn csrf_middleware(
    State(secure): State<bool>,
    jar: CookieJar,
    request: Request,
    next: Next,
) -> Response {
    let existing = jar.get(CSRF_COOKIE).map(|c| c.value().to_owned());
    let mut request = request;

    if is_mutating(request.method()) && !is_bearer_authenticated(&request) {
        let header = [CSRF_HEADER, V1_CSRF_HEADER]
            .iter()
            .find_map(|name| request.headers().get(*name))
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let mut valid = matches!((&existing, &header), (Some(cookie), Some(hdr)) if cookie == hdr);
        if !valid && header.is_none() && is_form(&request) {
            let (parts, body) = request.into_parts();
            let bytes = match axum::body::to_bytes(body, MAX_CSRF_FORM_BODY).await {
                Ok(bytes) => bytes,
                Err(_) => {
                    return (StatusCode::PAYLOAD_TOO_LARGE, "form body too large").into_response();
                }
            };
            valid = existing
                .as_deref()
                .is_some_and(|cookie| form_field(&bytes, CSRF_FORM_FIELD).as_deref() == Some(cookie));
            request = Request::from_parts(parts, axum::body::Body::from(bytes));
        }
        if !valid {
            // Reject, but still hand out a token so a first-contact client can
            // read it and retry successfully.
            let jar = ensure_csrf_cookie(jar, existing, secure);
            return (StatusCode::FORBIDDEN, jar, "CSRF token missing or invalid").into_response();
        }
    }

    let token = existing.clone().unwrap_or_else(new_csrf_token);
    request.extensions_mut().insert(CsrfToken(token.clone()));
    let response = next.run(request).await;
    let jar = match existing {
        Some(_) => jar,
        None => jar.add(build_cookie(CSRF_COOKIE, token, false, secure)),
    };
    (jar, response).into_response()
}

/// Whether the request's body is a URL-encoded form.
fn is_form(request: &Request) -> bool {
    request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/x-www-form-urlencoded"))
}

/// The first value of `name` in a URL-encoded form body.
fn form_field(body: &[u8], name: &str) -> Option<String> {
    crate::router::parse_form(&String::from_utf8_lossy(body))
        .into_iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v)
}

/// Ensure the jar carries a CSRF cookie, minting one when absent.
fn ensure_csrf_cookie(jar: CookieJar, existing: Option<String>, secure: bool) -> CookieJar {
    match existing {
        Some(_) => jar,
        None => jar.add(build_cookie(CSRF_COOKIE, new_csrf_token(), false, secure)),
    }
}
