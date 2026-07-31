//! The `_sc_runs` table: its schema, bootstrap, and the [`Run`] ⇄ row mapping
//! (design §9, §11.4).
//!
//! Written after **every** step, not at the end of a run. That is what makes a
//! reloaded chat panel show a conversation that is still going, what lets a
//! resumed process pick a run up where it stopped, and what §10.3's durable
//! engine will need with no second mechanism — the same table, discriminated by
//! [`RunKind`].
//!
//! One deliberate departure from §9's "every metadata table has a `name`": a run
//! has no name. It has a [`subject`](Run::subject) — what it is a run *of* — and
//! inventing a name column that always equalled it would be residue on every row.
//! §9's rule is about definitions an admin names and addresses; a run is neither
//! named nor addressed by anything but its id.

use chrono::{DateTime, Utc};
use sc_catalog::{Catalog, DataField, Table};
use sc_db::Row;
use sc_error::{Error, Result};
use sc_query::{Assignment, Delete, Expr, Insert, OrderBy, Select, Source, Statement, Value};
use sc_types::{Attrs, BasicType, TypeRef};
use serde_json::Value as Json;

use crate::run::{Run, RunId, RunKind, RunState};

/// Name of the runs table in the primary database.
pub const RUNS_TABLE: &str = "_sc_runs";

/// The UUID primary-key column (§9).
pub const COL_ID: &str = "id";
/// `agent` or `workflow` — which engine this run belongs to.
pub const COL_KIND: &str = "kind";
/// What is running: the agent's name.
pub const COL_SUBJECT: &str = "subject";
/// The human-readable description column (§9).
pub const COL_DESCRIPTION: &str = "description";
/// Where the run got to: `running` | `done` | `failed` | `aborted`.
pub const COL_STATE: &str = "state";
/// Why a failed run failed, NULL otherwise.
pub const COL_ERROR: &str = "error";
/// The resumable state — an `AgentLoop` for an agent run.
pub const COL_CONTEXT: &str = "context";
/// The user the run is on behalf of, NULL for one a trigger started.
pub const COL_USER: &str = "user_id";
/// The sparse per-run values column (§9).
pub const COL_ATTRIBUTES: &str = "attributes";
/// When the run was created.
pub const COL_CREATED_AT: &str = "created_at";
/// When it was last written — after every step.
pub const COL_UPDATED_AT: &str = "updated_at";

/// The fields of the `_sc_runs` table, in declaration order.
fn run_fields() -> Vec<DataField> {
    let text = || TypeRef::Basic(BasicType::Text);
    let json = || TypeRef::Basic(BasicType::Json);
    let uuid = || TypeRef::Basic(BasicType::Uuid);
    let ts = || TypeRef::Basic(BasicType::Timestamp);
    vec![
        DataField::plain(COL_ID, uuid()).required().primary_key(),
        DataField::plain(COL_KIND, text()).required(),
        DataField::plain(COL_SUBJECT, text()).required(),
        DataField::plain(COL_DESCRIPTION, text()),
        DataField::plain(COL_STATE, text()).required(),
        DataField::plain(COL_ERROR, text()),
        DataField::plain(COL_CONTEXT, json()).required(),
        // Nullable, and *not* a foreign key: a run is a record of what happened,
        // and deleting the user who chatted must not delete the evidence or be
        // blocked by it.
        DataField::plain(COL_USER, uuid()),
        DataField::plain(COL_ATTRIBUTES, json()).required(),
        DataField::plain(COL_CREATED_AT, ts()).required(),
        DataField::plain(COL_UPDATED_AT, ts()).required(),
    ]
}

/// Ensure the `_sc_runs` table exists, creating it if absent, and return it.
pub async fn bootstrap_runs(catalog: &Catalog) -> Result<Table> {
    catalog.bootstrap_table(RUNS_TABLE, &run_fields()).await
}

