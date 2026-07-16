//! Type system: basic types, attributes, validation (layer 3).
//!
//! The MVP milestone ships **only basic types** (technical design §6.1): every
//! database column maps to a [`BasicType`] — one of a fixed set of scalar
//! families mirroring the query layer's [`Value`](sc_query::Value) variants, or
//! an [`Other`](BasicType::Other) catch-all for unrecognised backend types. This
//! crate provides three things fields and the server need:
//!
//! - [`BasicType`] — the `Value`↔Postgres type mapping (resolve a driver
//!   `sql_type` to a value family and back to a canonical DDL type) plus
//!   value/type validation.
//! - [`TypeRef`] — the "rich or basic" type reference a field carries; basic
//!   only for now, modelled as an enum so rich types slot in later.
//! - [`BaseField`]/[`FormField`] — the shape of a field, and a field in a form
//!   (§6.2). `FormField` is **also** how every configurable extension point
//!   declares its settings (§13.3), so the admin UI renders one form for all of
//!   them and knows about none of them.
//! - [`Attrs`] — the JSON bag those settings land in; a `FormField` describes one
//!   entry of it.
//! - [`catchall`] — the reduced display/edit path (value → text, text → value)
//!   that stands in for the full `FieldView` trait until post-MVP.
//!
//! `BaseField` and `Attrs` live here rather than in `sc-catalog`, where they
//! started: neither is a catalog concept, and `FormField` needs both while
//! `sc-types` is layer 3 and cannot depend on layer 4. `DataField` stays in
//! `sc-catalog`, because its `Key`/`File` kinds reference catalog identifiers —
//! that was always the only part that had to. Both are re-exported from
//! `sc-catalog`, so `sc_catalog::{Attrs, BaseField}` still resolve.

mod attrs;
mod basic;
pub mod catchall;
mod field;
mod type_ref;

pub use attrs::Attrs;
pub use basic::BasicType;
pub use field::{BaseField, FormField};
pub use type_ref::TypeRef;

#[cfg(test)]
mod tests {
    use super::*;
    use sc_query::Value;
    use uuid::Uuid;

    #[test]
    fn maps_postgres_udt_names_to_basic_types() {
        // The aliases the driver's introspection actually emits (udt_name).
        assert_eq!(BasicType::from_sql_type("int8"), BasicType::Int);
        assert_eq!(BasicType::from_sql_type("int4"), BasicType::Int);
        assert_eq!(BasicType::from_sql_type("bool"), BasicType::Bool);
        assert_eq!(BasicType::from_sql_type("float8"), BasicType::Float);
        assert_eq!(BasicType::from_sql_type("numeric"), BasicType::Decimal);
        assert_eq!(BasicType::from_sql_type("text"), BasicType::Text);
        assert_eq!(BasicType::from_sql_type("varchar"), BasicType::Text);
        assert_eq!(BasicType::from_sql_type("bytea"), BasicType::Bytes);
        assert_eq!(BasicType::from_sql_type("jsonb"), BasicType::Json);
        assert_eq!(BasicType::from_sql_type("uuid"), BasicType::Uuid);
        assert_eq!(BasicType::from_sql_type("date"), BasicType::Date);
        assert_eq!(BasicType::from_sql_type("time"), BasicType::Time);
        assert_eq!(
            BasicType::from_sql_type("timestamptz"),
            BasicType::Timestamp
        );
    }

    #[test]
    fn resolution_is_case_insensitive_and_covers_friendly_aliases() {
        assert_eq!(BasicType::from_sql_type("BIGINT"), BasicType::Int);
        assert_eq!(
            BasicType::from_sql_type("Double Precision"),
            BasicType::Float
        );
        assert_eq!(
            BasicType::from_sql_type("timestamp with time zone"),
            BasicType::Timestamp
        );
    }

    #[test]
    fn unknown_types_become_other_and_stay_usable() {
        let t = BasicType::from_sql_type("inet");
        assert_eq!(t, BasicType::Other("inet".into()));
        // `Other` preserves its backend name for round-tripping DDL.
        assert_eq!(t.sql_type(), "inet");
        assert_eq!(t.value_kind(), None);
        // It is edited/displayed as text via the catch-all path.
        assert!(t.accepts(&Value::Text("::1".into())));
        assert!(!t.accepts(&Value::Int(1)));
    }

    #[test]
    fn canonical_sql_type_collapses_aliases() {
        // Every integer width the driver may report normalises to one DDL type.
        for udt in ["int2", "int4", "int8", "smallint", "bigint"] {
            assert_eq!(BasicType::from_sql_type(udt).sql_type(), "int8");
        }
        assert_eq!(BasicType::from_sql_type("varchar").sql_type(), "text");
        assert_eq!(BasicType::from_sql_type("json").sql_type(), "jsonb");
    }

