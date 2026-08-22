//! v1's `configFields` translated into this system's [`FormField`] (§6.2).
//!
//! A v1 action declares its settings as a list of objects:
//!
//! ```js
//! configFields: [
//!   { name: "channel", label: "Channel", type: "String", required: true },
//!   { name: "protocol", type: "String", attributes: { options: ["mqtt", "mqtts"] } },
//!   { name: "password", type: "String", fieldview: "password" },
//! ]
//! ```
//!
//! and this system declares them as [`FormField`]s — the one vocabulary every
//! configurable thing here already speaks. Translating at the boundary is what
//! lets the trigger form render a module's action with no code that knows what a
//! module is.
//!
//! **An unrecognised type is text, and is reported.** Dropping the field would
//! leave a control the admin cannot fill in for a setting the module still
//! reads; refusing the whole module for one exotic type would lose four working
//! actions to a fifth. So the field is rendered as text, the module carries an
//! issue saying which field and which type, and the admin can see both.

use sc_types::{BasicType, FormField, TypeRef};
use serde_json::Value as Json;

/// What a translated field set could not express faithfully.
///
/// Collected rather than returned as an error, for the reason in the module
/// docs: a module with one odd field is a module with one odd field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecIssue(pub String);

/// Translate v1's `configFields` into form fields, collecting what could not be
/// expressed.
///
/// `owner` names the thing whose fields these are (`the action mqtt_publish`),
/// so an issue reads as a sentence.
pub fn config_fields_to_form_fields(fields: &[Json], owner: &str) -> (Vec<FormField>, Vec<String>) {
    let mut out = Vec::new();
    let mut issues = Vec::new();
    for field in fields {
        match config_field(field, owner) {
            Ok((form_field, issue)) => {
                if let Some(issue) = issue {
                    issues.push(issue);
                }
                out.push(form_field);
            }
            Err(issue) => issues.push(issue),
        }
    }
    (out, issues)
}

/// One v1 field. `Err` is a field that could not be translated at all (it has no
/// name); `Ok`'s second half is one that was translated with a caveat.
fn config_field(field: &Json, owner: &str) -> Result<(FormField, Option<String>), String> {
    let Some(name) = field.get("name").and_then(Json::as_str) else {
        return Err(format!(
            "{owner} declares a setting with no name, which cannot be shown or stored"
        ));
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("{owner} declares a setting with an empty name"));
    }

    let declared = field
        .get("type")
        .and_then(Json::as_str)
        .unwrap_or("String")
        .trim()
        .to_owned();
    let (basic, issue) = match basic_type(&declared) {
        Some(basic) => (basic, None),
        None => (
            BasicType::Text,
            Some(format!(
                "{owner}'s setting `{name}` is of type `{declared}`, which this version does not \
                 know; it is shown as text"
            )),
        ),
    };

    let mut form_field = FormField::new(name, TypeRef::Basic(basic));
    form_field = form_field.label(
        field
            .get("label")
            .and_then(Json::as_str)
            .filter(|l| !l.trim().is_empty())
            .unwrap_or(name),
    );
    if truthy(field.get("required")) {
        form_field = form_field.required();
    }
    if let Some(default) = field.get("default")
        && !default.is_null()
    {
        form_field = form_field.default_value(default.clone());
    }
    if let Some(options) = options(field) {
        form_field = form_field.options(options);
    }
    // v1 spells "this is a password" three ways depending on how old the plugin
    // is; all three mean the same thing, and this system's word for it is
    // `secret` — which is what makes it redact on the way out and merge back on
    // the way in, everywhere, rather than in whichever screen remembered.
    if is_password(field) {
        form_field = form_field.secret();
    } else if is_textarea(field) {
        form_field = form_field.multiline();
    }
    // `sublabel` — v1's sentence under the control — has nowhere to go:
    // `FormField` carries no help text (a `ConfigDef` does, and that is a
    // settings-screen type). Dropped deliberately rather than folded into the
    // label, where it would read as part of the name.
    Ok((form_field, issue))
}

/// v1's type names, and what they are here.
///
/// The list is v1's built-in scalar types plus the two spellings of a JSON
/// setting. A **rich** v1 type (one a plugin defines) is not here and lands in
/// the caller's issue list as text, which is exactly what it is on the wire.
fn basic_type(declared: &str) -> Option<BasicType> {
    Some(match declared {
        "String" | "string" => BasicType::Text,
        "Integer" | "integer" | "Int" => BasicType::Int,
        "Float" | "float" | "Number" => BasicType::Float,
        "Bool" | "bool" | "Boolean" => BasicType::Bool,
        "Date" | "date" => BasicType::Date,
        "JSON" | "json" => BasicType::Json,
        // v1 renders a colour picker; the value is a `#rrggbb` string, and text
        // is what it is until this system has a colour type of its own.
        "Color" | "color" => BasicType::Text,
        _ => return None,
    })
}

