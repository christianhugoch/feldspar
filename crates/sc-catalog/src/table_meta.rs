//! The `_sc_tables` overlay: its schema, bootstrap, and the [`TableMeta`] ⇄ row
//! mapping (design §9).
//!
//! This is the first **overlay** table Saltcorn stores, and that word carries a
//! rule the other `_sc_*` tables do not have to keep. A file store or an
//! application *is* its row — delete the row and the object is gone (§13.2,
//! §14.1). A table is not: it exists in the database whether or not this table
//! has a row for it, and it must stay exactly as usable with no row as it is
//! today. So everything here **adds** to what introspection already yields —
//! access rules, a label, a description, sparse attributes — and nothing here
//! may restate a fact the database already knows. That is not a stylistic
//! preference: it is what keeps §9's "legacy databases just work" true, and it
//! is why a column like `nullable` must never appear in this table.
//!
//! The merge itself (TODO §1.2) lands next; this module is the storage under it.
//!
//! The shape follows the `_sc_file_stores` module column for column:
//! one column per value every row has, sparse values in `attributes`, and
//! **strict reads** — a missing or ill-typed column is an [`Error::invalid`]
//! naming the table and the column, never a silent default. Strictness matters
//! more here than it does for a store, because the values being read decide who
//! may read and write rows: a `min_role_read` that fails to parse must not
//! degrade to *some* role.
//!
//! Two deliberate differences from `_sc_file_stores`, both about the same thing:
//!
//! - **The access columns are `NOT NULL`.** A store's `min_role` is nullable
//!   because "no floor" is a real state for it. A table's rules are always a
//!   pair of roles — a row that exists says who may read and who may write, and
//!   a NULL there would be a third state ("inherit from what?") that has no
//!   answer.
//! - **A system (`_sc_*`) table may not have a row.** System tables are hidden
//!   from users (§9) and their access is not the admin's to widen; the check is
//!   here, on save, rather than only in the merge, so the state never exists.

use sc_db::Row;
use sc_error::{Error, Result};
use sc_query::{Assignment, Delete, Expr, Insert, Select, Source, Statement, Value};
use sc_types::{Attrs, BasicType, TypeRef};
use serde_json::Value as Json;
use uuid::Uuid;

use crate::catalog::Catalog;
use crate::field::DataField;
use crate::table::{AccessRules, Table};

/// Name of the table-overlay table in the primary database.
pub const TABLE_META_TABLE: &str = "_sc_tables";

/// The UUID primary-key column (§9).
pub const COL_ID: &str = "id";
/// The name of the table this row overlays — §9's required `name` column, which
/// for an overlay *is* the subject's name, and the key the merge joins on.
pub const COL_NAME: &str = "name";
/// The human label shown instead of the table name, when one is given.
pub const COL_LABEL: &str = "label";
/// The human-readable description column (§9).
pub const COL_DESCRIPTION: &str = "description";
/// Least-privileged role that may read the table's rows.
pub const COL_MIN_ROLE_READ: &str = "min_role_read";
/// Least-privileged role that may create, update or delete the table's rows.
pub const COL_MIN_ROLE_WRITE: &str = "min_role_write";
/// The sparse per-table values column (§9) — JSON, always an object.
pub const COL_ATTRIBUTES: &str = "attributes";

/// Identifies a stored table-overlay row.
///
/// Distinct from [`TableId`](crate::TableId), which is a table's *name* and is
/// what everything else references. This id is the row's identity, so an overlay
/// keeps its identity across edits — and, when table renaming eventually exists,
/// across a rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TableMetaId(pub Uuid);

impl TableMetaId {
    /// A fresh random id, for an overlay that has never been saved.
    pub fn new() -> TableMetaId {
        TableMetaId(Uuid::new_v4())
    }
}

impl Default for TableMetaId {
    fn default() -> TableMetaId {
        TableMetaId::new()
    }
}

/// The stored overlay for one table (design §9).
///
/// Deliberately **not** a [`Table`]: a `Table` is the merged view the rest of
/// the system sees — fields, primary key, source, all of which come from
/// introspection — while this is only the part an admin configured. Keeping the
/// two types apart is what stops "the overlay" and "the table" from drifting
/// into one another, and what makes "no row" expressible at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableMeta {
    /// The row's identity.
    pub id: TableMetaId,
    /// The name of the table this overlays. Unique across overlay rows.
    pub table_name: String,
    /// A human label; empty when none was given, in which case the table's own
    /// name is the label.
    pub label: String,
    /// A human description; empty when none was given.
    pub description: String,
    /// Who may read and who may write this table's rows.
    pub access: AccessRules,
    /// Sparse per-table values (§9).
    pub attributes: Attrs,
}

