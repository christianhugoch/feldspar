//! The type schema describing endpoint inputs and outputs (design §13.1).
//!
//! [`TypeSchema`] is the reified, serializable description of the shape of an
//! endpoint's request body and response value. It is deliberately small — just
//! enough to describe arguments and results and to emit TypeScript types — and
//! mirrors the "data, not fluent calls" philosophy of the query layer: a schema
//! is a plain value that can be inspected, stored, or turned into a TypeScript
//! declaration by the generator in [`crate::typescript`].
//!
//! The leaf of the schema is [`ValueType`], the scalar type set. It mirrors the
//! non-null variants of the query layer's `Value`, so a schema built from the
//! data layer's columns lines up one-to-one with the values that actually flow
//! over the wire.

use serde::{Deserialize, Serialize};

/// A scalar type: the leaf of a [`TypeSchema`]. The variants mirror the non-null
/// families of the query layer's `Value`, so every column/argument type maps to
/// exactly one of these and to exactly one TypeScript type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    /// Boolean — TS `boolean`.
    Bool,
    /// 64-bit signed integer — TS `number`.
    Int,
    /// IEEE-754 floating point — TS `number`.
    Float,
    /// Exact fixed-point decimal — TS `string` (arbitrary precision survives a
    /// string round-trip that a JS `number` would silently corrupt).
    Decimal,
    /// UTF-8 text — TS `string`.
    Text,
    /// Raw bytes, carried as base64 — TS `string`.
    Bytes,
    /// Arbitrary embedded JSON — TS `unknown`.
    Json,
    /// UUID, carried as its canonical string form — TS `string`.
    Uuid,
    /// Calendar date (ISO 8601) — TS `string`.
    Date,
    /// Wall-clock time (ISO 8601) — TS `string`.
    Time,
    /// Instant in time (RFC 3339) — TS `string`.
    Timestamp,
}

impl ValueType {
    /// The TypeScript type this scalar serializes as. See the per-variant docs
    /// for why non-string scalars (decimal, bytes) are carried as strings.
    pub fn ts_type(self) -> &'static str {
        match self {
            ValueType::Bool => "boolean",
            ValueType::Int | ValueType::Float => "number",
            ValueType::Json => "unknown",
            // Everything else is carried as a JSON string.
            ValueType::Decimal
            | ValueType::Text
            | ValueType::Bytes
            | ValueType::Uuid
            | ValueType::Date
            | ValueType::Time
            | ValueType::Timestamp => "string",
        }
    }
}

/// A named member of a [`TypeSchema::Struct`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructField {
    /// The field name — used verbatim as the JSON key and the TS property name.
    pub name: String,
    /// The field's type.
    pub schema: TypeSchema,
}

impl StructField {
    /// Convenience constructor.
    pub fn new(name: impl Into<String>, schema: TypeSchema) -> StructField {
        StructField {
            name: name.into(),
            schema,
        }
    }
}

/// The shape of an endpoint's input or output value (design §13.1).
///
/// This is enough to describe arguments and results and to emit TypeScript
/// types: a scalar, a record of named fields, a homogeneous array, or an
/// optional (nullable) wrapper. Richer shapes (unions, maps) are added when a
/// concrete endpoint needs them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeSchema {
    /// A scalar leaf.
    Value(ValueType),
    /// A record of named fields. An empty struct is the canonical "no value"
    /// shape (see [`TypeSchema::empty`]).
    Struct(Vec<StructField>),
    /// A homogeneous array.
    Array(Box<TypeSchema>),
    /// An optional value: present-or-`null`.
    Optional(Box<TypeSchema>),
}

impl TypeSchema {
    /// A scalar schema.
    pub fn value(t: ValueType) -> TypeSchema {
        TypeSchema::Value(t)
    }

    /// A struct schema from an iterator of fields.
    pub fn struct_of(fields: impl IntoIterator<Item = StructField>) -> TypeSchema {
        TypeSchema::Struct(fields.into_iter().collect())
    }

    /// An array of `inner`.
    pub fn array(inner: TypeSchema) -> TypeSchema {
        TypeSchema::Array(Box::new(inner))
    }

    /// An optional (nullable) `inner`.
    pub fn optional(inner: TypeSchema) -> TypeSchema {
        TypeSchema::Optional(Box::new(inner))
    }

    /// The canonical "no value" schema — an empty struct. Used for endpoints
    /// with no request body or no meaningful response payload.
    pub fn empty() -> TypeSchema {
        TypeSchema::Struct(Vec::new())
    }

    /// Whether this schema carries no value (an empty [`Struct`](TypeSchema::Struct)).
    pub fn is_empty(&self) -> bool {
        matches!(self, TypeSchema::Struct(fields) if fields.is_empty())
    }

    // --- scalar shortcuts, for readable endpoint definitions ----------------

    /// Shortcut for `Value(Text)`.
    pub fn text() -> TypeSchema {
        TypeSchema::Value(ValueType::Text)
    }

    /// Shortcut for `Value(Uuid)`.
    pub fn uuid() -> TypeSchema {
        TypeSchema::Value(ValueType::Uuid)
    }

    /// Shortcut for `Value(Int)`.
    pub fn int() -> TypeSchema {
        TypeSchema::Value(ValueType::Int)
    }

    /// Shortcut for `Value(Bool)`.
    pub fn bool() -> TypeSchema {
        TypeSchema::Value(ValueType::Bool)
    }

    /// Shortcut for `Value(Json)`.
    pub fn json() -> TypeSchema {
        TypeSchema::Value(ValueType::Json)
    }
}
