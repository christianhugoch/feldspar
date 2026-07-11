//! The reified endpoint model (design §13.1).
//!
//! Endpoints are Rust **values**, not just handler functions: an [`Endpoint`]
//! records its HTTP method, its path (literal segments plus typed parameters),
//! a [`TypeSchema`] for the request body and the response value, an
//! [`AuthRequirement`], and a [`HandlerRef`] naming the code that runs it. This
//! is what lets a route be fully described (and typed for consumers) even when
//! it is registered at runtime and is not known at compile time.
//!
//! An [`EndpointSet`] is the runtime registry those values live in. The admin
//! API (see [`crate::admin`]) is a fixed set of `Endpoint` constants registered
//! through this **same** machinery, so the admin SPA consumes a generated typed
//! client exactly as an application would.

use crate::schema::{TypeSchema, ValueType};
use serde::{Deserialize, Serialize};

/// An HTTP method. A small self-contained enum so the endpoint model stays free
/// of the web framework — `sc-server` maps these onto axum's `Method`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Method {
    /// HTTP `GET`.
    Get,
    /// HTTP `POST`.
    Post,
    /// HTTP `PUT`.
    Put,
    /// HTTP `PATCH`.
    Patch,
    /// HTTP `DELETE`.
    Delete,
}

impl Method {
    /// The uppercase method token (`"GET"`, `"POST"`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }

    /// Whether this method conventionally carries a request body.
    pub fn has_body(self) -> bool {
        matches!(self, Method::Post | Method::Put | Method::Patch)
    }
}

/// One segment of a [`PathSpec`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathSegment {
    /// A literal path segment, e.g. `tables`.
    Literal(String),
    /// A typed path parameter, e.g. `{id}` typed as a UUID.
    Param {
        /// The parameter name (the `{name}` placeholder).
        name: String,
        /// The parameter's scalar type — used to type the generated client.
        ty: ValueType,
    },
}

/// A request path: an ordered list of literal segments and typed parameters,
/// e.g. `/tables/{table}/rows` where `table` is typed. Built fluently with
/// [`PathSpec::root`] + [`lit`](PathSpec::lit) + [`param`](PathSpec::param).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathSpec {
    /// The ordered segments.
    pub segments: Vec<PathSegment>,
}

impl PathSpec {
    /// An empty path (`/`).
    pub fn root() -> PathSpec {
        PathSpec {
            segments: Vec::new(),
        }
    }

    /// Append one or more literal segments. A `/`-containing string is split, so
    /// `.lit("api/tables")` adds two segments.
    pub fn lit(mut self, s: &str) -> PathSpec {
        for part in s.split('/').filter(|p| !p.is_empty()) {
            self.segments.push(PathSegment::Literal(part.to_owned()));
        }
        self
    }

    /// Append a typed path parameter.
    pub fn param(mut self, name: &str, ty: ValueType) -> PathSpec {
        self.segments.push(PathSegment::Param {
            name: name.to_owned(),
            ty,
        });
        self
    }

    /// The typed parameters, in path order.
    pub fn params(&self) -> impl Iterator<Item = (&str, ValueType)> {
        self.segments.iter().filter_map(|seg| match seg {
            PathSegment::Param { name, ty } => Some((name.as_str(), *ty)),
            PathSegment::Literal(_) => None,
        })
    }

    /// Render the `matchit`/axum route pattern, e.g. `/tables/{id}/rows`.
    pub fn pattern(&self) -> String {
        let mut out = String::from("/");
        for (i, seg) in self.segments.iter().enumerate() {
            if i > 0 {
                out.push('/');
            }
            match seg {
                PathSegment::Literal(s) => out.push_str(s),
                PathSegment::Param { name, .. } => {
                    out.push('{');
                    out.push_str(name);
                    out.push('}');
                }
            }
        }
        out
    }
}

/// The authorization required to call an endpoint, enforced by the server via
/// the roles of §7. Roles run `1..=100` with **lower = more privilege**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthRequirement {
    /// Open to anyone, authenticated or not.
    Public,
    /// Any authenticated user.
    LoggedIn,
    /// A user whose role is at least as privileged as `min_role` (i.e. a role
    /// number `<= min_role`), per `User::meets_role`.
    MinRole(u8),
}

impl AuthRequirement {
    /// Requires the administrator role (role `1`).
    pub fn admin() -> AuthRequirement {
        AuthRequirement::MinRole(sc_auth::ROLE_ADMIN)
    }
}

