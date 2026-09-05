//! `kmeans` — clustering, where the answer is a number that means a group
//! (TODO task 4.5).
//!
//! The simplest of the built-ins and the one that shows the unsupervised shape
//! most clearly: there is no label, every column of the dataset is a feature,
//! and what a fitted instance answers for a row is which cluster it fell in.
//!
//! ## It asks the host to standardise, and that is why the flag exists
//!
//! k-means minimises squared Euclidean distance, so a column measured in metres
//! and a column measured in millimetres are not equally important to it — the
//! second dominates by a factor of a million, and the clusters it finds are the
//! clusters of that one column. [`ModelProvider::standardise`] is therefore
//! `true` here, and the centring and scaling constants are the *host's*: they go
//! on the instance's encoding, so a prediction is scaled exactly as its fit was
//! (§6). A provider that standardised privately would be a second, unrecorded
//! encoding, and the row it was asked about next month would be scaled by
//! today's constants.
//!
//! ## The centres are the assignment's, not the library's
//!
//! smartcore keeps its centroids private, so the parameter table is computed
//! from the fitted assignment over the training rows: the size of a cluster is
//! how many rows fell in it and its centre is their mean, which is the fixed
//! point k-means converges to. A cluster nothing fell in has a size of 0 and no
//! centre, reported as such rather than as a row of zeros — an empty cluster is
//! the answer to "why did asking for eight give me six", and zeros would look
//! like a cluster at the origin.

use async_trait::async_trait;
use sc_error::{Error, Result};
use sc_types::{Attrs, BasicType, FormField};
use serde_json::Value as Json;
use smartcore::cluster::kmeans::{KMeans as ScKMeans, KMeansParameters};
use smartcore::linalg::basic::matrix::DenseMatrix;

use crate::encode::Matrix;
use crate::frame::Frame;
use crate::provider::{
    FitResult, ModelProvider, OutcomeSpec, ParameterBlock, ParameterRow, Prediction,
};
use crate::providers::{cell, design, feature_names, from_state, to_state, whole_setting};

/// The hyperparameter holding how many clusters to find.
const K: &str = "k";
/// The hyperparameter bounding the iteration.
const MAX_ITER: &str = "max_iter";
/// The configuration key holding the seed the initial centres are drawn with.
const SEED: &str = "seed";

/// The smartcore model this provider fits and applies.
type Model = ScKMeans<f64, i64, DenseMatrix<f64>, Vec<i64>>;

/// A fitted clustering, as it is stored.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct State {
    /// The feature columns, in the order the centres index them.
    features: Vec<String>,
    /// smartcore's serialised model.
    model: Json,
}

/// k-means clustering.
pub struct KMeans;

#[async_trait]
impl ModelProvider for KMeans {
    fn name(&self) -> &str {
        "kmeans"
    }

    fn description(&self) -> &str {
        "k-means clustering: every row gets a cluster number, and every cluster a centre"
    }

    fn config_declaration(&self) -> Vec<FormField> {
        vec![
            FormField::new(SEED, BasicType::Int)
                .label("Random seed")
                .default_value(0),
        ]
    }

    fn hyperparameters(&self) -> Vec<FormField> {
        vec![
            FormField::new(K, BasicType::Int)
                .label("Clusters")
                .default_value(3),
            FormField::new(MAX_ITER, BasicType::Int)
                .label("Maximum iterations")
                .default_value(100),
        ]
    }

    fn outcome_spec(&self) -> OutcomeSpec {
        OutcomeSpec::Cluster
    }

    fn standardise(&self) -> bool {
        true
    }

