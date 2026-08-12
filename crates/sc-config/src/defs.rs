//! What configuration keys exist, and what each one is.
//!
//! A key is declared as a [`FormField`] — the vocabulary every configurable
//! thing in the system already speaks (§6.2) — grouped into a
//! [`ConfigSection`] so the settings screen has something to put a heading on.
//! That declaration is the *only* place a key is defined: it gives
//! [`crate::store`] the type to check a write against, gives the admin UI the
//! control to render, and gives the reader its default. Adding a setting is
//! adding one entry here.
//!
//! Sections are ordered as the screen shows them, and the fields within a
//! section as the form lays them out.

use std::sync::OnceLock;

use sc_types::FormField;

/// One configuration key: its declaration, plus the sentence under the control.
///
/// The help text is not part of [`FormField`] because a *label* is what every
/// consumer of that vocabulary needs and a paragraph is what only a settings
/// screen has room for — a file store's backend picker does not want one.
#[derive(Debug, Clone)]
pub struct ConfigDef {
    /// The key's name, type, default, options and `secret` flag.
    pub field: FormField,
    /// A sentence shown under the control. Empty when the label says it all.
    pub help: &'static str,
}

impl ConfigDef {
    /// Declare a key with no help text.
    pub fn new(field: FormField) -> ConfigDef {
        ConfigDef { field, help: "" }
    }

    /// Declare a key with the sentence that goes under its control.
    pub fn help(field: FormField, help: &'static str) -> ConfigDef {
        ConfigDef { field, help }
    }

    /// The key this definition declares.
    pub fn key(&self) -> &str {
        self.field.name()
    }
}

/// A group of keys shown together, with a heading and an explanation.
#[derive(Debug, Clone)]
pub struct ConfigSection {
    /// Stable identifier (`ssl`), used by the API and by tests.
    pub name: &'static str,
    /// The heading the screen shows.
    pub label: &'static str,
    /// A paragraph under the heading: what these settings do, and what taking
    /// effect requires.
    pub description: &'static str,
    /// The keys in this section, in form order.
    pub fields: Vec<ConfigDef>,
}

/// Every section, in screen order.
pub fn config_sections() -> &'static [ConfigSection] {
    static SECTIONS: OnceLock<Vec<ConfigSection>> = OnceLock::new();
    SECTIONS.get_or_init(|| vec![crate::ssl::ssl_section()])
}

/// Every declared key's [`FormField`], flattened — the spec a whole settings
/// payload is validated against.
pub fn config_spec() -> Vec<FormField> {
    config_sections()
        .iter()
        .flat_map(|section| section.fields.iter().map(|def| def.field.clone()))
        .collect()
}

/// The declaration for `key`, if there is one.
pub fn definition(key: &str) -> Option<FormField> {
    config_sections()
        .iter()
        .flat_map(|section| section.fields.iter())
        .find(|def| def.key() == key)
        .map(|def| def.field.clone())
}

/// Every declared key's name, in section order.
pub fn known_keys() -> Vec<&'static str> {
    config_sections()
        .iter()
        .flat_map(|section| section.fields.iter())
        .map(|def| def.field.name())
        // The declarations are `'static` (behind a `OnceLock`), so the names are
        // too — which is what lets an error message list them without cloning.
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_is_declared_once() {
        let keys = known_keys();
        let mut sorted = keys.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "duplicate configuration key");
        assert_eq!(config_spec().len(), keys.len());
    }

    #[test]
    fn a_key_is_found_by_name_and_a_typo_is_not() {
        assert!(definition(crate::ssl::SSL_MODE).is_some());
        assert!(definition("ssl-mode").is_none());
    }

    #[test]
    fn every_declared_key_has_a_label() {
        for section in config_sections() {
            assert!(!section.label.is_empty());
            for def in &section.fields {
                assert!(
                    !def.field.base.label.is_empty(),
                    "`{}` has no label",
                    def.key()
                );
            }
        }
    }
}
