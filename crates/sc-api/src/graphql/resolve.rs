//! Reading rows: what the root fields and the row objects actually do.
//!
//! Every resolver here is a closure built at mount time over two `String`s — the
//! table and, for a row field, the column — and everything else arrives with the
//! request ([`RequestContext`]). That is what makes the schema *data*: a
//! resolver knows the name of the thing it reads, and looks the thing itself up
//! in the live catalog each time.
//!
//! **Not a second data path.** A root list is
//! [`ownership::read_row_values_as`] — the same entry point the agent's
//! `query_table` uses, the same "meets the floor OR the formula grants it", the
//! same RLS routing. `_by_pk` is that query with a primary-key equality ANDed
//! in. Nothing here decides who may read what.
//!
//! **An outgoing key is projected, not fetched.** A requested `manager { email }`
//! does not become a second query: the resolver reads the selection set, asks
//! [`sc_expr::join_path_expr`] for the Ⱶ-join's correlated subquery, and projects
//! it as another column of the *same* `SELECT`, aliased by the join path itself
//! (`managerⱵemail`). The `manager` resolver then hands its children that row
//! with the prefix stripped, so a second hop (`managerⱵbossⱵname`) composes by
//! doing the same thing again. Only the requested leaves are projected — asking
//! for one column of a related row must not cost the whole row.
//!
//! A null key is `null`, not an error: the FK column itself rides back with the
//! row, and a null there ends the walk before any leaf is looked at. That is the
//! Ⱶ operator's own contract and it is what a GraphQL caller expects of a
//! nullable object field.

use std::collections::BTreeMap;

use async_graphql::dynamic::{FieldFuture, FieldValue, ResolverContext};
use async_graphql::{SelectionField, Value as GqlValue};
use base64::Engine;
use sc_catalog::{Catalog, DataFieldKind, Table};
use sc_error::{Error, Result};
use sc_expr::JOIN;
use sc_query::{Projection, Value};

use super::args;
use super::context::{RequestContext, request_context};
use crate::convert::value_to_json;
use crate::ownership;
use crate::rows::{self, RowQuery};

/// The shape every resolver in this module has.
pub type Resolver =
    Box<dyn for<'a> Fn(ResolverContext<'a>) -> FieldFuture<'a> + Send + Sync + 'static>;

/// One row as the resolvers pass it down: the values its `SELECT` returned,
/// keyed by column, plus whichever table they came from.
///
/// A joined row is the same type with the join prefix stripped from its keys, so
/// the row object's field resolvers are written once and work at every depth.
pub struct RowValue {
    /// The table these values are a row of.
    table: String,
    /// Column (or stripped join path) → value.
    values: BTreeMap<String, Value>,
}

impl RowValue {
    /// A row of `table`.
    pub fn new(table: impl Into<String>, values: BTreeMap<String, Value>) -> RowValue {
        RowValue {
            table: table.into(),
            values,
        }
    }

    /// The value of one column, if this row carries it.
    fn get(&self, column: &str) -> Option<&Value> {
        self.values.get(column)
    }

    /// The row on the other side of a Ⱶ-join: every key that starts with
    /// `field` + Ⱶ, with that prefix removed.
    fn joined(&self, field: &str, table: &str) -> RowValue {
        let prefix = format!("{field}{JOIN}");
        RowValue {
            table: table.to_owned(),
            values: self
                .values
                .iter()
                .filter_map(|(k, v)| {
                    k.strip_prefix(&prefix)
                        .map(|rest| (rest.to_owned(), v.clone()))
                })
                .collect(),
        }
    }
}

/// A `File` field on the wire: the stored path, and the URL the **REST**
/// provider serves the bytes at. Never the bytes themselves — one door into a
/// file is the point of having access rules on it.
pub struct FileRef {
    path: String,
    url: String,
}

/// The parent row, or the error saying a resolver was attached to something that
/// is not one — a schema-building mistake, so it says so plainly.
fn parent_row<'a>(ctx: &ResolverContext<'a>) -> async_graphql::Result<&'a RowValue> {
    ctx.parent_value
        .try_downcast_ref::<RowValue>()
        .map_err(|_| async_graphql::Error::new("this GraphQL field is not resolving over a row"))
}

/// A stored value as the GraphQL scalar it is carried by.
fn scalar(value: &Value) -> GqlValue {
    match value {
        Value::Null => GqlValue::Null,
        // The `Bytes` scalar is base64 text, as its description says; a JSON
        // array of byte numbers would be neither that nor useful.
        Value::Bytes(bytes) => {
            GqlValue::String(base64::engine::general_purpose::STANDARD.encode(bytes))
        }
        other => GqlValue::from_json(value_to_json(other)).unwrap_or(GqlValue::Null),
    }
}

