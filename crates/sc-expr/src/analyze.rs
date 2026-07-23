//! Free-variable analysis and validation against a [`SchemaShape`].
//!
//! Collection is pure syntax and happens once at parse time: every identifier
//! not bound by an arrow parameter is free, and member accesses on `user` are
//! recorded so `user.x` can be checked against the user table. Classification —
//! is this identifier a field, a Ⱶ-join path, `user`, an operation flag, a
//! whitelisted global, or a mistake — needs a shape, so it happens in
//! [`Formula::validate`], which the admin API calls on save and the catalog
//! merge calls on load.

use std::collections::BTreeSet;

use sc_error::{Error, Result};

use crate::ast::{Ast, MemberProp};
use crate::formula::Formula;
use crate::shape::SchemaShape;

/// The Ⱶ join operator (U+2C75). One character, category Lu — which is the
/// whole trick: it is a valid JavaScript identifier character, so a join path
/// is a single identifier to every parser and engine involved. Splitting on it
/// is this module's job alone.
pub const JOIN: char = 'Ⱶ';

/// Globals a formula may reference without them being fields. These exist in
/// the reified evaluator (they are JavaScript's own), so validation must not
/// reject them; the symbolic translator simply reports them untranslatable. A
/// field with one of these names shadows the global — consistent with Phase 3,
/// where row fields are bound over the global scope.
pub(crate) const GLOBALS: &[&str] = &[
    "undefined",
    "NaN",
    "Infinity",
    "Math",
    "Number",
    "String",
    "Boolean",
    "Array",
    "JSON",
    "Date",
];

/// The operation-flag variables (§ GOALS Authorization): which access operation
/// a formula is being evaluated for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OpFlag {
    /// `_read` — the operation is a select.
    Read,
    /// `_insert`.
    Insert,
    /// `_update`.
    Update,
    /// `_delete`.
    Delete,
    /// `_write` — insert, update or delete.
    Write,
}

impl OpFlag {
    /// The flag for a variable name, if it is one.
    pub(crate) fn from_ident(name: &str) -> Option<OpFlag> {
        match name {
            "_read" => Some(OpFlag::Read),
            "_insert" => Some(OpFlag::Insert),
            "_update" => Some(OpFlag::Update),
            "_delete" => Some(OpFlag::Delete),
            "_write" => Some(OpFlag::Write),
            _ => None,
        }
    }
}

/// The free variables of a formula, collected once at parse time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FreeVars {
    /// Every free identifier, `user` and flags included.
    pub idents: BTreeSet<String>,
    /// Property names statically accessed on `user` (`user.id` → `id`).
    pub user_props: BTreeSet<String>,
    /// True when `user` is indexed with a computed expression (`user[x]`),
    /// which membership checks cannot see through.
    pub user_dynamic: bool,
}

/// Collect the free variables of a lowered AST.
pub(crate) fn collect_free_vars(ast: &Ast) -> FreeVars {
    let mut out = FreeVars::default();
    let mut locals: Vec<String> = Vec::new();
    walk(ast, &mut locals, &mut out);
    out
}