impl TableMeta {
    /// An overlay for `table_name` carrying nothing but the default (admin-only)
    /// access rules — the base to add settings to.
    ///
    /// The default matches [`AccessRules::default`] rather than something more
    /// permissive on purpose: creating an overlay row must never be the thing
    /// that widens access to a table.
    pub fn new(table_name: impl Into<String>) -> TableMeta {
        TableMeta {
            id: TableMetaId::new(),
            table_name: table_name.into(),
            label: String::new(),
            description: String::new(),
            access: AccessRules::default(),
            attributes: Attrs::new(),
        }
    }

    /// Set the human label.
    pub fn label(mut self, label: impl Into<String>) -> TableMeta {
        self.label = label.into();
        self
    }

    /// Set the description.
    pub fn description(mut self, description: impl Into<String>) -> TableMeta {
        self.description = description.into();
        self
    }

    /// Set both access roles.
    pub fn access(mut self, min_role_read: u8, min_role_write: u8) -> TableMeta {
        self.access = AccessRules {
            min_role_read,
            min_role_write,
        };
        self
    }

    /// Give the row a specific id — for a caller updating a known row rather
    /// than creating one.
    pub fn id(mut self, id: TableMetaId) -> TableMeta {
        self.id = id;
        self
    }
}

/// The fields of the `_sc_tables` table, in declaration order.
///
/// `name` carries the `UNIQUE` constraint: it is the key the merge joins on, and
/// two overlay rows for one table is not a state that can be merged — there
/// would be no rule saying which one's access rules apply. As with
/// `_sc_file_stores`, the database is the authority, because two admins saving
/// concurrently cannot see each other's transaction.
fn table_meta_fields() -> Vec<DataField> {
    let text = || TypeRef::Basic(BasicType::Text);
    let json = || TypeRef::Basic(BasicType::Json);
    let uuid = || TypeRef::Basic(BasicType::Uuid);
    let int = || TypeRef::Basic(BasicType::Int);
    vec![
        DataField::plain(COL_ID, uuid()).required().primary_key(),
        DataField::plain(COL_NAME, text()).required().unique(),
        DataField::plain(COL_LABEL, text()),
        DataField::plain(COL_DESCRIPTION, text()),
        // Required, unlike a store's `min_role`: see the module docs.
        DataField::plain(COL_MIN_ROLE_READ, int()).required(),
        DataField::plain(COL_MIN_ROLE_WRITE, int()).required(),
        DataField::plain(COL_ATTRIBUTES, json()).required(),
    ]
}

/// Ensure the `_sc_tables` table exists, creating it if absent, and return it.
///
/// Idempotent, and safe against a database that has never seen Saltcorn — the
/// same contract as [`bootstrap_file_stores`](crate::bootstrap_file_stores).
/// Call once at startup, after the [`Catalog`] is initialised.
pub async fn bootstrap_table_meta(catalog: &Catalog) -> Result<Table> {
    if let Some(existing) = catalog.get(TABLE_META_TABLE)? {
        return Ok(existing);
    }
    catalog
        .create_table(TABLE_META_TABLE, &table_meta_fields())
        .await
}

