//! Filling a catalogue: the [`Translator`] seam, and the check nobody's prompt
//! is a substitute for (decision D9).
//!
//! The LLM translates and **the machine checks it**. A returned message whose
//! placeholder set differs from the source's is rejected and left untranslated,
//! with the key in the warning; so is one whose plural categories are not the
//! ones the target locale uses. An LLM that renames `{count}` to `{nombre}`
//! produces a string that renders a literal `{nombre}` in front of a user, and
//! no amount of prompt engineering is a substitute for checking.
//!
//! The trait is declared here, at layer 0, and implemented over `sc-llm` in
//! `sc-server` and `sc-cli` — the arrangement `StreamProvider`, `Mailer` and
//! `JsEvaluator` already have, and the reason this crate can sit below
//! `sc-types` while still being the thing that calls an LLM.
//!
//! Two surfaces, one function: `feldspar i18n translate --domain core --locale
//! fr` at development time, committed to git; a **Translate missing** button on
//! an application's Translations screen at run time, writing the app's own
//! catalogue.

use async_trait::async_trait;
use sc_error::Result;
use serde_json::Value as Json;

use crate::catalog::{Catalog, Message, source_text};
use crate::format::placeholders;
use crate::locale::Locale;
use crate::plural::categories_for;

/// How many messages go to the translator in one call.
///
/// Small enough that one bad batch is cheap to lose and that the response fits
/// comfortably in a reply; large enough that a 400-string catalogue is eight
/// calls rather than four hundred. A batch also gives the model the *other*
/// strings of the screen as context, which is most of why it is batched at all.
pub const BATCH: usize = 50;

/// Where a translation comes from.
///
/// `sources` are catalogue **keys** — English source text, possibly carrying a
/// `context\u{4}` prefix (see [`crate::context_key`]) — and the answer is keyed
/// by the same string. A key the implementation does not answer for is left
/// untranslated rather than guessed at, and the report says which.
///
/// A value is a string, or an object of plural forms: the catalogue's own shape
/// (proposal §1), so the implementation's prompt can show the format and the
/// answer can be parsed with [`Message::from_json`].
#[async_trait]
pub trait Translator: Send + Sync {
    /// Translate a batch of source keys into `target`.
    ///
    /// The `source` locale is the one the keys are written in — English for
    /// everything this milestone ships, but stated rather than assumed, because
    /// a module's catalogue need not be.
    async fn translate_batch(
        &self,
        source: &Locale,
        target: &Locale,
        keys: &[String],
    ) -> Result<std::collections::BTreeMap<String, Json>>;
}

/// One message the machine refused, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// The catalogue key. Named in every warning — it is the only thing that
    /// identifies the entry, and the admin's next move is to go and look at it.
    pub key: String,
    /// What was wrong, as a sentence.
    pub reason: String,
}

/// What one run of [`translate_missing`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranslateReport {
    /// The keys now translated.
    pub filled: Vec<String>,
    /// The keys whose translation was refused (D9's check), each with its
    /// reason. **These are left untranslated**, so the reader still gets correct
    /// English.
    pub rejected: Vec<Rejection>,
    /// The keys the translator simply did not answer for.
    pub unanswered: Vec<String>,
}

impl TranslateReport {
    /// The warnings a CLI prints and a screen shows, one line each.
    pub fn warnings(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .rejected
            .iter()
            .map(|r| format!("`{}`: {}", r.key, r.reason))
            .collect();
        for key in &self.unanswered {
            out.push(format!("`{key}`: the translator returned nothing"));
        }
        out
    }
}

/// Translate every key of `keys` that `catalog` has no entry for, in batches,
/// checking each answer before it is kept.
///
/// The catalogue's own locale is the target; `source` is the locale the keys are
/// written in. Nothing already translated is re-translated — filling a catalogue
/// is an operation an admin runs repeatedly as their application grows, and
/// re-asking for work already done would cost money and would overwrite a
/// correction somebody made by hand.
pub async fn translate_missing(
    catalog: &mut Catalog,
    source: &Locale,
    translator: &dyn Translator,
    keys: &[String],
) -> Result<TranslateReport> {
    let target = catalog.locale().clone();
    let missing: Vec<String> = keys
        .iter()
        .filter(|key| catalog.get(key).is_none())
        .cloned()
        .collect();

    let mut report = TranslateReport::default();
    for batch in missing.chunks(BATCH) {
        let answers = translator.translate_batch(source, &target, batch).await?;
        for key in batch {
            let Some(value) = answers.get(key) else {
                report.unanswered.push(key.clone());
                continue;
            };
            let message = match Message::from_json(key, value) {
                Ok(message) => message,
                Err(e) => {
                    report.rejected.push(Rejection {
                        key: key.clone(),
                        reason: e.to_string(),
                    });
                    continue;
                }
            };
            match check(key, &message, &target) {
                Ok(()) => {
                    catalog.insert(key.clone(), message);
                    report.filled.push(key.clone());
                }
                Err(reason) => report.rejected.push(Rejection {
                    key: key.clone(),
                    reason,
                }),
            }
        }
    }
    Ok(report)
}

