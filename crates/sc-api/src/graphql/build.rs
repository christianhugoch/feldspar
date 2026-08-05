//! Building the application's `async_graphql::dynamic::Schema` from its tables.
//!
//! The schema is **data**: an admin or an agent creates a table at runtime and
//! the API has it at the next mount, which is the whole reason the dynamic API
//! was chosen over every macro-driven library. So this module is a fold over
//! [`Table`] values producing types, and nothing here is known at compile time.
//!
//! The shape is Hasura-flavoured, because that is the quality bar the design
//! sets and the shape callers already know — with the deviations recorded in
//! docs/GRAPHQL_API.md §4, plus one more that arrived with the type mapping:
//! **`avg` has its own result type.** Hasura shares one `NumericFields` between
//! `sum` and `avg`; the mean of a `BigInt` column is not a `BigInt`, and a
//! schema that says it is has promised to round somebody's number.
//!
//! What each field *does* is [`resolve`](super::resolve)'s business; this module
//! decides only which resolver a field gets — and the aggregate result objects
//! are the one place where that mapping is not obvious: `sum` and `avg` are
//! *groups*, whose columns are the values, while `count` is a value itself, so
//! they take different resolvers over the same flat set of computed columns.

use async_graphql::dynamic::{
    Enum, Field, InputObject, InputValue, Object, Scalar, Schema, TypeRef,
};
use sc_catalog::{DataField, DataFieldKind, Table};
use sc_error::{Error, Result};

use super::args::{ARG_DISTINCT, ARG_LIMIT, ARG_OFFSET, ARG_ORDER_BY, ARG_WHERE};
use super::names::{
    self, FILE_VALUE, ORDER_DIRECTION, QUERY_ROOT, SCALAR_NAMES, SchemaNames, TableNames,
    comparison_type_name,
};
use super::resolve;
use super::types::{CUSTOM_SCALARS, column_scalar, field_type, scalar_name};
use crate::schema::ValueType;

/// Build the application's schema from the tables its names were derived from.
///
/// Failing here is a **mount failure**, never a half-served schema: an
/// application that cannot describe its own API must not come up answering some
/// of it. Where `async-graphql`'s own error names a type, the message names the
/// table that type came from — the thing an admin can actually act on.
pub fn build_schema(tables: &[Table], names: &SchemaNames) -> Result<Schema> {
    if names.tables().is_empty() {
        return Err(Error::config(
            "this application exposes no tables that can be projected into GraphQL, so its \
             schema would have no fields; declare a table, or disable the `graphql` provider"
                .to_owned(),
        ));
    }

    let mut builder = Schema::build(QUERY_ROOT, None, None);

    // The scalars GraphQL does not have, and the two enums/objects that are the
    // same for every table.
    for scalar in CUSTOM_SCALARS {
        builder = builder.register(Scalar::new(*scalar));
    }
    builder = builder.register(order_direction_enum());
    builder = builder.register(file_value_object());
    for scalar in SCALAR_NAMES {
        builder = builder.register(comparison_input(scalar));
    }

    let mut query = Object::new(QUERY_ROOT);
    for t in names.tables() {
        // Every derived table name came from a table in the set.
        let Some(table) = tables.iter().find(|x| x.name == t.table) else {
            continue;
        };
        builder = builder.register(row_object(table, t, names));
        builder = builder.register(bool_exp_input(table, t));
        builder = builder.register(order_by_input(table, t));
        builder = builder.register(select_column_enum(t));
        builder = builder.register(aggregate_object(table, t));
        if let Some(obj) = numeric_fields_object(table, t, &t.numeric_object, NumericAs::Column) {
            builder = builder.register(obj);
        }
        if let Some(obj) = numeric_fields_object(table, t, &t.avg_object, NumericAs::Average) {
            builder = builder.register(obj);
        }
        if let Some(obj) = comparable_fields_object(table, t) {
            builder = builder.register(obj);
        }
        query = add_root_fields(query, table, t);
    }

    builder
        .register(query)
        .finish()
        .map_err(|e| schema_error(e, names))
}

