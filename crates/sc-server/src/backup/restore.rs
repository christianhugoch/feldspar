//! Restoring a backup: a zip and a selection in, an installation changed.
//!
//! Three rules run through everything below.
//!
//! **A restore adds; it does not destroy.** Nothing here drops a table, deletes a
//! row, replaces an account or overwrites a file-store definition that already
//! exists. An admin who restores a backup onto a server that already has things
//! in it gets what was missing, and is *told* what was left alone. The alternative
//! — a restore that makes the server match the file — would mean an admin one
//! click away from deleting the installation they are standing in, and the
//! destructive verbs (drop a table, delete a user) already exist elsewhere in the
//! admin UI where they are individually deliberate.
//!
//! **Every item is restored on its own.** A restore is dozens of independent
//! acts, and one of them failing — an application naming a table that is not in
//! the file, an agent whose LLM provider does not exist here — must not lose the
//! other twenty. Each failure becomes a line in [`RestoreReport::warnings`]
//! naming what was skipped and why; only a file that is not a backup at all is an
//! error.
//!
//! **The parsers are the admin API's.** Every record goes back in through the
//! same `*_from_body` the endpoint that creates one uses, which is what makes the
//! validation identical: an agent restored from a backup is refused for exactly
//! the reasons an agent typed into the form would be.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use sc_api::{rows, schema_edit};
use sc_catalog::{
    Catalog, ConstraintKind, DataFieldKind, Table, TableConstraint, load_file_store_by_name,
};
use sc_db::{ColumnGenerator, SchemaChange};
use sc_error::{Error, Result};
use sc_query::{Expr, Insert, Statement};
use serde_json::{Map, Value as Json};

use super::{Available, MANIFEST_FILE, SSL_SECTION, Selection};
use crate::apps::AppMounts;
use crate::handlers::{
    agent_from_body, agents_of, application_from_body, backup_file_meta_from_json,
    field_spec_from_body, file_store_from_body, table_settings_from_body, trigger_from_body,
    trigger_table, triggers_of,
};

/// What a restore did, and what it declined to do.
#[derive(Debug, Clone, Default)]
pub struct RestoreReport {
    /// One line per thing that was restored, in the order it happened.
    pub restored: Vec<String>,
    /// One line per thing that was skipped, each saying why.
    pub warnings: Vec<String>,
}

impl RestoreReport {
    fn did(&mut self, line: impl Into<String>) {
        self.restored.push(line.into());
    }

    fn skipped(&mut self, line: impl Into<String>) {
        self.warnings.push(line.into());
    }

    /// Record the outcome of one item: a line either way, never silence.
    fn outcome(&mut self, what: &str, result: Result<String>) {
        match result {
            Ok(detail) if detail.is_empty() => self.did(what.to_owned()),
            Ok(detail) => self.did(format!("{what}: {detail}")),
            Err(e) => self.skipped(format!("{what}: {}", e.causes())),
        }
    }
}

/// The entries of a backup zip, read whole.
///
/// The archive is expanded into memory rather than read entry by entry as the
/// restore proceeds, because the restore is asynchronous and a zip reader's
/// borrow of the archive is not: holding one across an `await` is not expressible.
/// The cost is that a restore holds the uncompressed backup in memory, which is
/// the same trade the writer makes and for a rarer operation.
type Entries = BTreeMap<String, Vec<u8>>;

/// Read a backup's manifest without restoring anything: what the dialog is built
/// from after the file is uploaded.
pub fn inspect(archive: &[u8]) -> Result<(Available, Json)> {
    let entries = read_zip(archive)?;
    let manifest = manifest_of(&entries)?;
    let contents = manifest
        .get("contents")
        .ok_or_else(|| Error::invalid("this backup's manifest does not say what is in it"))?;
    Ok((Available::from_json(contents)?, Json::Object(manifest)))
}

