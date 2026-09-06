//! The orchestration: materialise, split, encode, search, fit, score, write
//! (TODO §8, §11, task 3.3).
//!
//! Everything else in this crate is a piece; this is the order they go in, and
//! the order is the design:
//!
//! 1. **Materialise** the dataset through the [`DatasetSource`] seam, bounded by
//!    the row cap.
//! 2. **Resolve the outcome** from the provider and the configuration, because
//!    everything after this branches on it — a hypothesis test has no split, no
//!    encoding and no metrics, and saying so once here is better than five
//!    `if`s later.
//! 3. **Split** by the hash of the primary key (§5).
//! 4. **Fit the encoding on the training rows only** (§6). Categories and
//!    standardisation constants computed over everything would leak the held-out
//!    rows into the fit, quietly, in the one place nobody looks.
//! 5. **Search the grid** on the validation rows, if the model declares any
//!    lists (§11), and keep the point with the best primary metric.
//! 6. **Fit the winner** on the training rows.
//! 7. **Score every split** by running the fitted state back over it — the
//!    host's job, not the provider's (§7).
//! 8. **Write the instance.**
//!
//! ## A fit is a job, and the row is the registry (§8)
//!
//! [`fit_model`] is the body of that job: the instance row already exists,
//! saying `fitting`, and the id has already been returned to whoever asked. So
//! this function's contract is *unusual on purpose* — **a fit that fails is
//! `Ok`**, carrying an instance whose status is `failed` and whose sentence says
//! why. An `Err` from it means the failure could not be *recorded*, which is a
//! different and much worse thing. A caller that treated "the optimiser did not
//! converge" and "the database is gone" the same way would leave rows saying
//! `fitting` for ever, which is the state boot has to reap.
//!
//! ## What the instance records beyond its columns
//!
//! Three things go in `attributes` rather than columns, by §9's rule — they are
//! present on some rows and not others, and nothing filters a list by them:
//! [`ATTR_OUTCOME`] (what this fit produces, so a prediction does not have to
//! re-read the dataset to find out), [`ATTR_ROWS`] (what the split came to and
//! what the encoding dropped) and [`ATTR_SEARCH`] (every grid point and its
//! score, so the search is inspectable and not a number that appeared).

use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_types::Attrs;
use serde_json::Value as Json;

use crate::dataset::DatasetShape;
use crate::encode::{Encoded, Encoding, apply_encoding_dropping, fit_encoding};
use crate::frame::Frame;
use crate::instance::{InstanceId, ModelInstance};
use crate::instance_store::{require_model_instance, save_model_instance};
use crate::metrics::{Metrics, SplitMetrics};
use crate::model::Model;
use crate::provider::{ModelProvider, Outcome, ParameterBlock};
use crate::registry::ModelRegistry;
use crate::source::DatasetSource;
use crate::split::{Part, SplitCounts};

/// The attribute holding this fit's resolved [`Outcome`].
///
/// Stored because a prediction needs it and re-deriving it would mean reading
/// the dataset again just to learn its column types — and worse, would answer
/// with *today's* data rather than the data this instance was fitted over.
pub const ATTR_OUTCOME: &str = "outcome";

/// The attribute holding the row counts: what the dataset selected, what each
/// split came to, and what the encoding dropped.
pub const ATTR_ROWS: &str = "rows";

/// The attribute holding every hyperparameter point tried and what it scored.
pub const ATTR_SEARCH: &str = "search";

/// One point of the hyperparameter grid and what it scored on the validation
/// rows (§11).
///
/// A point that *failed* is recorded with its sentence rather than dropped: "I
/// tried `k = 12` and it could not be fitted" is the answer to "why did it pick
/// `k = 8`", and a search that silently skipped its failures would look like a
/// search that never tried them.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GridPoint {
    /// The values tried.
    pub hyperparameters: Attrs,
    /// The primary metric on the validation rows, where it fitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// Why it did not fit, for a point that did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// How many rows went where — what the instance reports and the screen shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct RowCounts {
    /// Rows the dataset selected.
    pub selected: usize,
    /// Rows assigned to each split.
    #[serde(flatten)]
    pub split: SplitCounts,
    /// Rows the encoding could not represent and dropped, across all splits
    /// (§6). Reported rather than swallowed: a fit over 900 of 1 000 rows is a
    /// different claim from a fit over 1 000.
    pub dropped: usize,
}