/// An `async-graphql` schema error, attributed to the table it came from.
fn schema_error(err: async_graphql::dynamic::SchemaError, names: &SchemaNames) -> Error {
    let message = err.to_string();
    // The library's messages name the offending *type*; the admin knows tables.
    let culprit = message
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter_map(|word| names.table_of_type(word))
        .next();
    match culprit {
        Some(table) => Error::config(format!(
            "the GraphQL schema for table `{table}` could not be built: {message}"
        )),
        None => Error::config(format!("the GraphQL schema could not be built: {message}")),
    }
}

/// `enum OrderDirection { asc desc }`.
fn order_direction_enum() -> Enum {
    Enum::new(ORDER_DIRECTION).item("asc").item("desc")
}

/// The object a `File` field projects as: the stored path and the URL the REST
/// provider serves the bytes at.
fn file_value_object() -> Object {
    Object::new(FILE_VALUE)
        .description("A file reference: its path within its store, and the URL its bytes are served at by this application's REST API.")
        .field(Field::new(
            "path",
            TypeRef::named_nn(TypeRef::STRING),
            resolve::file_part("path"),
        ))
        .field(Field::new(
            "url",
            TypeRef::named_nn(TypeRef::STRING),
            resolve::file_part("url"),
        ))
}

/// `input StringComparison { eq ne gt gte lt lte in nin is_null like ilike }`.
///
/// One per scalar rather than one per column: `lt` means the same thing wherever
/// the scalar appears, and a type per column would be a schema nobody can read.
fn comparison_input(scalar: &str) -> InputObject {
    let mut input = InputObject::new(comparison_type_name(scalar))
        .field(InputValue::new("eq", TypeRef::named(scalar)))
        .field(InputValue::new("ne", TypeRef::named(scalar)));
    // Ordered comparisons need an ordering; embedded JSON has none.
    if scalar != names::JSON {
        for op in ["gt", "gte", "lt", "lte"] {
            input = input.field(InputValue::new(op, TypeRef::named(scalar)));
        }
    }
    input = input
        .field(InputValue::new("in", TypeRef::named_nn_list(scalar)))
        .field(InputValue::new("nin", TypeRef::named_nn_list(scalar)))
        .field(InputValue::new("is_null", TypeRef::named(TypeRef::BOOLEAN)));
    if scalar == TypeRef::STRING {
        input = input
            .field(InputValue::new("like", TypeRef::named(scalar)))
            .field(InputValue::new("ilike", TypeRef::named(scalar)));
    }
    input
}

/// The row object: the table's columns, then its inverse relations.
fn row_object(table: &Table, t: &TableNames, names: &SchemaNames) -> Object {
    let mut object = Object::new(&t.object);
    if !table.description.is_empty() {
        object = object.description(&table.description);
    }
    for name in &t.fields {
        let Some(field) = table.field(name) else {
            continue;
        };
        object = object.field(Field::new(
            name,
            field_type(field, names),
            row_field_resolver(field, names),
        ));
    }
    for rel in &t.relations {
        let Some(child) = names.get(&rel.child_table) else {
            continue;
        };
        object = object.field(
            list_arguments(
                Field::new(
                    &rel.list_field,
                    // Nullable, where the root list is not: the child's own
                    // access rules decide this field, and a caller refused by
                    // them must lose *this field*, not the parent row that a
                    // non-null list would propagate the error up to. Partial
                    // results are the point of the deviation.
                    TypeRef::named_nn_list(&child.object),
                    resolve::child_list_field(&rel.child_table, &rel.key_field, &rel.parent_field),
                ),
                child,
            )
            .description(format!(
                "Rows of `{}` whose `{}` references this row.",
                rel.child_table, rel.key_field
            )),
        );
        object = object.field(
            Field::new(
                &rel.aggregate_field,
                TypeRef::named_nn(&child.aggregate_object),
                resolve::child_aggregate_field(&rel.child_table),
            )
            .argument(InputValue::new(ARG_WHERE, TypeRef::named(&child.bool_exp)))
            .description(format!(
                "Aggregates over the rows of `{}` that reference this row — computed by the \
                 database as a correlated subquery, not by fetching them.",
                rel.child_table
            )),
        );
    }
    object
}

