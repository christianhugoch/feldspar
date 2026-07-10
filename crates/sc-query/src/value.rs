//! The universal row value type.
//!
//! [`Value`] is the single scalar type that flows through the whole query
//! layer: it is what a [`crate::Statement`] carries as a literal, what a driver
//! binds as a parameter, and what a [`RowStream`](crate) yields back. It is a
//! plain, serializable data value so that a statement can be inspected, cached,
//! or reconstructed by any code adapter (technical design §4).
//!
//! JSON is a first-class member ([`Value::Json`]), not an add-on type, and
//! literals of every variant are always parameterised on render, which is the
//! query-layer half of the injection-safety story.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single scalar value: the row value type used everywhere in the query
/// layer.
///
/// The variants mirror the physical column kinds the MVP supports. `f64` has no
/// total order or equality, so `Value` is deliberately only `PartialEq` (not
/// `Eq`/`Hash`); compare with that in mind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    /// SQL `NULL` / the absence of a value.
    Null,
    /// Boolean.
    Bool(bool),
    /// 64-bit signed integer.
    Int(i64),
    /// 64-bit IEEE-754 floating point.
    Float(f64),
    /// UTF-8 text.
    Text(String),
    /// Raw bytes (`bytea`).
    Bytes(Vec<u8>),
    /// Embedded JSON document — a first-class value, not an add-on type.
    Json(serde_json::Value),
    /// UUID (the primary-key type for bootstrap tables such as `users`).
    Uuid(Uuid),
    /// Calendar date with no time or zone.
    Date(NaiveDate),
    /// Wall-clock time of day with no date or zone.
    Time(NaiveTime),
    /// Instant in time, stored in UTC (maps to `timestamptz`).
    Timestamp(DateTime<Utc>),
    /// Exact fixed-point decimal (maps to `numeric`).
    Decimal(Decimal),
}

impl Value {
    /// Returns `true` if this is [`Value::Null`].
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The variant's stable, human-readable kind name (matches the serde tag).
    /// Useful for error messages and diagnostics.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Text(_) => "text",
            Value::Bytes(_) => "bytes",
            Value::Json(_) => "json",
            Value::Uuid(_) => "uuid",
            Value::Date(_) => "date",
            Value::Time(_) => "time",
            Value::Timestamp(_) => "timestamp",
            Value::Decimal(_) => "decimal",
        }
    }

    /// Borrow the inner string if this is [`Value::Text`].
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Return the inner integer if this is [`Value::Int`].
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Return the inner boolean if this is [`Value::Bool`].
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// A missing value maps to `NULL`.
impl<T> From<Option<T>> for Value
where
    Value: From<T>,
{
    fn from(opt: Option<T>) -> Self {
        match opt {
            Some(v) => Value::from(v),
            None => Value::Null,
        }
    }
}

macro_rules! impl_from {
    ($ty:ty, $variant:ident) => {
        impl From<$ty> for Value {
            fn from(v: $ty) -> Self {
                Value::$variant(v)
            }
        }
    };
}

impl_from!(bool, Bool);
impl_from!(i64, Int);
impl_from!(f64, Float);
impl_from!(String, Text);
impl_from!(Vec<u8>, Bytes);
impl_from!(serde_json::Value, Json);
impl_from!(Uuid, Uuid);
impl_from!(NaiveDate, Date);
impl_from!(NaiveTime, Time);
impl_from!(DateTime<Utc>, Timestamp);
impl_from!(Decimal, Decimal);

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_owned())
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Int(v.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_and_kind() {
        assert!(Value::Null.is_null());
        assert!(!Value::Int(1).is_null());
        assert_eq!(Value::Null.kind(), "null");
        assert_eq!(Value::Decimal(Decimal::new(1, 0)).kind(), "decimal");
        assert_eq!(Value::Json(serde_json::json!({})).kind(), "json");
    }

    #[test]
    fn from_conversions() {
        assert_eq!(Value::from(true), Value::Bool(true));
        assert_eq!(Value::from(7_i64), Value::Int(7));
        assert_eq!(Value::from(7_i32), Value::Int(7));
        assert_eq!(Value::from("hi"), Value::Text("hi".into()));
        assert_eq!(Value::from(String::from("hi")), Value::Text("hi".into()));
        assert_eq!(Value::from(vec![1u8, 2, 3]), Value::Bytes(vec![1, 2, 3]));
    }

    #[test]
    fn option_maps_to_null() {
        let some: Value = Some(3_i64).into();
        let none: Value = Option::<i64>::None.into();
        assert_eq!(some, Value::Int(3));
        assert_eq!(none, Value::Null);
    }

    #[test]
    fn accessors() {
        assert_eq!(Value::Text("x".into()).as_text(), Some("x"));
        assert_eq!(Value::Int(1).as_text(), None);
        assert_eq!(Value::Int(9).as_int(), Some(9));
        assert_eq!(Value::Bool(true).as_bool(), Some(true));
    }

    #[test]
    fn rich_type_roundtrips() {
        let uuid = Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0);
        let date = NaiveDate::from_ymd_opt(2026, 7, 10).expect("valid date");
        let time = NaiveTime::from_hms_opt(13, 30, 0).expect("valid time");
        let ts = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).expect("valid ts");
        let dec = Decimal::new(12345, 2); // 123.45

        for v in [
            Value::Uuid(uuid),
            Value::Date(date),
            Value::Time(time),
            Value::Timestamp(ts),
            Value::Decimal(dec),
        ] {
            let json = serde_json::to_string(&v).expect("serialize");
            let back: Value = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(v, back);
        }
    }

    #[test]
    fn serde_tag_shape() {
        // The externally tagged shape is stable and inspectable by code adapters.
        let v = Value::Int(42);
        let json = serde_json::to_value(&v).expect("serialize");
        assert_eq!(json, serde_json::json!({ "type": "int", "value": 42 }));

        let null = serde_json::to_value(Value::Null).expect("serialize");
        assert_eq!(null, serde_json::json!({ "type": "null" }));
    }
}