/// The root list field for one table.
pub fn list_field(table: impl Into<String>) -> Resolver {
    let table = table.into();
    Box::new(move |ctx| {
        let table = table.clone();
        FieldFuture::new(async move {
            let rc = request_context(&ctx)?;
            let table = rc.table(&table)?;
            let query = read_query(rc, &table, &ctx)?;
            Ok(Some(FieldValue::list(read(rc, &table, &query).await?)))
        })
    })
}

/// The root `_by_pk` field: the same read, with a primary-key equality ANDed in.
pub fn by_pk_field(table: impl Into<String>, pk: impl Into<String>) -> Resolver {
    let table = table.into();
    let pk = pk.into();
    Box::new(move |ctx| {
        let (table, pk) = (table.clone(), pk.clone());
        FieldFuture::new(async move {
            let rc = request_context(&ctx)?;
            let table = rc.table(&table)?;
            let key = ctx
                .args
                .get(&pk)
                .ok_or_else(|| Error::invalid(format!("`{pk}` is required")))?;
            let value = args::key_value(&table, &pk, key.as_value())?;
            let query = read_query(rc, &table, &ctx)?
                .and_filter(sc_query::Expr::col(&pk).eq(sc_query::Expr::lit(value)))
                .limit(1);
            Ok(read(rc, &table, &query).await?.into_iter().next())
        })
    })
}

/// One stored, calculated or unexposed-key column.
pub fn column_field(column: impl Into<String>) -> Resolver {
    let column = column.into();
    Box::new(move |ctx| {
        let column = column.clone();
        FieldFuture::new(async move {
            let row = parent_row(&ctx)?;
            Ok(row.get(&column).map(|v| FieldValue::value(scalar(v))))
        })
    })
}

/// An outgoing `Key` whose target this application exposes: the row on the other
/// side, built from the leaves the parent query already projected.
pub fn key_field(column: impl Into<String>, target: impl Into<String>) -> Resolver {
    let column = column.into();
    let target = target.into();
    Box::new(move |ctx| {
        let (column, target) = (column.clone(), target.clone());
        FieldFuture::new(async move {
            let row = parent_row(&ctx)?;
            // A null foreign key is a null relation, not an error — and the walk
            // stops here rather than handing down a row of nulls.
            if matches!(row.get(&column), Some(Value::Null)) {
                return Ok(None);
            }
            let joined = row.joined(&column, &target);
            if joined.values.is_empty() {
                return Ok(None);
            }
            Ok(Some(FieldValue::owned_any(joined)))
        })
    })
}

/// A `File` column: its stored path and the REST URL for its bytes.
pub fn file_field(column: impl Into<String>) -> Resolver {
    let column = column.into();
    Box::new(move |ctx| {
        let column = column.clone();
        FieldFuture::new(async move {
            let rc = request_context(&ctx)?;
            let row = parent_row(&ctx)?;
            let Some(Value::Text(path)) = row.get(&column) else {
                return Ok(None);
            };
            if path.is_empty() {
                return Ok(None);
            }
            let table = rc.table(&row.table)?;
            // The bytes are served by the REST provider, addressed by row and
            // field exactly as they are there. Without a primary key in the row
            // there is no address, and a `FileValue` with an unusable `url`
            // would be worse than none.
            let pk = rows::single_pk(&table)?;
            let Some(id) = row.get(&pk).map(value_to_json) else {
                return Ok(None);
            };
            let id = match id {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            };
            Ok(Some(FieldValue::owned_any(FileRef {
                path: path.clone(),
                url: format!(
                    "{}/{}/{id}/{column}",
                    rc.file_mount.trim_end_matches('/'),
                    row.table
                ),
            })))
        })
    })
}

/// `FileValue.path` / `FileValue.url`.
pub fn file_part(part: &'static str) -> Resolver {
    Box::new(move |ctx| {
        FieldFuture::new(async move {
            let file = ctx
                .parent_value
                .try_downcast_ref::<FileRef>()
                .map_err(|_| {
                    async_graphql::Error::new("this field is not resolving over a file")
                })?;
            let value = match part {
                "path" => &file.path,
                _ => &file.url,
            };
            Ok(Some(FieldValue::value(GqlValue::String(value.clone()))))
        })
    })
}

/// The [`RowQuery`] behind one list (or `_by_pk`) field: the caller's arguments,
/// plus the Ⱶ-join projections their selection set implies.
fn read_query(rc: &RequestContext, table: &Table, ctx: &ResolverContext<'_>) -> Result<RowQuery> {
    let query = args::row_query(table, ctx, rc.row_cap)?;
    Ok(query.projecting(join_projections(&rc.catalog, table, ctx.ctx.field())?))
}

