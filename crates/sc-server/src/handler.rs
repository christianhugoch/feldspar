//! The handler registry that endpoint dispatch resolves against.
//!
//! [`sc_api::Endpoint`] values are pure data: an endpoint names its handler via
//! [`HandlerRef::Named`](sc_api::HandlerRef), and the server resolves that name
//! here to the async code that runs it. Keeping handlers in a registry (rather
//! than baked into the endpoint values) is what lets the admin API and a
//! runtime-registered application API flow through the *same* dispatch machinery
//! (design §13.1).
//!
//! Handlers stay free of HTTP plumbing: they receive a [`HandlerCtx`] (path/query
//! params, parsed JSON body, and the authenticated [`User`], already checked
//! against the endpoint's auth requirement) and return a [`HandlerResponse`]. A
//! handler never touches cookies directly — instead it asks the dispatcher to
//! start or end the session via [`SessionAction`], so login/logout stay pure.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sc_auth::User;
use sc_error::Result;
use serde_json::Value;

/// A boxed, `Send` future — the return shape every handler produces.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// A registered handler: given a [`HandlerCtx`], produce a [`HandlerResponse`].
pub type HandlerFn = Arc<dyn Fn(HandlerCtx) -> BoxFuture<Result<HandlerResponse>> + Send + Sync>;

/// Everything a handler needs about the request, with authorization already
/// enforced by the dispatcher.
#[derive(Debug, Clone)]
pub struct HandlerCtx {
    /// Path parameters captured from the route pattern (e.g. `table`, `id`).
    pub path_params: HashMap<String, String>,
    /// Query-string parameters.
    pub query: HashMap<String, String>,
    /// The parsed JSON request body ([`Value::Null`] when there was no body).
    pub body: Value,
    /// The authenticated user, if any. Presence/role already satisfy the
    /// endpoint's [`AuthRequirement`](sc_api::AuthRequirement).
    pub user: Option<User>,
}

impl HandlerCtx {
    /// A required path parameter, or an [`Error::Invalid`](sc_error::Error) if
    /// absent (a routing/registration bug rather than user input).
    pub fn path_param(&self, name: &str) -> Result<&str> {
        self.path_params
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| sc_error::Error::invalid(format!("missing path parameter `{name}`")))
    }
}

/// What the dispatcher should do with the session cookie after a handler runs.
#[derive(Debug, Clone, Default)]
pub enum SessionAction {
    /// Leave the session cookie untouched.
    #[default]
    Keep,
    /// Start a session for this user (login): mint a token and set the cookie.
    Start(User),
    /// End the current session (logout): drop the token and clear the cookie.
    End,
}

/// A handler's result: a JSON body, an HTTP status, and an optional session
/// action for the dispatcher to apply.
#[derive(Debug, Clone)]
pub struct HandlerResponse {
    /// The JSON response body.
    pub body: Value,
    /// The HTTP status code (defaults to `200`).
    pub status: u16,
    /// The session change to apply, if any.
    pub session: SessionAction,
}

impl HandlerResponse {
    /// A `200 OK` response with a JSON body and no session change.
    pub fn ok(body: Value) -> HandlerResponse {
        HandlerResponse {
            body,
            status: 200,
            session: SessionAction::Keep,
        }
    }

    /// A `200 OK` response that also starts a session for `user` (login).
    pub fn start_session(user: User, body: Value) -> HandlerResponse {
        HandlerResponse {
            body,
            status: 200,
            session: SessionAction::Start(user),
        }
    }

    /// A `200 OK` response that also ends the current session (logout).
    pub fn end_session(body: Value) -> HandlerResponse {
        HandlerResponse {
            body,
            status: 200,
            session: SessionAction::End,
        }
    }

    /// Override the HTTP status (e.g. `201` for a created resource).
    pub fn with_status(mut self, status: u16) -> HandlerResponse {
        self.status = status;
        self
    }
}

/// A name → handler map. Endpoints are dispatched by resolving their
/// [`HandlerRef::Named`](sc_api::HandlerRef) here; an endpoint whose handler is
/// absent (or is guest code / SQL) yields `501 Not Implemented`.
#[derive(Clone, Default)]
pub struct HandlerRegistry {
    handlers: HashMap<String, HandlerFn>,
}

impl HandlerRegistry {
    /// An empty registry.
    pub fn new() -> HandlerRegistry {
        HandlerRegistry::default()
    }

    /// Register an async handler under `name`, replacing any previous one.
    ///
    /// Accepts any async closure `Fn(HandlerCtx) -> Future<Output = Result<HandlerResponse>>`.
    pub fn register<F, Fut>(&mut self, name: impl Into<String>, handler: F) -> &mut HandlerRegistry
    where
        F: Fn(HandlerCtx) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<HandlerResponse>> + Send + 'static,
    {
        let boxed: HandlerFn = Arc::new(move |ctx| Box::pin(handler(ctx)));
        self.handlers.insert(name.into(), boxed);
        self
    }

    /// Look up a handler by name.
    pub fn get(&self, name: &str) -> Option<&HandlerFn> {
        self.handlers.get(name)
    }

    /// The number of registered handlers.
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}
