//! The GraphQL [`ApiProvider`] (design §13.4).
//!
//! REST cannot express a *shape* a caller chooses. "For each department, the
//! number of employees earning below 50 000" is not a route; it is a question,
//! and answering it over REST means either inventing a route per question or
//! fetching every employee and counting in the browser. That is what this
//! provider is for, and it is why the milestone exists.
//!
//! It sits beside [`RestProvider`](crate::RestProvider) rather than replacing
//! it: an application enables both, on different mounts, over the same tables.
//! Two endpoints, because that is genuinely all a GraphQL API is —
//! `POST {mount}` carrying `{query, variables, operationName}`, and
//! `GET {mount}/schema.graphql` serving the SDL — so the provider participates
//! in the shared endpoint model and the TypeScript generation exactly as
//! everything else does.
//!
//! **Not a second data path.** Nothing here builds its own `SELECT`. Reads go
//! through `sc-api`'s row and ownership entry points and writes through the row
//! layer's own, so coercion, rich-type validation, `File`-field checks, table
//! events and §7.3 apply *because they are the same code* — not because this
//! provider remembered to do them. (Phases 3 onward; this phase serves the
//! contract and refuses every field honestly.)
//!
//! **One schema per application, not per role** (decision 4). Hasura compiles a
//! schema per role; we authorize at resolve time and refuse with a GraphQL error
//! naming the table. The schema describes what the *application* exposes,
//! exactly as its [`EndpointSet`](crate::EndpointSet) does — which is also why
//! both endpoints are [`Public`](crate::AuthRequirement::Public): the gate is
//! the table's, at resolve time, not the endpoint's.
//!
//! The three pieces: [`names`] derives every GraphQL name (and reports what it
//! could not name), [`types`] maps a column onto the wire, and [`build`] folds
//! the tables into an `async_graphql::dynamic::Schema`.

mod build;
pub mod names;
#[cfg(test)]
mod testing;
mod types;

use async_graphql::dynamic::Schema;
use async_trait::async_trait;
use sc_auth::User;
use sc_catalog::{Catalog, Table};
use sc_error::Result;
use serde_json::{Value as Json, json};

use crate::endpoint::{AuthRequirement, Endpoint, EndpointSet, HandlerRef, Method, PathSpec};
use crate::provider::{ApiProvider, ApiRequest, ApiResponse};
use crate::schema::{StructField, TypeSchema};

pub use names::SchemaNames;

/// The provider's registered name.
pub const GRAPHQL_PROVIDER: &str = "graphql";

/// The provider's default mount.
pub const DEFAULT_MOUNT: &str = "/graphql";

/// The endpoint (and generated client method) names. Fixed rather than derived
/// from a table, so they cannot collide with the REST provider's `listPosts` /
/// `login` in an application's combined endpoint set.
const QUERY_ENDPOINT: &str = "graphqlQuery";
const SCHEMA_ENDPOINT: &str = "graphqlSchema";

/// The last path segment the SDL is served at.
const SCHEMA_SEGMENT: &str = "schema.graphql";

/// A GraphQL projection of an application's tables (design §13.4).
pub struct GraphqlProvider {
    mount: String,
    endpoints: EndpointSet,
    schema: Schema,
    diagnostics: Vec<String>,
}