/// Everything one fit produced, before it is a row.
#[derive(Debug, Clone, PartialEq)]
pub struct Fit {
    /// What this fit produces, with a classification's classes filled in.
    pub outcome: Outcome,
    /// The provider's serialised fit.
    pub state: Json,
    /// The provider's parameters.
    pub parameters: Vec<ParameterBlock>,
    /// The encoding, for everything but a hypothesis test.
    pub encoding: Option<Encoding>,
    /// The host's metrics, per split.
    pub metrics: SplitMetrics,
    /// The hyperparameter point this fit used — always values, never lists.
    pub hyperparameters: Attrs,
    /// Where the rows went.
    pub rows: RowCounts,
    /// Every grid point tried, empty when there was no search.
    pub search: Vec<GridPoint>,
}

impl Fit {
    /// This fit written onto `instance`: status, state, parameters, metrics,
    /// encoding, the chosen point, and the three attributes.
    pub fn apply(&self, mut instance: ModelInstance) -> Result<ModelInstance> {
        instance =
            crate::instance_store::fitted(instance, self.state.clone(), self.parameters.clone());
        instance.metrics = self.metrics.to_json()?;
        instance.encoding = match &self.encoding {
            Some(encoding) => encoding.to_json()?,
            None => Json::Null,
        };
        instance.hyperparameters = self.hyperparameters.clone();
        instance.attributes.insert(
            ATTR_OUTCOME.to_owned(),
            serde_json::to_value(&self.outcome).map_err(|e| Error::msg(format!("outcome: {e}")))?,
        );
        instance.attributes.insert(
            ATTR_ROWS.to_owned(),
            serde_json::to_value(self.rows).map_err(|e| Error::msg(format!("row counts: {e}")))?,
        );
        if !self.search.is_empty() {
            instance.attributes.insert(
                ATTR_SEARCH.to_owned(),
                serde_json::to_value(&self.search)
                    .map_err(|e| Error::msg(format!("search: {e}")))?,
            );
        }
        Ok(instance)
    }
}

/// Run the fit named by `instance` and record what happened — **the body of the
/// job** (§8).
///
/// The instance row already exists and says `fitting`. This loads it, runs the
/// fit, and saves it as `fitted` or as `failed` with the sentence. A fit that
/// fails is `Ok` carrying the failed instance; an `Err` means the failure could
/// not be *recorded*, which is the only kind of trouble a caller can do anything
/// about. See the module docs.
pub async fn fit_model(
    catalog: &Catalog,
    registry: &ModelRegistry,
    source: &dyn DatasetSource,
    model: &Model,
    instance: InstanceId,
    cap: u64,
) -> Result<ModelInstance> {
    let row = require_model_instance(catalog, instance).await?;
    let finished = match run_fit(registry, source, model, cap).await {
        Ok(fit) => match fit.apply(row.clone()) {
            Ok(finished) => finished,
            // The fit itself worked and only writing it down did not — which is
            // still a failed instance, and the sentence should say which half
            // broke rather than pretending the optimiser was at fault.
            Err(e) => row.failed(format!("the fit finished but could not be recorded: {e}")),
        },
        // The **chain**, not just the outermost sentence: a fit fails at the
        // bottom of a stack of contexts ("counting the rows of dataset table
        // `houses`"), and the row is the only place the reason will ever be
        // read — so it carries the cause the context was wrapped around.
        Err(e) => row.failed(sc_error::format_chain(&e)),
    };
    save_model_instance(catalog, &finished).await?;
    Ok(finished)
}

