//! `query_table` — read one configured table (§11.3).
//!
//! The first trait, and the one that sets the pattern the rest follow. Three
//! properties are load-bearing:
//!
//! - **The tool's description and JSON schema are generated from the table's own
//!   fields.** A model told "call `query_books` with a `where` object" and left
//!   to guess the column names will guess, and a guess that misses is a wasted
//!   turn the person watching pays for. So the schema *is* the table: the
//!   filterable fields are enumerated, each with its type, and the ordering key
//!   is an enum of exactly the fields that can be ordered by.
//! - **The read goes through `sc_api::read_rows_as`**, which is the same §7.3
//!   rule the REST API applies. The caller is the run's, so an agent chatting
//!   with a user below the table's read floor sees exactly the rows that user's
//!   ownership formula grants and no others. This is not a check this crate
//!   performs — it is a check this crate is unable to skip.
//! - **The configuration bounds the tool, not the other way round.** The field
//!   allow-list is what the model may see, filter on and order by; `max_rows` is
//!   a ceiling the `limit` argument is clamped to rather than a default it can
//!   raise. An agent cannot widen its own grant by asking nicely.

use sc_agent::{AgentTrait, TraitCheck, TraitContext};
use sc_catalog::{Catalog, DataField, DataFieldKind, Table};
use sc_error::{Error, Result};
use sc_llm::ToolSpec;
use sc_query::{BinOp, Expr, InSet, OrderBy, UnOp, Value};
use sc_types::{Attrs, BasicType, FormField};
use serde_json::{Map, Value as Json, json};

use sc_api::rows::{self, RowQuery};

/// The table this trait reads.
pub const CFG_TABLE: &str = "table";
/// The fields the model may see, filter on and order by. Empty means all of them.
pub const CFG_FIELDS: &str = "fields";
/// The ceiling on how many rows one call may return.
pub const CFG_MAX_ROWS: &str = "max_rows";

/// The ceiling when the admin sets none.
///
/// Small on purpose. A tool result becomes context the model pays for on every
/// subsequent turn, so an unbounded read is not merely slow — it is a
/// conversation that gets more expensive and less coherent with each answer. An
/// admin who wants a thousand rows says so.
pub const DEFAULT_MAX_ROWS: i64 = 50;

/// Read rows of one configured table.
pub struct QueryTable;

/// The tool one `query_table` instance offers, derived from its table.
///
/// Public because it is the answer to "what will this be called?", which the
/// admin UI wants before the agent is saved and the collision check
/// (§11.2) wants at the moment of saving.
pub fn tool_name(table: &str) -> String {
    format!("query_{table}")
}

#[async_trait::async_trait]
impl AgentTrait for QueryTable {
    fn name(&self) -> &str {
        "query_table"
    }

    fn description(&self) -> &str {
        "Read rows of one table, filtered, ordered and bounded"
    }

    fn config_spec(&self) -> Vec<FormField> {
        vec![
            FormField::new(CFG_TABLE, BasicType::Text)
                .label("Table")
                .required(),
            FormField::new(CFG_FIELDS, BasicType::Json)
                .label("Fields")
                .default_value(Json::Array(Vec::new())),
            FormField::new(CFG_MAX_ROWS, BasicType::Int)
                .label("Maximum rows")
                .default_value(DEFAULT_MAX_ROWS),
        ]
    }

    async fn validate_config(&self, check: &TraitCheck<'_>) -> Result<()> {
        let table = configured_table(check.catalog, check.config)?;
        // Addressable by primary key: a row the model reports on that nobody can
        // then name is a half-useful answer, and every write trait beside this
        // one needs the key outright.
        rows::single_pk(&table)?;
        // Every named field is real. A stale allow-list would otherwise silently
        // narrow the tool to nothing, which reads as an empty table.
        for name in configured_fields(check.config)? {
            if table.field(&name).is_none() {
                return Err(Error::invalid(format!(
                    "`{}` has no field `{name}`",
                    table.name
                )));
            }
        }
        if max_rows(check.config)? == 0 {
            return Err(Error::invalid(format!(
                "`{CFG_MAX_ROWS}` must be at least 1"
            )));
        }
        Ok(())
    }