fn walk(ast: &Ast, locals: &mut Vec<String>, out: &mut FreeVars) {
    match ast {
        Ast::Ident(name) => {
            if !locals.iter().any(|l| l == name) {
                out.idents.insert(name.clone());
            }
        }
        Ast::Str(_) | Ast::Num(_) | Ast::Bool(_) | Ast::Null => {}
        Ast::Member { obj, prop, .. } => {
            // `user.x` is recorded for validation; `user` itself is still a
            // free identifier like any other.
            if let Ast::Ident(name) = &**obj
                && name == "user"
                && !locals.iter().any(|l| l == name)
            {
                match prop {
                    MemberProp::Static(p) => {
                        out.user_props.insert(p.clone());
                    }
                    MemberProp::Computed(_) => out.user_dynamic = true,
                }
            }
            walk(obj, locals, out);
            if let MemberProp::Computed(e) = prop {
                walk(e, locals, out);
            }
        }
        Ast::Call { callee, args, .. } => {
            walk(callee, locals, out);
            for a in args {
                walk(a, locals, out);
            }
        }
        Ast::Unary { expr, .. } => walk(expr, locals, out),
        Ast::Binary { l, r, .. } => {
            walk(l, locals, out);
            walk(r, locals, out);
        }
        Ast::Cond { test, cons, alt } => {
            walk(test, locals, out);
            walk(cons, locals, out);
            walk(alt, locals, out);
        }
        Ast::Array(elems) => {
            for e in elems {
                walk(e, locals, out);
            }
        }
        Ast::Template { exprs, .. } => {
            for e in exprs {
                walk(e, locals, out);
            }
        }
        Ast::Arrow { params, body } => {
            let depth = locals.len();
            locals.extend(params.iter().cloned());
            walk(body, locals, out);
            locals.truncate(depth);
        }
    }
}

/// A Ⱶ-join path, resolved link by link through Key fields.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct JoinPath {
    /// The identifier as written, Ⱶ and all (`publisherⱵname`) — the name the
    /// reified evaluator binds and error messages quote.
    pub ident: String,
    /// The segments: each but the last names a Key field to traverse; the last
    /// names a field on the final target table. Always at least two.
    pub segments: Vec<String>,
}

/// What a validated formula refers to — the classified counterpart of
/// [`FreeVars`], and what the later phases plan from: Phase 2's translator
/// turns `join_paths` into correlated subselects, Phase 5's runtime prefetches
/// them for the reified evaluator.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Analysis {
    /// Plain fields of the formula's own table that the formula reads.
    pub fields: BTreeSet<String>,
    /// Every Ⱶ-join path, resolved against the shape.
    pub join_paths: BTreeSet<JoinPath>,
    /// Operation flags used.
    pub flags: BTreeSet<OpFlag>,
    /// Whether `user` appears at all.
    pub uses_user: bool,
    /// Property names statically accessed on `user`.
    pub user_props: BTreeSet<String>,
    /// True when `user` is indexed dynamically (`user[x]`).
    pub user_dynamic: bool,
}

impl Formula {
    /// Check every free variable of this formula against `shape`, classifying
    /// each as a field of `table`, a Ⱶ-join path, `user`, an operation flag or
    /// a global — anything else is an error naming the identifier and the
    /// table. The admin API runs this on save; the catalog merge runs it on
    /// load (an already-stored formula that stops validating is reported and
    /// grants nothing).
    pub fn validate(&self, shape: &SchemaShape, table: &str) -> Result<Analysis> {
        let table_shape = shape
            .tables
            .get(table)
            .ok_or_else(|| invalid(table, format_args!("unknown table `{table}`")))?;
        let free = self.free_vars();
        let mut analysis = Analysis {
            user_props: free.user_props.clone(),
            user_dynamic: free.user_dynamic,
            ..Analysis::default()
        };
        for ident in &free.idents {
            if let Some(flag) = OpFlag::from_ident(ident) {
                analysis.flags.insert(flag);
            } else if ident == "user" {
                analysis.uses_user = true;
            } else if table_shape.fields.contains_key(ident) {
                // Field names win over globals, matching the reified scope
                // where row fields are bound over JavaScript's own globals.
                analysis.fields.insert(ident.clone());
            } else if ident.contains(JOIN) {
                analysis
                    .join_paths
                    .insert(resolve_join_path(shape, table, ident)?);
            } else if GLOBALS.contains(&ident.as_str()) {
                // Fine reified, untranslatable symbolically; nothing to record.
            } else {
                return Err(invalid(table, format_args!("unknown identifier `{ident}`")));
            }
        }
        if let Some(user_fields) = &shape.user_fields {
            for prop in &free.user_props {
                if !user_fields.contains(prop) {
                    return Err(invalid(
                        table,
                        format_args!("`user.{prop}`: the user has no field `{prop}`"),
                    ));
                }
            }
        }
        Ok(analysis)
    }
}

