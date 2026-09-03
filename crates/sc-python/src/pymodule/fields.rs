//! A Python plugin's field declarations, translated into this system's own
//! [`FormField`] and [`DataField`].
//!
//! `sc_module::spec` does the same job for the other language and the two are
//! deliberately not the same code, because they are not the same vocabulary. A
//! v1 plugin declares `{ type: "String", fieldview: "password" }` and this
//! system has to *guess* what it meant; a Python plugin declares
//! `sc.Field.string("api_key", secret=True)`, which is this system's own
//! vocabulary with nothing to guess — §2 of the API: "it declares its settings
//! in **this system's** field vocabulary, not v1's".
//!
//! So this file is short, and everything it does not have to do is the point:
//! no three spellings of a password, no `attributes.options`, no JavaScript
//! truthiness, and no "unknown type, shown as text" — a type that is not one of
//! the six is refused by [`Field`](super) in Python, on the line that wrote it.
//!
//! What crosses the seam is [`Field.to_json`]'s object:
//!
//! ```json
//! { "name": "api_key", "type": "string", "label": "API key",
//!   "required": true, "secret": true, "multiline": false }
//! ```

use sc_catalog::DataField;
use sc_types::{BasicType, FormField, TypeRef};
use serde_json::Value as Json;

/// One declared field as a [`FormField`], or `None` for a declaration with no
/// usable name — which has no control to be.
///
/// A missing or unknown `type` is text rather than a refusal, for the reason
/// `sc_module::spec` gives: the Python side already refuses what it can see, so
/// anything that arrives here malformed came from a plugin that built its own
/// dictionary, and one odd field should not cost an admin the other four.
pub(crate) fn form_field(declared: &Json) -> Option<FormField> {
    let name = name(declared)?;
    let mut field = FormField::new(name, TypeRef::Basic(basic_type(declared)));
    field = field.label(label(declared).unwrap_or(name));
    if flag(declared, "required") {
        field = field.required();
    }
    if let Some(default) = declared.get("default").filter(|d| !d.is_null()) {
        field = field.default_value(default.clone());
    }
    if let Some(Json::Array(options)) = declared.get("options")
        && !options.is_empty()
    {
        field = field.options(options.clone());
    }
    // Both, unlike v1's translation, which has to pick: `secret` and
    // `multiline` are separate flags here because the declaration has separate
    // flags, and a secret that is also multiline is a private key.
    if flag(declared, "secret") {
        field = field.secret();
    }
    if flag(declared, "multiline") {
        field = field.multiline();
    }
    Some(field)
}

/// One declared field as a **column** of a provided table.
///
/// The same vocabulary applied to a column rather than to a form control, which
/// is why `primary_key` and `unique` mean something here and nothing above: a
/// provided table with no primary key is a table nothing can address a row of.
pub(crate) fn data_field(declared: &Json) -> Option<DataField> {
    let name = name(declared)?;
    let mut field = DataField::plain(name, TypeRef::Basic(basic_type(declared)));
    field = field.label(label(declared).unwrap_or(name));
    if flag(declared, "primary_key") {
        field = field.primary_key();
    }
    if flag(declared, "required") {
        field = field.required();
    }
    if flag(declared, "unique") {
        field = field.unique();
    }
    Some(field)
}

/// Every declaration in a list, dropping the ones that are not.
pub(crate) fn form_fields(declared: &[Json]) -> Vec<FormField> {
    declared.iter().filter_map(form_field).collect()
}

fn name(declared: &Json) -> Option<&str> {
    declared
        .get("name")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

fn label(declared: &Json) -> Option<&str> {
    declared
        .get("label")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|label| !label.is_empty())
}

/// The six types `saltcorn.Field` offers, and what they are here.
fn basic_type(declared: &Json) -> BasicType {
    match declared.get("type").and_then(Json::as_str) {
        Some("int") => BasicType::Int,
        Some("float") => BasicType::Float,
        Some("bool") => BasicType::Bool,
        Some("date") => BasicType::Date,
        Some("json") => BasicType::Json,
        _ => BasicType::Text,
    }
}

/// A boolean flag. Strictly a boolean: Python has one and there is no reason to
/// accept anything else, which is the whole difference from v1's `truthy`.
fn flag(declared: &Json, key: &str) -> bool {
    declared.get(key).and_then(Json::as_bool).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_declared_setting_becomes_the_control_it_says_it_is() {
        let field = form_field(&json!({
            "name": "api_key", "type": "string", "label": "API key",
            "required": true, "secret": true, "multiline": false
        }))
        .expect("a named field");
        assert_eq!(field.name(), "api_key");
        assert_eq!(field.base.label, "API key");
        assert_eq!(field.base.type_, TypeRef::Basic(BasicType::Text));
        assert!(field.required);
        assert!(field.secret && !field.multiline);
    }

    #[test]
    fn the_six_types_map_across_and_an_unlabelled_field_is_labelled_by_its_name() {
        let fields = form_fields(&[
            json!({ "name": "s", "type": "string" }),
            json!({ "name": "n", "type": "int" }),
            json!({ "name": "f", "type": "float" }),
            json!({ "name": "b", "type": "bool" }),
            json!({ "name": "d", "type": "date" }),
            json!({ "name": "j", "type": "json" }),
        ]);
        let types: Vec<_> = fields.iter().map(|f| f.base.type_.clone()).collect();
        assert_eq!(
            types,
            vec![
                TypeRef::Basic(BasicType::Text),
                TypeRef::Basic(BasicType::Int),
                TypeRef::Basic(BasicType::Float),
                TypeRef::Basic(BasicType::Bool),
                TypeRef::Basic(BasicType::Date),
                TypeRef::Basic(BasicType::Json),
            ]
        );
        assert_eq!(fields[0].base.label, "s");
    }

    #[test]
    fn options_and_a_default_survive_and_a_nameless_declaration_is_dropped() {
        let field = form_field(&json!({
            "name": "region", "type": "string", "default": "eu", "options": ["eu", "us"]
        }))
        .expect("a named field");
        assert_eq!(field.default, Some(json!("eu")));
        assert_eq!(field.static_options(), &[json!("eu"), json!("us")]);
        assert!(form_field(&json!({ "type": "string" })).is_none());
        assert!(form_field(&json!({ "name": "  " })).is_none());
    }

    #[test]
    fn a_providers_fields_are_columns_with_a_primary_key() {
        let fields: Vec<DataField> = [
            json!({ "name": "id", "type": "int", "primary_key": true, "required": true }),
            json!({ "name": "name", "type": "string", "unique": true }),
            json!({ "label": "nameless" }),
        ]
        .iter()
        .filter_map(data_field)
        .collect();
        assert_eq!(fields.len(), 2);
        assert!(fields[0].primary_key && fields[0].required);
        assert_eq!(fields[0].base.type_, TypeRef::Basic(BasicType::Int));
        assert!(fields[1].unique && !fields[1].primary_key);
    }
}
