//! [`BaseField`] and [`FormField`]: the shape of a field, and a field in a form
//! (design §6.2).
//!
//! GOALS calls out that v1 conflated database fields and form fields, so v2
//! separates them: [`BaseField`] is what they share (name, label, type,
//! type-specific attributes), `DataField` (in `sc-catalog`, because its `Key`
//! kind references catalog identifiers) adds the column constraints, and
//! [`FormField`] adds what it takes to render and validate an input control.
//!
//! **A [`FormField`] is also how every configurable extension point declares its
//! settings.** A `Framework` (§13.3), `Action` (§10.1), `Agent` (§11.1),
//! `ModelProvider` (§14.2), `FieldView` (§6.3) and a `RichType`'s attributes
//! (§6.1) all answer the same question — *what should the admin be asked?* — and
//! all answer it with `Vec<FormField>`. That is the whole point of declaring
//! settings as data: the admin UI renders a form for whichever extension the
//! admin picked without a per-extension special case, and an extension supplied
//! by a guest language through `sc-code` is configured the same way as a built-in
//! one. §6.2 already says a form field "may derive from a `DataField` **or be
//! standalone**" — a setting is exactly the standalone case, so it needs no type
//! of its own.
//!
//! The values a `FormField` describes live in an [`Attrs`] bag, are stored in a
//! JSON column, and reach the admin UI as JSON, so [`default`](FormField::default)
//! and [`options`](FormField::options) are [`serde_json::Value`]s — the same
//! thing that ends up in the bag, not [`Value`](sc_query::Value)'s tagged
//! encoding.

use serde_json::Value as Json;

use crate::{Attrs, TypeRef};

/// Properties shared by every field: its identifier name, human label, type, and
/// type-specific attributes (design §6.2).
#[derive(Debug, Clone, PartialEq)]
pub struct BaseField {
    /// A valid identifier in SQL and every guest language.
    pub name: String,
    /// Human-facing string; defaults to the name.
    pub label: String,
    /// The field's type — basic in the MVP (rich types post-MVP).
    pub type_: TypeRef,
    /// Type-specific attributes (JSON object) — e.g. a rich type's min/max. What
    /// may go in here is itself described by that type's `attributes()`
    /// (§6.1), as a `Vec<FormField>`.
    pub attributes: Attrs,
}

impl BaseField {
    /// A base field with the given name and type; the label defaults to the name
    /// and there are no attributes.
    pub fn new(name: impl Into<String>, type_: TypeRef) -> BaseField {
        let name = name.into();
        BaseField {
            label: name.clone(),
            name,
            type_,
            attributes: Attrs::new(),
        }
    }
}

/// A field in a form: enough to render an input control for it, and to check
/// what comes back (design §6.2).
///
/// Either derived from a `DataField` (editing a row) or **standalone** — which is
/// what a configurable extension's settings are. See the module docs for why
/// settings do not get a type of their own.
///
/// ```
/// use sc_types::{BasicType, FormField};
///
/// let spec = FormField::new("source", BasicType::Text)
///     .label("Source directory")
///     .required()
///     .default_value("web");
/// assert_eq!(spec.default, Some(serde_json::json!("web")));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    /// The shared field properties.
    pub base: BaseField,
    /// Whether a value must be present.
    pub required: bool,
    /// The value used when none is given. `None` means there is no default,
    /// which for a [`required`](FormField::required) field means the admin must
    /// supply one.
    pub default: Option<Json>,
    /// The values this field is restricted to. Empty = unrestricted; non-empty
    /// makes it a choice, which the admin UI renders as a select.
    ///
    /// §6.2 sketches this as an `OptionsSource` of *static | server query |
    /// client code*. Only the static case is reachable in the MVP — there is no
    /// form runtime to evaluate the others — so this is the static list, and
    /// grows into the full source when something can use one.
    pub options: Vec<Json>,
    // Post-MVP (§6.2, §6.3, §12): `fieldview: FieldViewRef` and
    // `visibility: Option<Formula>`. Both name types that do not exist yet —
    // fieldviews and formulas are out of MVP scope — so they are left out rather
    // than invented here with no consumer.
}

impl FormField {
    /// A form field of the given name and type: optional, with no default and no
    /// restricted options, and a label defaulting to the name.
    pub fn new(name: impl Into<String>, type_: impl Into<TypeRef>) -> FormField {
        FormField {
            base: BaseField::new(name, type_.into()),
            required: false,
            default: None,
            options: Vec::new(),
        }
    }

    /// The field's name — the key it occupies in an [`Attrs`] bag.
    pub fn name(&self) -> &str {
        &self.base.name
    }