/// Check a translation against its key: the placeholders it may use, and the
/// plural categories it must supply.
///
/// The plural rule is **exactly the target locale's CLDR category set** — French
/// is one/many/other, Russian is one/few/many/other, Japanese is other. Not "at
/// least `other`", because a missing form is a count that renders in the wrong
/// grammar and is the failure this check exists for; not "whatever the source
/// had", because the source is English and English has two forms. It is a
/// mechanical rule with a mechanical answer, which is what lets the prompt state
/// the required set and the machine verify it.
///
/// `Ok(())` or the sentence explaining the refusal. Exported because this is
/// also the one thing `feldspar i18n check` **fails** on (task 2.4): coverage is
/// a number, a placeholder mismatch is a bug.
pub fn check(key: &str, message: &Message, target: &Locale) -> std::result::Result<(), String> {
    let english = source_text(key);
    let expected = placeholders(english);

    for form in message.forms() {
        let found = placeholders(form);
        if found != expected {
            return Err(format!(
                "the translation uses {} where the source uses {}",
                describe(&found),
                describe(&expected)
            ));
        }
    }

    match message.categories() {
        None => Ok(()),
        Some(found) => {
            // A plural entry for a message with no count is a translator that
            // has invented a distinction the call site cannot make: `form()`
            // would pick `other` for ever, and the other forms would be dead
            // text in a file somebody maintains.
            if !expected.contains("count") {
                return Err(
                    "the translation has plural forms but the source has no `{count}`".to_owned(),
                );
            }
            let wanted = categories_for(target);
            let mut missing: Vec<&str> = wanted
                .iter()
                .filter(|c| !found.contains(c))
                .map(|c| c.as_str())
                .collect();
            missing.sort_unstable();
            let mut extra: Vec<&str> = found
                .iter()
                .filter(|c| !wanted.contains(c))
                .map(|c| c.as_str())
                .collect();
            extra.sort_unstable();
            if !missing.is_empty() || !extra.is_empty() {
                return Err(format!(
                    "the plural forms are wrong for {}: expected {}, got {}",
                    target.as_str(),
                    wanted
                        .iter()
                        .map(|c| c.as_str())
                        .collect::<Vec<_>>()
                        .join("/"),
                    found
                        .iter()
                        .map(|c| c.as_str())
                        .collect::<Vec<_>>()
                        .join("/")
                ));
            }
            Ok(())
        }
    }
}