/// The `attributes.options` list, as strings.
///
/// v1 writes options two ways — `["a", "b"]` and `[{name: "a", label: "A"}]` —
/// and both mean the same set of stored values. The label of the second form is
/// dropped for the same reason `sublabel` is: there is nowhere for it to go
/// today, and inventing a place for it here would be a second vocabulary.
fn options(field: &Json) -> Option<Vec<Json>> {
    let options = field.get("attributes")?.get("options")?;
    let list = match options {
        Json::Array(values) => values
            .iter()
            .filter_map(|value| match value {
                Json::String(s) => Some(Json::String(s.clone())),
                Json::Object(o) => o.get("name").cloned(),
                other => Some(other.clone()),
            })
            .collect::<Vec<_>>(),
        // v1 also accepts a comma-separated string.
        Json::String(s) => s
            .split(',')
            .map(|o| Json::String(o.trim().to_owned()))
            .filter(|o| o.as_str() != Some(""))
            .collect(),
        _ => return None,
    };
    (!list.is_empty()).then_some(list)
}

/// Whether v1 asked for a password input, in any of the three spellings.
fn is_password(field: &Json) -> bool {
    let says = |key: &str, value: &str| field.get(key).and_then(Json::as_str) == Some(value);
    says("fieldview", "password")
        || says("input_type", "password")
        || says("type", "password")
        || truthy(field.get("secret"))
}

/// Whether v1 asked for a text area.
fn is_textarea(field: &Json) -> bool {
    field.get("fieldview").and_then(Json::as_str) == Some("textarea")
        || field.get("input_type").and_then(Json::as_str) == Some("textarea")
}

/// JavaScript truthiness for the flags a v1 field carries — a plugin writes
/// `required: true`, and an older one writes `required: "on"`.
fn truthy(value: Option<&Json>) -> bool {
    match value {
        Some(Json::Bool(b)) => *b,
        Some(Json::String(s)) => !s.is_empty() && s != "false",
        Some(Json::Number(n)) => n.as_f64().unwrap_or(0.0) != 0.0,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn translate(fields: Json) -> (Vec<FormField>, Vec<String>) {
        let Json::Array(fields) = fields else {
            panic!("test fields should be an array")
        };
        config_fields_to_form_fields(&fields, "the action mqtt_publish")
    }

    #[test]
    fn a_v1_string_field_becomes_a_required_text_setting() {
        let (fields, issues) = translate(json!([
            { "name": "channel", "label": "Channel", "type": "String", "required": true }
        ]));
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name(), "channel");
        assert_eq!(fields[0].base.label, "Channel");
        assert_eq!(fields[0].base.type_, TypeRef::Basic(BasicType::Text));
        assert!(fields[0].required);
    }

    #[test]
    fn the_scalar_types_map_across() {
        let (fields, issues) = translate(json!([
            { "name": "n", "type": "Integer" },
            { "name": "f", "type": "Float" },
            { "name": "b", "type": "Bool" },
            { "name": "d", "type": "Date" },
            { "name": "j", "type": "JSON" },
            { "name": "c", "type": "Color" }
        ]));
        assert!(issues.is_empty(), "{issues:?}");
        let types: Vec<_> = fields.iter().map(|f| f.base.type_.clone()).collect();
        assert_eq!(
            types,
            vec![
                TypeRef::Basic(BasicType::Int),
                TypeRef::Basic(BasicType::Float),
                TypeRef::Basic(BasicType::Bool),
                TypeRef::Basic(BasicType::Date),
                TypeRef::Basic(BasicType::Json),
                TypeRef::Basic(BasicType::Text),
            ]
        );
    }

    #[test]
    fn options_arrive_as_a_restricted_set_whichever_way_v1_wrote_them() {
        let (fields, _) = translate(json!([
            { "name": "protocol", "type": "String",
              "attributes": { "options": ["mqtt", "mqtts"] } },
            { "name": "mode", "type": "String",
              "attributes": { "options": [{ "name": "a", "label": "A" }] } },
            { "name": "old", "type": "String", "attributes": { "options": "x, y" } }
        ]));
        assert_eq!(fields[0].static_options(), &[json!("mqtt"), json!("mqtts")]);
        assert_eq!(fields[1].static_options(), &[json!("a")]);
        assert_eq!(fields[2].static_options(), &[json!("x"), json!("y")]);
    }

    #[test]
    fn a_password_is_a_secret_and_a_textarea_is_multiline() {
        let (fields, _) = translate(json!([
            { "name": "password", "type": "String", "fieldview": "password" },
            { "name": "key", "type": "String", "input_type": "password" },
            { "name": "ca", "type": "String", "fieldview": "textarea" }
        ]));
        assert!(fields[0].secret && !fields[0].multiline);
        assert!(fields[1].secret);
        assert!(fields[2].multiline && !fields[2].secret);
    }

    #[test]
    fn a_default_survives_and_an_unlabelled_field_is_labelled_by_its_name() {
        let (fields, _) = translate(json!([
            { "name": "port", "type": "Integer", "default": 1883 }
        ]));
        assert_eq!(fields[0].default, Some(json!(1883)));
        assert_eq!(fields[0].base.label, "port");
    }

    #[test]
    fn an_unknown_type_is_text_and_says_so() {
        let (fields, issues) = translate(json!([{ "name": "when", "type": "DateLocale" }]));
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].base.type_, TypeRef::Basic(BasicType::Text));
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("DateLocale"), "{}", issues[0]);
        assert!(issues[0].contains("when"), "{}", issues[0]);
    }

    #[test]
    fn a_nameless_field_is_dropped_with_a_reason_rather_than_rendered() {
        let (fields, issues) = translate(json!([{ "label": "Mystery", "type": "String" }]));
        assert!(fields.is_empty());
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("no name"), "{}", issues[0]);
    }
}
