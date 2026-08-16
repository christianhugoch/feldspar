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
//! - **Verbose and above** — plus a line when the request *arrives*, carrying
//!   its `Host`. A request that never finishes has no completion line, so
//!   without this a hanging request is invisible; and the `Host` is what chose
//!   the application (§13.2), which is the first thing to check when the wrong
//!   one answered.
//!
//! **`trace` adds nothing here**, deliberately. It did, briefly: every request's
//! headers, credentials redacted. Two lines of signal per request arrived
//! wrapped in eight of `accept-encoding` and `sec-fetch-mode`, which made the
//! one level that carries the model transcripts — the reason to turn `trace` on
//! at all — unreadable. A header dump is a debugging need better served by the
//! browser's own network panel, which already has it.
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
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
}