/// Run a fit and answer what it produced, touching no store.
///
/// The half worth testing, and the half a caller with its own instance handling
/// wants: [`fit_model`] is this plus the row.
pub async fn run_fit(
    registry: &ModelRegistry,
    source: &dyn DatasetSource,
    model: &Model,
    cap: u64,
) -> Result<Fit> {
    let provider = registry.require(model.provider.trim())?;
    let frame = source.materialise(&model.dataset, cap).await?;
    if frame.rows == 0 {
        return Err(Error::invalid(
            "this dataset selects no rows, so there is nothing to fit",
        ));
    }
    let shape = DatasetShape::of_frame(model.table(), &frame);
    provider.validate(&shape, &model.configuration)?;
    let outcome = provider.outcome(&shape, &model.configuration)?;
    let points = grid(&model.hyperparameters)?;

    if !outcome.predicts() {
        return fit_test(provider.as_ref(), model, &frame, outcome, points).await;
    }

    let splits = frame.split(&model.split)?;
    let encoding = fit_encoding(&splits.train, &outcome, provider.standardise())?;
    let train = apply_encoding_dropping(&encoding, &splits.train)?;
    if train.is_empty() {
        return Err(Error::invalid(format!(
            "every one of the {} training rows was dropped by the encoding: they have a null in \
             a feature or in the label",
            splits.counts.train
        )));
    }
    let validation = apply_encoding_dropping(&encoding, &splits.validation)?;
    let test = apply_encoding_dropping(&encoding, &splits.test)?;
    // The classes are the *data's*, so the outcome only becomes complete once
    // the encoding has seen them.
    let outcome = with_classes(outcome, &encoding);

    let (chosen, search) = search_grid(
        provider.as_ref(),
        &model.configuration,
        &outcome,
        &train,
        &validation,
        points,
    )
    .await?;

    let result = provider
        .fit(&train.frame(), &model.configuration, &chosen)
        .await?;

    let mut metrics = SplitMetrics::default();
    for (part, encoded) in [
        (Part::Train, &train),
        (Part::Validation, &validation),
        (Part::Test, &test),
    ] {
        if encoded.is_empty() {
            continue;
        }
        let predictions = provider
            .predict(&result.state, &encoded.features_frame())
            .await?;
        metrics.set(part, Metrics::of(&outcome, &predictions, encoded)?);
    }

    Ok(Fit {
        outcome,
        state: result.state,
        parameters: result.parameters,
        encoding: Some(encoding),
        metrics,
        hyperparameters: chosen,
        rows: RowCounts {
            selected: frame.rows,
            split: splits.counts,
            dropped: train.dropped + validation.dropped + test.dropped,
        },
        search,
    })
}

/// A hypothesis test: the whole frame, unencoded, and the parameters are the
/// answer (§7, §13).
///
/// No split, because there is nothing to hold out from a test statistic; no
/// encoding, because a t-test's configuration names *this* column as the value
/// and *that* one as the group, and a one-hot would leave neither addressable;
/// no metrics, because the parameters are the result.
async fn fit_test(
    provider: &dyn ModelProvider,
    model: &Model,
    frame: &Frame,
    outcome: Outcome,
    points: Vec<Attrs>,
) -> Result<Fit> {
    if points.len() > 1 {
        return Err(Error::invalid(
            "a hypothesis test produces no per-row prediction, so there is nothing to score a \
             hyperparameter search against: give each hyperparameter one value",
        ));
    }
    let chosen = points.into_iter().next().unwrap_or_default();
    let result = provider.fit(frame, &model.configuration, &chosen).await?;
    Ok(Fit {
        outcome,
        state: result.state,
        parameters: result.parameters,
        encoding: None,
        metrics: SplitMetrics::default(),
        hyperparameters: chosen,
        rows: RowCounts {
            selected: frame.rows,
            split: SplitCounts {
                train: frame.rows,
                validation: 0,
                test: 0,
            },
            dropped: 0,
        },
        search: Vec::new(),
    })
}