/// Which resolver one row field gets — decided by the same match
/// [`field_type`] uses, so the type a field promises and the value it produces
/// cannot disagree.
fn row_field_resolver(field: &DataField, names: &SchemaNames) -> resolve::Resolver {
    let name = &field.base.name;
    match &field.kind {
        // A key whose target this application exposes resolves *through* the
        // parent query's projected Ⱶ-join; one whose target it does not carries
        // the column's own value, exactly as its type says.
        DataFieldKind::Key { target_table, .. } => match names.get(&target_table.0) {
            Some(_) => resolve::key_field(name, &target_table.0),
            None => resolve::column_field(name),
        },
        DataFieldKind::File { .. } => resolve::file_field(name),
        DataFieldKind::Plain | DataFieldKind::Calc { .. } => resolve::column_field(name),
    }
}

/// `where` / `order_by` / `limit` / `offset` on a collection field.
fn list_arguments(field: Field, t: &TableNames) -> Field {
    field
        .argument(InputValue::new(ARG_WHERE, TypeRef::named(&t.bool_exp)))
        .argument(InputValue::new(
            ARG_ORDER_BY,
            TypeRef::named_nn_list(&t.order_by),
        ))
        .argument(InputValue::new(ARG_LIMIT, TypeRef::named(TypeRef::INT)))
        .argument(InputValue::new(ARG_OFFSET, TypeRef::named(TypeRef::INT)))
}

/// `input XBoolExp` — the per-column comparisons plus `_and`/`_or`/`_not`.
fn bool_exp_input(table: &Table, t: &TableNames) -> InputObject {
    let mut input = InputObject::new(&t.bool_exp)
        .field(InputValue::new("_and", TypeRef::named_nn_list(&t.bool_exp)))
        .field(InputValue::new("_or", TypeRef::named_nn_list(&t.bool_exp)))
        .field(InputValue::new("_not", TypeRef::named(&t.bool_exp)));
    for name in &t.fields {
        let Some(field) = table.field(name) else {
            continue;
        };
        // A filter is over the *column*, including for a `Key` field, whose
        // value is the foreign key it holds. Filtering through the relation is
        // a nested question and gets a nested answer later.
        input = input.field(InputValue::new(
            name,
            TypeRef::named(comparison_type_name(column_scalar(field))),
        ));
    }
    input
}

/// `input XOrderBy` — one nullable `OrderDirection` per column.
fn order_by_input(table: &Table, t: &TableNames) -> InputObject {
    let mut input = InputObject::new(&t.order_by);
    for name in &t.fields {
        if table.field(name).is_some() {
            input = input.field(InputValue::new(name, TypeRef::named(ORDER_DIRECTION)));
        }
    }
    input
}

/// `enum XSelectColumn` — the column `count(distinct:)` names.
fn select_column_enum(t: &TableNames) -> Enum {
    let mut e = Enum::new(&t.select_column);
    for name in &t.fields {
        e = e.item(name);
    }
    e
}

/// `type XAggregate { count sum avg min max }`.
///
/// `sum`/`avg` appear only when the table has a numeric column and `min`/`max`
/// only when it has a comparable one: GraphQL has no empty object type, and a
/// `sum` over nothing is not a thing to ask for.
fn aggregate_object(table: &Table, t: &TableNames) -> Object {
    let mut object = Object::new(&t.aggregate_object).field(
        Field::new(
            "count",
            TypeRef::named_nn(TypeRef::INT),
            resolve::agg_value_field(TypeRef::INT),
        )
        .argument(InputValue::new(
            ARG_DISTINCT,
            TypeRef::named(&t.select_column),
        )),
    );
    if aggregated_columns(table, t).any(is_numeric) {
        for (name, ty) in [("sum", &t.numeric_object), ("avg", &t.avg_object)] {
            object = object.field(Field::new(
                name,
                TypeRef::named_nn(ty),
                resolve::agg_group_field(),
            ));
        }
    }
    if aggregated_columns(table, t).any(is_comparable) {
        for name in ["min", "max"] {
            object = object.field(Field::new(
                name,
                TypeRef::named_nn(&t.comparable_object),
                resolve::agg_group_field(),
            ));
        }
    }
    object
}

