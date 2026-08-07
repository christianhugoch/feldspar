//! Custom SQL queries: an application's own endpoints, written as SQL (§13.4).
//!
//! An administrator writes a statement, names its parameters and their types,
//! picks an HTTP method and a sub-path, and gets a typed method on the app's
//! generated client — typed from the columns *Postgres itself* reported when the
//! query was prepared ([`describe_custom_query`]). It is the escape hatch for
//! everything the row layer's read cannot express: a window function, a
//! recursive CTE, a report nobody wants to assemble client-side.
//!
//! **A custom query's authority is its own, and this is the paragraph that says
//! so.** Raw SQL does not go through the row layer, so none of that layer's
//! rules reach it: ownership formulae do not filter it, rich types do not coerce
//! what it returns, `File`-field rules do not govern it, and a write inside one
//! raises no table event. What remains, deliberately:
//!
//! - **A role floor, defaulting to admin.** [`CustomQuery::min_role`] is
//!   required to be *stated*, and an unstated one is [`ROLE_ADMIN`]. The same
//!   rule a trigger's exposure is under (§10.2): an access nobody has thought
//!   about must not be the one that turns out to be public.
//! - **The caller context.** Every custom query runs inside the same
//!   caller-context transaction a row operation on an RLS table does, so an
//!   RLS-protected table's policies still decide what it can see — the one
//!   authorization rule that is enforced *below* the API and therefore still
//!   applies here.
//! - **Read-only for `GET`.** The method is the admin's choice (GOALS: "select
//!   HTTP method manually"); the transaction is not. See [`Access::ReadOnly`].
//!
//! **Arguments are values, never text.** The SQL is rewritten once
//! ([`sc_query::rewrite_named_params`]) so each `:name` becomes a placeholder,
//! and the caller's arguments are bound to those placeholders after being
//! coerced to the type the admin declared. There is no path by which anything a
//! caller sent becomes part of the statement's text.

use sc_auth::ROLE_ADMIN;
use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_query::{Statement, Value, rewrite_named_params};
use sc_types::{Attrs, BasicType, json_to_value};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::endpoint::{AuthRequirement, Endpoint, HandlerRef, Method, PathSpec, QueryParam};
use crate::provider::ApiRequest;
use crate::schema::{StructField, TypeSchema, ValueType};

/// The [`ApiConfig`](sc_app::ApiConfig)-level key an application's custom queries
/// are stored under.
///
/// They live in the same settings object the provider's declared settings do
/// (§13.4), but they are **not** a settings field: a list of records each
/// carrying a nested list of parameters is not a form, and describing it as one
/// would distort both. So the key is typed rather than spec'd, and
/// [`validate_api_config`](sc_app::validate_api_config) validates it as a value
/// of its own type before checking the rest against the provider's spec.
pub const CFG_QUERIES: &str = "queries";

/// One declared input parameter of a [`CustomQuery`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomParam {
    /// The parameter name — the `:name` in the SQL, the query-string key or body
    /// property a caller supplies it under, and the property name in the
    /// generated client.
    pub name: String,
    /// The type the admin declares it as. The caller's JSON is coerced to this
    /// before it is bound, so `"7"` from a query string binds an integer.
    #[serde(rename = "type")]
    pub ty: ValueType,
    /// Whether the caller must supply it. An omitted optional parameter binds
    /// SQL `NULL`, which is what makes `WHERE (:q IS NULL OR name = :q)` the
    /// idiom for an optional filter.
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

impl CustomParam {
    /// A required parameter of the given type.
    pub fn new(name: impl Into<String>, ty: ValueType) -> CustomParam {
        CustomParam {
            name: name.into(),
            ty,
            required: true,
        }
    }

    /// Mark the parameter optional (an omitted one binds `NULL`).
    pub fn optional(mut self) -> CustomParam {
        self.required = false;
        self
    }
}

