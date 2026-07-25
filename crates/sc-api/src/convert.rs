//! Converting between JSON (the wire shape of the typed API) and the query
//! layer's [`Value`] (technical design §13.2, §4).
//!
//! Row endpoints exchange plain JSON objects, but the query layer speaks
//! [`Value`]. These two helpers bridge them: [`json_to_value`] coerces an
//! incoming JSON scalar to the [`Value`] variant a column's [`BasicType`] calls
//! for (so a `timestamptz` column receives a real timestamp, not a string), and
//! [`value_to_json`] renders a value read back from the database as natural JSON
//! (not the tagged `{"type":…,"value":…}` form `Value`'s own `Serialize` emits).

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use sc_error::{Error, Result};
use sc_query::Value;
use sc_types::BasicType;
use serde_json::Value as Json;
use std::str::FromStr;
use uuid::Uuid;

/// Render a [`Value`] read from the database as natural JSON for an API response.
///
/// Temporal and decimal values become strings (ISO-8601 / plain decimal) so no
/// precision is lost; `Bytes` becomes an array of byte values; `Json` passes
/// through unchanged.
pub fn value_to_json(value: &Value) -> Json {
    match value {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) => Json::from(*i),
        Value::Float(f) => serde_json::Number::from_f64(*f).map_or(Json::Null, Json::Number),
        Value::Text(s) => Json::String(s.clone()),
        Value::Bytes(b) => Json::Array(b.iter().map(|byte| Json::from(*byte)).collect()),
        Value::Json(j) => j.clone(),
        Value::Uuid(u) => Json::String(u.to_string()),
        Value::Date(d) => Json::String(d.to_string()),
        Value::Time(t) => Json::String(t.to_string()),
        Value::Timestamp(ts) => Json::String(ts.to_rfc3339()),
        Value::Decimal(d) => Json::String(d.to_string()),
    }
}

/// Read a JSON value as the [`Value`] its own shape implies, with no column to
/// coerce it towards — the inverse of [`value_to_json`] as far as JSON can
/// express it (a timestamp that became a string comes back as text).
///
/// This is for the values that arrive as JSON with **no type declaration behind
/// them**: an event's row for a table the catalog no longer has, a field of a
/// caller object that is not a users-table column. Where a column *is* known,
/// [`json_to_value`] is the right function — it recovers the uuid, the timestamp
/// and the decimal that this one leaves as text, which is what lets a formula's
/// `user.id` compare against a `uuid` column in SQL.
pub fn json_to_natural_value(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Value::Int(i),
            (None, Some(f)) => Value::Float(f),
            // A number that is neither an i64 nor an f64 cannot occur in
            // `serde_json`; carrying it as text loses nothing that was there.
            (None, None) => Value::Text(n.to_string()),
        },
        Json::String(s) => Value::Text(s.clone()),
        // A composite has one faithful `Value`, and it is the JSON itself.
        Json::Array(_) | Json::Object(_) => Value::Json(json.clone()),
    }
}

/// Coerce an incoming JSON scalar to the [`Value`] variant a column of the given
/// [`BasicType`] expects.
///
/// JSON `null` always maps to [`Value::Null`]. Otherwise the target type drives
/// the parse: a string destined for a `uuid`/`date`/`timestamptz`/`numeric`
/// column is parsed into the corresponding value (an [`Error::invalid`] if it is
/// malformed), while a JSON object/array for a `json` column is embedded
/// verbatim. Types without a stricter target fall back to the JSON scalar's
/// natural mapping.
pub fn json_to_value(basic: &BasicType, json: &Json) -> Result<Value> {
    if json.is_null() {
        return Ok(Value::Null);
    }
    match basic {
        BasicType::Bool => match json {
            Json::Bool(b) => Ok(Value::Bool(*b)),
            _ => Err(type_error("bool", json)),
        },
        BasicType::Int => match json {
            Json::Number(n) => n
                .as_i64()
                .map(Value::Int)
                .ok_or_else(|| type_error("int", json)),
            Json::String(s) => s
                .parse::<i64>()
                .map(Value::Int)
                .map_err(|_| type_error("int", json)),
            _ => Err(type_error("int", json)),
        },
        BasicType::Float => match json {
            Json::Number(n) => n
                .as_f64()
                .map(Value::Float)
                .ok_or_else(|| type_error("float", json)),
            Json::String(s) => s
                .parse::<f64>()
                .map(Value::Float)
                .map_err(|_| type_error("float", json)),
            _ => Err(type_error("float", json)),
        },
        BasicType::Decimal => parse_str(json, "decimal", |s| {
            Decimal::from_str(s).map(Value::Decimal).ok()
        }),
        BasicType::Uuid => parse_str(json, "uuid", |s| Uuid::parse_str(s).map(Value::Uuid).ok()),
        BasicType::Date => parse_str(json, "date", |s| {
            NaiveDate::from_str(s).map(Value::Date).ok()
        }),
        BasicType::Time => parse_str(json, "time", |s| {
            NaiveTime::from_str(s).map(Value::Time).ok()
        }),
        BasicType::Timestamp => parse_str(json, "timestamp", |s| {
            DateTime::parse_from_rfc3339(s)
                .map(|dt| Value::Timestamp(dt.with_timezone(&Utc)))
                .ok()
        }),
        BasicType::Json => Ok(Value::Json(json.clone())),
        BasicType::Bytes => match json {
            Json::Array(items) => {
                let mut bytes = Vec::with_capacity(items.len());
                for item in items {
                    let byte = item
                        .as_u64()
                        .and_then(|n| u8::try_from(n).ok())
                        .ok_or_else(|| type_error("bytes", json))?;
                    bytes.push(byte);
                }
                Ok(Value::Bytes(bytes))
            }
            _ => Err(type_error("bytes", json)),
        },
        BasicType::Text | BasicType::Other(_) => match json {
            Json::String(s) => Ok(Value::Text(s.clone())),
            // A non-string for a text column is stringified rather than rejected,
            // matching the catch-all fieldview's lenient display path.
            other => Ok(Value::Text(other.to_string())),
        },
    }
}