/// How a numeric column is typed in an aggregate's result object.
#[derive(Clone, Copy)]
enum NumericAs {
    /// `sum` — the column's own scalar (a sum of integers is an integer).
    Column,
    /// `avg` — exact rather than the column's type: the mean of integers is not
    /// an integer, and `Float` would round a decimal.
    Average,
}

/// `type XNumericFields` / `type XAvgFields`, or `None` when there is nothing
/// numeric to put in one.
fn numeric_fields_object(
    table: &Table,
    t: &TableNames,
    type_name: &str,
    as_: NumericAs,
) -> Option<Object> {
    let mut object = Object::new(type_name);
    let mut any = false;
    for field in aggregated_columns(table, t).filter(|f| is_numeric(f)) {
        let name = &field.base.name;
        let scalar = match as_ {
            NumericAs::Column => column_scalar(field),
            // Postgres averages an integer or a decimal as `numeric` and a
            // float as `double precision`; the wire types follow.
            NumericAs::Average if column_scalar(field) == scalar_name(ValueType::Float) => {
                scalar_name(ValueType::Float)
            }
            NumericAs::Average => scalar_name(ValueType::Decimal),
        };
        object = object.field(Field::new(
            name,
            TypeRef::named(scalar),
            resolve::agg_value_field(scalar),
        ));
        any = true;
    }
    any.then_some(object)
}

/// `type XComparableFields`, or `None` when nothing in the table is comparable.
fn comparable_fields_object(table: &Table, t: &TableNames) -> Option<Object> {
    let mut object = Object::new(&t.comparable_object);
    let mut any = false;
    for field in aggregated_columns(table, t).filter(|f| is_comparable(f)) {
        let name = &field.base.name;
        object = object.field(Field::new(
            name,
            TypeRef::named(column_scalar(field)),
            resolve::agg_value_field(column_scalar(field)),
        ));
        any = true;
    }
    any.then_some(object)
}

/// The columns an aggregate may be taken over: the exposed **stored** ones.
///
/// A `File` field is excluded — the minimum of a set of paths is not a question
/// anybody is asking — and so is a calculated field, which has no column for the
/// database to aggregate. A `Key` is included: it holds a real value.
fn aggregated_columns<'a>(
    table: &'a Table,
    t: &'a TableNames,
) -> impl Iterator<Item = &'a DataField> {
    t.fields
        .iter()
        .filter_map(move |name| table.field(name))
        .filter(|f| matches!(f.kind, DataFieldKind::Plain | DataFieldKind::Key { .. }))
}

/// Whether `sum`/`avg` are meaningful over this column.
fn is_numeric(field: &DataField) -> bool {
    matches!(
        column_scalar(field),
        names::BIG_INT | names::DECIMAL | "Float"
    )
}

/// Whether `min`/`max` are meaningful over this column — everything with an
/// ordering, which is everything but embedded JSON.
fn is_comparable(field: &DataField) -> bool {
    column_scalar(field) != names::JSON
}