/// Save a run: insert its row, or update it in place if one with its [`RunId`]
/// already exists.
///
/// No validation, deliberately: a run is not a definition an admin wrote, it is
/// what happened. Refusing to store a step because the state looks odd would lose
/// the only record of the thing that went wrong.
pub async fn save_run(catalog: &Catalog, run: &Run) -> Result<()> {
    let columns = run_columns();
    let values = run_values(run);

    if load_run(catalog, run.id).await?.is_some() {
        let assignments = columns
            .iter()
            .zip(values)
            // Neither the id nor the creation instant is something a later step
            // may rewrite.
            .filter(|(col, _)| *col != COL_ID && *col != COL_CREATED_AT)
            .map(|(col, value)| Assignment::new(col.clone(), Expr::Lit(value)))
            .collect();
        let update = sc_query::Update::new(RUNS_TABLE, assignments)
            .filter(Expr::col(COL_ID).eq(Expr::lit(run.id.0)));
        exec(catalog, Statement::from(update)).await
    } else {
        let insert = Insert::row(
            RUNS_TABLE,
            columns,
            values.into_iter().map(Expr::Lit).collect(),
        );
        exec(catalog, Statement::from(insert)).await
    }
}

/// Load the run with this id, if it exists.
pub async fn load_run(catalog: &Catalog, id: RunId) -> Result<Option<Run>> {
    let select =
        Select::from(Source::table(RUNS_TABLE)).filter(Expr::col(COL_ID).eq(Expr::lit(id.0)));
    match rows(catalog, select).await?.first() {
        Some(row) => Ok(Some(run_from_row(row)?)),
        None => Ok(None),
    }
}

/// The run with this id, or a not-found error naming it.
pub async fn require_run(catalog: &Catalog, id: RunId) -> Result<Run> {
    load_run(catalog, id)
        .await?
        .ok_or_else(|| Error::not_found(format!("no run with id {id}")))
}

/// Every run of `subject`, newest first — the chat panel's history.
pub async fn list_runs(catalog: &Catalog, subject: &str) -> Result<Vec<Run>> {
    let mut select = Select::from(Source::table(RUNS_TABLE))
        .filter(Expr::col(COL_SUBJECT).eq(Expr::lit(subject)));
    select.order = vec![OrderBy::desc(Expr::col(COL_CREATED_AT))];
    rows(catalog, select)
        .await?
        .iter()
        .map(run_from_row)
        .collect()
}

/// Delete a run, returning whether one was there to delete.
pub async fn delete_run(catalog: &Catalog, id: RunId) -> Result<bool> {
    if load_run(catalog, id).await?.is_none() {
        return Ok(false);
    }
    let delete = Delete::from(RUNS_TABLE).filter(Expr::col(COL_ID).eq(Expr::lit(id.0)));
    exec(catalog, Statement::from(delete)).await?;
    Ok(true)
}

/// The row's columns, in the order [`run_values`] produces them.
fn run_columns() -> Vec<String> {
    [
        COL_ID,
        COL_KIND,
        COL_SUBJECT,
        COL_DESCRIPTION,
        COL_STATE,
        COL_ERROR,
        COL_CONTEXT,
        COL_USER,
        COL_ATTRIBUTES,
        COL_CREATED_AT,
        COL_UPDATED_AT,
    ]
    .iter()
    .map(|c| (*c).to_owned())
    .collect()
}

/// The run serialised to its row's values, in [`run_columns`] order.
fn run_values(run: &Run) -> Vec<Value> {
    vec![
        Value::Uuid(run.id.0),
        Value::Text(run.kind.as_str().to_owned()),
        Value::Text(run.subject.clone()),
        Value::Text(run.description.clone()),
        Value::Text(run.state.as_str().to_owned()),
        match &run.error {
            Some(error) => Value::Text(error.clone()),
            None => Value::Null,
        },
        Value::Json(run.context.clone()),
        match run.user {
            Some(user) => Value::Uuid(user),
            None => Value::Null,
        },
        Value::Json(Json::Object(run.attributes.clone())),
        Value::Timestamp(run.created_at),
        Value::Timestamp(run.updated_at),
    ]
}

/// Rebuild a [`Run`] from its `_sc_runs` row.
///
/// Strict, for the reason `_sc_agents`' reader is: a run read as something other
/// than what was stored would be resumed as something other than what was
/// running.
fn run_from_row(row: &Row) -> Result<Run> {
    let id = match row.get(COL_ID) {
        Some(Value::Uuid(u)) => RunId(*u),
        other => return Err(bad_column(COL_ID, "a uuid", other)),
    };
    Ok(Run {
        id,
        kind: RunKind::parse(&text(row, COL_KIND)?)
            .map_err(|e| Error::invalid(format!("run {id}: {e}")))?,
        subject: text(row, COL_SUBJECT)?,
        description: optional_text(row, COL_DESCRIPTION)?.unwrap_or_default(),
        state: RunState::parse(&text(row, COL_STATE)?)
            .map_err(|e| Error::invalid(format!("run {id}: {e}")))?,
        error: optional_text(row, COL_ERROR)?,
        context: match row.get(COL_CONTEXT) {
            Some(Value::Json(json)) => json.clone(),
            other => return Err(bad_column(COL_CONTEXT, "json", other)),
        },
        user: match row.get(COL_USER) {
            Some(Value::Uuid(u)) => Some(*u),
            Some(Value::Null) | None => None,
            other => return Err(bad_column(COL_USER, "a uuid", other)),
        },
        attributes: object(row, COL_ATTRIBUTES)?,
        created_at: timestamp(row, COL_CREATED_AT)?,
        updated_at: timestamp(row, COL_UPDATED_AT)?,
    })
}