    async fn fit(&self, frame: &Frame, config: &Attrs, hyper: &Attrs) -> Result<FitResult> {
        let seed = whole_setting(config, SEED, 0, 0)? as u64;
        let k = whole_setting(hyper, K, 3, 2)?;
        let max_iter = whole_setting(hyper, MAX_ITER, 100, 1)?;
        let features = feature_names(frame, None);
        let x = design(frame, &features)?;
        if x.rows() < k {
            return Err(Error::invalid(format!(
                "`{K}`: {k} clusters cannot be found in {} rows",
                x.rows()
            )));
        }
        let matrix = dense(&x)?;
        let parameters = KMeansParameters::default()
            .with_k(k)
            .with_max_iter(max_iter);
        let model: Model = ScKMeans::fit(
            &matrix,
            KMeansParameters {
                seed: Some(seed),
                ..parameters
            },
        )
        .map_err(|e| Error::invalid(format!("this clustering could not be fitted: {e}")))?;
        let assignments = assign(&model, &matrix)?;

        let state = State {
            features,
            model: to_state(&model)?,
        };
        Ok(FitResult::new(to_state(&state)?)
            .parameter(centres(&x, &assignments, k)?)
            .parameter(ParameterBlock::scalar("clusters", k as f64))
            .parameter(ParameterBlock::scalar("observations", x.rows() as f64)))
    }

    async fn predict(&self, state: &Json, frame: &Frame) -> Result<Vec<Prediction>> {
        let state: State = from_state(self.name(), state)?;
        let x = design(frame, &state.features)?;
        let model: Model = from_state(self.name(), &state.model)?;
        Ok(assign(&model, &dense(&x)?)?
            .into_iter()
            .map(|cluster| Prediction::Cluster { cluster })
            .collect())
    }
}

/// A feature matrix as smartcore's.
fn dense(x: &Matrix) -> Result<DenseMatrix<f64>> {
    DenseMatrix::from_2d_vec(&x.to_rows())
        .map_err(|e| Error::msg(format!("this fit's design matrix could not be built: {e}")))
}

/// Which cluster each row falls in.
fn assign(model: &Model, matrix: &DenseMatrix<f64>) -> Result<Vec<usize>> {
    model
        .predict(matrix)
        .map_err(|e| Error::invalid(format!("this clustering could not be applied: {e}")))?
        .into_iter()
        .map(|c| {
            usize::try_from(c)
                .map_err(|_| Error::msg(format!("this clustering answered cluster {c}")))
        })
        .collect()
}

