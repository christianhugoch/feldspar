//! Postgres row-level security for ownership formulas (§7.3, TODO Phase 6).
//!
//! When a table has `rls_enabled`, the *same* ownership formula the runtime
//! path (§5) evaluates is enforced by the database instead: generated RLS
//! policies decide every row, and the application's own checks are switched off
//! (GOALS: "then you no longer have to check"). Two halves live here:
//!
//! - **Caller context** ([`CallerContext`], [`run_in_context`]): every row
//!   operation on an RLS table runs inside a transaction that first `SET LOCAL`s
//!   the caller's role and identity as GUCs. The policies read them with
//!   `current_setting(name, true)`; a transaction that forgets to set them sees
//!   `NULL`, which every policy treats as "no access", so a forgotten context
//!   **fails closed** — the property is a shape of the policy, not a hope.
//! - **Policy generation** ([`enable_rls`], [`disable_rls`]): the formula's
//!   `sc_query::Expr` (translated with [`UserEnv::Guc`]) rendered — literals
//!   inlined, because `CREATE POLICY` is DDL and takes no binds — into four
//!   per-operation policies, atop `ENABLE`/`FORCE ROW LEVEL SECURITY`.

use sc_db::Row;
use sc_error::{Context, Error, Result};
use sc_expr::{Formula, Operation, SchemaShape, USER_GUC, UserEnv};
use sc_query::{SqlDialect, Statement, render_policy_expr};

use crate::catalog::Catalog;
use crate::table::Table;

/// The GUC carrying the caller's role number on the 1–100 scale. Policies read
/// `current_setting('sc.role', true)::int`; the runtime `SET LOCAL`s it.
pub const ROLE_GUC: &str = "sc.role";

/// The caller as an RLS transaction sees it: a role always, and the user's
/// fields as a JSON object when logged in (matching [`UserEnv::Guc`]'s
/// `current_setting('sc.user', …)::jsonb`).
#[derive(Debug, Clone)]
pub struct CallerContext {
    /// The caller's role (public when anonymous).
    pub role: u8,
    /// The logged-in user's fields as a JSON object string, or `None` when
    /// anonymous — in which case `sc.user` is left unset and the policies'
    /// `current_setting('sc.user', true)` reads `NULL`.
    pub user_json: Option<String>,
}

impl CallerContext {
    /// A context for `role` with no user (anonymous).
    pub fn anonymous(role: u8) -> CallerContext {
        CallerContext {
            role,
            user_json: None,
        }
    }
}

/// Run `stmt` inside a transaction that first sets the caller-context GUCs,
/// returning the statement's rows. This is the one path an RLS table's row
/// operations take: the GUCs and the statement share a connection and a
/// transaction, and `SET LOCAL` reverts both on commit.
///
/// A policy violation surfaces from Postgres as SQLSTATE `42501`
/// (`insufficient_privilege`); it is mapped to a [`NotFound`](Error::not_found)
/// so a denied write is indistinguishable from one that matched no row — the
/// same probe-free rule the runtime path keeps.
pub async fn run_in_context(
    catalog: &Catalog,
    context: &CallerContext,
    stmt: &Statement,
) -> Result<Vec<Row>> {
    let mut tx = catalog.primary().begin().await?;
    tx.set_local(ROLE_GUC, &context.role.to_string()).await?;
    if let Some(user_json) = &context.user_json {
        tx.set_local(USER_GUC, user_json).await?;
    }
    let outcome = match tx.query(stmt).await {
        Ok(stream) => stream.try_collect().await,
        Err(e) => Err(e),
    };
    match outcome {
        Ok(rows) => {
            tx.commit().await?;
            Ok(rows)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(map_policy_violation(e))
        }
    }
}

/// A policy violation becomes a not-found, so a denied write is
/// indistinguishable from one that matched no row; any other database error
/// passes through unchanged.
///
/// A `USING` policy filters rows silently (an update/delete simply affects
/// none), so the only error Postgres raises is a **`WITH CHECK` violation** on
/// an insert or update — SQLSTATE `42501`, whose message names "row-level
/// security policy". The SQLSTATE code is not in the error's rendered text by
/// the time it reaches here, so the message is what is matched.
fn map_policy_violation(e: Error) -> Error {
    let chain = sc_error::format_chain(&e);
    if chain.contains("row-level security") || chain.contains("42501") {
        Error::not_found("no such row")
    } else {
        e
    }
}

/// Enable RLS on `table` and (re)create its four policies from the ownership
/// formula, in one transaction — so a table is never left half-configured.
///
/// Emits, in order: `ALTER TABLE … ENABLE ROW LEVEL SECURITY`, then **`FORCE`**
/// (the server connects as the table's owner, whom RLS otherwise exempts —
/// without `FORCE` the policies would be decoration), a `DROP POLICY IF EXISTS`
/// for each of the four names (so this is idempotent and doubles as "recreate"
/// when the formula changes), and one `CREATE POLICY` per operation:
/// SELECT/DELETE with `USING`, INSERT with `WITH CHECK`, UPDATE with both.
///
/// Every policy also carries the **role floor**, so the role half of the access
/// rule moves into the database too:
/// `current_setting('sc.role', true)::int <= <min_role_op> OR (<formula>)`.
pub async fn enable_rls(catalog: &Catalog, table: &Table) -> Result<()> {
    let sql = enable_rls_sql(catalog, table)?;
    run_ddl(catalog, &sql).await
}