/// Run one read and wrap its rows for the executor.
async fn read(
    rc: &RequestContext,
    table: &Table,
    query: &RowQuery,
) -> Result<Vec<FieldValue<'static>>> {
    let rows = ownership::read_row_values_as(
        &rc.catalog,
        table,
        query,
        rc.role(),
        rc.user(),
        rc.evaluator(),
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|values| FieldValue::owned_any(RowValue::new(&table.name, values)))
        .collect())
}

/// The extra projections a selection set asks for: one correlated subquery per
/// requested leaf behind a `Key` field, aliased by the join path itself.
fn join_projections(
    cat: &Catalog,
    table: &Table,
    selection: SelectionField<'_>,
) -> Result<Vec<Projection>> {
    let mut idents = Vec::new();
    collect_join_leaves(cat, table, selection, None, &mut idents)?;
    if idents.is_empty() {
        return Ok(Vec::new());
    }
    let shape = cat.schema_shape()?;
    idents
        .into_iter()
        .map(|ident| {
            let expr = sc_expr::join_path_expr(&shape, &table.name, &ident).map_err(Error::from)?;
            Ok(Projection::expr_as(expr, ident))
        })
        .collect()
}

/// Walk a selection set collecting the Ⱶ-join identifiers it implies.
///
/// `prefix` is the join path reached so far — `None` at the row itself, where a
/// scalar needs no projection because `SELECT *` already has it.
fn collect_join_leaves(
    cat: &Catalog,
    table: &Table,
    selection: SelectionField<'_>,
    prefix: Option<&str>,
    out: &mut Vec<String>,
) -> Result<()> {
    for sub in selection.selection_set() {
        let name = sub.name();
        // Introspection fields (`__typename`) name no column.
        if name.starts_with("__") {
            continue;
        }
        let Some(field) = table.field(name) else {
            continue;
        };
        let ident = match prefix {
            Some(prefix) => format!("{prefix}{JOIN}{name}"),
            None => name.to_owned(),
        };
        match &field.kind {
            DataFieldKind::Key { target_table, .. } => {
                let Ok(target) = cat.require(&target_table.0) else {
                    // A key out of the application's own tables carries its own
                    // value, which `SELECT *` (or the leaf below) already has.
                    push_leaf(prefix, &ident, out);
                    continue;
                };
                // The foreign key's own value, so a null relation can be
                // answered as `null` without looking at a single leaf.
                push_leaf(prefix, &ident, out);
                collect_join_leaves(cat, &target, sub, Some(&ident), out)?;
            }
            // A `File` on the far side of a join needs the *target* row's key to
            // address its bytes by, so it is projected alongside the path.
            DataFieldKind::File { .. } if prefix.is_some() => {
                push_leaf(prefix, &ident, out);
                if let Ok(pk) = rows::single_pk(table) {
                    let key = match prefix {
                        Some(prefix) => format!("{prefix}{JOIN}{pk}"),
                        None => pk,
                    };
                    push_leaf(prefix, &key, out);
                }
            }
            _ => push_leaf(prefix, &ident, out),
        }
    }
    Ok(())
}

/// Record a leaf, but only when it is actually behind a join — a column of the
/// row itself is already in the `SELECT`.
fn push_leaf(prefix: Option<&str>, ident: &str, out: &mut Vec<String>) {
    if prefix.is_some() && !out.iter().any(|i| i == ident) {
        out.push(ident.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sc_query::Value;

    #[test]
    fn a_joined_row_is_the_parent_with_the_prefix_stripped() {
        // What makes a second hop free: the child resolver does to its row
        // exactly what the parent did to its own.
        let row = RowValue::new(
            "departments",
            BTreeMap::from([
                ("id".to_owned(), Value::Int(1)),
                ("manager".to_owned(), Value::Int(7)),
                (format!("manager{JOIN}email"), Value::Text("a@b".into())),
                (
                    format!("manager{JOIN}boss{JOIN}email"),
                    Value::Text("c@d".into()),
                ),
            ]),
        );
        let manager = row.joined("manager", "users");
        assert_eq!(manager.table, "users");
        assert_eq!(manager.get("email"), Some(&Value::Text("a@b".into())));
        let boss = manager.joined("boss", "users");
        assert_eq!(boss.get("email"), Some(&Value::Text("c@d".into())));
        // The parent's own columns do not leak into the joined row.
        assert_eq!(manager.get("id"), None);
    }

    #[test]
    fn bytes_reach_the_wire_as_base64_not_as_a_list_of_numbers() {
        assert_eq!(
            scalar(&Value::Bytes(vec![1, 2, 3])),
            GqlValue::String("AQID".into())
        );
    }

    #[test]
    fn a_decimal_stays_exact_on_the_wire() {
        // The reason the GraphQL read takes values rather than JSON numbers.
        let value = Value::Decimal("1.100".parse().expect("a decimal"));
        assert_eq!(scalar(&value), GqlValue::String("1.100".into()));
    }
}