/// One column of a custom query's result, as the **database** described it.
///
/// Written by the server from [`describe_custom_query`] on every save, never by
/// hand: the admin declares the parameters, the database types the result
/// (§13.4). A shape declared twice is a shape that goes stale the first time
/// anyone edits the SQL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryColumn {
    /// The column's name in the returned JSON object.
    pub name: String,
    /// Its wire type, mapped from the type Postgres reported.
    #[serde(rename = "type")]
    pub ty: ValueType,
}

/// An administrator-authored SQL endpoint (§13.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomQuery {
    /// The operation name: the generated client's method name, and the endpoint
    /// name. A valid TypeScript identifier, unique within the API.
    pub name: String,
    /// What it is for, shown in the admin editor and carried into the generated
    /// client's doc comment.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The HTTP method it answers on — **the admin's choice**, with no inference
    /// from the SQL (GOALS). What is inferred is the transaction: see
    /// [`read_only`](CustomQuery::read_only).
    pub method: Method,
    /// The sub-path within the API's mount, e.g. `/reports/top-authors`.
    pub path: String,
    /// The SQL, with `:name` parameters.
    pub sql: String,
    /// The declared parameters, in the order the editor lists them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<CustomParam>,
    /// The role floor for calling it. **Admin unless stated** — see the module
    /// documentation.
    #[serde(default = "admin_role")]
    pub min_role: u8,
    /// The result columns, as the database described them. Server-written; an
    /// empty list means "not described yet" and types the response as opaque
    /// JSON rather than lying about a shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<QueryColumn>,
}

fn admin_role() -> u8 {
    ROLE_ADMIN
}

impl CustomQuery {
    /// A query with no parameters, at the admin role floor.
    pub fn new(
        name: impl Into<String>,
        method: Method,
        path: impl Into<String>,
        sql: impl Into<String>,
    ) -> CustomQuery {
        CustomQuery {
            name: name.into(),
            description: String::new(),
            method,
            path: normalize_path(&path.into()),
            sql: sql.into(),
            params: Vec::new(),
            min_role: ROLE_ADMIN,
            columns: Vec::new(),
        }
    }

    /// Declare the query's parameters, in order.
    pub fn params(mut self, params: impl IntoIterator<Item = CustomParam>) -> CustomQuery {
        self.params = params.into_iter().collect();
        self
    }

    /// Set the role floor.
    pub fn min_role(mut self, min_role: u8) -> CustomQuery {
        self.min_role = min_role;
        self
    }

    /// Set the description.
    pub fn description(mut self, description: impl Into<String>) -> CustomQuery {
        self.description = description.into();
        self
    }

    /// Whether this query's transaction refuses writes.
    ///
    /// `GET` runs `READ ONLY` (decision 7): an `UPDATE` behind a `GET` is a
    /// mutation a cache, a crawler or a link prefetch can cause, so it fails
    /// loudly rather than quietly happening. Every other method commits.
    pub fn read_only(&self) -> bool {
        self.method == Method::Get
    }

    /// Whether the caller's arguments arrive in the query string rather than in
    /// a body — `GET` and `DELETE`, the methods that conventionally have none.
    pub fn args_in_query(&self) -> bool {
        !self.method.has_body()
    }

    /// The parameter declared under `name`.
    fn param(&self, name: &str) -> Option<&CustomParam> {
        self.params.iter().find(|p| p.name == name)
    }
}

/// The custom queries stored in an API provider's configuration.
///
/// A malformed `queries` value is an error rather than an empty list: a stored
/// query that cannot be read is an endpoint the app's client has a method for
/// and the server does not answer, and silently serving the smaller API is
/// exactly the drift §13.1 exists to prevent.
pub fn custom_queries(config: &Attrs) -> Result<Vec<CustomQuery>> {
    let Some(value) = config.get(CFG_QUERIES) else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    serde_json::from_value(value.clone()).map_err(|e| {
        Error::invalid(format!(
            "the API's `{CFG_QUERIES}` setting is not a list of custom SQL queries: {e}"
        ))
    })
}