    fn tools(&self, catalog: &Catalog, config: &Attrs) -> Vec<ToolSpec> {
        let configured = config_str(config, CFG_TABLE);
        let name = tool_name(&configured);
        // The table is gone, or the configuration never validated. Keep the tool
        // under the name the admin's configuration gives it — the collision check
        // and the admin UI both ask this question of agents that do not validate
        // — and describe what can still be described.
        let Ok(table) = configured_table(catalog, config) else {
            return vec![ToolSpec::new(
                name,
                format!("Read rows of the `{configured}` table (which no longer exists)"),
                json!({ "type": "object", "properties": {} }),
            )];
        };
        let visible = visible_fields(&table, config).unwrap_or_default();
        let ceiling = max_rows(config).unwrap_or(DEFAULT_MAX_ROWS as u64);
        vec![ToolSpec::new(
            name,
            describe(&table, &visible, ceiling),
            parameters(&table, &visible, ceiling),
        )]
    }

    async fn call(
        &self,
        config: &Attrs,
        _tool: &str,
        args: &Json,
        ctx: &mut TraitContext<'_>,
    ) -> Result<Json> {
        let table = configured_table(ctx.catalog, config)?;
        let visible = visible_fields(&table, config)?;
        let ceiling = max_rows(config)?;
        let args = arguments(args)?;

        let filter = where_expr(&table, &visible, args.get(ARG_WHERE))?;
        let order = order_by(&table, &visible, &args)?;
        let limit = requested_limit(&args, ceiling)?;

        // One more row than asked for, so "there are more" can be reported
        // rather than guessed at. A model that cannot tell a complete answer
        // from a truncated one will state the truncated one as fact.
        let query = RowQuery::new()
            .where_(filter)
            .order_by(order)
            .limit(limit.saturating_add(1));
        let found = sc_api::read_rows_as(
            ctx.catalog,
            &table,
            &query,
            ctx.caller.role,
            ctx.caller.user.as_ref(),
            ctx.evaluator,
        )
        .await?;

        let mut rows: Vec<Json> = found.as_array().cloned().unwrap_or_default();
        let more = rows.len() as u64 > limit;
        rows.truncate(limit as usize);
        let rows: Vec<Json> = rows.iter().map(|row| project(row, &visible)).collect();
        Ok(json!({
            "table": table.name,
            "rows": rows,
            "count": rows.len(),
            "more_rows_available": more,
        }))
    }
}

// --- the tool, described from the table -------------------------------------

/// What the model is told this tool does — the table, what it returns, how much
/// of it, and every field by name and type.
///
/// This is the whole of what the model has to go on when it decides whether to
/// call and what to ask for, so it is prose with the schema in it rather than a
/// label. Listing the fields here as well as in the parameter schema is
/// deliberate: the description is the part a model reads when *choosing* a tool,
/// and "does this table even have an author?" is the question it is choosing on.
fn describe(table: &Table, visible: &[String], ceiling: u64) -> String {
    let mut out = format!("Read rows of the `{}` table", table.name);
    if !table.description.trim().is_empty() {
        out.push_str(&format!(" ({})", table.description.trim()));
    }
    out.push_str(&format!(
        ". Returns matching rows as JSON objects, at most {ceiling} per call, \
         and says whether more were available.\n\nFields: {}.",
        field_list(table, visible)
    ));
    if visible.len() < table.fields.len() {
        out.push_str(" Other fields of this table are not available to you.");
    }
    out.push_str(
        "\n\nIn `where`, each entry is a field name against either a value to \
         match exactly or an object with one operator key: `eq`, `ne`, `gt`, \
         `gte`, `lt`, `lte`, `like`, `ilike` (text patterns, `%` matches any \
         run of characters), `in` (a list) or `is_null` (true or false). All \
         the entries must hold at once.",
    );
    out
}

