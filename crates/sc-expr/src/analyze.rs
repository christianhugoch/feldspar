//! Free-variable analysis and validation against a [`SchemaShape`].
//!
//! Collection is pure syntax and happens once at parse time: every identifier
//! not bound by an arrow parameter is free, and member accesses on an **ambient
//! object** ([`Ambient`] — `user`, `row`, `old`, `payload`) are recorded so `user.x` can be
//! checked against the user table and `row.x` against the triggering table.
//! Classification — is this identifier a field, a Ⱶ-join path, an ambient
//! object, an operation flag, a whitelisted global, or a mistake — needs a
//! shape, so it happens in [`Formula::validate`], which the admin API calls on
//! save and the catalog merge calls on load.

use std::collections::{BTreeMap, BTreeSet};

use sc_error::{Error, Result};

use crate::agg::{self, AggUse, INVERSE};
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

/// An **ambient object**: an identifier that binds to an object-or-null which
/// the formula does not *range over*.
///
/// The scope rule the whole formula language follows is that bare identifiers
/// name fields of the row a formula is about; everything else it can see is
/// ambient and spelled as an object. `user` was the first (§7.3) and is always in
/// scope, because every formula has a caller. `row` and `old` are the triggering
/// event's row and its pre-update state, in scope only where a
/// [`SchemaShape`](crate::SchemaShape) declares them — a trigger's `only_if` or
/// an action's configuration — so an ownership formula naming `row` is still the
/// unknown identifier it always was.
///
/// All three share one set of semantics, which is why they are one enum rather
/// than three special cases: object-or-null, member access null-guarded (a null
/// object yields null, never a throw), and members checked against the fields of
/// whatever table the object stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ambient {
    /// `user` — the caller's fields, or null when nobody is logged in.
    User,
    /// `row` — the row the triggering event is about.
    Row,
    /// `old` — that row as it was before an update; null on any other event.
    Old,
    /// `payload` — the event's free-form detail: a directly-run trigger's posted
    /// body, an error event's `{kind, message, …}`. Null where the event has
    /// none, and **fieldless**: unlike `row` and `user`, nothing declares what is
    /// in it, so `payload.anything` resolves and reads null when it is not there.
    Payload,
}

impl Ambient {
    /// Every ambient object, in scope-declaration order.
    pub const ALL: [Ambient; 4] = [Ambient::User, Ambient::Row, Ambient::Old, Ambient::Payload];

    /// The identifier this object is spelled with.
    pub fn as_str(self) -> &'static str {
        match self {
            Ambient::User => "user",
            Ambient::Row => "row",
            Ambient::Old => "old",
            Ambient::Payload => "payload",
        }
    }

    /// The ambient object an identifier names, if it names one. Whether it is
    /// *in scope* is a separate question the shape answers
    /// ([`SchemaShape::declares_ambient`](crate::SchemaShape::declares_ambient)).
    pub fn from_ident(name: &str) -> Option<Ambient> {
        Ambient::ALL.into_iter().find(|a| a.as_str() == name)
    }
}

impl std::fmt::Display for Ambient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

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
    /// Every free identifier, ambient objects and flags included.
    pub idents: BTreeSet<String>,
    /// Property names statically accessed on each ambient object
    /// (`user.id` → `User` ↦ `id`).
    pub ambient_props: BTreeMap<Ambient, BTreeSet<String>>,
    /// Ambient objects indexed with a computed expression (`user[x]`), which
    /// membership checks cannot see through.
    pub ambient_dynamic: BTreeSet<Ambient>,
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
            // `user.x` / `row.x` is recorded for validation; the object itself is
            // still a free identifier like any other.
            if let Ast::Ident(name) = &**obj
                && let Some(amb) = Ambient::from_ident(name)
                && !locals.iter().any(|l| l == name)
            {
                match prop {
                    MemberProp::Static(p) => {
                        out.ambient_props.entry(amb).or_default().insert(p.clone());
                    }
                    MemberProp::Computed(_) => {
                        out.ambient_dynamic.insert(amb);
                    }
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
    /// The ambient objects the formula reads, with what it reads from each. An
    /// object absent from the map is one the formula never names.
    pub ambient: BTreeMap<Ambient, AmbientUse>,
    /// Every aggregation over an incoming key (Phase 7), resolved against the
    /// shape — the prefetch plan now, the stored-calc trigger dependencies
    /// later.
    pub agg_uses: BTreeSet<AggUse>,
}

/// What a formula reads from one ambient object.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AmbientUse {
    /// Property names statically accessed (`user.id` → `id`) — the set validated
    /// against the object's fields.
    pub props: BTreeSet<String>,
    /// True when the object is indexed dynamically (`user[x]`), which membership
    /// checks cannot see through. Recorded, not rejected: it is legitimate
    /// reified and simply untranslatable symbolically.
    pub dynamic: bool,
}

impl Analysis {
    /// Whether the formula names `ambient` at all.
    pub fn uses(&self, ambient: Ambient) -> bool {
        self.ambient.contains_key(&ambient)
    }

