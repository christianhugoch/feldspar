//! Prefetching the values a **reified** formula evaluation needs (§7.3, Phase 7).
//!
//! The JavaScript evaluator does no I/O — it is handed a flat map of bindings —
//! so whoever calls it must first fetch the two kinds of value a formula can read
//! beyond the row's own columns: a **Ⱶ-join path** (`publisherⱵname`, walked link
//! by link through Key fields) and a **Ↄ-relation** (`order_linesↃorder`, the
//! child rows an aggregation ranges over).
//!
//! This lives in `sc-catalog` because it has two callers in different crates and
//! only a `Catalog` between them: ownership enforcement in `sc-api` (layer 8) and
//! a trigger's `only_if` in `sc-action` (layer 6), which cannot see `sc-api`. It
//! was written for the first and moved down here for the second — one
//! implementation, because two would drift and the drift would be *silent*: a
//! path that resolves in one and binds null in the other is a formula that grants
//! in one place and denies in the other.
//!
//! The read path does not use this: a `SELECT` can project a join path as a
//! correlated subselect alongside the row (`sc_expr::join_path_expr`), which is
//! one query instead of one per row. This is for the rows a query cannot project
//! from — a **proposed** row on the write path, and an event's row after the fact.

use std::collections::BTreeMap;

use sc_db::Row;
use sc_error::{Error, Result};
use sc_expr::{AggUse, Analysis, INVERSE, SchemaShape, value_to_json};
use sc_query::{Expr, Projection, Select, Source, Value};
use serde_json::{Map, Value as Json};

use crate::catalog::Catalog;
use crate::field::DataFieldKind;
use crate::table::Table;

/// Add one binding per Ⱶ-join path and per Ↄ-relation the `analysis` names to
/// `values`, leaving any the caller already supplied untouched.
///
/// After this, `values` is what a [`FormulaCall`](sc_expr::FormulaCall)'s `row`
/// wants: the row's own fields plus every derived value the formula reads, keyed
/// by the identifier it reads them under. An already-present key is *not*
/// refetched — the read path projects join values into the row it fetched, and
/// this must not undo that.
pub async fn prefetch_bindings(
    cat: &Catalog,
    table: &Table,
    analysis: &Analysis,
    shape: &SchemaShape,
    values: &mut BTreeMap<String, Value>,
) -> Result<()> {
    for path in &analysis.join_paths {
        if !values.contains_key(&path.ident) {
            let value = resolve_join_value(cat, table, &path.segments, values).await?;
            values.insert(path.ident.clone(), value);
        }
    }
    // Aggregations over incoming keys (Phase 7): the reified evaluator does no
    // I/O, so prefetch each relation's child rows and bind them under the
    // relation identifier (`childↃkey`) — the array the prelude aggregates.
    for agg in &analysis.agg_uses {
        let ident = format!("{}{}{}", agg.child_table, INVERSE, agg.key_field);
        if !values.contains_key(&ident) {
            let rows = resolve_agg_relation(cat, shape, agg, values).await?;
            values.insert(ident, rows);
        }
    }
    Ok(())
}

/// Fetch the child rows an aggregation ranges over, as a JSON array bound under
/// the relation identifier for the reified evaluator. The correlation is the
/// parent's own value in the column the child key targets; a null there (or no
/// matching children) is the empty relation.
async fn resolve_agg_relation(
    cat: &Catalog,
    shape: &SchemaShape,
    agg: &AggUse,
    values: &BTreeMap<String, Value>,
) -> Result<Value> {
    let empty = Value::Json(Json::Array(Vec::new()));
    // The parent column the child key references (the correlation target).
    let Some(parent_field) = shape
        .tables
        .get(&agg.child_table)
        .and_then(|t| t.fields.get(&agg.key_field))
        .and_then(|f| f.key.as_ref())
        .map(|k| k.target_field.clone())
    else {
        return Ok(empty);
    };
    let parent_value = values.get(&parent_field).cloned().unwrap_or(Value::Null);
    if parent_value.is_null() {
        return Ok(empty);
    }
    let child = cat.require(&agg.child_table)?;
    let select = Select::from(Source::table(child.name.clone()))
        .columns(vec![Projection::all()])
        .filter(Expr::col(agg.key_field.clone()).eq(Expr::lit(parent_value)));
    let fetched: Vec<Row> = cat
        .provider(&child)?
        .query(&select)
        .await?
        .try_collect()
        .await?;
    let rows: Vec<Json> = fetched
        .iter()
        .map(|row| {
            let obj: Map<String, Json> = row
                .columns()
                .iter()
                .zip(row.values().iter())
                .map(|(name, value)| (name.clone(), value_to_json(value)))
                .collect();
            Json::Object(obj)
        })
        .collect();
    Ok(Value::Json(Json::Array(rows)))
}

/// Resolve one Ⱶ-join path from a row's values by walking the Key links — the
/// path for *proposed* rows, which are not in the database to be projected
/// from. A null anywhere propagates (the Ⱶ optional-chaining contract).
async fn resolve_join_value(
    cat: &Catalog,
    table: &Table,
    segments: &[String],
    values: &BTreeMap<String, Value>,
) -> Result<Value> {
    let Some(first) = segments.first() else {
        return Ok(Value::Null);
    };
    let mut current = table.clone();
    let mut value = values.get(first).cloned().unwrap_or(Value::Null);
    for i in 1..segments.len() {
        if value.is_null() {
            return Ok(Value::Null);
        }
        let link = &segments[i - 1];
        let field = current
            .field(link)
            .ok_or_else(|| Error::invalid(format!("`{}` has no field `{link}`", current.name)))?;
        let DataFieldKind::Key {
            target_table,
            target_field,
            ..
        } = &field.kind
        else {
            return Err(Error::invalid(format!(
                "`{}`.`{link}` is not a Key field",
                current.name
            )));
        };
        let target = cat.require(&target_table.0)?;
        let next = &segments[i];
        let select = Select::from(Source::table(target.name.clone()))
            .columns(vec![Projection::expr(Expr::col(next.clone()))])
            .filter(Expr::col(target_field.0.clone()).eq(Expr::lit(value)))
            .limit(1);
        let fetched: Vec<Row> = cat
            .provider(&target)?
            .query(&select)
            .await?
            .try_collect()
            .await?;
        value = fetched
            .first()
            .and_then(|r| r.values().first().cloned())
            .unwrap_or(Value::Null);
        current = target;
    }
    Ok(value)
}