/// Resolve one Ⱶ-identifier into a [`JoinPath`]: every segment but the last
/// must be a Key field, followed link by link; the last must be a field of the
/// table the chain lands on.
fn resolve_join_path(shape: &SchemaShape, table: &str, ident: &str) -> Result<JoinPath> {
    let segments: Vec<String> = ident.split(JOIN).map(str::to_string).collect();
    if segments.iter().any(String::is_empty) {
        return Err(invalid(
            table,
            format_args!("`{ident}`: empty segment around Ⱶ"),
        ));
    }
    let mut current = table.to_string();
    for (i, segment) in segments.iter().enumerate() {
        let table_shape = shape.tables.get(&current).ok_or_else(|| {
            invalid(
                table,
                format_args!("`{ident}`: table `{current}` is not in the schema shape"),
            )
        })?;
        let field = table_shape.fields.get(segment).ok_or_else(|| {
            invalid(
                table,
                format_args!("`{ident}`: `{segment}` is not a field of table `{current}`"),
            )
        })?;
        let last = i == segments.len() - 1;
        if last {
            break;
        }
        let Some(key) = &field.key else {
            return Err(invalid(
                table,
                format_args!("`{ident}`: `{segment}` is not a Key field on `{current}`"),
            ));
        };
        current = key.target_table.clone();
    }
    Ok(JoinPath {
        ident: ident.to_string(),
        segments,
    })
}

