//! Endpoint model (typed Rust values) + API providers + TypeScript consumer
//! generation (layer 8; technical design §13.1).
//!
//! This crate reifies HTTP endpoints as **data**: an [`Endpoint`] records its
//! method, typed path, request/response [`TypeSchema`], auth requirement, and a
//! handler reference — described well enough to be dispatched by the server and
//! typed for a consumer even when registered at runtime. Endpoints live in an
//! [`EndpointSet`] runtime registry; the admin API ([`admin::admin_endpoints`])
//! is a fixed set built through the *same* machinery. [`generate_client`] emits a
//! type-checked TypeScript client from any set, so the server contract and its
//! consumers cannot drift.
//!
//! [`ApiProvider`] (design §13.4) is the other half: an application enables any
//! number of providers — REST, GraphQL, gRPC, tRPC, MCP — each mounted on a
//! sub-path and each *projecting* the shared endpoint set into its protocol.
//! [`RestProvider`] is the MVP's one provider. Because a provider's projection is
//! an ordinary [`EndpointSet`], an application's typed client comes from the same
//! [`generate_client`] the admin API uses.
//!
//! [`rows`] holds the table row CRUD both the admin API's handlers and a
//! provider run, [`filter`] the comparison vocabulary every filtering syntax
//! lowers through, [`schema_edit`] the schema-changing rule the admin handlers and
//! an agent's `manage_table_admin` trait both go through (§3.3, §11.3), [`auth`] the login vocabulary they share, and [`convert`] the
//! bridge from JSON to the query layer's `Value` — all kept here, below every API
//! surface, so there is one implementation rather than one per protocol.

pub mod auth;
pub mod convert;
pub mod filter;
pub mod rows;
pub mod schema_edit;

mod admin;
mod endpoint;
mod graphql;
mod ownership;
mod provider;
mod rest;
mod schema;
mod typescript;