impl GraphqlProvider {
    /// Project `tables` into a GraphQL API mounted at `mount`.
    ///
    /// The tables are the application's declared subset, already resolved
    /// against the catalog by the caller — the same arrangement
    /// [`RestProvider::project`](crate::RestProvider::project) has, and for the
    /// same reason: this crate sits below `sc-app`.
    ///
    /// A schema that cannot be built is a **mount failure** naming the table
    /// that caused it. An application whose API is half-described is worse than
    /// one that refuses to start: the half nobody notices is the half that is
    /// wrong.
    pub fn project(mount: impl Into<String>, tables: &[Table]) -> Result<GraphqlProvider> {
        let mount = normalize_mount(&mount.into());
        let names = SchemaNames::derive(tables);
        let schema = build::build_schema(tables, &names)?;

        let mut endpoints = EndpointSet::new();
        endpoints.register(
            Endpoint::new(QUERY_ENDPOINT, Method::Post, PathSpec::root().lit(&mount))
                .input(TypeSchema::struct_of([
                    StructField::new("query", TypeSchema::text()),
                    StructField::new("variables", TypeSchema::optional(TypeSchema::json())),
                    StructField::new("operationName", TypeSchema::optional(TypeSchema::text())),
                ]))
                .output(TypeSchema::json())
                // The endpoint is open because the *rows* are gated: a table's
                // role floor and its ownership formula decide at resolve time,
                // exactly as they do for a REST table with a formula. An
                // endpoint-level gate here would be a second, coarser rule that
                // disagreed with the first.
                .auth(AuthRequirement::Public)
                .handler(HandlerRef::named(QUERY_ENDPOINT)),
        );
        endpoints.register(
            Endpoint::new(
                SCHEMA_ENDPOINT,
                Method::Get,
                PathSpec::root().lit(&mount).lit(SCHEMA_SEGMENT),
            )
            // The SDL is text, not JSON — it is what a build feeds `gql.tada`
            // and what a developer reads.
            .binary_output()
            // Introspection is on, so the SDL is not a secret being kept: it
            // describes the tables the application already exposes over REST.
            .auth(AuthRequirement::Public)
            .handler(HandlerRef::named(SCHEMA_ENDPOINT)),
        );

        Ok(GraphqlProvider {
            mount,
            endpoints,
            schema,
            diagnostics: names.diagnostics().to_vec(),
        })
    }

    /// The schema's SDL — what `GET {mount}/schema.graphql` serves and what the
    /// build writes beside the generated client (Phase 8).
    pub fn sdl(&self) -> String {
        self.schema.sdl()
    }

    /// Everything the projection had to leave out, and why: a table or field
    /// whose name GraphQL cannot spell, or one that would have collided. Not an
    /// error — an application should not lose its whole API to one unnameable
    /// table — but not silent either.
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    /// Execute one GraphQL request body.
    async fn execute(&self, body: &Json) -> ApiResponse {
        let Some(query) = body.get("query").and_then(Json::as_str) else {
            // A missing document is not a GraphQL error (there is no document to
            // report it against), so it is an ordinary request error.
            return ApiResponse::error(400, "a GraphQL request needs a `query` field");
        };
        let mut request = async_graphql::Request::new(query);
        if let Some(variables) = body.get("variables").filter(|v| !v.is_null()) {
            request = request.variables(async_graphql::Variables::from_json(variables.clone()));
        }
        if let Some(name) = body.get("operationName").and_then(Json::as_str) {
            request = request.operation_name(name);
        }
        let response = self.schema.execute(request).await;
        // The legacy `application/json` rule: 200 with the errors in the body.
        // `application/graphql-response+json` needs content negotiation, and
        // `ApiRequest` carries no headers to negotiate with.
        match serde_json::to_value(&response) {
            Ok(json) => ApiResponse::ok(json),
            Err(e) => ApiResponse::error(500, format!("could not serialise the response: {e}")),
        }
    }
}

#[async_trait]
impl ApiProvider for GraphqlProvider {
    fn name(&self) -> &str {
        GRAPHQL_PROVIDER
    }

    fn mount(&self) -> String {
        self.mount.clone()
    }

    fn endpoints(&self) -> &EndpointSet {
        &self.endpoints
    }

    async fn handle(
        &self,
        req: ApiRequest,
        _cat: &Catalog,
        _user: Option<&User>,
    ) -> Result<ApiResponse> {
        // The catalog and the caller join this in Phase 3, where there is a row
        // to read and a rule to enforce; today the two endpoints are the
        // document and the schema, and neither consults either.
        Ok(self.route(&req).await)
    }
}

impl GraphqlProvider {
    /// Route one request to the SDL or to the executor.
    async fn route(&self, req: &ApiRequest) -> ApiResponse {
        let schema_path = format!("{}/{SCHEMA_SEGMENT}", self.mount.trim_end_matches('/'));
        if req.method == Method::Get && req.path == schema_path {
            return ApiResponse::file(self.sdl(), "text/plain; charset=utf-8");
        }
        if req.path != self.mount {
            return ApiResponse::error(404, format!("no GraphQL endpoint at `{}`", req.path));
        }
        if req.method != Method::Post {
            return ApiResponse::with_status(
                405,
                json!({ "error": "a GraphQL request is a POST" }),
            );
        }
        self.execute(&req.body).await
    }
}