fn describe(names: &std::collections::BTreeSet<&str>) -> String {
    match names.is_empty() {
        true => "no placeholders".to_owned(),
        false => names
            .iter()
            .map(|n| format!("{{{n}}}"))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;

    fn loc(tag: &str) -> Locale {
        Locale::parse(tag).unwrap()
    }

    /// A translator that answers from a table — and, for the keys the tests care
    /// about, answers *wrongly*, in each of the ways D9 names.
    struct Scripted(BTreeMap<String, Json>);

    #[async_trait]
    impl Translator for Scripted {
        async fn translate_batch(
            &self,
            _source: &Locale,
            _target: &Locale,
            keys: &[String],
        ) -> Result<BTreeMap<String, Json>> {
            Ok(keys
                .iter()
                .filter_map(|k| self.0.get(k).map(|v| (k.clone(), v.clone())))
                .collect())
        }
    }

    fn scripted(pairs: &[(&str, Json)]) -> Scripted {
        Scripted(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        )
    }

    #[tokio::test]
    async fn a_good_batch_is_kept() {
        let mut catalog = Catalog::new(loc("fr"));
        let translator = scripted(&[
            ("Incorrect password", json!("Mot de passe incorrect")),
            ("Delete {name}?", json!("Supprimer {name} ?")),
            (
                "{count} rows",
                // French's CLDR categories are one/many/other — `many` is the
                // form for exact millions — and a translation supplies all of
                // them or none of it is kept.
                json!({
                    "one": "{count} ligne",
                    "many": "{count} de lignes",
                    "other": "{count} lignes"
                }),
            ),
        ]);
        let keys: Vec<String> = ["Incorrect password", "Delete {name}?", "{count} rows"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();

        let report = translate_missing(&mut catalog, &Locale::source(), &translator, &keys)
            .await
            .unwrap();
        assert_eq!(report.filled.len(), 3);
        assert!(report.rejected.is_empty());
        assert!(report.warnings().is_empty());
        assert_eq!(
            catalog.render("Delete {name}?", &[("name", crate::Arg::from("Tâches"))]),
            "Supprimer Tâches ?"
        );
    }

    #[tokio::test]
    async fn a_renamed_placeholder_is_rejected_by_name() {
        let mut catalog = Catalog::new(loc("fr"));
        let translator = scripted(&[("Delete {name}?", json!("Supprimer {nom} ?"))]);
        let keys = vec!["Delete {name}?".to_owned()];

        let report = translate_missing(&mut catalog, &Locale::source(), &translator, &keys)
            .await
            .unwrap();
        assert!(report.filled.is_empty());
        assert_eq!(report.rejected.len(), 1);
        assert_eq!(report.rejected[0].key, "Delete {name}?");
        assert!(report.rejected[0].reason.contains("{nom}"), "{:?}", report);
        // And the reader still gets correct English.
        assert_eq!(catalog.render("Delete {name}?", &[]), "Delete {name}?");
        assert!(report.warnings()[0].contains("Delete {name}?"));
    }

    #[tokio::test]
    async fn a_lost_plural_category_is_rejected() {
        let mut catalog = Catalog::new(loc("fr"));
        // French uses one/many/other; this has only `other`.
        let translator = scripted(&[("{count} rows", json!({ "other": "{count} lignes" }))]);
        let keys = vec!["{count} rows".to_owned()];

        let report = translate_missing(&mut catalog, &Locale::source(), &translator, &keys)
            .await
            .unwrap();
        assert!(report.filled.is_empty());
        assert_eq!(report.rejected.len(), 1);
        assert!(report.rejected[0].reason.contains("one"), "{:?}", report);
        assert!(catalog.is_empty());
    }

    #[tokio::test]
    async fn a_category_the_locale_never_selects_is_rejected_too() {
        // `few` is Russian's, not French's: a form nothing will ever select is
        // dead text in a file somebody has to maintain.
        let mut catalog = Catalog::new(loc("fr"));
        let translator = scripted(&[(
            "{count} rows",
            json!({
                "one": "{count} ligne",
                "few": "{count} lignes",
                "many": "{count} de lignes",
                "other": "{count} lignes"
            }),
        )]);
        let keys = vec!["{count} rows".to_owned()];

        let report = translate_missing(&mut catalog, &Locale::source(), &translator, &keys)
            .await
            .unwrap();
        assert_eq!(report.rejected.len(), 1);
        assert!(report.rejected[0].reason.contains("few"), "{:?}", report);
    }

    #[tokio::test]
    async fn an_invented_plural_is_rejected() {
        let mut catalog = Catalog::new(loc("fr"));
        let translator = scripted(&[("Incorrect password", json!({ "one": "a", "other": "b" }))]);
        let keys = vec!["Incorrect password".to_owned()];

        let report = translate_missing(&mut catalog, &Locale::source(), &translator, &keys)
            .await
            .unwrap();
        assert_eq!(report.rejected.len(), 1);
        assert!(
            report.rejected[0].reason.contains("no `{count}`"),
            "{:?}",
            report
        );
    }

    #[tokio::test]
    async fn a_value_that_is_not_a_message_is_rejected_rather_than_stored() {
        let mut catalog = Catalog::new(loc("fr"));
        let translator = scripted(&[("Save", json!(42))]);
        let keys = vec!["Save".to_owned()];
        let report = translate_missing(&mut catalog, &Locale::source(), &translator, &keys)
            .await
            .unwrap();
        assert_eq!(report.rejected.len(), 1);
        assert!(report.rejected[0].reason.contains("Save"), "{:?}", report);
    }

    #[tokio::test]
    async fn what_is_already_translated_is_left_alone() {
        let mut catalog = Catalog::new(loc("fr"));
        catalog.insert(
            "Save",
            Message::Simple("Enregistrer (à la main)".to_owned()),
        );
        let translator = scripted(&[("Save", json!("Sauver")), ("Delete", json!("Supprimer"))]);
        let keys = vec!["Save".to_owned(), "Delete".to_owned()];

        let report = translate_missing(&mut catalog, &Locale::source(), &translator, &keys)
            .await
            .unwrap();
        assert_eq!(report.filled, ["Delete"]);
        assert_eq!(catalog.render("Save", &[]), "Enregistrer (à la main)");
    }

    #[tokio::test]
    async fn a_key_the_translator_skipped_is_reported() {
        let mut catalog = Catalog::new(loc("fr"));
        let translator = scripted(&[]);
        let keys = vec!["Save".to_owned()];
        let report = translate_missing(&mut catalog, &Locale::source(), &translator, &keys)
            .await
            .unwrap();
        assert_eq!(report.unanswered, ["Save"]);
        assert!(report.warnings()[0].contains("returned nothing"));
    }

    #[tokio::test]
    async fn more_than_one_batch_is_sent() {
        let pairs: Vec<(String, Json)> = (0..BATCH + 7)
            .map(|i| (format!("Message {i}"), json!(format!("Message {i} (fr)"))))
            .collect();
        let translator = Scripted(pairs.iter().cloned().collect());
        let keys: Vec<String> = pairs.iter().map(|(k, _)| k.clone()).collect();
        let mut catalog = Catalog::new(loc("fr"));

        let report = translate_missing(&mut catalog, &Locale::source(), &translator, &keys)
            .await
            .unwrap();
        assert_eq!(report.filled.len(), BATCH + 7);
        assert_eq!(catalog.len(), BATCH + 7);
    }

    #[test]
    fn a_context_key_is_checked_against_its_source_text_only() {
        let key = crate::context_key("verb", "Order {name}");
        let message = Message::Simple("Commander {name}".to_owned());
        assert!(check(&key, &message, &loc("fr")).is_ok());
    }
}
