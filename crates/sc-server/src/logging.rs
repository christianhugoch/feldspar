//! Request logging: what the server says about the requests it serves.
//!
//! One middleware, wrapped **outermost** around the router, so what it reports
//! is what the client got — the status a security-header layer or the CSRF
//! guard produced counts, not the one `dispatch` would have returned on its own.
//! It is a layer rather than a line in `dispatch` for the same reason: the
//! upload route, the backup routes, the language-server upgrade and the app
//! subdomains do not go through `dispatch`, and "all server requests" has to
//! mean all of them.
//!
//! The ladder, from [`sc_log::Verbosity`]:
//!
//! - **Info** — one line per finished request: method, target, status, duration.
//!   This is the rung the Development setting's help text promises requests at.
//! - **Verbose** — plus a line when the request *arrives*, carrying its `Host`.
//!   A request that never finishes has no completion line, so without this a
//!   hanging request is invisible; and the `Host` is what chose the application
//!   (§13.2), which is the first thing to check when the wrong one answered.
//! - **Trace** — plus the request's headers, with anything that authenticates
//!   the caller redacted.
//!
//! Below Info the middleware does nothing at all — it checks the level before
//! it clones so much as the URI, so a server at the default verbosity pays a
//! relaxed atomic load per request.

use std::time::{Duration, Instant};

use axum::extract::Request;
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use sc_log::Verbosity;

/// Header values that are never printed: whoever holds one is the caller, so a
/// log carrying them is a log that can be replayed.
const REDACTED_HEADERS: [&str; 4] = ["cookie", "set-cookie", "authorization", "x-csrf-token"];

/// What replaces a redacted header's value.
const REDACTION: &str = "‹redacted›";

/// Log every request, at whatever level the server is set to.
pub async fn log_requests(req: Request, next: Next) -> Response {
    // The whole middleware is skipped below Info, which is what makes leaving
    // it in the stack free.
    if !sc_log::enabled(Verbosity::Info) {
        return next.run(req).await;
    }

    let method = req.method().clone();
    let target = request_target(&req);
    if sc_log::enabled(Verbosity::Verbose) {
        sc_log::log_verbose!("{}", arrival_line(&method, &target, host_of(req.headers())));
    }
    if sc_log::enabled(Verbosity::Trace) {
        for line in header_lines(req.headers()) {
            sc_log::log_trace!("{line}");
        }
    }

    let started = Instant::now();
    let response = next.run(req).await;
    sc_log::log_info!(
        "{}",
        request_line(&method, &target, response.status(), started.elapsed())
    );
    response
}

/// The path and query as the client asked for them.
fn request_target(req: &Request) -> String {
    req.uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| req.uri().path().to_owned())
}

/// The `Host` header, if the client sent a readable one.
fn host_of(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::HOST).and_then(|v| v.to_str().ok())
}

/// The Info line for a finished request.
///
/// A pure function so the format is pinned by a test rather than by reading
/// stdout: the arrow and the units are what somebody will `grep` and eyeball a
/// thousand times.
pub fn request_line(
    method: &Method,
    target: &str,
    status: StatusCode,
    elapsed: Duration,
) -> String {
    format!(
        "{method} {target} → {} in {:.1}ms",
        status.as_u16(),
        elapsed.as_secs_f64() * 1000.0
    )
}

/// The Verbose line for a request that has just arrived.
pub fn arrival_line(method: &Method, target: &str, host: Option<&str>) -> String {
    match host {
        Some(host) => format!("→ {method} {target} (host {host})"),
        None => format!("→ {method} {target}"),
    }
}

/// The Trace lines for a request's headers, with credentials redacted.
///
/// Redaction is by header name against a fixed list rather than by guessing at
/// values: a session cookie and a CSRF token are exactly as good as a password
/// to whoever reads the log, and this log is a file somebody will paste into an
/// issue.
pub fn header_lines(headers: &HeaderMap) -> Vec<String> {
    headers
        .iter()
        .map(|(name, value)| {
            let name = name.as_str();
            if REDACTED_HEADERS.contains(&name) {
                return format!("  {name}: {REDACTION}");
            }
            match value.to_str() {
                Ok(value) => format!("  {name}: {value}"),
                // A header that is not UTF-8 is still a header that was sent.
                Err(_) => format!("  {name}: ‹{} bytes›", value.len()),
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn a_finished_request_is_one_greppable_line() {
        let line = request_line(
            &Method::POST,
            "/api/updateSettings",
            StatusCode::OK,
            Duration::from_micros(12_340),
        );
        assert_eq!(line, "POST /api/updateSettings → 200 in 12.3ms");
    }

    /// The query string is part of the target: `?table=books` is often the
    /// whole difference between two requests to the same path.
    #[test]
    fn an_arriving_request_carries_its_host_and_query() {
        let req = Request::builder()
            .method(Method::GET)
            .uri("http://blog.example.com/api/listActions?table=books")
            .header(header::HOST, "blog.example.com")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(
            arrival_line(req.method(), &request_target(&req), host_of(req.headers())),
            "→ GET /api/listActions?table=books (host blog.example.com)"
        );
    }

    /// The one thing a header dump must never do.
    #[test]
    fn a_traced_request_does_not_print_its_credentials() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("sc_session=secret"),
        );
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer sk-secret"),
        );
        headers.insert("x-csrf-token", HeaderValue::from_static("csrf-secret"));
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));

        let lines = header_lines(&headers).join("\n");
        assert!(!lines.contains("secret"), "{lines}");
        assert_eq!(lines.matches(REDACTION).count(), 3, "{lines}");
        assert!(lines.contains("accept: application/json"), "{lines}");
    }
}