    /// Set the human-facing label.
    pub fn label(mut self, label: impl Into<String>) -> FormField {
        self.base.label = label.into();
        self
    }

    /// Mark the field required.
    pub fn required(mut self) -> FormField {
        self.required = true;
        self
    }

    /// Set the default value.
    ///
    /// Named `default_value` rather than `default` so it is not confused with
    /// [`Default::default`]; the field it sets is [`default`](FormField::default).
    pub fn default_value(mut self, value: impl Into<Json>) -> FormField {
        self.default = Some(value.into());
        self
    }

    /// Restrict the field to a set of values, rendered as a select.
    pub fn options(mut self, options: impl IntoIterator<Item = impl Into<Json>>) -> FormField {
        self.options = options.into_iter().map(Into::into).collect();
        self
    }

    /// The value to use for this field given the `attrs` actually supplied: the
    /// stored value if present, otherwise the default.
    ///
    /// Absent *and* no default yields `None` — which for a required field is what
    /// makes the config invalid.
    pub fn resolve<'a>(&'a self, attrs: &'a Attrs) -> Option<&'a Json> {
        attrs.get(&self.base.name).or(self.default.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BasicType;
    use serde_json::json;

    #[test]
    fn base_field_label_defaults_to_name() {
        let f = BaseField::new("title", TypeRef::Basic(BasicType::Text));
        assert_eq!(f.name, "title");
        assert_eq!(f.label, "title");
        assert!(f.attributes.is_empty());
    }

    #[test]
    fn a_new_form_field_is_optional_unrestricted_and_labelled_by_name() {
        let f = FormField::new("source", BasicType::Text);
        assert_eq!(f.name(), "source");
        assert_eq!(f.base.label, "source");
        assert_eq!(f.base.type_, TypeRef::Basic(BasicType::Text));
        assert!(!f.required);
        assert_eq!(f.default, None);
        assert!(f.options.is_empty());
    }

    #[test]
    fn the_setters_chain() {
        let f = FormField::new("workers", BasicType::Int)
            .label("Worker processes")
            .required()
            .default_value(4);
        assert_eq!(f.base.label, "Worker processes");
        assert!(f.required);
        assert_eq!(f.default, Some(json!(4)));
    }

    #[test]
    fn a_form_field_describes_json_not_a_tagged_value() {
        // The values it describes live in an `Attrs`, are stored as a JSON column
        // and reach the admin UI as JSON, so a default is the plain JSON that
        // ends up in the bag — not `Value`'s tagged encoding.
        assert_eq!(
            FormField::new("minify", BasicType::Bool)
                .default_value(true)
                .default,
            Some(json!(true))
        );
        assert_eq!(
            FormField::new("source", BasicType::Text)
                .default_value("web")
                .default,
            Some(json!("web"))
        );
    }

    #[test]
    fn options_make_a_field_a_choice() {
        let f = FormField::new("bundler", BasicType::Text).options(["vite", "webpack"]);
        assert_eq!(f.options, vec![json!("vite"), json!("webpack")]);
    }

    #[test]
    fn resolve_prefers_the_supplied_value_then_the_default() {
        let f = FormField::new("source", BasicType::Text).default_value("web");
        let mut attrs = Attrs::new();

        // Absent: the default stands in.
        assert_eq!(f.resolve(&attrs), Some(&json!("web")));

        // Supplied: the admin's value wins.
        attrs.insert("source".to_owned(), json!("frontend"));
        assert_eq!(f.resolve(&attrs), Some(&json!("frontend")));

        // Absent with no default is what makes a required field invalid.
        let no_default = FormField::new("output", BasicType::Text).required();
        assert_eq!(no_default.resolve(&Attrs::new()), None);
    }

    #[test]
    fn a_settings_spec_is_just_form_fields() {
        // What a `Framework::config_spec()` returns (§13.3): no `AttrSpec`, no
        // per-extension type — the same `FormField` a row editor renders.
        let spec: Vec<FormField> = vec![
            FormField::new("store", BasicType::Text).required(),
            FormField::new("source", BasicType::Text).default_value("web"),
            FormField::new("minify", BasicType::Bool).default_value(false),
        ];
        let mut config = Attrs::new();
        config.insert("store".to_owned(), json!("apps"));

        let resolved: Vec<Option<&Json>> = spec.iter().map(|f| f.resolve(&config)).collect();
        assert_eq!(
            resolved,
            [
                Some(&json!("apps")),
                Some(&json!("web")),
                Some(&json!(false))
            ]
        );
    }
}