/// The size and the centre of every cluster, as the screen shows them.
fn centres(x: &Matrix, assignments: &[usize], k: usize) -> Result<ParameterBlock> {
    let width = x.width();
    let mut sums = vec![vec![0.0f64; width]; k];
    let mut sizes = vec![0usize; k];
    for (i, cluster) in assignments.iter().enumerate() {
        let Some(row) = x.row(i) else { continue };
        let Some(sum) = sums.get_mut(*cluster) else {
            continue;
        };
        for (total, value) in sum.iter_mut().zip(row) {
            *total += value;
        }
        sizes[*cluster] += 1;
    }
    let rows = (0..k)
        .map(|c| {
            let mut cells = vec![Json::from(c), Json::from(sizes[c])];
            for value in &sums[c] {
                // An empty cluster has no centre: null, not zero.
                cells.push(if sizes[c] == 0 {
                    Json::Null
                } else {
                    cell(value / sizes[c] as f64)
                });
            }
            ParameterRow { cells }
        })
        .collect();
    let mut columns = vec!["cluster".to_owned(), "size".to_owned()];
    columns.extend(x.columns().iter().cloned());
    ParameterBlock::table("Cluster centres", columns, rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::testing::{attrs, close, floats, frame, number, scalar, table};

    /// Three blobs of four points each, far enough apart that the right answer
    /// is not a matter of opinion: around (0, 0), (10, 10) and (0, 10).
    fn blobs() -> crate::frame::Frame {
        frame(vec![
            (
                "x",
                floats(&[
                    0., 1., 0., 1., //
                    10., 11., 10., 11., //
                    0., 1., 0., 1.,
                ]),
            ),
            (
                "y",
                floats(&[
                    0., 0., 1., 1., //
                    10., 10., 11., 11., //
                    10., 10., 11., 11.,
                ]),
            ),
        ])
    }

    #[tokio::test]
    async fn three_obvious_blobs_come_back_as_three_clusters() {
        let data = blobs();
        let config = attrs(&[("seed", 7.into())]);
        let fit = KMeans
            .fit(&data, &config, &attrs(&[("k", 3.into())]))
            .await
            .unwrap();
        let predictions = KMeans.predict(&fit.state, &data).await.unwrap();
        let clusters: Vec<usize> = predictions
            .iter()
            .map(|p| match p {
                Prediction::Cluster { cluster } => *cluster,
                other => panic!("a clustering answers a cluster, not {other:?}"),
            })
            .collect();
        assert_eq!(clusters.len(), 12);

        // Every blob is one cluster, and no two blobs share one. The *numbers*
        // are the fit's own — a clustering has no natural labelling — so the
        // assertion is about the partition and not about which number a blob got.
        for blob in clusters.chunks(4) {
            assert!(
                blob.iter().all(|c| *c == blob[0]),
                "a blob was split across clusters: {clusters:?}"
            );
        }
        let mut distinct: Vec<usize> = clusters.chunks(4).map(|b| b[0]).collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            3,
            "two blobs shared a cluster: {clusters:?}"
        );

        // Each cluster holds one blob, and its centre is that blob's own middle.
        let (columns, rows) = table(&fit.parameters, "Cluster centres");
        assert_eq!(columns, ["cluster", "size", "x", "y"]);
        assert_eq!(rows.len(), 3);
        let mut centres: Vec<(f64, f64)> = rows
            .iter()
            .map(|row| {
                assert_eq!(number(&row[1]), 4.0, "every blob has four points");
                (number(&row[2]), number(&row[3]))
            })
            .collect();
        centres.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let expected = [(0.5, 0.5), (0.5, 10.5), (10.5, 10.5)];
        for ((x, y), (ex, ey)) in centres.iter().zip(expected) {
            close(*x, ex, 1e-9);
            close(*y, ey, 1e-9);
        }
        close(scalar(&fit.parameters, "clusters"), 3.0, 0.0);
        close(scalar(&fit.parameters, "observations"), 12.0, 0.0);
    }

    /// The same seed and the same rows give the same partition, which is what
    /// makes two instances of one model comparable at all.
    #[tokio::test]
    async fn a_seeded_fit_is_the_same_fit_twice() {
        let data = blobs();
        let config = attrs(&[("seed", 42.into())]);
        let hyper = attrs(&[("k", 3.into())]);
        let first = KMeans.fit(&data, &config, &hyper).await.unwrap();
        let second = KMeans.fit(&data, &config, &hyper).await.unwrap();
        assert_eq!(
            KMeans.predict(&first.state, &data).await.unwrap(),
            KMeans.predict(&second.state, &data).await.unwrap()
        );
    }

    /// More clusters than rows is not a fit with empty clusters, it is a
    /// question with no answer — refused by name.
    #[tokio::test]
    async fn more_clusters_than_rows_is_refused_by_name() {
        let two = frame(vec![("x", floats(&[1., 2.])), ("y", floats(&[1., 2.]))]);
        let err = KMeans
            .fit(&two, &Attrs::new(), &attrs(&[("k", 5.into())]))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("`k`") && err.contains("2 rows"), "{err}");
    }

    /// A clustering asks the host to standardise, and that is a declaration the
    /// registry and the instance both read — so it is asserted here rather than
    /// left to whoever writes the fit.
    #[test]
    fn it_asks_the_host_to_standardise() {
        assert!(KMeans.standardise());
        assert!(KMeans.kind().standardise);
        assert_eq!(KMeans.outcome_spec(), OutcomeSpec::Cluster);
    }
}
