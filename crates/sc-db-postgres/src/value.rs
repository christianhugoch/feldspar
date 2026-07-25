//! Wire-level mapping between the query layer's [`Value`] and tokio-postgres.
//!
//! Two directions, both **basic types only** for this milestone (rich types and
//! their fieldviews are a later phase):
//!
//! - **Bind → parameter**: [`PgParam`] wraps a `&Value` and implements
//!   [`ToSql`], so the ordered binds produced by rendering a statement can be
//!   passed straight to `Client::query`. Every literal already left the SQL as a
//!   placeholder (the query layer's injection guarantee); this just encodes the
//!   value the placeholder points at. A [`Value::Null`] encodes as SQL `NULL`
//!   for *any* column type — unlike `Option::<T>::None`, which only accepts one.
//! - **Result column → [`Value`]**: [`decode`] reads one column of a
//!   [`tokio_postgres::Row`] according to its Postgres type, handling `NULL`.

use bytes::BytesMut;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use rust_decimal::Decimal;
use sc_error::{Error, Result};
use sc_query::Value;
use tokio_postgres::Row as PgRow;
use tokio_postgres::types::{FromSql, IsNull, ToSql, Type};
use uuid::Uuid;

/// The boxed error type tokio-postgres' `ToSql` methods return.
type BoxError = Box<dyn std::error::Error + Sync + Send>;

/// A [`Value`] borrowed for use as a SQL bind parameter.
///
/// Encoding dispatches on the variant to the underlying type's own `ToSql`, so
/// each non-null value is type-checked against the target column exactly as that
/// type would be. `Null` short-circuits to a typeless SQL `NULL`.
#[derive(Debug)]
pub struct PgParam<'a>(pub &'a Value);

impl PgParam<'_> {
    fn encode(&self, ty: &Type, out: &mut BytesMut) -> std::result::Result<IsNull, BoxError> {
        match self.0 {
            // A NULL is valid for any column type, so bypass the per-type
            // `accepts` gate that the concrete `ToSql` impls apply.
            Value::Null => Ok(IsNull::Yes),
            Value::Bool(v) => v.to_sql_checked(ty, out),
            // An integer literal reaches a placeholder whose inferred type is
            // whatever the surrounding expression wants: it may be a narrower
            // integer (`int2`/`int4`) or `numeric` — the latter is common when a
            // formula compares against an aggregate (`sum(x) > 100`), since
            // `sum` widens to `numeric`. `i64::to_sql` only accepts `int8`, so
            // coerce to the target rather than error the whole query.
            Value::Int(v) => match *ty {
                Type::INT2 => (*v as i16).to_sql_checked(ty, out),
                Type::INT4 => (*v as i32).to_sql_checked(ty, out),
                Type::NUMERIC => Decimal::from(*v).to_sql_checked(ty, out),
                _ => v.to_sql_checked(ty, out),
            },
            Value::Float(v) => match *ty {
                Type::NUMERIC => Decimal::try_from(*v)
                    .map_err(BoxError::from)
                    .and_then(|d| d.to_sql_checked(ty, out)),
                _ => v.to_sql_checked(ty, out),
            },
            Value::Text(v) => v.to_sql_checked(ty, out),
            Value::Bytes(v) => v.to_sql_checked(ty, out),
            Value::Json(v) => v.to_sql_checked(ty, out),
            Value::Uuid(v) => v.to_sql_checked(ty, out),
            Value::Date(v) => v.to_sql_checked(ty, out),
            Value::Time(v) => v.to_sql_checked(ty, out),
            Value::Timestamp(v) => v.to_sql_checked(ty, out),
            Value::Decimal(v) => v.to_sql_checked(ty, out),
        }
    }
}

impl ToSql for PgParam<'_> {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> std::result::Result<IsNull, BoxError>
    where
        Self: Sized,
    {
        self.encode(ty, out)
    }

    // We override `to_sql_checked` to do our own per-variant dispatch, so this
    // gate is unused; accept everything and let `encode` decide.
    fn accepts(_ty: &Type) -> bool
    where
        Self: Sized,
    {
        true
    }

    fn to_sql_checked(
        &self,
        ty: &Type,
        out: &mut BytesMut,
    ) -> std::result::Result<IsNull, BoxError> {
        self.encode(ty, out)
    }
}