/// Store `queries` in an API provider's configuration, dropping the key when
/// there are none (so an API with no custom queries carries no empty list).
pub fn set_custom_queries(config: &mut Attrs, queries: &[CustomQuery]) -> Result<()> {
    if queries.is_empty() {
        config.remove(CFG_QUERIES);
        return Ok(());
    }
    let value = serde_json::to_value(queries)
        .map_err(|e| Error::msg(format!("cannot store the custom queries: {e}")))?;
    config.insert(CFG_QUERIES.to_owned(), value);
    Ok(())
}

/// Check a set of custom queries against each other and against the table routes
/// they share a mount with — everything that can be decided without a database.
///
/// `tables` is the application's declared table subset, because a query's path
/// and name both live in the same space a table's four endpoints do: a query at
/// `/books` would be shadowed by the `books` table's own list route (`resolve`
/// takes the first match, and the tables are registered first), and a query
/// named `listBooks` would collide with the table's client method. Both are
/// refused here rather than surviving as an endpoint nobody can reach.
pub fn validate_custom_queries(queries: &[CustomQuery], tables: &[String]) -> Result<()> {
    for (i, q) in queries.iter().enumerate() {
        validate_custom_query(q, tables)?;
        if let Some(other) = queries[..i].iter().find(|o| o.name == q.name) {
            return Err(Error::invalid(format!(
                "two custom SQL queries are named `{}`; the name is the client's \
                 method name, so it has to be unique",
                other.name
            )));
        }
        if let Some(other) = queries[..i]
            .iter()
            .find(|o| o.method == q.method && o.path == q.path)
        {
            return Err(Error::invalid(format!(
                "custom SQL queries `{}` and `{}` both answer {} {}; one of them \
                 would be unreachable",
                other.name,
                q.name,
                q.method.as_str(),
                q.path
            )));
        }
    }
    Ok(())
}

/// The endpoint-name prefixes a table's projection uses, which a custom query's
/// name may therefore not reproduce for a declared table.
const TABLE_OPS: [&str; 6] = ["list", "create", "update", "delete", "download", "upload"];

/// Everything one custom query has to satisfy on its own.
fn validate_custom_query(q: &CustomQuery, tables: &[String]) -> Result<()> {
    if !is_identifier(&q.name) {
        return Err(Error::invalid(format!(
            "custom SQL query name `{}` is not usable as a client method name; \
             use letters, digits and underscores, starting with a letter",
            q.name
        )));
    }
    for table in tables {
        for op in TABLE_OPS {
            if q.name == super::op_name(op, table) {
                return Err(Error::invalid(format!(
                    "custom SQL query `{}` has the same name as the `{table}` table's \
                     own endpoint; give it another name",
                    q.name
                )));
            }
        }
    }
    if !(1..=100).contains(&q.min_role) {
        return Err(Error::invalid(format!(
            "custom SQL query `{}` has a role floor of {}; roles run 1..=100",
            q.name, q.min_role
        )));
    }

    let path = normalize_path(&q.path);
    let mut segments = path.trim_matches('/').split('/');
    let first = segments.next().unwrap_or_default();
    if first.is_empty() {
        return Err(Error::invalid(format!(
            "custom SQL query `{}` needs a sub-path, e.g. `/reports/sales`",
            q.name
        )));
    }
    for segment in path.trim_matches('/').split('/') {
        if segment.is_empty()
            || !segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(Error::invalid(format!(
                "custom SQL query `{}` has an unusable path segment `{segment}` in \
                 `{path}`; use letters, digits, `-`, `_` and `.`",
                q.name
            )));
        }
    }
    if let Some(table) = tables.iter().find(|t| *t == first) {
        return Err(Error::invalid(format!(
            "custom SQL query `{}` is mounted at `{path}`, which the `{table}` \
             table's own routes already answer; give it another sub-path",
            q.name
        )));
    }
    if let Some(reserved) = super::RESERVED_SEGMENTS.iter().find(|r| **r == first) {
        return Err(Error::invalid(format!(
            "custom SQL query `{}` is mounted at `{path}`, but `{reserved}` is \
             reserved by the API's own routes; give it another sub-path",
            q.name
        )));
    }

    // The parameters: each usable as an identifier and declared once.
    for (i, p) in q.params.iter().enumerate() {
        if !is_identifier(&p.name) {
            return Err(Error::invalid(format!(
                "custom SQL query `{}` has a parameter named `{}`, which is not a \
                 usable parameter name",
                q.name, p.name
            )));
        }
        if q.params[..i].iter().any(|o| o.name == p.name) {
            return Err(Error::invalid(format!(
                "custom SQL query `{}` declares the parameter `{}` twice",
                q.name, p.name
            )));
        }
    }

    check_sql(q)?;
    Ok(())
}

