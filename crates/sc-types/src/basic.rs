//! Basic types: the MVP type system.
//!
//! A [`BasicType`] is a database type Saltcorn knows how to move a [`Value`]
//! through, but which carries no rich attributes or validation beyond "the value
//! is of the right scalar family" (technical design §6.1). The milestone ships
//! **only** basic types — rich types (typed attributes, dedicated fieldviews)
//! are added incrementally later.
//!
//! Each known variant corresponds one-to-one with a [`Value`] variant, so the
//! driver's `sql_type` string (a Postgres `udt_name` such as `int8`, `text`,
//! `timestamptz`) can be resolved to the [`Value`] family it round-trips as, and
//! back to a canonical SQL type for DDL. A DB type we do not recognise is still
//! *usable* — it becomes [`BasicType::Other`] and flows through the catch-all
//! display/edit path (see [`crate::catchall`]) as text, exactly as the design
//! requires ("a Basic type is any DB type not mapped to a rich type").

use sc_error::{Error, Result};
use sc_query::Value;

/// A database type the MVP understands, as one of a fixed set of scalar families
/// (each mirroring a [`Value`] variant) plus an [`Other`](BasicType::Other)
/// catch-all for unrecognised backend types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BasicType {
    /// Boolean (`bool`) — [`Value::Bool`].
    Bool,
    /// 64-bit signed integer (`int2`/`int4`/`int8`) — [`Value::Int`].
    Int,
    /// IEEE-754 floating point (`float4`/`float8`) — [`Value::Float`].
    Float,
    /// Exact fixed-point decimal (`numeric`) — [`Value::Decimal`].
    Decimal,
    /// UTF-8 text (`text`/`varchar`/`bpchar`) — [`Value::Text`].
    Text,
    /// Raw bytes (`bytea`) — [`Value::Bytes`].
    Bytes,
    /// JSON document (`json`/`jsonb`) — [`Value::Json`].
    Json,
    /// UUID (`uuid`) — [`Value::Uuid`].
    Uuid,
    /// Calendar date (`date`) — [`Value::Date`].
    Date,
    /// Wall-clock time (`time`) — [`Value::Time`].
    Time,
    /// Instant in time (`timestamptz`/`timestamp`) — [`Value::Timestamp`].
    Timestamp,
    /// A backend type not mapped to any of the above. Still usable, but only
    /// through the catch-all display/edit path, where it is treated as text. The
    /// wrapped string is the backend's own type name so it survives round-trips.
    Other(String),
}

impl BasicType {
    /// Resolve a backend SQL type name (a Postgres `udt_name`) to a basic type.
    ///
    /// The match is case-insensitive and covers the common Postgres aliases. Any
    /// type not in the table becomes [`BasicType::Other`] carrying the original
    /// name — never an error, because every column must remain usable.
    pub fn from_sql_type(sql_type: &str) -> BasicType {
        match sql_type.trim().to_ascii_lowercase().as_str() {
            "bool" | "boolean" => BasicType::Bool,
            "int2" | "int4" | "int8" | "smallint" | "integer" | "bigint" | "serial"
            | "bigserial" | "smallserial" => BasicType::Int,
            "float4" | "float8" | "real" | "double precision" => BasicType::Float,
            "numeric" | "decimal" => BasicType::Decimal,
            "text" | "varchar" | "character varying" | "bpchar" | "char" | "character" | "name"
            | "citext" => BasicType::Text,
            "bytea" => BasicType::Bytes,
            "json" | "jsonb" => BasicType::Json,
            "uuid" => BasicType::Uuid,
            "date" => BasicType::Date,
            "time" | "timetz" | "time without time zone" | "time with time zone" => BasicType::Time,
            "timestamp"
            | "timestamptz"
            | "timestamp without time zone"
            | "timestamp with time zone" => BasicType::Timestamp,
            other => BasicType::Other(other.to_owned()),
        }
    }