/// Read one column of `row` as a [`Value`], mapping the Postgres type to a basic
/// `Value` variant and turning SQL `NULL` into [`Value::Null`].
///
/// Unmapped types are an explicit error rather than a silent coercion — this
/// milestone deliberately covers only the basic types.
pub fn decode(row: &PgRow, idx: usize) -> Result<Value> {
    let column = row
        .columns()
        .get(idx)
        .ok_or_else(|| Error::database(format!("no column at index {idx}")))?;
    let ty = column.type_();

    let value = if *ty == Type::BOOL {
        get::<Option<bool>>(row, idx)?.map_or(Value::Null, Value::Bool)
    } else if *ty == Type::INT2 {
        get::<Option<i16>>(row, idx)?.map_or(Value::Null, |n| Value::Int(n as i64))
    } else if *ty == Type::INT4 {
        get::<Option<i32>>(row, idx)?.map_or(Value::Null, |n| Value::Int(n as i64))
    } else if *ty == Type::INT8 {
        get::<Option<i64>>(row, idx)?.map_or(Value::Null, Value::Int)
    } else if *ty == Type::FLOAT4 {
        get::<Option<f32>>(row, idx)?.map_or(Value::Null, |n| Value::Float(n as f64))
    } else if *ty == Type::FLOAT8 {
        get::<Option<f64>>(row, idx)?.map_or(Value::Null, Value::Float)
    } else if *ty == Type::TEXT || *ty == Type::VARCHAR || *ty == Type::BPCHAR || *ty == Type::NAME
    {
        get::<Option<String>>(row, idx)?.map_or(Value::Null, Value::Text)
    } else if *ty == Type::BYTEA {
        get::<Option<Vec<u8>>>(row, idx)?.map_or(Value::Null, Value::Bytes)
    } else if *ty == Type::UUID {
        get::<Option<Uuid>>(row, idx)?.map_or(Value::Null, Value::Uuid)
    } else if *ty == Type::JSON || *ty == Type::JSONB {
        get::<Option<serde_json::Value>>(row, idx)?.map_or(Value::Null, Value::Json)
    } else if *ty == Type::DATE {
        get::<Option<NaiveDate>>(row, idx)?.map_or(Value::Null, Value::Date)
    } else if *ty == Type::TIME {
        get::<Option<NaiveTime>>(row, idx)?.map_or(Value::Null, Value::Time)
    } else if *ty == Type::TIMESTAMPTZ {
        get::<Option<DateTime<Utc>>>(row, idx)?.map_or(Value::Null, Value::Timestamp)
    } else if *ty == Type::TIMESTAMP {
        // No zone in the column; interpret the naive timestamp as UTC.
        get::<Option<NaiveDateTime>>(row, idx)?.map_or(Value::Null, |n| {
            Value::Timestamp(DateTime::<Utc>::from_naive_utc_and_offset(n, Utc))
        })
    } else if *ty == Type::NUMERIC {
        get::<Option<Decimal>>(row, idx)?.map_or(Value::Null, Value::Decimal)
    } else {
        return Err(Error::database(format!(
            "unsupported column type `{ty}` for column `{}` (basic types only this milestone)",
            column.name()
        )));
    };
    Ok(value)
}

/// Read a typed value from a column, converting the driver error into ours.
fn get<'a, T: FromSql<'a>>(row: &'a PgRow, idx: usize) -> Result<T> {
    row.try_get::<usize, T>(idx)
        .map_err(|e| Error::database(format!("decode column {idx}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An integer literal whose placeholder Postgres infers as `numeric`
    /// (comparing against `sum(…)`, say) must encode, not error — the whole
    /// point of the per-target coercion.
    #[test]
    fn an_int_encodes_into_a_numeric_placeholder() {
        let mut out = BytesMut::new();
        // Plain `i64::to_sql` refuses NUMERIC; `PgParam` coerces to Decimal.
        assert!(
            PgParam(&Value::Int(100))
                .to_sql_checked(&Type::NUMERIC, &mut out)
                .is_ok()
        );
        assert!(
            PgParam(&Value::Int(7))
                .to_sql_checked(&Type::INT4, &mut out)
                .is_ok()
        );
        assert!(
            PgParam(&Value::Int(7))
                .to_sql_checked(&Type::INT2, &mut out)
                .is_ok()
        );
        assert!(
            PgParam(&Value::Float(2.5))
                .to_sql_checked(&Type::NUMERIC, &mut out)
                .is_ok()
        );
        // The int8 path still works.
        assert!(
            PgParam(&Value::Int(9))
                .to_sql_checked(&Type::INT8, &mut out)
                .is_ok()
        );
    }
}