/// The SQL half: one statement, and the declared parameters and the used ones
/// are the same set.
///
/// The rewriting is what reads the SQL, so this is where the reading happens —
/// there is one scanner, and "which `:name`s are in here" is its answer, not a
/// second regular expression's.
fn check_sql(q: &CustomQuery) -> Result<()> {
    // A parameter's *placement* is dialect-specific but its presence is not, so
    // the counting dialect below is enough to answer both questions here. The
    // real dialect renders the statement that runs.
    let named = rewrite_named_params(&CountingDialect, &q.sql)
        .map_err(|e| Error::invalid(format!("custom SQL query `{}`: {e}", q.name)))?;
    if named.statements == 0 {
        return Err(Error::invalid(format!(
            "custom SQL query `{}` has no SQL",
            q.name
        )));
    }
    if named.statements > 1 {
        return Err(Error::invalid(format!(
            "custom SQL query `{}` holds {} statements; a custom query is one \
             statement — a migration is not an API endpoint",
            q.name, named.statements
        )));
    }
    for used in &named.params {
        if q.param(used).is_none() {
            return Err(Error::invalid(format!(
                "custom SQL query `{}` uses `:{used}`, which is not one of its \
                 declared parameters",
                q.name
            )));
        }
    }
    for declared in &q.params {
        if !named.params.contains(&declared.name) {
            return Err(Error::invalid(format!(
                "custom SQL query `{}` declares the parameter `{}` but its SQL \
                 never uses `:{}`",
                q.name, declared.name, declared.name
            )));
        }
    }
    Ok(())
}

/// A dialect used only to *read* SQL — the placeholder text it renders is never
/// executed, so its only requirement is that it not be mistaken for anything
/// else in the text it produces.
struct CountingDialect;

impl sc_query::SqlDialect for CountingDialect {
    fn quote_ident(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }
    fn placeholder(&self, position: usize) -> String {
        format!("${position}")
    }
}

/// Prepare `query` against the primary database and report the result columns it
/// says it will produce (§13.4, decision 5).
///
/// This is validation and typing in one call, which is the point: a query that
/// will not prepare cannot be saved, and one that will is typed by the database
/// rather than by a declaration that can go stale.
pub async fn describe_custom_query(
    catalog: &Catalog,
    query: &CustomQuery,
) -> Result<Vec<QueryColumn>> {
    let driver = catalog.primary();
    let (named, types) = prepare(catalog, query)?;

    let described = driver
        .describe(&named.sql, &types)
        .await
        .map_err(|e| Error::invalid(format!("custom SQL query `{}`: {e}", query.name)))?;

    let mut columns: Vec<QueryColumn> = Vec::with_capacity(described.len());
    for column in &described {
        // Two columns of one name collapse into one JSON property, so the row a
        // caller receives would silently lose one. The fix is an alias, and the
        // admin is the only one who can choose it.
        if columns.iter().any(|c| c.name == column.name) {
            return Err(Error::invalid(format!(
                "custom SQL query `{}` returns two columns named `{}`; give one of \
                 them an alias, or the response would carry only one",
                query.name, column.name
            )));
        }
        columns.push(QueryColumn {
            name: column.name.clone(),
            ty: ValueType::from_basic(&BasicType::from_sql_type(&column.sql_type)),
        });
    }
    Ok(columns)
}

