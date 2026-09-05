//! Applying a fitted instance to rows (TODO §6, §10, task 3.4).
//!
//! Four steps, and the third is the one the milestone is careful about:
//!
//! 1. Check the instance can answer at all — it is `fitted`, and its outcome
//!    produces something per row.
//! 2. Encode the frame **with the instance's own encoding**, strictly: a null
//!    feature or a category the fit never saw is an error naming the column and
//!    the value, never a row of zeros (§6).
//! 3. Ask the provider, which answers in class *indices*.
//! 4. Map those back through the target encoding into class **names**, because
//!    the index is an implementation detail of that encoding and nobody's row
//!    wants to hold a `2`.
//!
//! Step 4 is why [`Prediction::ClassIndex`] and [`Prediction::Class`] are
//! different variants rather than one with a flag: a provider answers the first
//! and only this module produces the second, so "is this a name or an index" is
//! never a question about where you are in the call stack.
//!
//! **A prediction takes a frame, not a row.** A single row is a frame of one.
//! Batching is what makes a provider in another language usable at all — the
//! call is the cost, not the arithmetic — and it is what lets the fit's metric
//! pass score 50 000 rows in one call rather than in 50 000.

use sc_error::{Error, Result};
use serde_json::Value as Json;

use crate::encode::{Encoding, apply_encoding};
use crate::fit::ATTR_OUTCOME;
use crate::frame::Frame;
use crate::instance::ModelInstance;
use crate::provider::{Outcome, Prediction};
use crate::registry::ModelRegistry;

impl ModelInstance {
    /// The [`Outcome`] this fit produces, as it was recorded at fit time.
    ///
    /// Read off the instance rather than recomputed, because recomputing means
    /// reading the dataset again — and would answer with *today's* column types
    /// rather than the ones this instance was fitted over.
    pub fn outcome(&self) -> Result<Outcome> {
        let json = self.attributes.get(ATTR_OUTCOME).ok_or_else(|| {
            Error::invalid(format!(
                "instance {} does not record what it produces, so it cannot be applied",
                self.id
            ))
        })?;
        serde_json::from_value(json.clone()).map_err(|e| {
            Error::invalid(format!(
                "instance {}: its recorded outcome cannot be read: {e}",
                self.id
            ))
        })
    }

    /// The encoding this fit was made with.
    pub fn encoding(&self) -> Result<Encoding> {
        if self.encoding.is_null() {
            return Err(Error::invalid(format!(
                "instance {} has no stored encoding, so it cannot be applied to a row",
                self.id
            )));
        }
        Encoding::from_json(&self.encoding)
    }
}

/// Apply `instance` to `frame`, answering one prediction per row **in row
/// order**.
///
/// `provider` is looked up from the registry by the name the *model* carries,
/// not by anything on the instance: a fit belongs to its model, and an instance
/// whose provider has been uninstalled should say that in those words rather
/// than by not being found.
pub async fn predict_rows(
    registry: &ModelRegistry,
    provider: &str,
    instance: &ModelInstance,
    frame: &Frame,
) -> Result<Vec<Prediction>> {
    if !instance.is_usable() {
        return Err(Error::invalid(match instance.error() {
            Some(why) => format!(
                "instance {} did not finish fitting, so it cannot predict: {why}",
                instance.id
            ),
            None => format!(
                "instance {} is `{}`, so it cannot predict yet",
                instance.id, instance.status
            ),
        }));
    }
    let outcome = instance.outcome()?;
    if !outcome.predicts() {
        return Err(Error::invalid(format!(
            "instance {} is a hypothesis test: its parameters are the answer, and there is no \
             per-row prediction to make",
            instance.id
        )));
    }
    let encoding = instance.encoding()?;
    // Strict: at predict time a row we cannot represent is an error, not a
    // dropped row. Dropping would answer fewer predictions than there were rows,
    // and the caller lines them up against the rows it asked about.
    let encoded = apply_encoding(&encoding, frame)?;
    let provider = registry.require(provider.trim())?;
    let raw = provider
        .predict(&instance.state, &encoded.features_frame())
        .await?;
    if raw.len() != frame.rows {
        return Err(Error::msg(format!(
            "the model provider `{}` answered {} predictions for {} rows",
            provider.name(),
            raw.len(),
            frame.rows
        )));
    }
    name_classes(raw, &encoding)
}