/// Save a table overlay: insert its row, or update it in place if a row with its
/// [`TableMetaId`] already exists.
///
/// What is checked here, and why each is checked *here* rather than at merge
/// time:
///
/// - **A name** — an overlay of nothing has no meaning.
/// - **Roles in `1..=100`** — the role scale is fixed (`sc-auth`), so an
///   out-of-range role is not a value to clamp; clamping would silently decide
///   who can reach the data.
/// - **Not a system table** — `_sc_*` tables are hidden from users (§9); their
///   access is not the admin's to configure, and refusing the row means the
///   merge never has to decide what to do with one.
/// - **The name is not already claimed by another row** — the database's
///   `UNIQUE` constraint remains the authority, but this check names the
///   conflict rather than surfacing a raw constraint violation.
///
/// Deliberately **not** checked: that the table exists. See
/// [`orphan_table_meta`] for why an overlay may legitimately outlive its table,
/// and note that requiring existence here would make the row unsavable in
/// exactly the situation where an admin is repairing one.
pub async fn save_table_meta(catalog: &Catalog, meta: &TableMeta) -> Result<()> {
    let name = meta.table_name.trim();
    if name.is_empty() {
        return Err(Error::invalid("a table overlay needs a table name"));
    }
    if name.starts_with("_sc_") {
        return Err(Error::invalid(format!(
            "`{name}` is a system table; its access rules are not configurable"
        )));
    }
    validate_access(name, &meta.access)?;

    if let Some(other) = load_table_meta_by_name(catalog, name).await?
        && other.id != meta.id
    {
        return Err(Error::invalid(format!(
            "table `{name}` already has an overlay row; \
             a table has at most one"
        )));
    }

    let columns = meta_columns();
    let values = meta_values(meta);
    if load_table_meta(catalog, meta.id).await?.is_some() {
        let assignments = columns
            .iter()
            .zip(values)
            // The id is the row's identity, not something to reassign.
            .filter(|(col, _)| *col != COL_ID)
            .map(|(col, value)| Assignment::new(col.clone(), Expr::Lit(value)))
            .collect();
        let update = sc_query::Update::new(TABLE_META_TABLE, assignments)
            .filter(Expr::col(COL_ID).eq(Expr::lit(meta.id.0)));
        run(catalog, Statement::from(update)).await?;
    } else {
        let insert = Insert::row(
            TABLE_META_TABLE,
            columns,
            values.into_iter().map(Expr::Lit).collect(),
        );
        run(catalog, Statement::from(insert)).await?;
    }
    Ok(())
}

/// Load the overlay with this id, if it exists.
pub async fn load_table_meta(catalog: &Catalog, id: TableMetaId) -> Result<Option<TableMeta>> {
    load_one(catalog, Expr::col(COL_ID).eq(Expr::lit(id.0))).await
}

/// Load the overlay for the table named `name`, if any — the lookup the merge
/// performs.
pub async fn load_table_meta_by_name(catalog: &Catalog, name: &str) -> Result<Option<TableMeta>> {
    load_one(catalog, Expr::col(COL_NAME).eq(Expr::lit(name))).await
}

/// Every stored overlay, ordered by table name.
pub async fn list_table_meta(catalog: &Catalog) -> Result<Vec<TableMeta>> {
    let select = Select::from(Source::table(TABLE_META_TABLE));
    let mut metas: Vec<TableMeta> = rows(catalog, select)
        .await?
        .iter()
        .map(table_meta_from_row)
        .collect::<Result<_>>()?;
    metas.sort_by(|a, b| a.table_name.cmp(&b.table_name));
    Ok(metas)
}

/// The overlays whose table is not in the catalog, ordered by table name.
///
/// **An orphan is kept, not deleted.** A table that is dropped and recreated —
/// by a restore, or by a migration run outside Saltcorn — would otherwise
/// silently lose its access rules, and losing access rules is the worst kind of
/// silent loss: the table comes back with [`AccessRules::default`], which is
/// admin-only, so an application that served it stops working with nothing
/// anywhere saying why. Keeping the row means recreating the table restores its
/// configuration, and this function is how the admin UI can say "these rows
/// describe tables that are not here" instead of the admin discovering it.
///
/// Names the catalog does not know about are the definition of "not here": the
/// cache is rebuilt from introspection on every reload, so a table absent from
/// it is absent from the database.
pub async fn orphan_table_meta(catalog: &Catalog) -> Result<Vec<TableMeta>> {
    let mut orphans = Vec::new();
    for meta in list_table_meta(catalog).await? {
        if catalog.get(&meta.table_name)?.is_none() {
            orphans.push(meta);
        }
    }
    Ok(orphans)
}

/// Delete a table overlay, returning whether one was there to delete.
///
/// **This removes the row and nothing else** — the table, its columns and every
/// row in it are untouched. Deleting an overlay is "forget what I configured",
/// which returns the table to its unconfigured state
/// ([`AccessRules::default`]), not "drop the table". The parallel with
/// [`delete_file_store`](crate::delete_file_store) is exact, and stronger here:
/// for an overlay, the subject existing independently of its row is not a
/// caveat, it is the whole point.
pub async fn delete_table_meta(catalog: &Catalog, id: TableMetaId) -> Result<bool> {
    if load_table_meta(catalog, id).await?.is_none() {
        return Ok(false);
    }
    let delete = Delete::from(TABLE_META_TABLE).filter(Expr::col(COL_ID).eq(Expr::lit(id.0)));
    run(catalog, Statement::from(delete)).await?;
    Ok(true)
}

