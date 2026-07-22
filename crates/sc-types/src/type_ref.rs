//! [`TypeRef`]: the type a field carries.
//!
//! The technical design's `BaseField` holds `type_: TypeRef` — "rich or basic"
//! (§6.2). A [`Basic`](TypeRef::Basic) type is any DB type, usable through the
//! catch-all fieldviews; a [`Rich`](TypeRef::Rich) type is one Saltcorn
//! understands, with typed attributes and validation (§6.1, [`crate::rich`]).
//!
//! **A rich type is never guessed from the database.** [`from_sql_type`] always
//! returns a basic type: a column is rich only because the `_sc_fields` overlay
//! says so (§3.2). Inferring "this `text` column is an `Email`" from the schema
//! is exactly the kind of magic that makes a legacy database behave surprisingly,
//! so it is deliberately not done — the mapping runs the other way, from an
//! overlay's stored type name to a [`RichTypeRef`] via
//! [`RichTypeRef::resolve`](crate::RichTypeRef::resolve).

use sc_error::Result;
use sc_query::Value;

use crate::{Attrs, BasicType, RichTypeRef};

/// The type of a field: a [`BasicType`], or a rich type ([`RichTypeRef`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    /// A basic type — any DB type, usable through the catch-all fieldviews.
    Basic(BasicType),
    /// A rich type — one with typed attributes and validation (§6.1). A field is
    /// rich only because the overlay says so; introspection never produces this.
    Rich(RichTypeRef),
}

impl TypeRef {
    /// Resolve a backend SQL type name to a **basic** type reference.
    ///
    /// Always basic: introspection never yields a rich type (see the module
    /// docs). A column becomes rich only when the `_sc_fields` overlay names a
    /// rich type for it (§3.2), which resolves through
    /// [`RichTypeRef::resolve`](crate::RichTypeRef::resolve), not here.
    pub fn from_sql_type(sql_type: &str) -> TypeRef {
        TypeRef::Basic(BasicType::from_sql_type(sql_type))
    }

    /// A rich type reference as a [`TypeRef`].
    pub fn rich(rich: RichTypeRef) -> TypeRef {
        TypeRef::Rich(rich)
    }

    /// Borrow the underlying [`BasicType`] when this is a basic type, or `None`
    /// for a rich type.
    pub fn as_basic(&self) -> Option<&BasicType> {
        match self {
            TypeRef::Basic(b) => Some(b),
            TypeRef::Rich(_) => None,
        }
    }

    /// Borrow the [`RichTypeRef`] when this is a rich type, or `None` for a basic
    /// one.
    pub fn as_rich(&self) -> Option<&RichTypeRef> {
        match self {
            TypeRef::Rich(r) => Some(r),
            TypeRef::Basic(_) => None,
        }
    }

    /// The canonical Postgres type for DDL, delegating to the underlying type. A
    /// rich type emits the first of its `sql_types()`.
    pub fn sql_type(&self) -> &str {
        match self {
            TypeRef::Basic(b) => b.sql_type(),
            TypeRef::Rich(r) => r.sql_type(),
        }
    }

    /// A stable, human-readable type name.
    pub fn name(&self) -> &str {
        match self {
            TypeRef::Basic(b) => b.name(),
            TypeRef::Rich(r) => r.name(),
        }
    }

    /// Validate a value against this type.
    ///
    /// The rich path also depends on a field's attributes (`max_length`, a
    /// numeric range); this shorthand validates against **no** attributes, which
    /// is the right answer for a basic type and the lenient one for a rich type.
    /// The write path uses [`validate_with`](TypeRef::validate_with) with the
    /// field's real attributes (§2.3).
    pub fn validate(&self, value: &Value) -> Result<()> {
        self.validate_with(value, &Attrs::new())
    }

    /// Validate a value against this type **and** a field's attributes — the form
    /// the write path uses (§2.3). A basic type ignores the attributes; a rich
    /// type validates against them.
    pub fn validate_with(&self, value: &Value, attrs: &Attrs) -> Result<()> {
        match self {
            TypeRef::Basic(b) => b.validate(value),
            TypeRef::Rich(r) => r.validate(value, attrs),
        }
    }
}

impl From<BasicType> for TypeRef {
    fn from(b: BasicType) -> Self {
        TypeRef::Basic(b)
    }
}

impl From<RichTypeRef> for TypeRef {
    fn from(r: RichTypeRef) -> Self {
        TypeRef::Rich(r)
    }
}