/// `id (int, primary key), title (text), author (int, references authors)` — the
/// table as one line.
fn field_list(table: &Table, visible: &[String]) -> String {
    table
        .fields
        .iter()
        .filter(|f| visible.contains(&f.base.name))
        .map(|f| format!("{} ({})", f.base.name, field_note(f)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One field's type and whatever else the model needs to know to use it.
fn field_note(field: &DataField) -> String {
    let mut note = field.base.type_.name().to_owned();
    if field.primary_key {
        note.push_str(", primary key");
    }
    match &field.kind {
        DataFieldKind::Key { target_table, .. } => {
            note.push_str(&format!(", references {}", target_table.0));
        }
        // A calculated field is computed on read and has no column, so it comes
        // back with every row but cannot be filtered or ordered on — and being
        // told that up front is cheaper than a failed call that says it.
        DataFieldKind::Calc { .. } => note.push_str(", computed; not filterable"),
        DataFieldKind::File { .. } => note.push_str(", a file path"),
        DataFieldKind::Plain => {}
    }
    note
}

/// The tool's JSON Schema, generated from the table.
fn parameters(table: &Table, visible: &[String], ceiling: u64) -> Json {
    // Only the stored fields: `where` and `order_by` become SQL, and a
    // calculated field has no column to put in it.
    let queryable: Vec<&DataField> = table
        .fields
        .iter()
        .filter(|f| visible.contains(&f.base.name) && !f.is_calc())
        .collect();
    let mut conditions = Map::new();
    for field in &queryable {
        conditions.insert(
            field.base.name.clone(),
            json!({
                "description": format!("{} ({})", field.base.name, field_note(field)),
            }),
        );
    }
    let names: Vec<&str> = queryable.iter().map(|f| f.base.name.as_str()).collect();
    json!({
        "type": "object",
        "properties": {
            ARG_WHERE: {
                "type": "object",
                "description":
                    "Which rows: field name → an exact value, or an object with \
                     one of the operator keys. Omit it for every row.",
                "properties": Json::Object(conditions),
                "additionalProperties": false,
            },
            ARG_ORDER_BY: {
                "type": "string",
                "description": "Sort the rows by this field.",
                "enum": names,
            },
            ARG_DESCENDING: {
                "type": "boolean",
                "description":
                    "Sort from the largest down instead of the smallest up. \
                     Only meaningful with `order_by`.",
            },
            ARG_LIMIT: {
                "type": "integer",
                "description": format!(
                    "At most this many rows. The ceiling — and the default — is {ceiling}."
                ),
                "minimum": 1,
                "maximum": ceiling,
            },
        },
        "additionalProperties": false,
    })
}

// --- the tool's arguments ---------------------------------------------------

/// The filter object.
const ARG_WHERE: &str = "where";
/// The field to order by.
const ARG_ORDER_BY: &str = "order_by";
/// Whether that ordering descends.
const ARG_DESCENDING: &str = "descending";
/// How many rows at most.
const ARG_LIMIT: &str = "limit";

/// Every argument this tool takes — so one it does not is refused **by name**.
const ARGUMENTS: [&str; 4] = [ARG_WHERE, ARG_ORDER_BY, ARG_DESCENDING, ARG_LIMIT];

/// The arguments object, or an empty one.
///
/// A missing or null argument bag means "every row": both vendors send one for a
/// tool whose parameters are all optional, and refusing it would fail the most
/// ordinary call there is. Anything else that is not an object is the model
/// having produced something the schema did not describe, and saying so is what
/// lets it correct itself.
fn arguments(args: &Json) -> Result<Map<String, Json>> {
    let obj = match args {
        Json::Null => Map::new(),
        Json::Object(map) => map.clone(),
        other => {
            return Err(Error::invalid(format!(
                "the arguments should be an object, got {other}"
            )));
        }
    };
    for key in obj.keys() {
        if !ARGUMENTS.contains(&key.as_str()) {
            return Err(Error::invalid(format!(
                "unknown argument `{key}`; this tool takes {}",
                ARGUMENTS.join(", ")
            )));
        }
    }
    Ok(obj)
}

/// The `limit`, clamped to the configured ceiling.
///
/// Clamped rather than refused: a model that asked for 500 from a tool bounded
/// at 50 has made a recoverable mistake, and 50 rows plus `more_rows_available`
/// tells it exactly that. A `limit` of zero or a negative one is not a smaller
/// request, it is a malformed one.
fn requested_limit(args: &Map<String, Json>, ceiling: u64) -> Result<u64> {
    match args.get(ARG_LIMIT) {
        None | Some(Json::Null) => Ok(ceiling),
        Some(Json::Number(n)) => match n.as_i64() {
            Some(n) if n >= 1 => Ok((n as u64).min(ceiling)),
            _ => Err(Error::invalid(format!(
                "`{ARG_LIMIT}` should be a whole number of at least 1, got {n}"
            ))),
        },
        Some(other) => Err(Error::invalid(format!(
            "`{ARG_LIMIT}` should be a number, got {other}"
        ))),
    }
}

/// The `ORDER BY`, from `order_by` plus `descending`.
fn order_by(table: &Table, visible: &[String], args: &Map<String, Json>) -> Result<Vec<OrderBy>> {
    let Some(key) = args.get(ARG_ORDER_BY).filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let key = key.as_str().ok_or_else(|| {
        Error::invalid(format!(
            "`{ARG_ORDER_BY}` should be a field name, got {key}"
        ))
    })?;
    // Ordering happens in the database, so a calculated field — which has no
    // column — cannot be ordered by any more than it can be filtered on.
    let field = queryable_field(table, visible, key, ARG_ORDER_BY)?;
    let descending = match args.get(ARG_DESCENDING) {
        None | Some(Json::Null) => false,
        Some(Json::Bool(b)) => *b,
        Some(other) => {
            return Err(Error::invalid(format!(
                "`{ARG_DESCENDING}` should be true or false, got {other}"
            )));
        }
    };
    let col = Expr::col(field.base.name.clone());
    Ok(vec![match descending {
        true => OrderBy::desc(col),
        false => OrderBy::asc(col),
    }])
}

// --- the `where` object -----------------------------------------------------

/// The comparisons a `where` entry may ask for, in the order the tool's
/// description lists them.
const OPERATORS: [&str; 10] = [
    "eq", "ne", "gt", "gte", "lt", "lte", "like", "ilike", "in", "is_null",
];

/// The predicate a `where` object translates to — every entry ANDed.
///
/// `None` for an absent or empty object, which is "every row" rather than "no
/// rows": a model that wants a count of everything sends `{}`, and reading that
/// as an unsatisfiable filter would answer zero.
fn where_expr(table: &Table, visible: &[String], where_: Option<&Json>) -> Result<Option<Expr>> {
    let Some(where_) = where_.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let obj = where_.as_object().ok_or_else(|| {
        Error::invalid(format!(
            "`{ARG_WHERE}` should be an object of field conditions, got {where_}"
        ))
    })?;
    let mut predicate: Option<Expr> = None;
    for (name, condition) in obj {
        let field = queryable_field(table, visible, name, ARG_WHERE)?;
        let expr = condition_expr(table, field, condition)?;
        predicate = Some(match predicate {
            Some(existing) => existing.and(expr),
            None => expr,
        });
    }
    Ok(predicate)
}

/// One field's condition.
///
/// A JSON object whose single key is one of [`OPERATORS`] is that comparison;
/// **anything else is a literal to match exactly**, including an object destined
/// for a `Json` column. The rule is stated that way round — and in the tool's
/// own description — because the alternative (an object is always an operator)
/// makes a `Json` column unfilterable, and a model that means equality can
/// always say `{"eq": …}`.
fn condition_expr(table: &Table, field: &DataField, condition: &Json) -> Result<Expr> {
    let name = &field.base.name;
    let col = || Expr::col(name.clone());
    if let Json::Object(map) = condition
        && map.len() == 1
        && let Some((op, operand)) = map.iter().next()
        && OPERATORS.contains(&op.as_str())
    {
        let literal =
            |json: &Json| -> Result<Expr> { Ok(Expr::lit(rows::column_value(table, name, json)?)) };
        let text = |json: &Json| -> Result<Expr> {
            json.as_str()
                .map(|s| Expr::lit(Value::Text(s.to_owned())))
                .ok_or_else(|| {
                    Error::invalid(format!("`{name}`: `{op}` takes a text pattern, got {json}"))
                })
        };
        return Ok(match op.as_str() {
            // `eq`/`ne` against null mean the null tests, because SQL's `=` is
            // never true of one and a model writing `{"eq": null}` means "unset".
            "eq" if operand.is_null() => Expr::unary(UnOp::IsNull, col()),
            "ne" if operand.is_null() => Expr::unary(UnOp::IsNotNull, col()),
            "eq" => Expr::binary(BinOp::Eq, col(), literal(operand)?),
            "ne" => Expr::binary(BinOp::Ne, col(), literal(operand)?),
            "gt" => Expr::binary(BinOp::Gt, col(), literal(operand)?),
            "gte" => Expr::binary(BinOp::Ge, col(), literal(operand)?),
            "lt" => Expr::binary(BinOp::Lt, col(), literal(operand)?),
            "lte" => Expr::binary(BinOp::Le, col(), literal(operand)?),
            "like" => Expr::binary(BinOp::Like, col(), text(operand)?),
            "ilike" => Expr::binary(BinOp::ILike, col(), text(operand)?),
            "in" => {
                let items = operand.as_array().ok_or_else(|| {
                    Error::invalid(format!("`{name}`: `in` takes a list, got {operand}"))
                })?;
                if items.is_empty() {
                    return Err(Error::invalid(format!(
                        "`{name}`: `in` needs at least one value"
                    )));
                }
                let set: Result<Vec<Expr>> = items.iter().map(literal).collect();
                Expr::In {
                    e: Box::new(col()),
                    set: InSet::List(set?),
                }
            }
            "is_null" => {
                let want = operand.as_bool().ok_or_else(|| {
                    Error::invalid(format!(
                        "`{name}`: `is_null` takes true or false, got {operand}"
                    ))
                })?;
                Expr::unary(if want { UnOp::IsNull } else { UnOp::IsNotNull }, col())
            }
            other => {
                return Err(Error::invalid(format!(
                    "`{name}`: unknown operator `{other}`"
                )));
            }
        });
    }
    Ok(match condition {
        Json::Null => Expr::unary(UnOp::IsNull, col()),
        other => Expr::binary(
            BinOp::Eq,
            col(),
            Expr::lit(rows::column_value(table, name, other)?),
        ),
    })
}

// --- the configuration ------------------------------------------------------

/// A string setting, or the empty string.
fn config_str(config: &Attrs, key: &str) -> String {
    config
        .get(key)
        .and_then(Json::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// The configured table, resolved against the catalog.
fn configured_table(catalog: &Catalog, config: &Attrs) -> Result<Table> {
    let name = config_str(config, CFG_TABLE);
    if name.is_empty() {
        return Err(Error::invalid(format!("`{CFG_TABLE}` is required")));
    }
    catalog.require(&name)
}

/// The configured allow-list, as written. Empty means "every field".
fn configured_fields(config: &Attrs) -> Result<Vec<String>> {
    match config.get(CFG_FIELDS) {
        None | Some(Json::Null) => Ok(Vec::new()),
        Some(Json::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        Error::invalid(format!("`{CFG_FIELDS}` should be a list of field names"))
                    })
            })
            .collect(),
        Some(other) => Err(Error::invalid(format!(
            "`{CFG_FIELDS}` should be a list of field names, got {other}"
        ))),
    }
}

/// The fields this instance exposes, in the table's own declaration order.
///
/// Table order rather than allow-list order, so the tool's description reads
/// like the table does and two instances over the same table cannot describe it
/// differently depending on how the admin typed the list.
fn visible_fields(table: &Table, config: &Attrs) -> Result<Vec<String>> {
    let allowed = configured_fields(config)?;
    Ok(table
        .fields
        .iter()
        .map(|f| f.base.name.clone())
        .filter(|name| allowed.is_empty() || allowed.contains(name))
        .collect())
}

/// The configured ceiling.
fn max_rows(config: &Attrs) -> Result<u64> {
    match config.get(CFG_MAX_ROWS) {
        None | Some(Json::Null) => Ok(DEFAULT_MAX_ROWS as u64),
        Some(Json::Number(n)) => match n.as_i64() {
            Some(n) if n >= 0 => Ok(n as u64),
            _ => Err(Error::invalid(format!(
                "`{CFG_MAX_ROWS}` should be a whole number, got {n}"
            ))),
        },
        Some(other) => Err(Error::invalid(format!(
            "`{CFG_MAX_ROWS}` should be a number, got {other}"
        ))),
    }
}

/// A field the model may filter on or order by: visible, real, and backed by a
/// column.
///
/// The error names the alternatives, because a model that guessed a column name
/// can only recover if it is told the ones that exist — and being told is
/// cheaper than a second round trip through a failed query.
fn queryable_field<'a>(
    table: &'a Table,
    visible: &[String],
    name: &str,
    what: &str,
) -> Result<&'a DataField> {
    if !visible.contains(&name.to_owned()) {
        return Err(Error::invalid(format!(
            "`{what}`: `{}` has no field `{name}` you may use; the fields are {}",
            table.name,
            visible.join(", ")
        )));
    }
    let field = table
        .field(name)
        .ok_or_else(|| Error::invalid(format!("`{}` has no field `{name}`", table.name)))?;
    if field.is_calc() {
        return Err(Error::invalid(format!(
            "`{what}`: `{name}` is a calculated field; it is returned with each row \
             but cannot be filtered or ordered on"
        )));
    }
    Ok(field)
}