/// Check both roles are on the `1..=100` scale, naming the table and which role
/// is wrong.
fn validate_access(table: &str, access: &AccessRules) -> Result<()> {
    for (column, role) in [
        (COL_MIN_ROLE_READ, access.min_role_read),
        (COL_MIN_ROLE_WRITE, access.min_role_write),
    ] {
        if !(1..=100).contains(&role) {
            return Err(Error::invalid(format!(
                "table `{table}`: {column} should be a role between 1 and 100, got {role}"
            )));
        }
    }
    Ok(())
}

/// The row's columns, in the order [`meta_values`] produces them.
fn meta_columns() -> Vec<String> {
    [
        COL_ID,
        COL_NAME,
        COL_LABEL,
        COL_DESCRIPTION,
        COL_MIN_ROLE_READ,
        COL_MIN_ROLE_WRITE,
        COL_ATTRIBUTES,
    ]
    .iter()
    .map(|c| (*c).to_owned())
    .collect()
}

/// The overlay serialised to its row's values, in [`meta_columns`] order.
fn meta_values(meta: &TableMeta) -> Vec<Value> {
    vec![
        Value::Uuid(meta.id.0),
        Value::Text(meta.table_name.trim().to_owned()),
        Value::Text(meta.label.clone()),
        Value::Text(meta.description.clone()),
        Value::Int(i64::from(meta.access.min_role_read)),
        Value::Int(i64::from(meta.access.min_role_write)),
        Value::Json(Json::Object(meta.attributes.clone())),
    ]
}

/// Rebuild a [`TableMeta`] from its `_sc_tables` row. The strictness note in the
/// module docs applies throughout.
fn table_meta_from_row(row: &Row) -> Result<TableMeta> {
    let id = match row.get(COL_ID) {
        Some(Value::Uuid(u)) => TableMetaId(*u),
        other => return Err(bad_column(COL_ID, "a uuid", other)),
    };
    Ok(TableMeta {
        id,
        table_name: text(row, COL_NAME)?,
        label: optional_text(row, COL_LABEL)?,
        description: optional_text(row, COL_DESCRIPTION)?,
        access: AccessRules {
            min_role_read: role(row, COL_MIN_ROLE_READ)?,
            min_role_write: role(row, COL_MIN_ROLE_WRITE)?,
        },
        attributes: object(row, COL_ATTRIBUTES)?,
    })
}

/// A required text column.
fn text(row: &Row, column: &str) -> Result<String> {
    match row.get(column) {
        Some(Value::Text(t)) => Ok(t.clone()),
        other => Err(bad_column(column, "text", other)),
    }
}

/// A text column where NULL means "none given", not a broken row.
fn optional_text(row: &Row, column: &str) -> Result<String> {
    match row.get(column) {
        Some(Value::Text(t)) => Ok(t.clone()),
        Some(Value::Null) | None => Ok(String::new()),
        other => Err(bad_column(column, "text", other)),
    }
}

/// A role column: an integer on the `1..=100` scale.
///
/// An out-of-range value is a corrupt row, not something to clamp — the nearest
/// valid role would quietly change who can reach the data, which is precisely
/// what this column decides.
fn role(row: &Row, column: &str) -> Result<u8> {
    match row.get(column) {
        Some(Value::Int(i)) => u8::try_from(*i)
            .ok()
            .filter(|r| (1..=100).contains(r))
            .ok_or_else(|| {
                Error::invalid(format!(
                    "{TABLE_META_TABLE}.{column} should be a role between 1 and 100, got {i}"
                ))
            }),
        other => Err(bad_column(column, "an integer role", other)),
    }
}

/// A JSON column that must hold an object.
fn object(row: &Row, column: &str) -> Result<Attrs> {
    match row.get(column) {
        Some(Value::Json(Json::Object(o))) => Ok(o.clone()),
        Some(Value::Json(_)) => Err(Error::invalid(format!(
            "{TABLE_META_TABLE}.{column} should be a json object"
        ))),
        other => Err(bad_column(column, "json", other)),
    }
}

fn bad_column(column: &str, expected: &str, got: Option<&Value>) -> Error {
    match got {
        Some(value) => Error::invalid(format!(
            "{TABLE_META_TABLE}.{column} should be {expected}, got {}",
            value.kind()
        )),
        None => Error::invalid(format!("row has no `{column}` column")),
    }
}

/// Run a statement that returns no rows of interest.
async fn run(catalog: &Catalog, statement: Statement) -> Result<()> {
    catalog
        .primary()
        .query(&statement)
        .await?
        .try_collect()
        .await?;
    Ok(())
}