/// The endpoint a custom query projects into the API's set (§13.1).
///
/// Its parameters are query parameters for a method with no body and a typed
/// body for one that has, its output is the described columns, and its auth is
/// the query's own role floor — so a custom query is typed in the generated
/// client exactly like everything else, which is the whole reason it is a
/// projected `Endpoint` rather than a special case in the router.
pub fn custom_endpoint(mount: &str, query: &CustomQuery) -> Endpoint {
    let mut path = PathSpec::root().lit(mount);
    path = path.lit(&query.path);

    let row = if query.columns.is_empty() {
        // Not described (yet): opaque JSON is honest, an invented shape is not.
        TypeSchema::json()
    } else {
        TypeSchema::struct_of(
            query
                .columns
                .iter()
                // Every column of a query is nullable in principle — an outer
                // join, a `CASE` with no `ELSE`, an aggregate over no rows — and
                // the backend commits to nothing about it, so the client is told
                // the truth rather than a convenient half of it.
                .map(|c| StructField::new(&c.name, TypeSchema::optional(TypeSchema::value(c.ty)))),
        )
    };

    let endpoint = Endpoint::new(&query.name, query.method, path)
        .output(TypeSchema::array(row))
        .auth(AuthRequirement::MinRole(query.min_role))
        .handler(HandlerRef::Sql(query.name.clone()));

    if query.args_in_query() {
        endpoint.query(query.params.iter().map(|p| {
            let param = QueryParam::new(&p.name, p.ty);
            if p.required { param.required() } else { param }
        }))
    } else {
        endpoint.input(TypeSchema::struct_of(query.params.iter().map(|p| {
            let field = TypeSchema::value(p.ty);
            StructField::new(
                &p.name,
                if p.required {
                    field
                } else {
                    TypeSchema::optional(field)
                },
            )
        })))
    }
}

/// The query's SQL with this database's placeholders in it, and its declared
/// parameter types in **placeholder** order — which is the order the database
/// wants them, and not necessarily the order the admin listed them in.
///
/// One derivation, shared by describing a query and by running one, so a query
/// is executed under exactly the types it was typed under. Two derivations would
/// be two answers, and the one that is wrong is the one nobody is reading until
/// a caller hits it.
fn prepare(catalog: &Catalog, query: &CustomQuery) -> Result<(sc_query::NamedSql, Vec<String>)> {
    let named = rewrite_named_params(catalog.primary().dialect(), &query.sql)
        .map_err(|e| Error::invalid(format!("custom SQL query `{}`: {e}", query.name)))?;
    let types = named
        .params
        .iter()
        .map(|name| {
            query
                .param(name)
                .map(|p| p.ty.to_basic().sql_type().to_owned())
                .ok_or_else(|| {
                    Error::invalid(format!(
                        "custom SQL query `{}` uses `:{name}`, which is not one of \
                         its declared parameters",
                        query.name
                    ))
                })
        })
        .collect::<Result<Vec<String>>>()?;
    Ok((named, types))
}

/// Build the statement a call runs: the admin's SQL with the dialect's
/// placeholders, and the caller's arguments coerced and bound in placeholder
/// order.
///
/// The rewriting happens per call rather than at projection time because the
/// placeholders are the *driver's*, and a provider is not tied to one — the same
/// projection types the client whichever database answers it. It is a linear
/// scan of a short string next to a database round trip.
pub(crate) fn custom_statement(
    catalog: &Catalog,
    query: &CustomQuery,
    req: &ApiRequest,
) -> Result<Statement> {
    let (named, types) = prepare(catalog, query)?;
    let binds = named
        .params
        .iter()
        .map(|name| bind(query, req, name))
        .collect::<Result<Vec<Value>>>()?;
    Ok(Statement::raw_typed(named.sql, binds, types))
}

