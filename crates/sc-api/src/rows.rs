//! Table row CRUD, shared by every API surface (design §13.1, §13.4).
//!
//! Reading and writing a table's rows as JSON is the substance of *both* the
//! admin API's row endpoints and an application's REST API: same tables, same
//! query layer, same wire shape. So it lives here once, below both, rather than
//! in either. `sc-server`'s admin handlers call these functions, and so does
//! [`RestProvider`](crate::RestProvider) — which is what makes "the app's API and
//! the admin API are the same machinery" true in the code and not just in the
//! docs.
//!
//! Rows cross the wire as plain JSON objects keyed by column name;
//! [`crate::convert`] bridges those to the query layer's [`Value`]. A row is
//! addressed by its **single-column primary key** (composite keys are post-MVP).
//! These functions do no authorization of their own — the caller enforces the
//! endpoint's [`AuthRequirement`](crate::AuthRequirement) first, so every API
//! surface goes through the same §7 layer.

use sc_catalog::{Catalog, Table};
use sc_db::Row;
use sc_error::{Error, Result};
use sc_query::{
    Assignment, Delete, Expr, Insert, Projection, Select, Source, Statement, Update, Value,
};
use serde_json::{Map, Value as Json, json};

use crate::convert::{json_to_value, value_to_json};

/// Every row of `table`, as a JSON array of objects.
pub async fn list_rows(catalog: &Catalog, table: &Table) -> Result<Json> {
    let select = Select::from(Source::table(table.name.clone())).columns(vec![Projection::all()]);
    let rows: Vec<Row> = catalog
        .provider(table)
        .query(&select)
        .await?
        .try_collect()
        .await?;
    Ok(Json::Array(rows.iter().map(row_to_json).collect()))
}

/// Insert a row from a JSON object, returning the inserted row (with any
/// database-generated columns filled in).
pub async fn create_row(catalog: &Catalog, table: &Table, body: &Json) -> Result<Json> {
    let obj = require_object(body)?;
    let mut columns = Vec::with_capacity(obj.len());
    let mut values = Vec::with_capacity(obj.len());
    for (key, json) in obj {
        let value = column_value(table, key, json)?;
        columns.push(key.clone());
        values.push(Expr::lit(value));
    }
    if columns.is_empty() {
        return Err(Error::invalid("no fields to insert"));
    }
    let insert =
        Insert::row(table.name.clone(), columns, values).returning(vec![Projection::all()]);
    let row = write_one(catalog, table, Statement::from(insert)).await?;
    Ok(row_to_json(&row))
}

/// Update the row of `table` whose primary key is `id`, returning the updated
/// row. The primary key addresses the row and is not reassignable through the
/// body.
pub async fn update_row(catalog: &Catalog, table: &Table, id: &str, body: &Json) -> Result<Json> {
    let obj = require_object(body)?;
    let pk = single_pk(table)?;
    let mut assignments = Vec::with_capacity(obj.len());
    for (key, json) in obj {
        if key == &pk {
            continue;
        }
        let value = column_value(table, key, json)?;
        assignments.push(Assignment::new(key.clone(), Expr::lit(value)));
    }
    if assignments.is_empty() {
        return Err(Error::invalid("no fields to update"));
    }
    let update = Update {
        table: table.name.clone(),
        assignments,
        filter: Some(pk_filter(table, &pk, id)?),
        returning: vec![Projection::all()],
    };
    let rows = write(catalog, table, Statement::from(update)).await?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| Error::not_found(format!("no row with {pk} = {id}")))?;
    Ok(row_to_json(&row))
}

/// Delete the row of `table` whose primary key is `id`. Deleting a row that is
/// not there is a [`NotFound`](Error::NotFound), not a silent success.
pub async fn delete_row(catalog: &Catalog, table: &Table, id: &str) -> Result<Json> {
    let pk = single_pk(table)?;
    let delete = Delete {
        table: table.name.clone(),
        filter: Some(pk_filter(table, &pk, id)?),
        returning: vec![Projection::expr(Expr::col(pk.clone()))],
    };
    let rows = write(catalog, table, Statement::from(delete)).await?;
    if rows.is_empty() {
        return Err(Error::not_found(format!("no row with {pk} = {id}")));
    }
    Ok(json!({ "deleted": true }))
}

/// A row as a JSON object keyed by column name, values in natural JSON.
pub fn row_to_json(row: &Row) -> Json {
    let mut map = Map::with_capacity(row.len());
    for (name, value) in row.columns().iter().zip(row.values()) {
        map.insert(name.clone(), value_to_json(value));
    }
    Json::Object(map)
}

/// Coerce a JSON value for a named column of `table`, rejecting unknown columns.
pub fn column_value(table: &Table, column: &str, json: &Json) -> Result<Value> {
    let field = table
        .field(column)
        .ok_or_else(|| Error::invalid(format!("`{}` has no field `{column}`", table.name)))?;
    let basic = field
        .base
        .type_
        .as_basic()
        .ok_or_else(|| Error::invalid(format!("field `{column}` has no basic type")))?;
    json_to_value(basic, json)
}

/// The single primary-key column of `table`, or an error when the table has a
/// composite or absent key (row addressing needs exactly one — composite keys
/// are post-MVP).
pub fn single_pk(table: &Table) -> Result<String> {
    match table.primary_key.as_slice() {
        [pk] => Ok(pk.clone()),
        [] => Err(Error::invalid(format!(
            "table `{}` has no primary key to address rows by",
            table.name
        ))),
        _ => Err(Error::invalid(format!(
            "table `{}` has a composite primary key (unsupported for row addressing)",
            table.name
        ))),
    }
}

/// `pk = <id>`, coercing the path-parameter string to the key column's type.
fn pk_filter(table: &Table, pk: &str, id: &str) -> Result<Expr> {
    let value = column_value(table, pk, &Json::String(id.to_owned()))?;
    Ok(Expr::col(pk).eq(Expr::lit(value)))
}

/// Run a write statement against the table's provider, collecting `RETURNING` rows.
async fn write(catalog: &Catalog, table: &Table, statement: Statement) -> Result<Vec<Row>> {
    catalog
        .provider(table)
        .write(&statement)
        .await?
        .try_collect()
        .await
}

/// Run a write expected to return exactly one row (an insert with `RETURNING`).
async fn write_one(catalog: &Catalog, table: &Table, statement: Statement) -> Result<Row> {
    write(catalog, table, statement)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| Error::msg("write returned no row"))
}

/// A JSON body that must be an object, e.g. a row.
pub fn require_object(body: &Json) -> Result<&Map<String, Json>> {
    body.as_object()
        .ok_or_else(|| Error::invalid("expected a JSON object body"))
}