    #[test]
    fn value_kind_agrees_with_value_kind_names() {
        // The type's expected kind must match what `Value::kind` reports, so the
        // two halves of the mapping stay in lock-step.
        let cases = [
            (BasicType::Bool, Value::Bool(true)),
            (BasicType::Int, Value::Int(1)),
            (BasicType::Float, Value::Float(1.0)),
            (BasicType::Text, Value::Text("x".into())),
            (BasicType::Uuid, Value::Uuid(Uuid::nil())),
        ];
        for (ty, val) in cases {
            assert_eq!(ty.value_kind(), Some(val.kind()));
            assert_eq!(BasicType::of_value(&val), Some(ty));
        }
    }

    #[test]
    fn null_is_accepted_by_every_type_but_has_no_type_of_its_own() {
        assert!(BasicType::Int.accepts(&Value::Null));
        assert!(BasicType::Text.accepts(&Value::Null));
        assert_eq!(BasicType::of_value(&Value::Null), None);
    }

    #[test]
    fn validate_rejects_mismatched_value_families() {
        assert!(BasicType::Int.validate(&Value::Int(5)).is_ok());
        let err = BasicType::Int
            .validate(&Value::Text("nope".into()))
            .unwrap_err();
        assert!(err.to_string().contains("int"));
    }

    #[test]
    fn type_ref_delegates_to_basic() {
        let t = TypeRef::from_sql_type("int8");
        assert_eq!(t, TypeRef::Basic(BasicType::Int));
        assert_eq!(t.sql_type(), "int8");
        assert_eq!(t.name(), "int");
        assert!(t.validate(&Value::Int(3)).is_ok());
        assert!(t.validate(&Value::Bool(true)).is_err());
        assert_eq!(t.as_basic(), Some(&BasicType::Int));
    }

    #[test]
    fn catchall_round_trips_every_basic_value() {
        use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
        use rust_decimal::Decimal;

        let values = [
            (BasicType::Bool, Value::Bool(true)),
            (BasicType::Int, Value::Int(-42)),
            (BasicType::Float, Value::Float(3.5)),
            (BasicType::Decimal, Value::Decimal(Decimal::new(12345, 2))),
            (BasicType::Text, Value::Text("hello world".into())),
            (BasicType::Bytes, Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef])),
            (
                BasicType::Json,
                Value::Json(serde_json::json!({"a": 1, "b": [true, null]})),
            ),
            (
                BasicType::Uuid,
                Value::Uuid(Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0)),
            ),
            (
                BasicType::Date,
                Value::Date(NaiveDate::from_ymd_opt(2026, 7, 11).expect("valid date")),
            ),
            (
                BasicType::Time,
                Value::Time(NaiveTime::from_hms_opt(13, 30, 5).expect("valid time")),
            ),
            (
                BasicType::Timestamp,
                Value::Timestamp(
                    DateTime::<Utc>::from_timestamp(1_700_000_000, 0).expect("valid ts"),
                ),
            ),
        ];
        for (ty, val) in values {
            let text = catchall::display(&val);
            let back = catchall::parse(&ty, &text).expect("parse back");
            assert_eq!(back, val, "round-trip failed for {}", ty.name());
        }
    }

    #[test]
    fn catchall_empty_input_is_null_and_null_displays_empty() {
        assert_eq!(catchall::display(&Value::Null), "");
        assert_eq!(
            catchall::parse(&BasicType::Int, "").expect("parse empty"),
            Value::Null
        );
        assert_eq!(
            catchall::parse(&BasicType::Text, "   ").expect("parse blank"),
            Value::Null
        );
    }

    #[test]
    fn catchall_reports_parse_errors() {
        let err = catchall::parse(&BasicType::Int, "not-a-number").unwrap_err();
        assert!(err.to_string().contains("int"));
        assert!(catchall::parse(&BasicType::Uuid, "xyz").is_err());
        assert!(catchall::parse(&BasicType::Json, "{bad").is_err());
    }

    #[test]
    fn catchall_accepts_common_boolean_spellings() {
        for s in ["true", "T", "1", "on", "yes"] {
            assert_eq!(
                catchall::parse(&BasicType::Bool, s).expect("parse true"),
                Value::Bool(true)
            );
        }
        for s in ["false", "f", "0", "off", "no"] {
            assert_eq!(
                catchall::parse(&BasicType::Bool, s).expect("parse false"),
                Value::Bool(false)
            );
        }
    }

    #[test]
    fn catchall_preserves_text_whitespace() {
        // Text fields keep exact whitespace; only fully-blank input is Null.
        assert_eq!(
            catchall::parse(&BasicType::Text, " padded ").expect("parse text"),
            Value::Text(" padded ".into())
        );
    }
}