/// Restore the parts of `archive` that `selection` asks for.
pub async fn restore_backup(
    catalog: &Catalog,
    apps: &AppMounts,
    archive: &[u8],
    selection: &Selection,
) -> Result<RestoreReport> {
    let entries = read_zip(archive)?;
    let manifest = manifest_of(&entries)?;
    let contents = Available::from_json(
        manifest
            .get("contents")
            .ok_or_else(|| Error::invalid("this backup's manifest does not say what is in it"))?,
    )?;
    // What the file holds, narrowed by what was asked for: a selection naming
    // something the backup does not carry is quietly dropped rather than
    // reported, because it is the dialog's own list that was edited.
    let selection = selection.intersect(&contents);
    let mut report = RestoreReport::default();

    // --- roles and users, before anything points at them --------------------
    if selection.users {
        restore_users(catalog, &entries, &mut report).await;
    }

    // --- tables: every table, then every column, then the rows --------------
    //
    // In that order because a `Key` field can only resolve against a table that
    // exists, and *all* of them exist once the first pass is done — which is what
    // lets a backup with two tables referencing each other be restored at all.
    let mut restored_tables = Vec::new();
    for name in &selection.tables {
        match restore_table(catalog, &entries, name).await {
            Ok(line) => {
                report.did(line);
                restored_tables.push(name.clone());
            }
            Err(e) => report.skipped(format!("table `{name}`: {}", e.causes())),
        }
    }
    for name in &restored_tables {
        restore_fields(catalog, &entries, name, &mut report).await;
    }

    // Rows in dependency order, so a row holding a foreign key is inserted after
    // the row it points at.
    for name in order_by_references(catalog, &restored_tables) {
        if !selection.includes_data(&name) {
            continue;
        }
        match table_rows(&entries, &format!("tables/{name}/rows.json")) {
            Ok(rows) => {
                let table = match catalog.require(&name) {
                    Ok(table) => table,
                    Err(e) => {
                        report.skipped(format!("rows of `{name}`: {}", e.causes()));
                        continue;
                    }
                };
                let (inserted, problems) = insert_rows(catalog, &table, &rows).await;
                report.did(format!("{inserted} rows into `{name}`"));
                for problem in problems {
                    report.skipped(format!("a row of `{name}`: {problem}"));
                }
                rewind_identities(catalog, &table, &mut report).await;
            }
            Err(e) => report.skipped(format!("rows of `{name}`: {}", e.causes())),
        }
    }

    // Constraints last, **after** the rows, exactly as `pg_dump` orders them: a
    // unique constraint created over the restored data checks it in one pass
    // rather than once per insert, and a row constraint created first would have
    // judged every row as it arrived — against a table whose other rows were not
    // in yet (§5.1).
    for name in &restored_tables {
        restore_constraints(catalog, &entries, name, &mut report).await;
    }

    // --- file stores: the definition, then the bytes, then the rules ---------
    for name in &selection.file_stores {
        restore_file_store(catalog, &entries, name, &mut report).await;
    }

    // --- applications, agents, triggers -------------------------------------
    //
    // Applications after the file stores, deliberately: building one is a bundler
    // run over its source tree, and that tree is what the stores just restored.
    for subdomain in &selection.applications {
        restore_application(catalog, apps, &entries, subdomain, &mut report).await;
    }
    if selection.agents {
        restore_agents(catalog, apps, &entries, &mut report).await;
    }
    if selection.triggers {
        restore_triggers(catalog, apps, &entries, &selection, &mut report).await;
    }

    // --- the SSL settings ---------------------------------------------------
    if selection.ssl {
        let result = restore_ssl(catalog, &entries).await;
        report.outcome("the SSL settings", result);
    }

    Ok(report)
}

// --- the parts -----------------------------------------------------------------

/// Roles, then accounts.
///
/// **An account that is already here is left exactly as it is**, matched by id or
/// by email address. That rule is what makes the restore safe to run on a server
/// somebody is signed in to: the alternative would let a backup overwrite the
/// password of the admin running it, and lock them out of the screen they are
/// standing on.
async fn restore_users(catalog: &Catalog, entries: &Entries, report: &mut RestoreReport) {
    let document = match json_entry(entries, "users.json") {
        Ok(document) => document,
        Err(e) => {
            report.skipped(format!("users: {}", e.causes()));
            return;
        }
    };

    for value in array_field(&document, "roles") {
        let result = restore_role(catalog, &value).await;
        let name = value
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or("a role")
            .to_owned();
        report.outcome(&format!("role `{name}`"), result);
    }

    let users = array_field(&document, "users");
    let Ok(table) = catalog.require(sc_auth::USERS_TABLE) else {
        report.skipped("users: this server has no users table");
        return;
    };
    let existing = existing_users(catalog).await.unwrap_or_default();
    let mut restored = 0;
    for user in &users {
        let email = user
            .get(sc_auth::COL_EMAIL)
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_owned();
        let id = user
            .get(sc_auth::COL_ID)
            .and_then(Json::as_str)
            .unwrap_or("");
        if existing.contains(&email) || existing.contains(id) {
            report.skipped(format!(
                "user `{email}` is already on this server; kept as it is"
            ));
            continue;
        }
        let (inserted, problems) = insert_rows(catalog, &table, std::slice::from_ref(user)).await;
        restored += inserted;
        for problem in problems {
            report.skipped(format!("user `{email}`: {problem}"));
        }
    }
    if restored > 0 {
        report.did(format!("{restored} users"));
    }
}