/// Apply a string parser to a JSON string, erroring for a non-string or a parse
/// failure.
fn parse_str(json: &Json, ty: &str, parse: impl Fn(&str) -> Option<Value>) -> Result<Value> {
    match json {
        Json::String(s) => parse(s).ok_or_else(|| type_error(ty, json)),
        _ => Err(type_error(ty, json)),
    }
}

/// An [`Error::invalid`] describing a value that does not fit its column type.
fn type_error(ty: &str, json: &Json) -> Error {
    Error::invalid(format!("value {json} is not a valid {ty}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_scalar_types() {
        let uuid = Uuid::new_v4();
        let cases = [
            (BasicType::Bool, Json::Bool(true), Value::Bool(true)),
            (BasicType::Int, Json::from(7_i64), Value::Int(7)),
            (
                BasicType::Text,
                Json::String("hi".into()),
                Value::Text("hi".into()),
            ),
            (
                BasicType::Uuid,
                Json::String(uuid.to_string()),
                Value::Uuid(uuid),
            ),
        ];
        for (ty, json, expected) in cases {
            let value = json_to_value(&ty, &json).unwrap();
            assert_eq!(value, expected);
            assert_eq!(value_to_json(&value), json);
        }
    }

    #[test]
    fn null_maps_regardless_of_type() {
        assert_eq!(
            json_to_value(&BasicType::Int, &Json::Null).unwrap(),
            Value::Null
        );
        assert_eq!(value_to_json(&Value::Null), Json::Null);
    }

    #[test]
    fn rejects_malformed_typed_string() {
        assert!(json_to_value(&BasicType::Uuid, &Json::String("nope".into())).is_err());
        assert!(json_to_value(&BasicType::Int, &Json::String("x".into())).is_err());
    }

    #[test]
    fn a_typeless_json_value_reads_as_its_own_shape() {
        assert_eq!(json_to_natural_value(&Json::Null), Value::Null);
        assert_eq!(
            json_to_natural_value(&Json::Bool(false)),
            Value::Bool(false)
        );
        assert_eq!(json_to_natural_value(&Json::from(4_i64)), Value::Int(4));
        assert_eq!(
            json_to_natural_value(&Json::from(1.5_f64)),
            Value::Float(1.5)
        );
        assert_eq!(
            json_to_natural_value(&Json::String("x".into())),
            Value::Text("x".into())
        );
        // A composite stays composite rather than being stringified.
        let obj = serde_json::json!({ "a": 1 });
        assert_eq!(json_to_natural_value(&obj), Value::Json(obj.clone()));
        // And the round trip back to JSON is the identity for every one of them.
        for json in [Json::Null, Json::Bool(true), Json::from(2_i64), obj] {
            assert_eq!(value_to_json(&json_to_natural_value(&json)), json);
        }
    }

    #[test]
    fn text_column_stringifies_non_strings() {
        let v = json_to_value(&BasicType::Text, &Json::from(3_i64)).unwrap();
        assert_eq!(v, Value::Text("3".into()));
    }
}