/// One row narrowed to the visible fields.
fn project(row: &Json, visible: &[String]) -> Json {
    let Some(obj) = row.as_object() else {
        return row.clone();
    };
    let mut out = Map::with_capacity(visible.len());
    for name in visible {
        if let Some(value) = obj.get(name) {
            out.insert(name.clone(), value.clone());
        }
    }
    Json::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tools_name_is_derived_from_its_table() {
        assert_eq!(tool_name("books"), "query_books");
    }

    #[test]
    fn the_limit_is_clamped_to_the_ceiling_rather_than_refused() {
        let args = |json: Json| json.as_object().cloned().unwrap();
        assert_eq!(requested_limit(&args(json!({})), 50).unwrap(), 50);
        assert_eq!(requested_limit(&args(json!({"limit": 5})), 50).unwrap(), 5);
        assert_eq!(
            requested_limit(&args(json!({"limit": 500})), 50).unwrap(),
            50
        );
        // Zero and a negative are malformed, not small.
        assert!(requested_limit(&args(json!({"limit": 0})), 50).is_err());
        assert!(requested_limit(&args(json!({"limit": -3})), 50).is_err());
        assert!(requested_limit(&args(json!({"limit": "5"})), 50).is_err());
    }

    #[test]
    fn an_argument_the_schema_does_not_describe_is_refused_by_name() {
        let err = arguments(&json!({"sql": "drop table books"})).unwrap_err();
        assert!(err.to_string().contains("`sql`"), "{err}");
        // …while an absent bag is the ordinary "everything" call.
        assert!(arguments(&Json::Null).unwrap().is_empty());
        assert!(arguments(&json!({})).unwrap().is_empty());
        assert!(arguments(&json!([])).is_err());
    }

    #[test]
    fn the_allow_list_is_read_in_the_tables_order_not_the_admins() {
        let config: Attrs = json!({"fields": ["pages", "title"]})
            .as_object()
            .cloned()
            .unwrap();
        assert_eq!(
            configured_fields(&config).unwrap(),
            vec!["pages".to_owned(), "title".to_owned()]
        );
        let empty = Attrs::new();
        assert!(configured_fields(&empty).unwrap().is_empty());
        let wrong: Attrs = json!({"fields": "title"}).as_object().cloned().unwrap();
        assert!(configured_fields(&wrong).is_err());
    }

    #[test]
    fn the_ceiling_defaults_and_refuses_nonsense() {
        assert_eq!(max_rows(&Attrs::new()).unwrap(), DEFAULT_MAX_ROWS as u64);
        let cfg: Attrs = json!({"max_rows": 7}).as_object().cloned().unwrap();
        assert_eq!(max_rows(&cfg).unwrap(), 7);
        let cfg: Attrs = json!({"max_rows": -1}).as_object().cloned().unwrap();
        assert!(max_rows(&cfg).is_err());
    }

    #[test]
    fn a_row_is_narrowed_to_the_visible_fields() {
        let row = json!({"id": 1, "title": "A", "secret": "x"});
        let visible = vec!["id".to_owned(), "title".to_owned()];
        assert_eq!(project(&row, &visible), json!({"id": 1, "title": "A"}));
    }
}