/// Every identifier an existing account can be recognised by: its id and its
/// email, in one set, because either colliding means "this account is here".
async fn existing_users(catalog: &Catalog) -> Result<BTreeSet<String>> {
    let table = catalog.require(sc_auth::USERS_TABLE)?;
    let mut out = BTreeSet::new();
    for row in rows_of(catalog, &table).await? {
        if let Some(id) = row.get(sc_auth::COL_ID).and_then(Json::as_str) {
            out.insert(id.to_owned());
        }
        if let Some(email) = row.get(sc_auth::COL_EMAIL).and_then(Json::as_str) {
            out.insert(email.to_owned());
        }
    }
    Ok(out)
}

async fn restore_role(catalog: &Catalog, value: &Json) -> Result<String> {
    let obj = value
        .as_object()
        .ok_or_else(|| Error::invalid("a role must be an object"))?;
    let number = obj
        .get("role")
        .and_then(Json::as_i64)
        .and_then(|n| u8::try_from(n).ok())
        .filter(|r| sc_auth::role_in_range(*r))
        .ok_or_else(|| Error::invalid("a role must have a number between 1 and 100"))?;
    let name = obj
        .get("name")
        .and_then(Json::as_str)
        .filter(|n| !n.trim().is_empty())
        .ok_or_else(|| Error::invalid("a role must have a name"))?;
    let mut role = sc_auth::Role::new(number, name.trim());
    role.description = obj
        .get("description")
        .and_then(Json::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    sc_auth::save_role(catalog, &role).await?;
    Ok(String::new())
}

/// Create the table if it is not here, or apply the backup's settings to it if it
/// is. Columns are a separate pass — see [`restore_backup`].
async fn restore_table(catalog: &Catalog, entries: &Entries, name: &str) -> Result<String> {
    let document = json_entry(entries, &format!("tables/{name}/table.json"))?;
    let table = document
        .get("table")
        .and_then(Json::as_object)
        .ok_or_else(|| Error::invalid("a table entry must carry a `table` object"))?;
    let settings = table_settings_from_body(table)?;
    let exists = catalog.get(name)?.is_some();
    let operation = if exists {
        schema_edit::Operation::AlterTable {
            table: name.to_owned(),
            settings,
        }
    } else {
        schema_edit::Operation::CreateTable {
            name: name.to_owned(),
            // A backup carries Saltcorn's own schema; a table that lived on a
            // connection is that connection's to restore, not this archive's.
            database: String::new(),
            settings,
            // Deliberately none: the columns go in one at a time in the next
            // pass, so one column the schema editor refuses does not take the
            // whole table with it.
            fields: Vec::new(),
        }
    };
    schema_edit::apply(catalog, &[operation], &schema_edit::ApplyOptions::default()).await?;
    Ok(if exists {
        format!("table `{name}` (settings; it was already here)")
    } else {
        format!("table `{name}`")
    })
}

/// Add the columns the backup describes and this table has not got.
///
/// A column that is already here is left alone rather than altered: its type is
/// the database's answer, and a restore that rewrote a live column's type would
/// be a migration nobody asked for.
async fn restore_fields(
    catalog: &Catalog,
    entries: &Entries,
    name: &str,
    report: &mut RestoreReport,
) {
    let Ok(document) = json_entry(entries, &format!("tables/{name}/table.json")) else {
        return;
    };
    for value in array_field(&document, "fields") {
        let Some(field) = value.as_object() else {
            continue;
        };
        let field_name = field
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_owned();
        if field_name.is_empty() {
            continue;
        }
        let live = match catalog.require(name) {
            Ok(table) => table,
            Err(e) => {
                report.skipped(format!("columns of `{name}`: {}", e.causes()));
                return;
            }
        };
        if live.field(&field_name).is_some() {
            // A column an existing table already has: not news.
            continue;
        }
        // The primary key travels as what it is — a field that says it is one —
        // so a restored table has the key the backup had, composite or not.
        // Nothing invents a key here or anywhere else (GOALS).
        let result = add_field(catalog, name, field).await;
        report.outcome(&format!("column `{name}.{field_name}`"), result);
    }
}

/// Add the constraints the backup describes and this table has not got.
///
/// Without this a restore would hand back a table that **accepts what the
/// original refused** — the columns and the rows, with none of the rules that
/// were the point of half of them — and would say nothing about it. Constraints
/// are not stored in an `_sc_*` table (§5.1), so they travel in the backup as
/// what the database reported, which is also how a constraint somebody added by
/// hand comes back.
///
/// Two things are deliberately not attempted. A constraint the table already has
/// is left alone rather than replaced, like a column that is already there. And
/// an index over an **expression** that Saltcorn did not create is reported as
/// skipped rather than approximated: what could be recreated from it is an index
/// over no columns, which is not the index the backup described.
async fn restore_constraints(
    catalog: &Catalog,
    entries: &Entries,
    name: &str,
    report: &mut RestoreReport,
) {
    let Ok(document) = json_entry(entries, &format!("tables/{name}/table.json")) else {
        return;
    };
    for value in array_field(&document, "constraints") {
        let Some(obj) = value.as_object() else {
            continue;
        };
        let constraint_name = obj
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_owned();
        if constraint_name.is_empty() {
            continue;
        }
        let live = match catalog.require(name) {
            Ok(table) => table,
            Err(e) => {
                report.skipped(format!("constraints of `{name}`: {}", e.causes()));
                return;
            }
        };
        if live.constraints.iter().any(|c| c.name == constraint_name) {
            continue;
        }
        let what = format!("constraint `{constraint_name}` on `{name}`");
        let constraint = match constraint_from_backup(obj) {
            Ok(constraint) => constraint,
            Err(e) => {
                report.skipped(format!("{what}: {}", e.causes()));
                continue;
            }
        };
        let result = schema_edit::apply(
            catalog,
            &[schema_edit::Operation::AddConstraint {
                table: name.to_owned(),
                given_name: String::new(),
                constraint,
            }],
            &schema_edit::ApplyOptions::default(),
        )
        .await
        .map(|_| what.clone());
        report.outcome(&what, result);
    }
}

/// One constraint out of a backup's `table.json`, keeping the name it had — a
/// restored constraint reports itself by the same name a violation would have
/// named before the backup was taken.
fn constraint_from_backup(obj: &Map<String, Json>) -> Result<TableConstraint> {
    let text = |key: &str| {
        obj.get(key)
            .and_then(Json::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let fields: Vec<String> = obj
        .get("fields")
        .and_then(Json::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Json::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let type_name = obj
        .get("type")
        .and_then(Json::as_str)
        .ok_or_else(|| Error::invalid("a constraint entry needs a `type`"))?;
    let kind = match type_name {
        "unique" => ConstraintKind::Unique { fields },
        "index" if fields.is_empty() => {
            return Err(Error::invalid(
                "an index over an expression is not one this restore can recreate; \
                 add it by hand",
            ));
        }
        "index" => ConstraintKind::Index {
            fields,
            expression: None,
            method: text("method").unwrap_or_else(|| "btree".to_owned()),
        },
        "full_text_search" => ConstraintKind::FullTextSearch {
            language: text("language").unwrap_or_else(|| "english".to_owned()),
        },
        "formula" => ConstraintKind::Formula {
            formula: text("formula")
                .ok_or_else(|| Error::invalid("a row constraint entry needs a `formula`"))?,
        },
        other => {
            return Err(Error::invalid(format!(
                "`{other}` is not a constraint type"
            )));
        }
    };
    let name = obj
        .get("name")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut constraint = TableConstraint::new(name, kind);
    constraint.error_message = text("error_message");
    Ok(constraint)
}

async fn add_field(catalog: &Catalog, table: &str, field: &Map<String, Json>) -> Result<String> {
    let mut spec = field_spec_from_body(field)?;
    // A reference's storage type is its target's, and the schema editor is what
    // knows that (it fills the type in from the table pointed at). The backup
    // carries the *introspected* type, which is the same answer for as long as
    // both ends agree — and is the wrong thing to insist on when they might not.
    if matches!(spec.kind, DataFieldKind::Key { .. }) {
        spec.type_name = String::new();
    }
    schema_edit::apply(
        catalog,
        &[schema_edit::Operation::AddField {
            table: table.to_owned(),
            field: spec,
        }],
        &schema_edit::ApplyOptions::default(),
    )
    .await?;
    Ok(String::new())
}

/// The restored tables ordered so that a table comes after everything its `Key`
/// fields point at.
///
/// A cycle (two tables referencing each other) cannot be ordered and is left in
/// place at the end: its rows are attempted, and whichever ones the database
/// refuses are reported row by row. A restore that silently dropped them would be
/// the worse answer.
fn order_by_references(catalog: &Catalog, tables: &[String]) -> Vec<String> {
    let mut remaining: Vec<String> = tables.to_vec();
    let mut done: Vec<String> = Vec::new();
    // At most one pass per table: each pass either places something or the rest
    // is a cycle.
    while !remaining.is_empty() {
        let mut placed = false;
        let mut next = Vec::new();
        for name in remaining {
            let ready = match catalog.get(&name) {
                Ok(Some(table)) => table.fields.iter().all(|field| match &field.kind {
                    DataFieldKind::Key { target_table, .. } => {
                        target_table.0 == name
                            || !tables.contains(&target_table.0)
                            || done.contains(&target_table.0)
                    }
                    _ => true,
                }),
                _ => true,
            };
            if ready {
                done.push(name);
                placed = true;
            } else {
                next.push(name);
            }
        }
        if !placed {
            done.extend(next);
            break;
        }
        remaining = next;
    }
    done
}

/// Insert rows as they were backed up — primary keys included, so a foreign key
/// pointing at one still points at it.
///
/// Not through [`rows::create_row`], and that is the substance of this function:
/// Wind every identity column of `table` past the keys the restore just wrote.
///
/// A backup carries its rows' keys and they are inserted as given — the identity
/// is `BY DEFAULT`, so an explicit value is accepted, which is the whole reason
/// it is not `ALWAYS`. But the sequence behind it never saw those inserts and is
/// still sitting at 1, so the first row written *after* a restore would be handed
/// a key some restored row already has. Re-applying the generator is what winds
/// it, and it is the same change the schema editor emits when a key is switched
/// on over rows that are already there — the two situations are the same
/// situation.
///
/// Reported rather than raised: a restore that got the rows in is worth having
/// even if one sequence could not be wound, and the report is where a partial
/// restore says what to fix by hand.
async fn rewind_identities(catalog: &Catalog, table: &Table, report: &mut RestoreReport) {
    for field in &table.fields {
        if field.generated != Some(ColumnGenerator::Identity) {
            continue;
        }
        let change = SchemaChange::SetColumnGenerator {
            table: table.name.clone(),
            column: field.base.name.clone(),
            generator: Some(ColumnGenerator::Identity),
        };
        if let Err(e) = catalog.primary().apply_schema(&change).await {
            report.skipped(format!(
                "the numbering of `{}.{}` could not be wound past the restored rows: {}",
                table.name,
                field.base.name,
                e.causes()
            ));
        }
    }
}

/// that path raises the table's insert **triggers** (§10.2), which during a
/// restore would fire an installation's automation for every historical row it
/// ever had. A restore puts data back; it does not replay it.
///
/// Returns how many rows went in, and one message per row that did not.
async fn insert_rows(catalog: &Catalog, table: &Table, values: &[Json]) -> (usize, Vec<String>) {
    let mut inserted = 0;
    let mut problems = Vec::new();
    for value in values {
        match insert_row(catalog, table, value).await {
            Ok(()) => inserted += 1,
            Err(e) => problems.push(e.causes()),
        }
    }
    (inserted, problems)
}

async fn insert_row(catalog: &Catalog, table: &Table, value: &Json) -> Result<()> {
    let obj = value
        .as_object()
        .ok_or_else(|| Error::invalid("a row must be an object"))?;
    let mut columns = Vec::with_capacity(obj.len());
    let mut literals = Vec::with_capacity(obj.len());
    for (column, json) in obj {
        match table.field(column) {
            // A calculated field has no column to write (§3.4); it is computed
            // on read, and the backup carries the value it had.
            Some(field) if field.is_calc() => continue,
            // A column this server has not got: the backup is from an
            // installation whose table had more in it. Skipped rather than
            // failing the row, because the row is still worth having.
            None => continue,
            Some(_) => {}
        }
        columns.push(column.clone());
        literals.push(Expr::lit(rows::column_value(table, column, json)?));
    }
    if columns.is_empty() {
        return Err(Error::invalid(
            "no column of this row matches the table's columns",
        ));
    }
    let insert = Insert::row(table.name.clone(), columns, literals);
    catalog
        .primary()
        .query(&Statement::from(insert))
        .await?
        .try_collect()
        .await?;
    Ok(())
}

/// A store's definition (only when there is not one already), then its files.
///
/// **An existing store keeps its definition.** A backup's store points at a path
/// on the machine it was taken from, and a restore onto a different machine must
/// not repoint a working store at a directory that is not there. The files are
/// restored into whatever the store here already is, which is what an admin who
/// set the store up before restoring meant.
async fn restore_file_store(
    catalog: &Catalog,
    entries: &Entries,
    name: &str,
    report: &mut RestoreReport,
) {
    let document = match json_entry(entries, &format!("file-stores/{name}/store.json")) {
        Ok(document) => document,
        Err(e) => {
            report.skipped(format!("file store `{name}`: {}", e.causes()));
            return;
        }
    };

    match load_file_store_by_name(catalog, name).await {
        Ok(Some(_)) => report.skipped(format!(
            "file store `{name}` is already defined here; its definition was kept and the \
             backup's files were written into it"
        )),
        Ok(None) => {
            let result = define_store(catalog, document.get("definition")).await;
            report.outcome(&format!("file store `{name}`"), result);
        }
        Err(e) => {
            report.skipped(format!("file store `{name}`: {}", e.causes()));
            return;
        }
    }

    let Ok(store) = catalog.require_file_store(name) else {
        report.skipped(format!(
            "files of `{name}`: the store is not connected on this server"
        ));
        return;
    };
    let prefix = format!("file-stores/{name}/files/");
    let mut written = 0;
    let mut metadata: BTreeMap<String, Json> = BTreeMap::new();
    for value in array_field(&document, "files") {
        if let Some(path) = value.get("path").and_then(Json::as_str) {
            metadata.insert(path.to_owned(), value.clone());
        }
    }
    for (entry, bytes) in entries {
        let Some(path) = entry.strip_prefix(&prefix) else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        // The store itself refuses a path that escapes its root, and this refuses
        // it before the bytes are read: a zip is a file somebody can hand us, and
        // `..` in an entry name is the oldest trick there is.
        if path.split('/').any(|part| part == ".." || part == ".") {
            report.skipped(format!(
                "file `{path}` of `{name}`: the path is not relative"
            ));
            continue;
        }
        match store.write(path, bytes.clone().into()).await {
            Ok(()) => written += 1,
            Err(e) => {
                report.skipped(format!("file `{path}` of `{name}`: {}", e.causes()));
                continue;
            }
        }
        // The rules the file carried, after the bytes it carried them for.
        if let Some(value) = metadata.get(path)
            && let Ok((_, meta)) = backup_file_meta_from_json(value)
            && (meta.min_role.is_some() || !meta.attributes.is_empty())
            && let Err(e) = store.set_meta(path, &meta).await
        {
            report.skipped(format!("metadata of `{path}` in `{name}`: {}", e.causes()));
        }
    }
    if written > 0 {
        report.did(format!("{written} files into `{name}`"));
    }
}

async fn define_store(catalog: &Catalog, definition: Option<&Json>) -> Result<String> {
    let definition =
        definition.ok_or_else(|| Error::invalid("the entry carries no store definition"))?;
    let def = file_store_from_body(sc_files::FileStoreDefId::new(), definition)?;
    sc_catalog::check_file_store_saveable(catalog, &def).await?;
    sc_catalog::save_file_store(catalog, &def).await?;
    // Connected straight away, as `createFileStore` does, so the files below have
    // somewhere to go — and so an unreachable directory is reported now.
    match sc_catalog::connect_file_store_def(catalog, &def) {
        Ok(()) => Ok(String::new()),
        Err(e) => Ok(format!("defined, but not connected: {}", e.causes())),
    }
}

/// One application, under the id it had, so what references it still does.
/// One application, under the id it had, **built and mounted**.
///
/// The build is the point of doing it here rather than leaving it to the
/// Applications screen: a restored installation that does not serve its
/// applications is not a restored installation, and the admin has no way to know
/// which of them needed a button pressed. So each one goes through the same
/// `build_and_mount` the Build button does — the bundler over the source tree the
/// file stores restored a moment ago (which is why applications come *after* them),
/// then a live remount, so the app answers on its subdomain as soon as the restore
/// finishes. A first build installs the project's dependencies too, so a source
/// tree that arrived without its `node_modules` is not a special case.
///
/// A build failure is a **warning against a saved application**, not a lost one:
/// the definition is already stored, the reason is the bundler's own message, and
/// pressing Build after fixing it is exactly the repair.
async fn restore_application(
    catalog: &Catalog,
    apps: &AppMounts,
    entries: &Entries,
    subdomain: &str,
    report: &mut RestoreReport,
) {
    let saved = async {
        let document = json_entry(entries, &format!("applications/{subdomain}.json"))?;
        let id = document
            .get("id")
            .and_then(Json::as_str)
            .and_then(|raw| uuid::Uuid::parse_str(raw).ok())
            .map(sc_app::AppId)
            .unwrap_or_else(sc_app::AppId::new);
        let app = application_from_body(id, &document)?;
        sc_app::save_application(catalog, &app).await
    }
    .await;
    let app = match saved {
        Ok(app) => app,
        Err(e) => {
            report.skipped(format!("application `{subdomain}`: {}", e.causes()));
            return;
        }
    };
    report.did(format!("application `{subdomain}`"));

    match crate::apps::build_and_mount(apps, app).await {
        Ok(built) => report.did(format!(
            "application `{subdomain}` built and serving{}",
            if built.installed {
                " (its dependencies were installed)"
            } else {
                ""
            }
        )),
        Err(e) => report.skipped(format!(
            "application `{subdomain}` is restored but did not build, so it is not serving \
             yet — build it from the Applications screen once its source is in place: {}",
            e.causes()
        )),
    }
}

async fn restore_agents(
    catalog: &Catalog,
    apps: &AppMounts,
    entries: &Entries,
    report: &mut RestoreReport,
) {
    let document = match json_entry(entries, "agents.json") {
        Ok(document) => document,
        Err(e) => {
            report.skipped(format!("agents: {}", e.causes()));
            return;
        }
    };
    let services = match agents_of(apps) {
        Ok(services) => services,
        Err(e) => {
            report.skipped(format!("agents: {}", e.causes()));
            return;
        }
    };
    let registry = services.registry();
    for value in array_list(&document) {
        let name = value
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or("an agent")
            .to_owned();
        let result = async {
            let id = value
                .get("id")
                .and_then(Json::as_str)
                .and_then(|raw| uuid::Uuid::parse_str(raw).ok())
                .map(sc_agent::AgentId)
                .unwrap_or_else(sc_agent::AgentId::new);
            let agent = agent_from_body(id, &value)?;
            sc_agent::save_agent(catalog, registry.as_ref(), &agent).await?;
            Ok(String::new())
        }
        .await;
        report.outcome(&format!("agent `{name}`"), result);
    }
}

async fn restore_triggers(
    catalog: &Catalog,
    apps: &AppMounts,
    entries: &Entries,
    selection: &Selection,
    report: &mut RestoreReport,
) {
    let document = match json_entry(entries, "triggers.json") {
        Ok(document) => document,
        Err(e) => {
            report.skipped(format!("triggers: {}", e.causes()));
            return;
        }
    };
    let dispatcher = match triggers_of(apps) {
        Ok(dispatcher) => dispatcher,
        Err(e) => {
            report.skipped(format!("triggers: {}", e.causes()));
            return;
        }
    };
    let mut any = false;
    for value in array_list(&document) {
        let name = value
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or("a trigger")
            .to_owned();
        let result = async {
            let id = value
                .get("id")
                .and_then(Json::as_str)
                .and_then(|raw| uuid::Uuid::parse_str(raw).ok())
                .map(sc_action::TriggerId)
                .unwrap_or_else(sc_action::TriggerId::new);
            let trigger = trigger_from_body(id, &value)?;
            // The same rule the writer applied, applied again on the way in: a
            // trigger on a table this restore is not bringing has nothing to fire
            // on. The file may have been taken with the table included and
            // restored without it.
            if !selection.includes_trigger(trigger_table(&trigger)) {
                return Err(Error::invalid(
                    "it fires on a table this restore is not bringing",
                ));
            }
            sc_action::save_trigger(catalog, &dispatcher.registry(), &trigger).await?;
            Ok(String::new())
        }
        .await;
        any |= result.is_ok();
        report.outcome(&format!("trigger `{name}`"), result);
    }
    // One reload for the batch: the live set is what will fire, and it must match
    // the rows now rather than at the next restart.
    if any && let Err(e) = dispatcher.reload(catalog).await {
        report.skipped(format!(
            "the restored triggers are saved but not live yet: {}",
            e.causes()
        ));
    }
}

/// The SSL settings, checked against their declarations on the way in like any
/// other save. Not applied to the running listener: a certificate takes effect
/// when the server restarts, which is what the settings screen says too.
async fn restore_ssl(catalog: &Catalog, entries: &Entries) -> Result<String> {
    let document = json_entry(entries, "settings/ssl.json")?;
    let values = document
        .as_object()
        .ok_or_else(|| Error::invalid("the SSL settings must be an object"))?;
    // Only the SSL section's own keys. The entry is written by this system, but a
    // zip is a file somebody can edit: without the check, `settings/ssl.json` would
    // be a way to write *any* declared configuration value — including the ones no
    // settings form shows — under a heading that says certificates.
    let ssl_keys: Vec<&str> = sc_config::config_sections()
        .iter()
        .filter(|section| section.name == SSL_SECTION)
        .flat_map(|section| section.fields.iter().map(|def| def.key()))
        .collect();
    let mut attrs = sc_types::Attrs::new();
    for (key, value) in values {
        if !ssl_keys.contains(&key.as_str()) {
            continue;
        }
        attrs.insert(key.clone(), value.clone());
    }
    if attrs.is_empty() {
        return Ok("nothing to restore".to_owned());
    }
    sc_config::set_config_many(catalog, &attrs).await?;
    Ok("they take effect when the server restarts".to_owned())
}

// --- reading the file ----------------------------------------------------------

/// Expand a zip into path → bytes, refusing anything that is not a zip.
fn read_zip(archive: &[u8]) -> Result<Entries> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive)).map_err(|e| {
        Error::invalid(format!(
            "this file is not a zip archive, so it is not a Saltcorn backup: {e}"
        ))
    })?;
    let mut out = Entries::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|e| {
            Error::invalid(format!("the backup's entry {index} cannot be read: {e}"))
        })?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_owned();
        let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| Error::invalid(format!("`{name}` cannot be read from the backup: {e}")))?;
        out.insert(name, bytes);
    }
    Ok(out)
}