/// One argument, read off the request and coerced to its declared type.
fn bind(query: &CustomQuery, req: &ApiRequest, name: &str) -> Result<Value> {
    let param = query.param(name).ok_or_else(|| {
        Error::invalid(format!(
            "custom SQL query `{}` uses `:{name}`, which is not one of its \
             declared parameters",
            query.name
        ))
    })?;
    let supplied: Option<Json> = if query.args_in_query() {
        req.query_get(name).map(|v| Json::String(v.to_owned()))
    } else {
        req.body.get(name).filter(|v| !v.is_null()).cloned()
    };
    let Some(supplied) = supplied else {
        if param.required {
            return Err(Error::invalid(format!(
                "this query needs the argument `{name}` ({})",
                param.ty.to_basic().name()
            )));
        }
        return Ok(Value::Null);
    };
    // Coerced against the declared type before the statement is prepared, so a
    // wrongly-typed argument is a 400 rather than a database error — and a
    // string that happens to be SQL is a *string*, because this is the only
    // route by which a caller's value reaches the query at all.
    json_to_value(&param.ty.to_basic(), &supplied)
        .map_err(|e| Error::invalid(format!("argument `{name}`: {e}")))
}

/// A result row as JSON, by the column names the database reported.
pub(crate) fn rows_to_json(rows: &[sc_db::Row]) -> Json {
    Json::Array(
        rows.iter()
            .map(|row| {
                let mut out = serde_json::Map::new();
                for (name, value) in row.columns().iter().zip(row.values()) {
                    out.insert(name.clone(), crate::convert::value_to_json(value));
                }
                Json::Object(out)
            })
            .collect(),
    )
}

/// Normalise a query's sub-path to a single leading slash and no trailing one.
fn normalize_path(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('/');
    format!("/{trimmed}")
}