    /// The properties the formula reads from `ambient` (empty when unused).
    pub fn ambient_props(&self, ambient: Ambient) -> impl Iterator<Item = &str> {
        self.ambient
            .get(&ambient)
            .into_iter()
            .flat_map(|use_| use_.props.iter().map(String::as_str))
    }

    /// The first ambient object used that is **not** in `allowed` — what a caller
    /// with a narrower scope refuses by name.
    ///
    /// A calculated field allows none of them, an ownership formula only `user`,
    /// a trigger formula all three; each is one call rather than three copies of
    /// the same walk.
    pub fn ambient_outside(&self, allowed: &[Ambient]) -> Option<Ambient> {
        self.ambient
            .keys()
            .copied()
            .find(|amb| !allowed.contains(amb))
    }
}

impl Formula {
    /// Check every free variable of this formula against `shape`, classifying
    /// each as a field of `table`, a Ⱶ-join path, an in-scope [`Ambient`] object,
    /// an operation flag or a global — anything else is an error naming the
    /// identifier and the table. The admin API runs this on save; the catalog
    /// merge runs it on load (an already-stored formula that stops validating is
    /// reported and grants nothing).
    ///
    /// Which ambient objects are in scope is the shape's to declare: `user`
    /// always is, `row`/`old` only where a trigger's shape says so, so a formula
    /// naming `row` in an ownership setting is refused as the unknown identifier
    /// it is rather than quietly evaluating to null.
    pub fn validate(&self, shape: &SchemaShape, table: &str) -> Result<Analysis> {
        let table_shape = shape
            .tables
            .get(table)
            .ok_or_else(|| invalid(table, format_args!("unknown table `{table}`")))?;
        let free = self.free_vars();
        let mut analysis = Analysis::default();
        for (amb, props) in &free.ambient_props {
            if shape.declares_ambient(*amb) {
                analysis.ambient.entry(*amb).or_default().props = props.clone();
            }
        }
        for amb in &free.ambient_dynamic {
            if shape.declares_ambient(*amb) {
                analysis.ambient.entry(*amb).or_default().dynamic = true;
            }
        }
        // Aggregations first: an inverse-relation identifier is only meaningful
        // as the root of a curated chain, so the whole chain is validated here
        // and its relation identifier is *skipped* in the free-variable loop.
        walk_aggregations(self.ast(), shape, table, &mut analysis)?;
        for ident in &free.idents {
            if let Some(flag) = OpFlag::from_ident(ident) {
                analysis.flags.insert(flag);
            } else if let Some(amb) =
                Ambient::from_ident(ident).filter(|amb| shape.declares_ambient(*amb))
            {
                analysis.ambient.entry(amb).or_default();
            } else if table_shape.fields.contains_key(ident) {
                // Field names win over globals, matching the reified scope
                // where row fields are bound over JavaScript's own globals.
                analysis.fields.insert(ident.clone());
            } else if ident.contains(INVERSE) {
                // An inverse relation, validated by `walk_aggregations` above.
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
        // Every ambient member checked against the fields of whatever the object
        // stands for, where the caller has declared them. An undeclared field set
        // means "the caller does not know", which skips the check rather than
        // rejecting every access.
        for (amb, use_) in &analysis.ambient {
            let Some(fields) = shape.ambient_field_set(*amb) else {
                continue;
            };
            for prop in &use_.props {
                if !fields.contains(prop) {
                    return Err(invalid(
                        table,
                        format_args!("`{amb}.{prop}`: `{amb}` has no field `{prop}`"),
                    ));
                }
            }
        }
        Ok(analysis)
    }
}

/// Walk `ast` collecting and validating every aggregation chain. At each node
/// that reads as a chain (rooted at an inverse-relation identifier) the chain is
/// resolved and field-checked and its [`AggUse`] recorded; the walk then
/// descends only into the chain's *sub-expressions* (arrow bodies, value
/// arguments — parent-scope syntax that may hold further aggregations), not its
/// method spine. Non-chain nodes are walked child by child.
fn walk_aggregations(
    ast: &Ast,
    shape: &SchemaShape,
    table: &str,
    analysis: &mut Analysis,
) -> Result<()> {
    if let Some(chain) = agg::parse_chain(ast) {
        let chain = chain?;
        let rel = chain.resolve(shape, table)?;
        chain.validate_fields(shape, &rel)?;
        analysis.agg_uses.insert(chain.agg_use(&rel));
        // Descend into the parent-scope sub-expressions the chain carries.
        for sub in chain.sub_expressions() {
            walk_aggregations(sub, shape, table, analysis)?;
        }
        return Ok(());
    }
    for child in child_nodes(ast) {
        walk_aggregations(child, shape, table, analysis)?;
    }
    Ok(())
}

/// The direct sub-expressions of a node, for the aggregation walk.
fn child_nodes(ast: &Ast) -> Vec<&Ast> {
    match ast {
        Ast::Ident(_) | Ast::Str(_) | Ast::Num(_) | Ast::Bool(_) | Ast::Null => Vec::new(),
        Ast::Member { obj, prop, .. } => {
            let mut v = vec![&**obj];
            if let MemberProp::Computed(e) = prop {
                v.push(e);
            }
            v
        }
        Ast::Call { callee, args, .. } => {
            let mut v = vec![&**callee];
            v.extend(args.iter());
            v
        }
        Ast::Unary { expr, .. } => vec![expr],
        Ast::Binary { l, r, .. } => vec![l, r],
        Ast::Cond { test, cons, alt } => vec![test, cons, alt],
        Ast::Array(elems) => elems.iter().collect(),
        Ast::Template { exprs, .. } => exprs.iter().collect(),
        Ast::Arrow { body, .. } => vec![body],
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

    /// The shape a trigger's formula is validated against: `row`/`old` in scope
    /// with the triggering table's fields (decision 7).
    fn trigger_shape() -> SchemaShape {
        let event_fields = ["id", "title", "pages", "owner"];
        shape()
            .ambient_fields(Ambient::Row, Some(event_fields))
            .ambient_fields(Ambient::Old, Some(event_fields))
    }

    fn validate_trigger(src: &str) -> Result<Analysis> {
        Formula::parse(src)
            .unwrap()
            .validate(&trigger_shape(), "books")
    }

    #[test]
    fn ambient_row_and_old_are_in_scope_only_where_declared() {
        // Declared: classified as ambient objects, with their properties recorded
        // and the bare field scope untouched.
        let a = validate_trigger("title !== old.title && row.owner === user.id").unwrap();
        assert_eq!(a.fields, BTreeSet::from(["title".to_string()]));
        assert!(a.uses(Ambient::Old) && a.uses(Ambient::Row) && a.uses(Ambient::User));
        assert_eq!(a.ambient_props(Ambient::Old).collect::<Vec<_>>(), ["title"]);
        assert_eq!(a.ambient_props(Ambient::Row).collect::<Vec<_>>(), ["owner"]);

        // Undeclared (an ownership formula's shape): the same identifier is the
        // unknown identifier it always was — not a silent null.
        let err = validate("row.owner === user.id").unwrap_err().to_string();
        assert!(err.contains("unknown identifier `row`"), "got: {err}");
        assert!(!validate("old !== null").is_ok());
    }

    #[test]
    fn an_unknown_ambient_member_is_refused_by_name() {
        let err = validate_trigger("old.shoe_size === 1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("`old.shoe_size`"), "got: {err}");
        assert!(err.contains("has no field `shoe_size`"), "got: {err}");
        // `user`'s check is the same one, worded the same way.
        let err = validate_trigger("user.shoe_size === 1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("`user.shoe_size`"), "got: {err}");
    }

    #[test]
    fn ambient_outside_the_allowed_set_is_reported_for_the_caller_to_refuse() {
        let a = validate_trigger("row.title === title").unwrap();
        // A calc field allows no ambient object at all; an ownership formula only
        // `user`; a trigger formula all three.
        assert_eq!(a.ambient_outside(&[]), Some(Ambient::Row));
        assert_eq!(a.ambient_outside(&[Ambient::User]), Some(Ambient::Row));
        assert_eq!(a.ambient_outside(&Ambient::ALL), None);
        let user_only = validate("owner === user.id").unwrap();
        assert_eq!(user_only.ambient_outside(&[Ambient::User]), None);
        assert_eq!(user_only.ambient_outside(&[]), Some(Ambient::User));
    }

    #[test]
    fn an_ambient_object_shadows_a_field_of_the_same_name_where_it_is_in_scope() {
        // A table with a field called `row` keeps it in an ownership formula…
        let with_row_field = SchemaShape::new().table("t", TableShape::new().field("row"));
        let a = Formula::parse("row === 1")
            .unwrap()
            .validate(&with_row_field, "t")
            .unwrap();
        assert_eq!(a.fields, BTreeSet::from(["row".to_string()]));
        assert!(!a.uses(Ambient::Row));
        // …and loses it to the ambient object inside a trigger formula, which is
        // the same rule `user` has always had. Documented, not accidental.
        let shadowed = with_row_field.ambient_fields(Ambient::Row, None::<Vec<String>>);
        let a = Formula::parse("row === 1")
            .unwrap()
            .validate(&shadowed, "t")
            .unwrap();
        assert!(a.fields.is_empty() && a.uses(Ambient::Row));
    }

    #[test]
    fn fields_user_and_flags_classify() {
        let a = validate("_read || (owner === user.id && title !== '')").unwrap();
        assert_eq!(
            a.fields,
            BTreeSet::from(["owner".to_string(), "title".to_string()])
        );
        assert_eq!(a.flags, BTreeSet::from([OpFlag::Read]));
        assert!(a.uses(Ambient::User));
        assert_eq!(
            a.ambient_props(Ambient::User).collect::<Vec<_>>(),
            vec!["id"]
        );
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
        let unknowing = shape().ambient_fields(Ambient::User, None::<Vec<String>>);
        let a = Formula::parse("owner === user.shoe_size")
            .unwrap()
            .validate(&unknowing, "books")
            .unwrap();
        assert!(a.uses(Ambient::User));
    }

    #[test]
    fn dynamic_user_access_is_recorded_not_rejected() {
        let a = validate("owner === user[title]").unwrap();
        assert!(a.ambient[&Ambient::User].dynamic);
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