/// The manifest, with the format and version it declares checked.
///
/// Refused by name rather than by the confusing absence of everything else: a zip
/// of holiday photographs and a backup from a future version are different
/// mistakes, and both deserve to be told apart from "nothing was restored".
fn manifest_of(entries: &Entries) -> Result<Map<String, Json>> {
    let manifest = json_entry(entries, MANIFEST_FILE).map_err(|_| {
        Error::invalid(format!(
            "this zip has no `{MANIFEST_FILE}`, so it is not a Saltcorn backup"
        ))
    })?;
    let obj = manifest
        .as_object()
        .ok_or_else(|| Error::invalid(format!("`{MANIFEST_FILE}` is not an object")))?;
    match obj.get("format").and_then(Json::as_str) {
        Some(format) if format == super::FORMAT => {}
        Some(other) => {
            return Err(Error::invalid(format!(
                "this is a `{other}` archive, not a Saltcorn backup"
            )));
        }
        None => return Err(Error::invalid("this backup does not say what format it is")),
    }
    match obj.get("version").and_then(Json::as_i64) {
        Some(version) if version == super::FORMAT_VERSION => {}
        Some(other) => {
            return Err(Error::invalid(format!(
                "this backup is in format version {other}; this server reads version {}",
                super::FORMAT_VERSION
            )));
        }
        None => {
            return Err(Error::invalid(
                "this backup does not say what version it is",
            ));
        }
    }
    Ok(obj.clone())
}