/// Whether `s` is usable as a TypeScript method name and as a `:name` parameter.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn query() -> CustomQuery {
        CustomQuery::new(
            "topAuthors",
            Method::Get,
            "/reports/top-authors",
            "SELECT author, count(*) AS n FROM books WHERE year > :since GROUP BY author",
        )
        .params([CustomParam::new("since", ValueType::Int)])
    }

    #[test]
    fn a_well_formed_query_validates() {
        validate_custom_queries(&[query()], &["books".to_owned()]).unwrap();
    }

    #[test]
    fn an_undeclared_parameter_is_refused_naming_it() {
        let mut q = query();
        q.sql = "SELECT * FROM books WHERE year > :since AND author = :author".into();
        let msg = validate_custom_queries(&[q], &[]).unwrap_err().to_string();
        assert!(msg.contains(":author"), "{msg}");
    }

    #[test]
    fn a_declared_parameter_the_sql_never_uses_is_refused() {
        // Not pedantry: it is an argument the client will demand and the query
        // will ignore, which reads as a filter that does nothing.
        let q = query().params([
            CustomParam::new("since", ValueType::Int),
            CustomParam::new("unused", ValueType::Text),
        ]);
        let msg = validate_custom_queries(&[q], &[]).unwrap_err().to_string();
        assert!(msg.contains("unused"), "{msg}");
    }

    #[test]
    fn a_second_statement_is_refused() {
        let mut q = query();
        q.params = Vec::new();
        q.sql = "SELECT 1; DROP TABLE books".into();
        let msg = validate_custom_queries(&[q], &[]).unwrap_err().to_string();
        assert!(msg.contains("one statement"), "{msg}");
    }

    #[test]
    fn empty_sql_is_refused() {
        let mut q = query();
        q.params = Vec::new();
        q.sql = "  -- nothing\n".into();
        assert!(validate_custom_queries(&[q], &[]).is_err());
    }

    #[test]
    fn a_path_that_a_table_already_answers_is_refused_naming_the_table() {
        let mut q = query();
        q.path = "/books/summary".into();
        let msg = validate_custom_queries(&[q], &["books".to_owned()])
            .unwrap_err()
            .to_string();
        assert!(msg.contains("books"), "{msg}");
    }

    #[test]
    fn a_path_under_a_reserved_segment_is_refused() {
        for reserved in ["/actions/run", "/login", "/whoami/me"] {
            let mut q = query();
            q.path = reserved.into();
            assert!(
                validate_custom_queries(&[q], &[]).is_err(),
                "{reserved} should be refused"
            );
        }
    }

    #[test]
    fn a_name_that_is_not_a_method_name_is_refused() {
        for name in ["top authors", "2reports", "", "top-authors"] {
            let mut q = query();
            q.name = name.into();
            assert!(validate_custom_queries(&[q], &[]).is_err(), "{name}");
        }
    }

    #[test]
    fn a_name_a_table_endpoint_already_uses_is_refused() {
        let mut q = query();
        q.name = "listBooks".into();
        let msg = validate_custom_queries(&[q], &["books".to_owned()])
            .unwrap_err()
            .to_string();
        assert!(msg.contains("listBooks"), "{msg}");
    }

    #[test]
    fn two_queries_may_not_share_a_name_or_a_route() {
        let a = query();
        let mut b = query();
        b.path = "/reports/other".into();
        let msg = validate_custom_queries(&[a.clone(), b], &[])
            .unwrap_err()
            .to_string();
        assert!(msg.contains("named `topAuthors`"), "{msg}");

        let mut b = query();
        b.name = "otherReport".into();
        let msg = validate_custom_queries(&[a, b], &[])
            .unwrap_err()
            .to_string();
        assert!(msg.contains("unreachable"), "{msg}");
    }

    #[test]
    fn the_role_floor_is_admin_unless_stated() {
        // Read off the serde default rather than the constructor, because the
        // stored form is where an unstated floor actually arrives from.
        let stored = json!({
            "name": "report",
            "method": "GET",
            "path": "/report",
            "sql": "SELECT 1 AS n"
        });
        let q: CustomQuery = serde_json::from_value(stored).unwrap();
        assert_eq!(q.min_role, ROLE_ADMIN);
        assert!(q.read_only());
    }

    #[test]
    fn queries_round_trip_through_the_provider_config() {
        let mut config = Attrs::new();
        set_custom_queries(&mut config, &[query()]).unwrap();
        assert_eq!(custom_queries(&config).unwrap(), vec![query()]);

        // …and an empty list carries no key at all.
        set_custom_queries(&mut config, &[]).unwrap();
        assert!(!config.contains_key(CFG_QUERIES));
        assert!(custom_queries(&config).unwrap().is_empty());
    }

    #[test]
    fn a_malformed_queries_setting_is_an_error_not_an_empty_api() {
        let mut config = Attrs::new();
        config.insert(CFG_QUERIES.to_owned(), json!("not a list"));
        assert!(custom_queries(&config).is_err());
    }

    #[test]
    fn a_get_projects_its_parameters_as_query_parameters() {
        let mut q = query();
        q.columns = vec![
            QueryColumn {
                name: "author".into(),
                ty: ValueType::Text,
            },
            QueryColumn {
                name: "n".into(),
                ty: ValueType::Int,
            },
        ];
        let ep = custom_endpoint("/api", &q);
        assert_eq!(ep.method, Method::Get);
        assert_eq!(ep.path.pattern(), "/api/reports/top-authors");
        assert_eq!(ep.auth, AuthRequirement::MinRole(ROLE_ADMIN));
        assert_eq!(ep.handler, HandlerRef::Sql("topAuthors".into()));
        assert!(ep.input.is_empty());
        assert_eq!(ep.query.len(), 1);
        assert_eq!(ep.query[0].name, "since");
        assert!(ep.query[0].required);
        assert_eq!(
            ep.output,
            TypeSchema::array(TypeSchema::struct_of([
                StructField::new("author", TypeSchema::optional(TypeSchema::text())),
                StructField::new("n", TypeSchema::optional(TypeSchema::int())),
            ]))
        );
    }

    #[test]
    fn a_post_projects_its_parameters_as_a_typed_body() {
        let mut q = query();
        q.method = Method::Post;
        q.params = vec![
            CustomParam::new("since", ValueType::Int),
            CustomParam::new("author", ValueType::Text).optional(),
        ];
        q.sql = "SELECT :since AS a, :author AS b".into();
        let ep = custom_endpoint("/api", &q);
        assert!(ep.query.is_empty());
        assert_eq!(
            ep.input,
            TypeSchema::struct_of([
                StructField::new("since", TypeSchema::int()),
                StructField::new("author", TypeSchema::optional(TypeSchema::text())),
            ])
        );
        assert!(!q.read_only(), "only GET is read-only");
    }
}