/// Run a select and collect its rows.
async fn rows(catalog: &Catalog, select: Select) -> Result<Vec<Row>> {
    catalog
        .primary()
        .query(&Statement::from(select))
        .await?
        .try_collect()
        .await
}

/// Load the single overlay matching `filter`, if any.
async fn load_one(catalog: &Catalog, filter: Expr) -> Result<Option<TableMeta>> {
    let select = Select::from(Source::table(TABLE_META_TABLE))
        .filter(filter)
        .limit(1);
    match rows(catalog, select).await?.first() {
        Some(row) => Ok(Some(table_meta_from_row(row)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_the_section_9_required_columns() {
        let fields = table_meta_fields();
        let by_name = |n: &str| fields.iter().find(|f| f.base.name == n).unwrap();

        // §9: every system metadata table MUST have id (UUID), name,
        // description, attributes (JSON object).
        let id = by_name(COL_ID);
        assert!(id.primary_key && id.required);
        assert_eq!(id.base.type_, TypeRef::Basic(BasicType::Uuid));
        assert!(by_name(COL_NAME).required);
        assert!(!by_name(COL_DESCRIPTION).required);
        assert_eq!(
            by_name(COL_ATTRIBUTES).base.type_,
            TypeRef::Basic(BasicType::Json)
        );
    }

    #[test]
    fn name_is_the_unique_key_the_merge_joins_on() {
        let fields = table_meta_fields();
        let name = fields.iter().find(|f| f.base.name == COL_NAME).unwrap();
        assert!(name.required && name.unique);
        assert_eq!(name.base.type_, TypeRef::Basic(BasicType::Text));
    }

    #[test]
    fn both_access_roles_are_required_because_the_pair_is_the_state() {
        // Unlike a file store's nullable `min_role`, a NULL here would be a
        // third state with no meaning: a row that exists states both roles.
        let fields = table_meta_fields();
        for column in [COL_MIN_ROLE_READ, COL_MIN_ROLE_WRITE] {
            let f = fields.iter().find(|f| f.base.name == column).unwrap();
            assert!(f.required, "{column} should be NOT NULL");
            assert_eq!(f.base.type_, TypeRef::Basic(BasicType::Int));
        }
    }

    #[test]
    fn the_overlay_carries_nothing_the_database_already_knows() {
        // The precedence rule of §1.2 holds only while the two sets do not
        // intersect. A column here named for a fact introspection yields —
        // nullability, a type, the primary key — would break it, so the column
        // set is asserted in full rather than merely spot-checked.
        let names: Vec<String> = table_meta_fields()
            .iter()
            .map(|f| f.base.name.clone())
            .collect();
        assert_eq!(
            names,
            vec![
                COL_ID,
                COL_NAME,
                COL_LABEL,
                COL_DESCRIPTION,
                COL_MIN_ROLE_READ,
                COL_MIN_ROLE_WRITE,
                COL_ATTRIBUTES,
            ]
        );
    }

    #[test]
    fn the_table_is_a_hidden_system_table() {
        assert!(TABLE_META_TABLE.starts_with("_sc_"));
    }

    #[test]
    fn columns_and_values_stay_in_step() {
        // The insert pairs these two positionally, so a column added to one and
        // not the other would write a role into the wrong column.
        let meta = TableMeta::new("books");
        assert_eq!(meta_columns().len(), meta_values(&meta).len());
        assert_eq!(meta_columns().len(), table_meta_fields().len());
    }

    #[test]
    fn a_new_overlay_is_admin_only() {
        // Creating an overlay row must never be the thing that widens access.
        let meta = TableMeta::new("books");
        assert_eq!(meta.access, AccessRules::default());
        assert_eq!(meta.access.min_role_read, 1);
        assert_eq!(meta.access.min_role_write, 1);
    }

    #[test]
    fn roles_outside_the_scale_are_rejected_rather_than_clamped() {
        assert!(validate_access("books", &AccessRules::default()).is_ok());
        assert!(
            validate_access(
                "books",
                &AccessRules {
                    min_role_read: 100,
                    min_role_write: 1,
                },
            )
            .is_ok()
        );
        let err = validate_access(
            "books",
            &AccessRules {
                min_role_read: 0,
                min_role_write: 1,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains(COL_MIN_ROLE_READ), "{err}");
        assert!(
            validate_access(
                "books",
                &AccessRules {
                    min_role_read: 1,
                    min_role_write: 101,
                },
            )
            .is_err()
        );
    }
}