/// Turn every class index into the class name the fit's encoding gave it.
///
/// A provider that answered a name directly is left alone — that is what a
/// module's provider may well do — and every other variant passes through.
pub fn name_classes(predictions: Vec<Prediction>, encoding: &Encoding) -> Result<Vec<Prediction>> {
    predictions
        .into_iter()
        .map(|prediction| match prediction {
            Prediction::ClassIndex { index, probability } => {
                let target = encoding.target.as_ref().ok_or_else(|| {
                    Error::msg(
                        "the provider answered a class, but this instance was not fitted with a \
                         label to map it through"
                            .to_owned(),
                    )
                })?;
                Ok(Prediction::class(target.class(index)?, probability))
            }
            other => Ok(other),
        })
        .collect()
}

/// The value each prediction writes into a row (§12) — what `predict_row` hands
/// the row layer.
pub fn prediction_values(predictions: &[Prediction]) -> Result<Vec<Json>> {
    predictions.iter().map(Prediction::to_json).collect()
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use std::sync::Arc;

    use super::*;
    use crate::encode::{TargetEncoding, fit_encoding};
    use crate::frame::Column;
    use crate::instance::ModelInstance;
    use crate::model::ModelId;
    use crate::provider::{FitResult, ModelProvider, OutcomeSpec};
    use sc_types::{Attrs, FormField};

    /// A provider that answers the class index its state names, for every row.
    struct FixedClass;

    #[async_trait]
    impl ModelProvider for FixedClass {
        fn name(&self) -> &str {
            "fixed_class"
        }

        fn description(&self) -> &str {
            "answers one class index"
        }

        fn config_declaration(&self) -> Vec<FormField> {
            vec![crate::provider::column_field("label", "Label")]
        }

        fn outcome_spec(&self) -> OutcomeSpec {
            OutcomeSpec::Classification {
                label: "label".to_owned(),
            }
        }

        async fn fit(&self, _f: &Frame, _c: &Attrs, _h: &Attrs) -> Result<FitResult> {
            Ok(FitResult::new(serde_json::json!({ "class": 1 })))
        }

        async fn predict(&self, state: &Json, frame: &Frame) -> Result<Vec<Prediction>> {
            let index = state.get("class").and_then(Json::as_u64).unwrap_or(0) as usize;
            Ok(vec![Prediction::class_index(index, Some(0.75)); frame.rows])
        }
    }

    fn registry() -> ModelRegistry {
        let mut registry = ModelRegistry::new();
        registry.register(Arc::new(FixedClass)).expect("register");
        registry
    }

    fn training() -> Frame {
        Frame::new(
            vec![
                (
                    "region".to_owned(),
                    Column::Str(vec![Some("north".into()), Some("south".into())]),
                ),
                ("area".to_owned(), Column::Float(vec![Some(1.0), Some(2.0)])),
                (
                    "sold".to_owned(),
                    Column::Str(vec![Some("no".into()), Some("yes".into())]),
                ),
            ],
            vec!["int:1".to_owned(), "int:2".to_owned()],
        )
        .expect("frame")
    }

    /// A fitted instance over [`training`], answering class 1.
    fn instance() -> ModelInstance {
        let outcome = Outcome::Classification {
            label: "sold".to_owned(),
            classes: None,
        };
        let encoding = fit_encoding(&training(), &outcome, false).expect("encoding");
        let mut instance = ModelInstance::starting(ModelId::new());
        instance =
            crate::instance_store::fitted(instance, serde_json::json!({ "class": 1 }), Vec::new());
        instance.encoding = encoding.to_json().expect("json");
        instance.attributes.insert(
            ATTR_OUTCOME.to_owned(),
            serde_json::to_value(Outcome::Classification {
                label: "sold".to_owned(),
                classes: encoding.classes().map(<[String]>::to_vec),
            })
            .expect("outcome"),
        );
        instance
    }

    #[tokio::test]
    async fn a_class_index_comes_back_as_the_name_the_fit_saw() {
        let rows = Frame::new(
            vec![
                ("region".to_owned(), Column::Str(vec![Some("south".into())])),
                ("area".to_owned(), Column::Float(vec![Some(3.0)])),
                ("sold".to_owned(), Column::Str(vec![Some("no".into())])),
            ],
            Vec::new(),
        )
        .expect("frame");
        let predictions = predict_rows(&registry(), "fixed_class", &instance(), &rows)
            .await
            .expect("predict");
        assert_eq!(predictions, vec![Prediction::class("yes", Some(0.75))]);
        // And it is a value a row can hold — an index never would have been.
        assert_eq!(
            prediction_values(&predictions).expect("values"),
            vec![Json::from("yes")]
        );
    }

    #[tokio::test]
    async fn a_category_the_fit_never_saw_is_refused_rather_than_predicted() {
        let rows = Frame::new(
            vec![
                ("region".to_owned(), Column::Str(vec![Some("west".into())])),
                ("area".to_owned(), Column::Float(vec![Some(3.0)])),
                ("sold".to_owned(), Column::Str(vec![Some("no".into())])),
            ],
            Vec::new(),
        )
        .expect("frame");
        let err = predict_rows(&registry(), "fixed_class", &instance(), &rows)
            .await
            .expect_err("unseen category");
        assert!(err.to_string().contains("`west`"), "{err}");
    }

    #[tokio::test]
    async fn an_instance_that_did_not_finish_says_so_rather_than_predicting() {
        let unfinished = ModelInstance::starting(ModelId::new());
        let err = predict_rows(&registry(), "fixed_class", &unfinished, &training())
            .await
            .expect_err("still fitting");
        assert!(err.to_string().contains("cannot predict"), "{err}");

        let failed = ModelInstance::starting(ModelId::new()).failed("the dataset selects no rows");
        let err = predict_rows(&registry(), "fixed_class", &failed, &training())
            .await
            .expect_err("failed");
        assert!(err.to_string().contains("selects no rows"), "{err}");
    }

    #[tokio::test]
    async fn a_hypothesis_test_has_no_per_row_answer_to_give() {
        let mut instance = instance();
        instance.attributes.insert(
            ATTR_OUTCOME.to_owned(),
            serde_json::to_value(Outcome::Test).expect("outcome"),
        );
        let err = predict_rows(&registry(), "fixed_class", &instance, &training())
            .await
            .expect_err("a test");
        assert!(err.to_string().contains("hypothesis test"), "{err}");
    }

    #[test]
    fn a_class_index_outside_the_fitted_classes_is_caught_and_not_translated() {
        let encoding = Encoding {
            columns: Vec::new(),
            target: Some(TargetEncoding {
                column: "sold".to_owned(),
                classes: Some(vec!["no".to_owned(), "yes".to_owned()]),
            }),
        };
        let err = name_classes(vec![Prediction::class_index(5, None)], &encoding)
            .expect_err("out of range");
        assert!(err.to_string().contains("2 classes"), "{err}");
        // Everything else passes through untouched.
        assert_eq!(
            name_classes(vec![Prediction::number(1.5)], &encoding).expect("passthrough"),
            vec![Prediction::number(1.5)]
        );
    }

    #[tokio::test]
    async fn an_instance_with_no_stored_encoding_cannot_be_applied() {
        let mut instance = instance();
        instance.encoding = Json::Null;
        let err = predict_rows(&registry(), "fixed_class", &instance, &training())
            .await
            .expect_err("no encoding");
        assert!(err.to_string().contains("no stored encoding"), "{err}");
    }
}