fn invalid(table: &str, msg: std::fmt::Arguments<'_>) -> Error {
    Error::invalid(format!("formula on `{table}`: {msg}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::TableShape;

    /// books(id, title, owner, publisher→publishers.id);
    /// publishers(id, name, country→countries.code); countries(code, name).
    fn shape() -> SchemaShape {
        SchemaShape::new()
            .table(
                "books",
                TableShape::new()
                    .field("id")
                    .field("title")
                    .field("owner")
                    .key_field("publisher", "publishers", "id"),
            )
            .table(
                "publishers",
                TableShape::new().field("id").field("name").key_field(
                    "country",
                    "countries",
                    "code",
                ),
            )
            .table("countries", TableShape::new().field("code").field("name"))
            .user_fields(["id", "role", "email"])
    }

    fn validate(src: &str) -> Result<Analysis> {
        Formula::parse(src).unwrap().validate(&shape(), "books")
    }

    #[test]
    fn fields_user_and_flags_classify() {
        let a = validate("_read || (owner === user.id && title !== '')").unwrap();
        assert_eq!(
            a.fields,
            BTreeSet::from(["owner".to_string(), "title".to_string()])
        );
        assert_eq!(a.flags, BTreeSet::from([OpFlag::Read]));
        assert!(a.uses_user);
        assert_eq!(a.user_props, BTreeSet::from(["id".to_string()]));
        assert!(a.join_paths.is_empty());
    }

    #[test]
    fn every_operation_flag_is_recognised() {
        let a = validate("_read || _insert || _update || _delete || _write").unwrap();
        assert_eq!(
            a.flags,
            BTreeSet::from([
                OpFlag::Read,
                OpFlag::Insert,
                OpFlag::Update,
                OpFlag::Delete,
                OpFlag::Write,
            ])
        );
    }

    #[test]
    fn a_join_path_resolves_through_key_fields() {
        let a = validate("publisherⱵname === 'ACME'").unwrap();
        let path = a.join_paths.first().unwrap();
        assert_eq!(path.ident, "publisherⱵname");
        assert_eq!(path.segments, vec!["publisher", "name"]);
    }

    #[test]
    fn a_join_path_chains_to_any_depth() {
        let a = validate("publisherⱵcountryⱵname === user.email").unwrap();
        let path = a.join_paths.first().unwrap();
        assert_eq!(path.segments, vec!["publisher", "country", "name"]);
    }

    #[test]
    fn an_unknown_identifier_is_named_with_its_table() {
        let err = validate("writer === user.id").unwrap_err().to_string();
        assert!(err.contains("unknown identifier `writer`"), "got: {err}");
        assert!(err.contains("`books`"), "got: {err}");
    }

    #[test]
    fn a_join_through_a_non_key_field_is_refused() {
        let err = validate("titleⱵname === 'x'").unwrap_err().to_string();
        assert!(
            err.contains("`title` is not a Key field on `books`"),
            "got: {err}"
        );
    }

    #[test]
    fn a_join_to_a_missing_target_field_is_refused() {
        let err = validate("publisherⱵcity === 'x'").unwrap_err().to_string();
        assert!(
            err.contains("`city` is not a field of table `publishers`"),
            "got: {err}"
        );
    }

    #[test]
    fn an_empty_join_segment_is_refused() {
        // `Ⱶname` is a lone identifier starting with the join character.
        let err = validate("Ⱶname === 'x'").unwrap_err().to_string();
        assert!(err.contains("empty segment"), "got: {err}");
    }

    #[test]
    fn an_unknown_user_field_is_refused_only_when_fields_are_known() {
        let err = validate("owner === user.shoe_size")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no field `shoe_size`"), "got: {err}");
        // With user fields undeclared, the same formula passes — the caller
        // said it does not know, so nothing can be contradicted.
        let mut unknowing = shape();
        unknowing.user_fields = None;
        let a = Formula::parse("owner === user.shoe_size")
            .unwrap()
            .validate(&unknowing, "books")
            .unwrap();
        assert!(a.uses_user);
    }

    #[test]
    fn dynamic_user_access_is_recorded_not_rejected() {
        let a = validate("owner === user[title]").unwrap();
        assert!(a.user_dynamic);
        // The computed index is itself a free variable and classified.
        assert!(a.fields.contains("title"));
    }

    #[test]
    fn globals_pass_validation_and_fields_shadow_them() {
        // `Math` and `undefined` are JavaScript's own, not unknown identifiers.
        assert!(validate("Math.abs(owner) > 0 && title !== undefined").is_ok());
        // A field named like a global classifies as the field.
        let s = SchemaShape::new().table("t", TableShape::new().field("Math"));
        let a = Formula::parse("Math === 1")
            .unwrap()
            .validate(&s, "t")
            .unwrap();
        assert!(a.fields.contains("Math"));
    }

    #[test]
    fn arrow_parameters_are_not_free_variables() {
        let s = SchemaShape::new().table("t", TableShape::new().field("groups").field("dept"));
        let a = Formula::parse("groups.some(g => g === dept)")
            .unwrap()
            .validate(&s, "t")
            .unwrap();
        // `g` is bound by the arrow; only the two fields are free.
        assert_eq!(
            a.fields,
            BTreeSet::from(["groups".to_string(), "dept".to_string()])
        );
    }

    #[test]
    fn an_arrow_parameter_shadows_only_inside_its_body() {
        let s = SchemaShape::new().table("t", TableShape::new().field("xs"));
        // Outside the arrow, `g` is free again — and unknown.
        let err = Formula::parse("xs.some(g => g > 0) && g")
            .unwrap()
            .validate(&s, "t")
            .unwrap_err();
        assert!(
            err.to_string().contains("unknown identifier `g`"),
            "got: {err}"
        );
    }

    #[test]
    fn validating_against_a_missing_table_fails() {
        let err = validate_on_missing().unwrap_err().to_string();
        assert!(err.contains("unknown table"), "got: {err}");
    }

    fn validate_on_missing() -> Result<Analysis> {
        Formula::parse("a === 1")
            .unwrap()
            .validate(&SchemaShape::new(), "absent")
    }
}
