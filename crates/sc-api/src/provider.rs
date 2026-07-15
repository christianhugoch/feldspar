//! The [`ApiProvider`] trait (design §13.4).
//!
//! An application enables any number of API providers, each mounted on a
//! sub-path: REST, GraphQL, gRPC, tRPC, MCP. Every provider projects the
//! application's shared [`EndpointSet`](crate::EndpointSet) (§13.1) into its own
//! protocol, so the endpoints are described once and each protocol is a
//! *rendering* of them — which is also what lets every provider participate in
//! the shared TypeScript consumer generation.
//!
//! The MVP ships one provider, [`RestProvider`](crate::RestProvider). The trait
//! exists now so the others slot in without a shape change.
//!
//! Providers are transport-agnostic: [`ApiRequest`]/[`ApiResponse`] speak method,
//! path, query, and JSON — not axum types — so `sc-server` maps HTTP onto them
//! and a provider stays testable without a socket.

use async_trait::async_trait;
use sc_auth::User;
use sc_catalog::Catalog;
use sc_error::Result;
use serde_json::Value as Json;
use std::collections::HashMap;

use crate::endpoint::{EndpointSet, Method};

/// A request routed to an API provider.
///
/// `path` is relative to the **application**, not to the provider's mount: a
/// provider mounted at `/api` receiving `/api/posts` sees `path == "/api/posts"`
/// and strips its own mount. That keeps the request as it arrived and leaves
/// mount handling in one place (the provider that owns the mount).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiRequest {
    /// The HTTP method.
    pub method: Method,
    /// The request path within the application, with a leading slash.
    pub path: String,
    /// Query-string parameters.
    pub query: HashMap<String, String>,
    /// The parsed JSON request body ([`Json::Null`] when there was none).
    pub body: Json,
}

impl ApiRequest {
    /// A request with no query parameters and no body.
    pub fn new(method: Method, path: impl Into<String>) -> ApiRequest {
        ApiRequest {
            method,
            path: path.into(),
            query: HashMap::new(),
            body: Json::Null,
        }
    }

    /// A `GET` for `path`.
    pub fn get(path: impl Into<String>) -> ApiRequest {
        ApiRequest::new(Method::Get, path)
    }

    /// Attach a JSON body, returning `self` for chaining.
    pub fn body(mut self, body: Json) -> ApiRequest {
        self.body = body;
        self
    }

    /// Set a query parameter, returning `self` for chaining.
    pub fn query(mut self, key: impl Into<String>, value: impl Into<String>) -> ApiRequest {
        self.query.insert(key.into(), value.into());
        self
    }
}

/// What the transport should do with the caller's session once a handler or
/// provider has run.
///
/// Authentication is a **transport** concern — a session token rides in a cookie
/// for a browser and could ride in a header for another caller — so the code
/// that authenticates says only *what happened to the session*, and whoever owns
/// the wire decides how to carry it. This keeps login/logout pure and testable:
/// `sc-server` turns `Start` into a minted token in a `Set-Cookie`, and `End`
/// into dropping the token and clearing the cookie.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum SessionAction {
    /// Leave the session untouched.
    #[default]
    Keep,
    /// Start a session for this user (login): mint a token and carry it.
    Start(User),
    /// End the current session (logout): drop the token.
    End,
}

/// A provider's response: an HTTP status, a JSON body, and any session change.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiResponse {
    /// The HTTP status code.
    pub status: u16,
    /// The JSON response body.
    pub body: Json,
    /// The session change for the transport to apply, if any.
    pub session: SessionAction,
}

impl ApiResponse {
    /// A `200 OK` carrying `body`, with no session change.
    pub fn ok(body: Json) -> ApiResponse {
        ApiResponse {
            status: 200,
            body,
            session: SessionAction::Keep,
        }
    }

    /// A response with an explicit status and no session change.
    pub fn with_status(status: u16, body: Json) -> ApiResponse {
        ApiResponse {
            status,
            body,
            session: SessionAction::Keep,
        }
    }

    /// An error response: `{"error": message}` with `status`.
    pub fn error(status: u16, message: impl Into<String>) -> ApiResponse {
        ApiResponse {
            status,
            body: serde_json::json!({ "error": message.into() }),
            session: SessionAction::Keep,
        }
    }

    /// A `200 OK` that also starts a session for `user` (login).
    pub fn start_session(user: User, body: Json) -> ApiResponse {
        ApiResponse {
            status: 200,
            body,
            session: SessionAction::Start(user),
        }
    }

    /// A `200 OK` that also ends the current session (logout).
    pub fn end_session(body: Json) -> ApiResponse {
        ApiResponse {
            status: 200,
            body,
            session: SessionAction::End,
        }
    }
}

/// One protocol projection of an application's API (design §13.4).
///
/// All API access flows through the same authorization layer (§7): a provider
/// enforces each endpoint's [`AuthRequirement`](crate::AuthRequirement) against
/// the caller before running it, so an API caller sees exactly what a user of
/// that role would.
#[async_trait]
pub trait ApiProvider: Send + Sync {
    /// The provider's registered name (`rest`, `graphql`, `grpc`, `trpc`, `mcp`).
    fn name(&self) -> &str;

    /// The sub-path within the application this provider is mounted at, e.g.
    /// `/api`.
    fn mount(&self) -> String;

    /// The endpoints this provider projects — the application's `Endpoint` set
    /// rendered into this protocol. The server mounts these and the TypeScript
    /// generator types them.
    fn endpoints(&self) -> &EndpointSet;

    /// Handle one request. `user` is the authenticated caller, or `None` for an
    /// anonymous one; the provider is responsible for enforcing each endpoint's
    /// auth requirement.
    async fn handle(&self, req: ApiRequest, cat: &Catalog, user: Option<&User>)
    -> Result<ApiResponse>;
}