fn text(row: &Row, column: &str) -> Result<String> {
    match row.get(column) {
        Some(Value::Text(t)) => Ok(t.clone()),
        other => Err(bad_column(column, "text", other)),
    }
}

fn optional_text(row: &Row, column: &str) -> Result<Option<String>> {
    match row.get(column) {
        Some(Value::Text(t)) if t.is_empty() => Ok(None),
        Some(Value::Text(t)) => Ok(Some(t.clone())),
        Some(Value::Null) | None => Ok(None),
        other => Err(bad_column(column, "text", other)),
    }
}

fn object(row: &Row, column: &str) -> Result<Attrs> {
    match row.get(column) {
        Some(Value::Json(Json::Object(o))) => Ok(o.clone()),
        Some(Value::Json(_)) => Err(Error::invalid(format!(
            "{RUNS_TABLE}.{column} should be a json object"
        ))),
        other => Err(bad_column(column, "json", other)),
    }
}

fn timestamp(row: &Row, column: &str) -> Result<DateTime<Utc>> {
    match row.get(column) {
        Some(Value::Timestamp(t)) => Ok(*t),
        other => Err(bad_column(column, "a timestamp", other)),
    }
}

fn bad_column(column: &str, expected: &str, got: Option<&Value>) -> Error {
    match got {
        Some(value) => Error::invalid(format!(
            "{RUNS_TABLE}.{column} should be {expected}, got {}",
            value.kind()
        )),
        None => Error::invalid(format!("row has no `{column}` column")),
    }
}

async fn exec(catalog: &Catalog, statement: Statement) -> Result<()> {
    catalog
        .primary()
        .query(&statement)
        .await?
        .try_collect()
        .await?;
    Ok(())
}

async fn rows(catalog: &Catalog, select: Select) -> Result<Vec<Row>> {
    catalog
        .primary()
        .query(&Statement::from(select))
        .await?
        .try_collect()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_trait::RunCaller;
    use crate::machine::AgentLoop;

    #[test]
    fn schema_carries_the_kind_that_discriminates_the_two_engines() {
        let fields = run_fields();
        let by_name = |n: &str| fields.iter().find(|f| f.base.name == n).unwrap();
        assert!(by_name(COL_ID).primary_key);
        assert!(by_name(COL_KIND).required);
        assert!(by_name(COL_SUBJECT).required);
        assert!(by_name(COL_CONTEXT).required);
        assert!(by_name(COL_STATE).required);
        // Both instants are required: a run that cannot say when it last moved
        // is one nothing can decide is stuck.
        assert!(by_name(COL_CREATED_AT).required && by_name(COL_UPDATED_AT).required);
        // The user and the error are the two honest nulls.
        assert!(!by_name(COL_USER).required && !by_name(COL_ERROR).required);
        // A run has no name; see the module docs.
        assert!(fields.iter().all(|f| f.base.name != "name"));
    }

    #[test]
    fn columns_and_values_stay_in_step() {
        let run = Run::new("a", &RunCaller::system(), &AgentLoop::new(20));
        assert_eq!(run_columns().len(), run_values(&run).len());
        let declared: Vec<String> = run_fields().iter().map(|f| f.base.name.clone()).collect();
        assert_eq!(run_columns(), declared);
    }

    #[test]
    fn a_system_run_stores_a_null_user_and_a_live_run_a_null_error() {
        let run = Run::new("a", &RunCaller::system(), &AgentLoop::new(20));
        let values = run_values(&run);
        assert_eq!(values[5], Value::Null); // error
        assert_eq!(values[7], Value::Null); // user
        assert_eq!(values[4], Value::Text("running".to_owned()));
    }
}