    /// The canonical Postgres type this maps to for DDL (`apply_schema`).
    ///
    /// Recognised variants collapse their aliases to one preferred name (e.g.
    /// every integer width becomes `int8`); [`Other`](BasicType::Other) yields
    /// the backend name it was constructed from.
    pub fn sql_type(&self) -> &str {
        match self {
            BasicType::Bool => "bool",
            BasicType::Int => "int8",
            BasicType::Float => "float8",
            BasicType::Decimal => "numeric",
            BasicType::Text => "text",
            BasicType::Bytes => "bytea",
            BasicType::Json => "jsonb",
            BasicType::Uuid => "uuid",
            BasicType::Date => "date",
            BasicType::Time => "time",
            BasicType::Timestamp => "timestamptz",
            BasicType::Other(name) => name,
        }
    }

    /// A stable, human-readable name. For recognised variants this equals the
    /// corresponding [`Value::kind`]; [`Other`](BasicType::Other) reports its
    /// backend type name.
    pub fn name(&self) -> &str {
        match self {
            BasicType::Bool => "bool",
            BasicType::Int => "int",
            BasicType::Float => "float",
            BasicType::Decimal => "decimal",
            BasicType::Text => "text",
            BasicType::Bytes => "bytes",
            BasicType::Json => "json",
            BasicType::Uuid => "uuid",
            BasicType::Date => "date",
            BasicType::Time => "time",
            BasicType::Timestamp => "timestamp",
            BasicType::Other(name) => name,
        }
    }

    /// The [`Value::kind`] a non-null value of this type must have, or `None`
    /// for [`Other`](BasicType::Other) (which accepts text only via the
    /// catch-all path but imposes no `Value`-family constraint here).
    pub fn value_kind(&self) -> Option<&'static str> {
        Some(match self {
            BasicType::Bool => "bool",
            BasicType::Int => "int",
            BasicType::Float => "float",
            BasicType::Decimal => "decimal",
            BasicType::Text => "text",
            BasicType::Bytes => "bytes",
            BasicType::Json => "json",
            BasicType::Uuid => "uuid",
            BasicType::Date => "date",
            BasicType::Time => "time",
            BasicType::Timestamp => "timestamp",
            BasicType::Other(_) => return None,
        })
    }

    /// The basic type a [`Value`] belongs to, or `None` for [`Value::Null`]
    /// (which carries no type of its own). This is the reverse of the SQL-type
    /// mapping: it answers "what column type could hold this value".
    pub fn of_value(value: &Value) -> Option<BasicType> {
        Some(match value {
            Value::Null => return None,
            Value::Bool(_) => BasicType::Bool,
            Value::Int(_) => BasicType::Int,
            Value::Float(_) => BasicType::Float,
            Value::Decimal(_) => BasicType::Decimal,
            Value::Text(_) => BasicType::Text,
            Value::Bytes(_) => BasicType::Bytes,
            Value::Json(_) => BasicType::Json,
            Value::Uuid(_) => BasicType::Uuid,
            Value::Date(_) => BasicType::Date,
            Value::Time(_) => BasicType::Time,
            Value::Timestamp(_) => BasicType::Timestamp,
        })
    }

    /// Whether a value is compatible with this type. [`Value::Null`] is always
    /// accepted (nullability is a field-level concern, not a type one); an
    /// [`Other`](BasicType::Other) type accepts only text.
    pub fn accepts(&self, value: &Value) -> bool {
        if value.is_null() {
            return true;
        }
        match self.value_kind() {
            Some(kind) => value.kind() == kind,
            // `Other` imposes no family constraint beyond "not a structured
            // value"; the catch-all path renders/edits it as text.
            None => matches!(value, Value::Text(_)),
        }
    }

    /// Validate that `value` is compatible with this type, returning a
    /// descriptive [`Error::Invalid`] when it is not (principle 5: no silent
    /// coercion).
    pub fn validate(&self, value: &Value) -> Result<()> {
        if self.accepts(value) {
            Ok(())
        } else {
            Err(Error::invalid(format!(
                "value of kind `{}` is not compatible with type `{}`",
                value.kind(),
                self.name()
            )))
        }
    }
}
