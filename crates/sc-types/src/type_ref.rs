//! [`TypeRef`]: the type a field carries.
//!
//! The technical design's `BaseField` holds `type_: TypeRef` — "rich or basic"
//! (§6.2). The MVP ships only basic types, so [`TypeRef`] currently has a single
//! [`Basic`](TypeRef::Basic) variant. It is modelled as an enum now, rather than
//! an alias for [`BasicType`], so that a future `Rich(RichTypeRef)` variant can
//! be added without changing every field definition — the layering the design
//! calls for is expressed in the type up front.

use sc_error::Result;
use sc_query::Value;

use crate::BasicType;

/// The type of a field: a [`BasicType`] today, with room for rich types later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    /// A basic type — any DB type, usable through the catch-all fieldviews.
    Basic(BasicType),
}

impl TypeRef {
    /// Resolve a backend SQL type name to a (basic) type reference.
    pub fn from_sql_type(sql_type: &str) -> TypeRef {
        TypeRef::Basic(BasicType::from_sql_type(sql_type))
    }

    /// Borrow the underlying [`BasicType`] when this is a basic type. Always
    /// `Some` in the MVP; kept as an `Option` for forward compatibility with
    /// rich types.
    pub fn as_basic(&self) -> Option<&BasicType> {
        match self {
            TypeRef::Basic(b) => Some(b),
        }
    }

    /// The canonical Postgres type for DDL, delegating to the basic type.
    pub fn sql_type(&self) -> &str {
        match self {
            TypeRef::Basic(b) => b.sql_type(),
        }
    }

    /// A stable, human-readable type name.
    pub fn name(&self) -> &str {
        match self {
            TypeRef::Basic(b) => b.name(),
        }
    }

    /// Validate a value against this type.
    pub fn validate(&self, value: &Value) -> Result<()> {
        match self {
            TypeRef::Basic(b) => b.validate(value),
        }
    }
}

impl From<BasicType> for TypeRef {
    fn from(b: BasicType) -> Self {
        TypeRef::Basic(b)
    }
}