/// A reference to the code that runs an endpoint. The endpoint model itself is
/// framework-agnostic, so it names the handler rather than holding it; the
/// server resolves [`Named`](HandlerRef::Named) handlers through its dispatch
/// table, and custom application routes carry guest code or SQL directly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandlerRef {
    /// A built-in Rust handler, resolved by name at mount time.
    Named(String),
    /// Custom developer-authored guest code (language tag + source). Stubbed for
    /// the MVP; carried so the model is complete (design §13.4).
    GuestCode {
        /// The language the source is written in.
        language: String,
        /// The source code.
        source: String,
    },
    /// A custom SQL query. Stubbed for the MVP (design §13.4).
    Sql(String),
}

impl HandlerRef {
    /// A named built-in handler.
    pub fn named(name: impl Into<String>) -> HandlerRef {
        HandlerRef::Named(name.into())
    }
}

/// A single HTTP endpoint, described as a value (design §13.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Endpoint {
    /// A unique operation name; also the generated TypeScript client method name.
    pub name: String,
    /// The HTTP method.
    pub method: Method,
    /// The request path (literal segments + typed parameters).
    pub path: PathSpec,
    /// The request body schema (an empty struct means "no body").
    pub input: TypeSchema,
    /// The response value schema.
    pub output: TypeSchema,
    /// The authorization required to call it.
    pub auth: AuthRequirement,
    /// The handler that runs it.
    pub handler: HandlerRef,
}

impl Endpoint {
    /// Start a new endpoint. Defaults: empty input/output, [`LoggedIn`](AuthRequirement::LoggedIn)
    /// auth, and a [`Named`](HandlerRef::Named) handler equal to `name`. Override
    /// with the builder setters.
    pub fn new(name: impl Into<String>, method: Method, path: PathSpec) -> Endpoint {
        let name = name.into();
        let handler = HandlerRef::Named(name.clone());
        Endpoint {
            name,
            method,
            path,
            input: TypeSchema::empty(),
            output: TypeSchema::empty(),
            auth: AuthRequirement::LoggedIn,
            handler,
        }
    }

    /// Set the request body schema.
    pub fn input(mut self, schema: TypeSchema) -> Endpoint {
        self.input = schema;
        self
    }

    /// Set the response value schema.
    pub fn output(mut self, schema: TypeSchema) -> Endpoint {
        self.output = schema;
        self
    }

    /// Set the authorization requirement.
    pub fn auth(mut self, auth: AuthRequirement) -> Endpoint {
        self.auth = auth;
        self
    }

    /// Set the handler reference (defaults to `Named(name)`).
    pub fn handler(mut self, handler: HandlerRef) -> Endpoint {
        self.handler = handler;
        self
    }
}

/// A runtime registry of [`Endpoint`] values.
///
/// The set is a plain runtime value: application and custom routes are
/// registered here at runtime (they are not known at compile time), and the
/// admin API is built as fixed constants and registered through the *same*
/// machinery. `sc-server` mounts a set as JSON routes and the TypeScript
/// generator emits a typed client from it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EndpointSet {
    endpoints: Vec<Endpoint>,
}

impl EndpointSet {
    /// An empty set.
    pub fn new() -> EndpointSet {
        EndpointSet {
            endpoints: Vec::new(),
        }
    }

    /// Register an endpoint, returning `&mut self` for chaining. Panics on a
    /// duplicate `name`, since names must be unique to key the client methods
    /// and the server dispatch table.
    pub fn register(&mut self, endpoint: Endpoint) -> &mut EndpointSet {
        assert!(
            !self.endpoints.iter().any(|e| e.name == endpoint.name),
            "duplicate endpoint name `{}`",
            endpoint.name
        );
        self.endpoints.push(endpoint);
        self
    }

    /// Register an endpoint, consuming and returning the set (builder style).
    pub fn with(mut self, endpoint: Endpoint) -> EndpointSet {
        self.register(endpoint);
        self
    }

    /// Iterate the registered endpoints in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &Endpoint> {
        self.endpoints.iter()
    }

    /// The number of registered endpoints.
    pub fn len(&self) -> usize {
        self.endpoints.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.endpoints.is_empty()
    }

    /// Find an endpoint by name.
    pub fn find(&self, name: &str) -> Option<&Endpoint> {
        self.endpoints.iter().find(|e| e.name == name)
    }
}

impl<'a> IntoIterator for &'a EndpointSet {
    type Item = &'a Endpoint;
    type IntoIter = std::slice::Iter<'a, Endpoint>;

    fn into_iter(self) -> Self::IntoIter {
        self.endpoints.iter()
    }
}

impl FromIterator<Endpoint> for EndpointSet {
    fn from_iter<I: IntoIterator<Item = Endpoint>>(iter: I) -> EndpointSet {
        let mut set = EndpointSet::new();
        for e in iter {
            set.register(e);
        }
        set
    }
}
