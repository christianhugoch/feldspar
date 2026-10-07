//! The admin API, from below it (§13.6): the seam the administrative tools
//! reach the server's own handlers through.
//!
//! Creating an application is not one write. The admin's Create button makes the
//! file store it asked for, saves the record, scaffolds the project with its
//! generated client, creates the agent that will build it and mounts what can be
//! mounted — and the agent half needs `sc-agent`'s trait registry, the mount half
//! the server's `AppMounts`. The tools that let an agent create an application
//! (`sc_app::mcp`) sit far below both, and so does the `admin_copilot` trait that
//! offers them. A second implementation of that sequence for the tools would be
//! the drift `sc_api::mcp`'s module comment was written to prevent.
//!
//! So the tools **call the handler the button calls**, by its endpoint name,
//! through this trait — declared here, speaking JSON, because a [`Catalog`] is
//! what every caller already holds, and installed by `sc-server` when it builds
//! its router: the arrangement [`ModelHost`](crate::ModelHost) already has, for
//! the same reason.
//!
//! **Authorization is the caller's.** The host runs a handler the way the
//! router would after the endpoint's `AuthRequirement` passed; every tool that
//! reaches it is an administrative tool that has already refused a caller below
//! role 1.
//!
//! [`Catalog`]: crate::Catalog

use async_trait::async_trait;
use sc_error::Result;
use serde_json::Value as Json;
use uuid::Uuid;

/// One call of one admin API endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct AdminCall {
    /// The endpoint's name, as the admin API declares it (`createApplication`).
    pub endpoint: String,
    /// Its path parameters, by name.
    pub path_params: Vec<(String, String)>,
    /// The JSON body (`null` for none).
    pub body: Json,
    /// The user the call runs as, by id; `None` for a system run.
    pub user: Option<Uuid>,
}

impl AdminCall {
    /// A call of `endpoint` with `body`, as nobody in particular.
    pub fn new(endpoint: impl Into<String>, body: Json) -> AdminCall {
        AdminCall {
            endpoint: endpoint.into(),
            path_params: Vec::new(),
            body,
            user: None,
        }
    }

    /// Bind a path parameter.
    pub fn param(mut self, name: impl Into<String>, value: impl Into<String>) -> AdminCall {
        self.path_params.push((name.into(), value.into()));
        self
    }

    /// Run as this user.
    pub fn user(mut self, user: Option<Uuid>) -> AdminCall {
        self.user = user;
        self
    }
}

/// The server's admin handlers, by endpoint name.
#[async_trait]
pub trait AdminHost: Send + Sync {
    /// Run the endpoint's handler and return its response body. An `Err` is the
    /// handler's own refusal, in the words the admin UI would have shown.
    async fn call_admin(&self, call: AdminCall) -> Result<Json>;
}
