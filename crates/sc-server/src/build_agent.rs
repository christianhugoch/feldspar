//! The routes a build host's agent talks to (`TODO-build-hosts.md`, design
//! §13.3 "Build hosts"): `feldspar build-agent` on a Mac asks this server for
//! work, and the server never calls it.
//!
//! They authenticate by the build host's bearer token and by **nothing else**:
//! a session cookie is ignored, and an API token is refused, as a build host
//! token is everywhere else. A bearer request is exempt from the CSRF check
//! (`security::is_bearer_authenticated`), which is safe for the reason it is
//! on `/mcp`.
//!
//! B1 has one route, the heartbeat. The agent says who it is and whether its
//! machine has what its targets require; the server keeps that, so the admin
//! sees the host online, and answers with the targets and their requirements.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use sc_app::{
    AgentReport, authenticate_build_host, check_protocol, heartbeat_answer, record_heartbeat,
};
use serde_json::{Value as Json, json};

use crate::router::AppState;

/// The heartbeat: `POST` with an [`AgentReport`], answered with
/// [`heartbeat_answer`].
pub const BUILD_AGENT_HEARTBEAT_ROUTE: &str = "/build-agent/heartbeat";

/// Ceiling on a heartbeat's body: a report is a few hundred bytes.
const MAX_BODY_BYTES: usize = 64 * 1024;

pub(crate) async fn heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(catalog) = state.apps.catalog().cloned() else {
        return refusal(StatusCode::NOT_FOUND, "this server has no build hosts");
    };
    let Some(presented) = crate::security::bearer_token(&headers) else {
        return refusal(
            StatusCode::UNAUTHORIZED,
            "a build agent authenticates by `Authorization: Bearer <token>`: the token \
             shown when the host was added under Settings → Build hosts",
        );
    };
    let host = match authenticate_build_host(&catalog, &presented).await {
        Ok(host) => host,
        Err(e) => return refusal(StatusCode::UNAUTHORIZED, &e.to_string()),
    };
    if body.len() > MAX_BODY_BYTES {
        return refusal(
            StatusCode::PAYLOAD_TOO_LARGE,
            "a heartbeat is at most 64 KiB",
        );
    }
    let report = match serde_json::from_slice::<Json>(&body)
        .map_err(|_| sc_error::Error::invalid("a heartbeat's body is JSON"))
        .and_then(|value| AgentReport::from_json(&value))
    {
        Ok(report) => report,
        Err(e) => return refusal(StatusCode::BAD_REQUEST, &e.to_string()),
    };

    // An agent on another protocol is turned away, and the reason is kept in
    // its report, so the admin sees why the host is idle.
    let (status, stored, answer) = match check_protocol(&report) {
        Ok(()) => (StatusCode::OK, report.to_json(), heartbeat_answer(&host)),
        Err(e) => {
            let why = e.to_string();
            let mut stored = report.to_json();
            stored["refused"] = Json::String(why.clone());
            (StatusCode::CONFLICT, stored, json!({ "error": why }))
        }
    };
    if let Err(e) = record_heartbeat(&catalog, host.id, &stored).await {
        sc_log::log_error!(
            "build agent `{}`: keeping its heartbeat failed: {e}",
            host.name
        );
        return refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the heartbeat could not be kept",
        );
    }
    (status, axum::Json(answer)).into_response()
}

fn refusal(status: StatusCode, why: &str) -> Response {
    (status, axum::Json(json!({ "error": why }))).into_response()
}