pub use admin::{ADMIN_API_PREFIX, admin_endpoints};
pub use endpoint::{
    AuthRequirement, Endpoint, EndpointSet, HandlerRef, Method, PathSegment, PathSpec, QueryParam,
};
pub use graphql::{
    CFG_AGGREGATES as GRAPHQL_CFG_AGGREGATES, DEFAULT_FILE_MOUNT as GRAPHQL_DEFAULT_FILE_MOUNT,
    DEFAULT_MAX_COMPLEXITY, DEFAULT_MAX_DEPTH, DEFAULT_MOUNT as GRAPHQL_DEFAULT_MOUNT,
    DEFAULT_ROW_CAP as GRAPHQL_DEFAULT_ROW_CAP, DEFAULT_STATEMENT_BUDGET, GRAPHQL_CLIENT_FILE,
    GRAPHQL_PROVIDER, GRAPHQL_SCHEMA_FILE, GraphqlLimits, GraphqlProvider, SchemaNames,
    generate_graphql_client, graphql_config_spec,
};
pub use ownership::{
    caller_context, caller_context_at, delete_row_as, insert_row_as, read_row_values_as,
    read_rows_as, update_row_as,
};
pub use provider::{ApiProvider, ApiRequest, ApiResponse, RawBody, SessionAction};
pub use rest::custom::{
    CFG_QUERIES as REST_CFG_QUERIES, CustomParam, CustomQuery, QueryColumn, custom_queries,
    describe_custom_query, set_custom_queries, validate_custom_queries,
};
pub use rest::{
    AUTH_ENDPOINTS, DEFAULT_ROW_CAP as REST_DEFAULT_ROW_CAP, REST_PROVIDER, RestProvider, op_name,
    rest_config_spec, rest_row_cap,
};
pub use schema::{StructField, TypeSchema, ValueType};
pub use typescript::generate_client;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_spec_builds_pattern_and_typed_params() {
        let path = PathSpec::root()
            .lit("api/tables")
            .param("table", ValueType::Text)
            .lit("rows")
            .param("id", ValueType::Uuid);
        assert_eq!(path.pattern(), "/api/tables/{table}/rows/{id}");
        let params: Vec<_> = path.params().collect();
        assert_eq!(
            params,
            vec![("table", ValueType::Text), ("id", ValueType::Uuid)]
        );
    }

    #[test]
    fn endpoint_defaults_and_builder() {
        let ep = Endpoint::new("login", Method::Post, PathSpec::root().lit("api/login"));
        // Defaults: empty i/o, logged-in auth, handler named after the endpoint.
        assert!(ep.input.is_empty());
        assert!(ep.output.is_empty());
        assert_eq!(ep.auth, AuthRequirement::LoggedIn);
        assert_eq!(ep.handler, HandlerRef::named("login"));

        let ep = ep.auth(AuthRequirement::Public).output(TypeSchema::text());
        assert_eq!(ep.auth, AuthRequirement::Public);
        assert!(!ep.output.is_empty());
    }

    #[test]
    fn endpoint_set_is_a_runtime_registry() {
        let mut set = EndpointSet::new();
        set.register(Endpoint::new("a", Method::Get, PathSpec::root().lit("a")));
        set.register(Endpoint::new("b", Method::Get, PathSpec::root().lit("b")));
        assert_eq!(set.len(), 2);
        assert!(set.find("a").is_some());
        assert!(set.find("missing").is_none());
        // Iteration preserves registration order.
        let names: Vec<_> = set.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    #[should_panic(expected = "duplicate endpoint name")]
    fn duplicate_endpoint_names_panic() {
        let mut set = EndpointSet::new();
        set.register(Endpoint::new("dup", Method::Get, PathSpec::root().lit("x")));
        set.register(Endpoint::new(
            "dup",
            Method::Post,
            PathSpec::root().lit("y"),
        ));
    }

    #[test]
    fn admin_endpoints_use_the_same_machinery() {
        let set = admin_endpoints();
        // A representative sample of the fixed admin contract.
        assert!(set.find("login").is_some());
        assert!(set.find("createFirstUser").is_some());
        assert!(set.find("listTables").is_some());
        assert!(set.find("createRow").is_some());
        assert!(set.find("listUsers").is_some());

        // Auth is set as the design requires: bootstrap is public, admin routes gated.
        assert_eq!(set.find("login").unwrap().auth, AuthRequirement::Public);
        assert_eq!(
            set.find("listTables").unwrap().auth,
            AuthRequirement::admin()
        );
    }

    #[test]
    fn typeschema_renders_to_typescript() {
        // Optional struct field becomes `?`-optional and `| null`.
        let schema = TypeSchema::struct_of([
            StructField::new("id", TypeSchema::uuid()),
            StructField::new("tags", TypeSchema::array(TypeSchema::text())),
            StructField::new("note", TypeSchema::optional(TypeSchema::text())),
        ]);
        let client = generate_client(&EndpointSet::new().with(
            Endpoint::new("thing", Method::Post, PathSpec::root().lit("thing")).input(schema),
        ));
        assert!(client.contains("id: string"));
        assert!(client.contains("tags: Array<string>"));
        assert!(client.contains("note?: string | null"));
    }

    #[test]
    fn generated_client_has_typed_methods_and_urls() {
        let ts = generate_client(&admin_endpoints());

        // Type declarations for endpoints with a body/response.
        assert!(ts.contains("export type LoginRequest = "));
        assert!(ts.contains("export type LoginResponse = "));

        // The client interface and factory.
        assert!(ts.contains("export interface ApiClient {"));
        assert!(ts.contains("export function createClient("));

        // Path params are typed method args; the URL interpolates them.
        assert!(ts.contains("listRows(table: string): Promise<ListRowsResponse>"));
        assert!(ts.contains("/api/tables/${table}/rows"));

        // A void endpoint (logout has no response payload).
        assert!(ts.contains("logout(): Promise<void>"));

        // Correct HTTP verbs are emitted.
        assert!(ts.contains("method: \"POST\""));
        assert!(ts.contains("method: \"DELETE\""));
    }
}