/// The root fields for one table: the list, the single row, and the aggregate.
fn add_root_fields(query: Object, table: &Table, t: &TableNames) -> Object {
    let query = query.field(
        list_arguments(
            Field::new(
                &t.list_field,
                TypeRef::named_nn_list_nn(&t.object),
                resolve::list_field(&t.table),
            ),
            t,
        )
        .description(format!("Rows of `{}`.", t.table)),
    );
    // `_by_pk` needs one column to address a row by — the same rule the row
    // layer enforces, and the same one the REST projection applies before it
    // emits `PUT`/`DELETE`.
    let by_pk = crate::rows::single_pk(table)
        .ok()
        .filter(|pk| t.fields.contains(pk))
        .and_then(|pk| table.field(&pk).map(|f| (pk.clone(), column_scalar(f))));
    let query = match by_pk {
        Some((pk, scalar)) => query.field(
            Field::new(
                &t.by_pk_field,
                TypeRef::named(&t.object),
                resolve::by_pk_field(&t.table, &pk),
            )
            .argument(InputValue::new(&pk, TypeRef::named_nn(scalar)))
            .description(format!("The row of `{}` with this primary key.", t.table)),
        ),
        None => query,
    };
    query.field(
        Field::new(
            &t.aggregate_field,
            TypeRef::named_nn(&t.aggregate_object),
            resolve::aggregate_field(&t.table),
        )
        .argument(InputValue::new(ARG_WHERE, TypeRef::named(&t.bool_exp)))
        .description(format!("Aggregates over the rows of `{}`.", t.table)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphql::testing::{
        file_field, id_field, key_field, plain_field, table_of, typed_field,
    };
    use sc_types::BasicType;

    fn sdl_of(tables: &[Table]) -> String {
        let names = SchemaNames::derive(tables);
        build_schema(tables, &names).expect("schema builds").sdl()
    }

    #[test]
    fn an_application_with_no_projectable_table_is_a_mount_failure() {
        // Not an empty schema served cheerfully: a GraphQL API with no fields
        // is a configuration mistake, and this is where it is named.
        let names = SchemaNames::derive(&[]);
        let err = build_schema(&[], &names).unwrap_err();
        assert!(format!("{err}").contains("no tables"), "{err}");
    }

    #[test]
    fn the_root_carries_a_list_a_by_pk_and_an_aggregate_per_table() {
        let sdl = sdl_of(&[table_of(
            "departments",
            vec![id_field(), plain_field("name")],
        )]);
        assert!(
            sdl.contains(
                "departments(where: DepartmentsBoolExp, order_by: [DepartmentsOrderBy!], \
                 limit: Int, offset: Int): [Departments!]!"
            ),
            "{sdl}"
        );
        assert!(
            sdl.contains("departments_by_pk(id: BigInt!): Departments"),
            "{sdl}"
        );
        assert!(
            sdl.contains("departments_aggregate(where: DepartmentsBoolExp): DepartmentsAggregate!"),
            "{sdl}"
        );
    }

    #[test]
    fn a_table_with_no_single_column_primary_key_has_no_by_pk() {
        // The same rule the REST projection applies before emitting PUT/DELETE:
        // with nothing to address a row by, the field would be a promise the
        // row layer cannot keep.
        let sdl = sdl_of(&[table_of("notes", vec![plain_field("body")])]);
        assert!(sdl.contains("notes(where:"), "{sdl}");
        assert!(!sdl.contains("notes_by_pk"), "{sdl}");
    }

    #[test]
    fn a_relation_appears_on_the_parent_as_a_list_and_an_aggregate() {
        let sdl = sdl_of(&[
            table_of("departments", vec![id_field()]),
            table_of(
                "employees",
                vec![id_field(), key_field("department", "departments", "id")],
            ),
        ]);
        assert!(
            sdl.contains(
                "employees(where: EmployeesBoolExp, order_by: [EmployeesOrderBy!], \
                 limit: Int, offset: Int): [Employees!]!"
            ),
            "{sdl}"
        );
        assert!(
            sdl.contains("employees_aggregate(where: EmployeesBoolExp): EmployeesAggregate!"),
            "{sdl}"
        );
        // The outgoing key is the target's object type, not its raw value.
        assert!(sdl.contains("department: Departments"), "{sdl}");
    }

    #[test]
    fn avg_does_not_promise_to_round() {
        // A BigInt column's sum is a BigInt and its average is not.
        let sdl = sdl_of(&[table_of(
            "employees",
            vec![id_field(), typed_field("salary", BasicType::Int)],
        )]);
        assert!(sdl.contains("type EmployeesNumericFields {"), "{sdl}");
        assert!(sdl.contains("type EmployeesAvgFields {"), "{sdl}");
        let numeric = sdl
            .split("type EmployeesNumericFields {")
            .nth(1)
            .and_then(|s| s.split('}').next())
            .unwrap_or_default()
            .to_owned();
        assert!(numeric.contains("salary: BigInt"), "{numeric}");
        let avg = sdl
            .split("type EmployeesAvgFields {")
            .nth(1)
            .and_then(|s| s.split('}').next())
            .unwrap_or_default()
            .to_owned();
        assert!(avg.contains("salary: Decimal"), "{avg}");
    }

    #[test]
    fn a_table_with_nothing_numeric_has_no_sum_or_avg() {
        // GraphQL has no empty object type, so the fields that would need one
        // are simply absent rather than pointing at an unbuildable type.
        let sdl = sdl_of(&[table_of("notes", vec![plain_field("body")])]);
        let agg = sdl
            .split("type NotesAggregate {")
            .nth(1)
            .and_then(|s| s.split('}').next())
            .unwrap_or_default()
            .to_owned();
        assert!(agg.contains("count("), "{agg}");
        assert!(!agg.contains("sum"), "{agg}");
        assert!(!agg.contains("avg"), "{agg}");
        // Text is comparable, so min/max are there.
        assert!(agg.contains("min:"), "{agg}");
        assert!(!sdl.contains("NotesNumericFields"), "{sdl}");
    }

    #[test]
    fn a_file_field_is_a_path_and_a_url() {
        let sdl = sdl_of(&[table_of(
            "avatars",
            vec![id_field(), file_field("image", "uploads")],
        )]);
        assert!(sdl.contains("image: FileValue"), "{sdl}");
        assert!(sdl.contains("type FileValue {"), "{sdl}");
        assert!(sdl.contains("path: String!"), "{sdl}");
        assert!(sdl.contains("url: String!"), "{sdl}");
    }

    #[test]
    fn comparison_inputs_are_per_scalar_and_text_gets_the_pattern_operators() {
        let sdl = sdl_of(&[table_of("notes", vec![plain_field("body")])]);
        assert!(sdl.contains("input StringComparison {"), "{sdl}");
        assert!(sdl.contains("input BigIntComparison {"), "{sdl}");
        let string_cmp = sdl
            .split("input StringComparison {")
            .nth(1)
            .and_then(|s| s.split('}').next())
            .unwrap_or_default()
            .to_owned();
        for op in [
            "eq", "ne", "gt", "gte", "lt", "lte", "in", "nin", "is_null", "like", "ilike",
        ] {
            assert!(
                string_cmp.contains(op),
                "StringComparison lacks {op}: {string_cmp}"
            );
        }
        // JSON has no ordering, so it has no ordered comparisons.
        let json_cmp = sdl
            .split("input JSONComparison {")
            .nth(1)
            .and_then(|s| s.split('}').next())
            .unwrap_or_default()
            .to_owned();
        assert!(json_cmp.contains("eq"), "{json_cmp}");
        assert!(!json_cmp.contains("gte"), "{json_cmp}");
    }

    #[test]
    fn an_aggregate_is_wired_to_the_read_path_not_to_a_placeholder() {
        // Executed without a request context, `count` fails reaching for the
        // caller it is to be authorized against — which is the proof that it
        // *is* a read of rows, rather than a number this module invented. A
        // plausible zero here would be the exact failure decision 5 forbids.
        let tables = [table_of("departments", vec![id_field()])];
        let names = SchemaNames::derive(&tables);
        let schema = build_schema(&tables, &names).expect("builds");
        let response = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(schema.execute("{ departments_aggregate { count } }"));
        assert!(!response.errors.is_empty(), "{response:?}");
        assert!(
            response.errors[0].message.contains("RequestContext"),
            "{:?}",
            response.errors
        );
    }

    #[test]
    fn introspection_stays_on() {
        // It describes tables the application already exposes over REST, and
        // every browser tool needs it.
        let tables = [table_of("departments", vec![id_field()])];
        let names = SchemaNames::derive(&tables);
        let schema = build_schema(&tables, &names).expect("builds");
        let response = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(schema.execute("{ __schema { queryType { name } } }"));
        assert!(response.errors.is_empty(), "{:?}", response.errors);
    }
}