/// One entry, parsed as JSON.
fn json_entry(entries: &Entries, path: &str) -> Result<Json> {
    let bytes = entries
        .get(path)
        .ok_or_else(|| Error::invalid(format!("the backup has no `{path}`")))?;
    serde_json::from_slice(bytes)
        .map_err(|e| Error::invalid(format!("`{path}` in the backup is not valid JSON: {e}")))
}

/// A table's rows entry — absent means "no rows were backed up", which is not an
/// error: a table can be backed up with its metadata alone.
fn table_rows(entries: &Entries, path: &str) -> Result<Vec<Json>> {
    if !entries.contains_key(path) {
        return Ok(Vec::new());
    }
    match json_entry(entries, path)? {
        Json::Array(values) => Ok(values),
        _ => Err(Error::invalid(format!("`{path}` must be an array of rows"))),
    }
}

/// A named array of objects in a document, or nothing.
fn array_field(document: &Json, key: &str) -> Vec<Json> {
    document
        .get(key)
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default()
}

/// A document that is itself an array.
fn array_list(document: &Json) -> Vec<Json> {
    document.as_array().cloned().unwrap_or_default()
}

/// Every row of a table as JSON — the restore's own read, for the accounts that
/// are already here.
async fn rows_of(catalog: &Catalog, table: &Table) -> Result<Vec<Json>> {
    let json = rows::list_rows(catalog, table).await?;
    Ok(match json {
        Json::Array(values) => values,
        _ => Vec::new(),
    })
}