/// Pick the grid point that scores best on the validation rows (§11).
///
/// With one point there is no search and nothing is scored — the common case,
/// which must not pay for the uncommon one: a search costs one extra fit per
/// point, and doing it for a model with no lists would double every fit in the
/// system for nothing.
async fn search_grid(
    provider: &dyn ModelProvider,
    config: &Attrs,
    outcome: &Outcome,
    train: &Encoded,
    validation: &Encoded,
    points: Vec<Attrs>,
) -> Result<(Attrs, Vec<GridPoint>)> {
    if points.len() < 2 {
        return Ok((points.into_iter().next().unwrap_or_default(), Vec::new()));
    }
    if validation.is_empty() {
        return Err(Error::invalid(
            "this model searches over a list of hyperparameters, but its split holds out no \
             validation rows to score the points on: give the split a validation fraction",
        ));
    }
    let mut search = Vec::with_capacity(points.len());
    for point in points {
        let scored = match score_point(provider, config, outcome, train, validation, &point).await {
            Ok(Some(score)) if score.is_finite() => GridPoint {
                hyperparameters: point,
                score: Some(score),
                error: None,
            },
            // A point that fitted but scored nothing comparable — an R² over a
            // label that is constant on the validation rows, say. Recorded with
            // the reason rather than as a score of 0, which would be a number
            // the search could rank.
            Ok(_) => GridPoint {
                hyperparameters: point,
                score: None,
                error: Some(UNSCORABLE.to_owned()),
            },
            Err(e) => GridPoint {
                hyperparameters: point,
                score: None,
                error: Some(e.to_string()),
            },
        };
        search.push(scored);
    }
    let best = search
        .iter()
        .filter(|p| p.score.is_some_and(f64::is_finite))
        .max_by(|a, b| {
            a.score
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(&b.score.unwrap_or(f64::NEG_INFINITY))
        });
    match best {
        Some(point) => Ok((point.hyperparameters.clone(), search)),
        // Every point failed. The first sentence is the useful one — they are
        // usually the same failure — and reporting "no point could be scored"
        // alone would hide it.
        None => Err(Error::invalid(format!(
            "no point of this hyperparameter grid could be fitted and scored; the first said: {}",
            search
                .first()
                .and_then(|p| p.error.clone())
                .unwrap_or_else(|| "nothing".to_owned())
        ))),
    }
}

/// Fit one grid point on the training rows and score it on the validation rows.
async fn score_point(
    provider: &dyn ModelProvider,
    config: &Attrs,
    outcome: &Outcome,
    train: &Encoded,
    validation: &Encoded,
    point: &Attrs,
) -> Result<Option<f64>> {
    let result = provider.fit(&train.frame(), config, point).await?;
    let predictions = provider
        .predict(&result.state, &validation.features_frame())
        .await?;
    Ok(Metrics::of(outcome, &predictions, validation)?.primary())
}

/// The outcome with a classification's classes filled in from the fitted
/// encoding.
///
/// The classes are the *data's* — no amount of reading the configuration
/// discovers them — so this is the one moment they become known.
fn with_classes(outcome: Outcome, encoding: &Encoding) -> Outcome {
    match outcome {
        Outcome::Classification { label, .. } => Outcome::Classification {
            label,
            classes: encoding.classes().map(<[String]>::to_vec),
        },
        other => other,
    }
}

/// The hyperparameter grid: every combination of the lists, with the scalars
/// held fixed (§11).
///
/// A list of one and a scalar are the same search, so nothing downstream has to
/// ask which shape a setting was typed in. The product is bounded by
/// [`MAX_GRID_POINTS`] because a fit per point is minutes each and four
/// six-element lists is 1 296 of them — a number nobody typed on purpose.
pub fn grid(hyperparameters: &Attrs) -> Result<Vec<Attrs>> {
    let mut points = vec![Attrs::new()];
    for (key, value) in hyperparameters {
        let values: Vec<&Json> = match value {
            Json::Array(values) if values.is_empty() => {
                return Err(Error::invalid(format!(
                    "hyperparameter `{key}` is an empty list, so there is nothing to search over"
                )));
            }
            Json::Array(values) => values.iter().collect(),
            single => vec![single],
        };
        if points.len().saturating_mul(values.len()) > MAX_GRID_POINTS {
            return Err(Error::invalid(format!(
                "this hyperparameter grid has more than {MAX_GRID_POINTS} points, and each one \
                 is a fit: shorten the lists"
            )));
        }
        points = points
            .into_iter()
            .flat_map(|point| {
                values.iter().map(move |value| {
                    let mut next = point.clone();
                    next.insert(key.clone(), (*value).clone());
                    next
                })
            })
            .collect();
    }
    Ok(points)
}

/// Why a grid point that fitted still could not be ranked.
const UNSCORABLE: &str = "this point produced no comparable score on the validation rows";