/// Disable RLS on `table`: drop its four policies and the `ENABLE`/`FORCE`
/// flags, idempotently. A dropped table takes its policies with it, so this is
/// only for a table that still exists and is being un-secured.
pub async fn disable_rls(catalog: &Catalog, table_name: &str) -> Result<()> {
    let sql = disable_rls_sql(catalog, table_name);
    run_ddl(catalog, &sql).await
}

/// The DDL that [`enable_rls`] runs, as one script — pure so it is unit-tested
/// without a database.
pub(crate) fn enable_rls_sql(catalog: &Catalog, table: &Table) -> Result<String> {
    let formula = table.ownership.as_ref().ok_or_else(|| {
        Error::invalid(format!(
            "cannot enable row-level security on `{}`: it has no live ownership formula",
            table.name
        ))
    })?;
    let dialect = catalog.primary().dialect();
    let shape = catalog.schema_shape()?;
    let env = UserEnv::Guc {
        field_types: catalog.user_field_types()?,
    };
    let ident = dialect.quote_ident(&table.name);

    let mut out = String::new();
    out.push_str(&format!("ALTER TABLE {ident} ENABLE ROW LEVEL SECURITY;\n"));
    out.push_str(&format!("ALTER TABLE {ident} FORCE ROW LEVEL SECURITY;\n"));
    for (op, _) in POLICY_OPS {
        out.push_str(&format!(
            "DROP POLICY IF EXISTS {} ON {ident};\n",
            policy_name(*op)
        ));
    }
    for (op, command) in POLICY_OPS {
        out.push_str(&policy_clause(
            dialect, &shape, &env, table, formula, *op, command,
        )?);
        out.push('\n');
    }
    Ok(out)
}

/// The DDL that [`disable_rls`] runs.
pub(crate) fn disable_rls_sql(catalog: &Catalog, table_name: &str) -> String {
    let ident = catalog.primary().dialect().quote_ident(table_name);
    let mut out = String::new();
    for (op, _) in POLICY_OPS {
        out.push_str(&format!(
            "DROP POLICY IF EXISTS {} ON {ident};\n",
            policy_name(*op)
        ));
    }
    out.push_str(&format!(
        "ALTER TABLE {ident} NO FORCE ROW LEVEL SECURITY;\n"
    ));
    out.push_str(&format!(
        "ALTER TABLE {ident} DISABLE ROW LEVEL SECURITY;\n"
    ));
    out
}

/// The four SQL commands an RLS table needs a policy for, and the [`Operation`]
/// whose flags fold into each.
const POLICY_OPS: &[(Operation, PolicyCommand)] = &[
    (Operation::Read, PolicyCommand::Using("SELECT")),
    (Operation::Insert, PolicyCommand::WithCheck("INSERT")),
    (Operation::Update, PolicyCommand::Both),
    (Operation::Delete, PolicyCommand::Using("DELETE")),
];

/// How a command's policy applies the predicate: `USING` gates the rows it
/// reads/removes, `WITH CHECK` the rows it writes, and `UPDATE` needs both.
enum PolicyCommand {
    Using(&'static str),
    WithCheck(&'static str),
    Both,
}

/// A stable, table-scoped policy name per command — fixed so the
/// drop-then-create in [`enable_rls_sql`] finds the previous one.
fn policy_name(op: Operation) -> String {
    let suffix = match op {
        Operation::Read => "select",
        Operation::Insert => "insert",
        Operation::Update => "update",
        Operation::Delete => "delete",
    };
    format!("sc_owner_{suffix}")
}

/// One `CREATE POLICY` statement: the role floor OR the translated formula.
fn policy_clause(
    dialect: &dyn SqlDialect,
    shape: &SchemaShape,
    env: &UserEnv,
    table: &Table,
    formula: &Formula,
    op: Operation,
    command: &PolicyCommand,
) -> Result<String> {
    let pred = sc_expr::translate(formula, op, env, shape, &table.name).map_err(Error::from)?;
    let floor = match op {
        Operation::Read => table.access.min_role_read,
        _ => table.access.min_role_write,
    };
    // A lower role number is more privileged, so "at or above the floor" is
    // `<= floor`. `NULLIF(…, '')` folds both an unset GUC and the empty-string
    // default a set-once custom GUC leaves on a reused connection to NULL, and
    // `NULL <= n` is NULL → not granted: fail closed on the role half too.
    let role_clause = format!("NULLIF(current_setting('{ROLE_GUC}', true), '')::int <= {floor}");
    let formula_sql = render_policy_expr(dialect, &pred)?;
    let expr = format!("({role_clause} OR ({formula_sql}))");
    let ident = dialect.quote_ident(&table.name);
    let name = policy_name(op);
    Ok(match command {
        PolicyCommand::Using(cmd) => {
            format!("CREATE POLICY {name} ON {ident} FOR {cmd} USING ({expr});")
        }
        PolicyCommand::WithCheck(cmd) => {
            format!("CREATE POLICY {name} ON {ident} FOR {cmd} WITH CHECK ({expr});")
        }
        PolicyCommand::Both => {
            format!(
                "CREATE POLICY {name} ON {ident} FOR UPDATE USING ({expr}) WITH CHECK ({expr});"
            )
        }
    })
}

/// Run a multi-statement DDL script in one transaction.
async fn run_ddl(catalog: &Catalog, sql: &str) -> Result<()> {
    let mut tx = catalog.primary().begin().await?;
    match tx.batch(sql).await {
        Ok(()) => tx.commit().await,
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e).context("applying row-level-security policies")
        }
    }
}