/// Normalise a mount to a single leading slash and no trailing slash — the same
/// rule the REST provider applies, so two providers' mounts are comparable.
fn normalize_mount(raw: &str) -> String {
    let trimmed = raw.trim_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else {
        format!("/{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphql::testing::{id_field, plain_field, table_of};

    fn provider() -> GraphqlProvider {
        GraphqlProvider::project(
            DEFAULT_MOUNT,
            &[table_of(
                "departments",
                vec![id_field(), plain_field("name")],
            )],
        )
        .expect("projects")
    }

    #[test]
    fn the_provider_projects_exactly_two_endpoints() {
        let p = provider();
        assert_eq!(p.name(), "graphql");
        assert_eq!(p.mount(), "/graphql");
        assert_eq!(p.endpoints().len(), 2);

        let query = p.endpoints().find("graphqlQuery").expect("query endpoint");
        assert_eq!(query.method, Method::Post);
        assert_eq!(query.path.pattern(), "/graphql");
        assert_eq!(query.auth, AuthRequirement::Public);

        let sdl = p.endpoints().find("graphqlSchema").expect("sdl endpoint");
        assert_eq!(sdl.method, Method::Get);
        assert_eq!(sdl.path.pattern(), "/graphql/schema.graphql");
        assert!(sdl.binary_output);
    }

    #[test]
    fn the_endpoint_names_cannot_collide_with_the_rest_providers() {
        // An application collects every provider's endpoints into one set, and a
        // duplicate name there is a configuration error. The REST projection
        // names its operations after tables and its auth after itself; these two
        // are neither.
        let rest = crate::RestProvider::project(
            "/api",
            &[table_of(
                "departments",
                vec![id_field(), plain_field("name")],
            )],
        );
        let graphql = provider();
        for ep in graphql.endpoints().iter() {
            assert!(
                rest.endpoints().find(&ep.name).is_none(),
                "`{}` collides with the REST projection",
                ep.name
            );
        }
    }

    #[test]
    fn a_mount_is_normalised_the_way_rests_is() {
        let p = GraphqlProvider::project("graphql/", &[table_of("t", vec![id_field()])])
            .expect("projects");
        assert_eq!(p.mount(), "/graphql");
    }

    /// Run `route` on a request, on a current-thread runtime.
    fn route(p: &GraphqlProvider, req: ApiRequest) -> ApiResponse {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(p.route(&req))
    }

    #[test]
    fn the_sdl_endpoint_serves_the_schema_as_text() {
        let response = route(&provider(), ApiRequest::get("/graphql/schema.graphql"));
        let raw = response.raw.expect("raw body");
        assert_eq!(raw.content_type, "text/plain; charset=utf-8");
        let sdl = String::from_utf8(raw.bytes.to_vec()).expect("utf-8");
        assert!(sdl.contains("type Departments {"), "{sdl}");
    }

    #[test]
    fn a_query_without_a_document_is_a_request_error_not_a_graphql_one() {
        let response = route(
            &provider(),
            ApiRequest::new(Method::Post, "/graphql").body(json!({})),
        );
        assert_eq!(response.status, 400);
    }

    #[test]
    fn a_graphql_error_is_a_200_with_the_errors_in_the_body() {
        // The legacy `application/json` rule, which is what applies until
        // `ApiRequest` carries an `Accept` header to negotiate with.
        let response = route(
            &provider(),
            ApiRequest::new(Method::Post, "/graphql")
                .body(json!({ "query": "{ departments { id } }" })),
        );
        assert_eq!(response.status, 200);
        let errors = response.body.get("errors").expect("errors in the body");
        assert!(
            format!("{errors}").contains("not implemented"),
            "{}",
            response.body
        );
    }

    #[test]
    fn a_get_on_the_document_endpoint_is_a_405_and_a_stray_path_a_404() {
        let p = provider();
        assert_eq!(route(&p, ApiRequest::get("/graphql")).status, 405);
        assert_eq!(route(&p, ApiRequest::get("/graphql/nope")).status, 404);
    }
}