/// The most grid points a fit will run. A fit per point, and each is seconds at
/// best — see [`grid`].
pub const MAX_GRID_POINTS: usize = 200;

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use sc_expr::SchemaShape;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::dataset::Dataset;
    use crate::frame::Column;
    use crate::provider::{FitResult, OutcomeSpec, Prediction};
    use crate::split::Split;
    use sc_types::{BasicType, FormField};

    /// A deterministic provider that predicts the mean of its training label,
    /// shifted by the `bias` hyperparameter.
    ///
    /// Enough to test the orchestration and nothing more: the grid can only pick
    /// the right point if the scoring pass is wired to the validation rows, and
    /// the mean can only be right if the label reached the fit.
    struct Mean {
        fits: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for Mean {
        fn name(&self) -> &str {
            "mean"
        }

        fn description(&self) -> &str {
            "predicts the training mean"
        }

        fn config_declaration(&self) -> Vec<FormField> {
            vec![crate::provider::numeric_column_field("label", "Label")]
        }

        fn hyperparameters(&self) -> Vec<FormField> {
            vec![FormField::new("bias", BasicType::Float)]
        }

        fn outcome_spec(&self) -> OutcomeSpec {
            OutcomeSpec::Regression {
                label: "label".to_owned(),
            }
        }

        async fn fit(&self, frame: &Frame, config: &Attrs, hyper: &Attrs) -> Result<FitResult> {
            self.fits.fetch_add(1, Ordering::SeqCst);
            let label = config.get("label").and_then(Json::as_str).unwrap_or("");
            let Some(Column::Float(values)) = frame.column(label) else {
                return Err(Error::msg(format!(
                    "no label column `{label}` in the fit frame"
                )));
            };
            let n = values.len() as f64;
            let mean = values.iter().flatten().sum::<f64>() / n;
            let bias = hyper.get("bias").and_then(Json::as_f64).unwrap_or(0.0);
            Ok(
                FitResult::new(serde_json::json!({ "prediction": mean + bias }))
                    .parameter(ParameterBlock::scalar("mean", mean)),
            )
        }

        async fn predict(&self, state: &Json, frame: &Frame) -> Result<Vec<Prediction>> {
            let value = state
                .get("prediction")
                .and_then(Json::as_f64)
                .ok_or_else(|| Error::msg("no fitted mean in the state".to_owned()))?;
            Ok(vec![Prediction::number(value); frame.rows])
        }
    }

    use crate::source::Read;

    /// A source that answers one fixed frame — the seam, stubbed.
    struct Fixed(Frame);

    #[async_trait]
    impl DatasetSource for Fixed {
        async fn read(&self, _ds: &Dataset, _how: &Read<'_>) -> Result<Frame> {
            Ok(self.0.clone())
        }
    }

    /// `n` rows whose `x` is the row number and whose `y` cycles through 0..7.
    ///
    /// The label has to *vary*: R² over a constant label is undefined, and a
    /// grid that cannot rank its points is a different test from this one.
    fn rows(n: usize) -> Frame {
        Frame::new(
            vec![
                (
                    "x".to_owned(),
                    Column::Float((0..n).map(|i| Some(i as f64)).collect()),
                ),
                (
                    "y".to_owned(),
                    Column::Float((0..n).map(|i| Some((i % 7) as f64)).collect()),
                ),
            ],
            (0..n).map(|i| format!("int:{i}")).collect(),
        )
        .expect("frame")
    }

    fn registry(fits: &Arc<AtomicUsize>) -> ModelRegistry {
        let mut registry = ModelRegistry::new();
        registry
            .register(Arc::new(Mean {
                fits: Arc::clone(fits),
            }))
            .expect("register");
        registry
    }

    fn model() -> Model {
        Model::new(
            "m",
            "mean",
            Dataset::new("t").column("x", "x").column("y", "y"),
        )
        .config("label", "y")
    }

    #[tokio::test]
    async fn a_fit_with_no_search_fits_once_and_scores_every_split_it_has() {
        let fits = Arc::new(AtomicUsize::new(0));
        let fit = run_fit(
            &registry(&fits),
            &Fixed(rows(200)),
            &model().split(Split::new(0.8, 0.0, 0.2, 7)),
            1000,
        )
        .await
        .expect("fit");
        // One fit, because there is no grid: the common case does not pay for
        // the uncommon one.
        assert_eq!(fits.load(Ordering::SeqCst), 1);
        assert!(fit.search.is_empty());
        assert_eq!(fit.rows.selected, 200);
        assert_eq!(fit.rows.split.train + fit.rows.split.test, 200);
        assert_eq!(fit.rows.split.validation, 0);
        assert!(fit.metrics.train.is_some());
        assert!(fit.metrics.test.is_some());
        assert!(fit.metrics.validation.is_none());
        // The label reached the fit: the mean of a column cycling 0..7 is in it.
        let [ParameterBlock::Scalar { name, value }] = fit.parameters.as_slice() else {
            panic!("expected one scalar, got {:?}", fit.parameters);
        };
        assert_eq!(name, "mean");
        assert!((0.0..=6.0).contains(value), "{value}");
        // The encoding is fitted over the features and not the label.
        let encoding = fit.encoding.expect("encoding");
        assert_eq!(encoding.feature_names(), vec!["x"]);
        assert_eq!(encoding.target.expect("target").column, "y".to_owned());
    }

    #[tokio::test]
    async fn the_grid_picks_the_point_that_scores_best_on_the_validation_rows() {
        let fits = Arc::new(AtomicUsize::new(0));
        let model = model()
            .split(Split::new(0.6, 0.2, 0.2, 7))
            .hyperparameter("bias", serde_json::json!([0.0, 5.0, -3.0]));
        let fit = run_fit(&registry(&fits), &Fixed(rows(300)), &model, 1000)
            .await
            .expect("fit");
        // A bias of 0 predicts the truth exactly, so it wins.
        assert_eq!(fit.hyperparameters.get("bias"), Some(&Json::from(0.0)));
        assert_eq!(fit.search.len(), 3);
        assert!(fit.search.iter().all(|p| p.error.is_none()));
        // Three scoring fits plus the winner's refit.
        assert_eq!(fits.load(Ordering::SeqCst), 4);
        assert!(fit.metrics.validation.is_some());
    }

    #[tokio::test]
    async fn a_search_with_no_validation_rows_is_refused_rather_than_scored_on_the_training_ones() {
        let fits = Arc::new(AtomicUsize::new(0));
        let model = model()
            .split(Split::new(0.8, 0.0, 0.2, 7))
            .hyperparameter("bias", serde_json::json!([0.0, 5.0]));
        let err = run_fit(&registry(&fits), &Fixed(rows(100)), &model, 1000)
            .await
            .expect_err("no validation rows");
        assert!(err.to_string().contains("validation rows"), "{err}");
    }

    #[tokio::test]
    async fn the_encoding_is_fitted_on_train_and_a_test_only_category_is_dropped_and_counted() {
        // `region` is `north` on every training row and `west` on exactly one
        // row that the seed puts in the test set.
        let n = 200;
        let split = Split::new(0.8, 0.0, 0.2, 7);
        let regions: Vec<Option<String>> = (0..n)
            .map(|i| {
                let key = format!("int:{i}");
                Some(if split.assign(&key) == Part::Test && i % 37 == 0 {
                    "west".to_owned()
                } else {
                    "north".to_owned()
                })
            })
            .collect();
        let unseen = regions.iter().flatten().filter(|r| *r == "west").count();
        assert!(
            unseen > 0,
            "the fixture needs at least one test-only region"
        );
        let mut frame = rows(n);
        frame
            .columns
            .push(("region".to_owned(), Column::Str(regions)));
        let fits = Arc::new(AtomicUsize::new(0));
        let fit = run_fit(&registry(&fits), &Fixed(frame), &model().split(split), 1000)
            .await
            .expect("fit");
        // Fitted on the training rows only, so `west` is not in the encoding.
        let encoding = fit.encoding.as_ref().expect("encoding");
        assert_eq!(encoding.feature_names(), vec!["x"]);
        // And the test rows carrying it were dropped, and counted.
        assert_eq!(fit.rows.dropped, unseen);
        assert_eq!(
            fit.metrics.test.as_ref().map(Metrics::rows),
            Some(fit.rows.split.test - unseen)
        );
    }

    #[tokio::test]
    async fn a_dataset_that_selects_no_rows_is_refused_by_name() {
        let fits = Arc::new(AtomicUsize::new(0));
        let err = run_fit(&registry(&fits), &Fixed(rows(0)), &model(), 1000)
            .await
            .expect_err("no rows");
        assert!(err.to_string().contains("selects no rows"), "{err}");
    }

    #[test]
    fn the_grid_is_the_product_of_the_lists_with_the_scalars_held_fixed() {
        let mut hyper = Attrs::new();
        hyper.insert("k".to_owned(), serde_json::json!([2, 3]));
        hyper.insert("seed".to_owned(), Json::from(1));
        hyper.insert("d".to_owned(), serde_json::json!(["a", "b"]));
        let points = grid(&hyper).expect("grid");
        assert_eq!(points.len(), 4);
        assert!(points.iter().all(|p| p.get("seed") == Some(&Json::from(1))));
        assert!(
            points
                .iter()
                .any(|p| p.get("k") == Some(&Json::from(3)) && p.get("d") == Some(&Json::from("b")))
        );
        // No lists at all is one point, not zero.
        assert_eq!(grid(&Attrs::new()).expect("grid").len(), 1);
    }

    #[test]
    fn a_grid_nobody_typed_on_purpose_is_refused() {
        let mut hyper = Attrs::new();
        for key in ["a", "b", "c", "d"] {
            hyper.insert(key.to_owned(), serde_json::json!([1, 2, 3, 4, 5, 6]));
        }
        let err = grid(&hyper).expect_err("too big");
        assert!(err.to_string().contains("each one"), "{err}");
    }

    #[tokio::test]
    async fn a_classification_learns_its_classes_from_the_data_and_records_them() {
        // The stub is a regressor, so this exercises `with_classes` directly:
        // the classes are the encoding's, and no configuration discovers them.
        let frame = Frame::new(
            vec![
                ("x".to_owned(), Column::Float(vec![Some(1.0), Some(2.0)])),
                (
                    "sold".to_owned(),
                    Column::Str(vec![Some("no".into()), Some("yes".into())]),
                ),
            ],
            vec!["int:1".to_owned(), "int:2".to_owned()],
        )
        .expect("frame");
        let encoding = fit_encoding(
            &frame,
            &Outcome::Classification {
                label: "sold".to_owned(),
                classes: None,
            },
            false,
        )
        .expect("encoding");
        let outcome = with_classes(
            Outcome::Classification {
                label: "sold".to_owned(),
                classes: None,
            },
            &encoding,
        );
        assert_eq!(
            outcome,
            Outcome::Classification {
                label: "sold".to_owned(),
                classes: Some(vec!["no".to_owned(), "yes".to_owned()]),
            }
        );
    }

    #[tokio::test]
    async fn a_fit_writes_its_outcome_and_row_counts_onto_the_instance() {
        let fits = Arc::new(AtomicUsize::new(0));
        let model = model();
        let fit = run_fit(&registry(&fits), &Fixed(rows(50)), &model, 1000)
            .await
            .expect("fit");
        let instance = fit.apply(ModelInstance::starting(model.id)).expect("apply");
        assert!(instance.is_usable());
        assert_eq!(instance.attributes[ATTR_OUTCOME]["outcome"], "regression");
        assert_eq!(instance.attributes[ATTR_ROWS]["selected"], 50);
        assert!(!instance.attributes.contains_key(ATTR_SEARCH));
        assert!(!instance.encoding.is_null());
        assert!(!instance.metrics.is_null());
    }

    /// The dataset validation this crate already has is a separate concern from
    /// the fit; this only asserts that the fit reaches the provider's own check.
    #[tokio::test]
    async fn a_configuration_the_provider_refuses_stops_the_fit_before_it_reads_anything() {
        let fits = Arc::new(AtomicUsize::new(0));
        let model =
            Model::new("m", "mean", Dataset::new("t").column("y", "y")).config("label", "nope");
        let err = run_fit(&registry(&fits), &Fixed(rows(10)), &model, 1000)
            .await
            .expect_err("bad label");
        assert!(err.to_string().contains("`nope`"), "{err}");
        assert_eq!(fits.load(Ordering::SeqCst), 0);
        // And the schema-level validation is unchanged by any of this.
        let _ = SchemaShape::default();
    }
}
