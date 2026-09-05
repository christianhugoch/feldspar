//! Predictive models: the pieces `sc-model` declares but cannot supply
//! (TODO "Predictive models", §4).
//!
//! `sc-model` is at layer 6 so a module can supply a model provider — the same
//! placement argument `sc-action` carries — and the price is that it cannot read
//! a row: `sc_api::rows` is layer 8. So it declares
//! [`DatasetSource`](sc_model::DatasetSource) and this is where the seam is
//! filled in, exactly as `sc-agent` declares `ProviderConnector` and
//! [`agents`](crate::agents) supplies it.
//!
//! Reading **through the row layer** rather than around it is the whole point of
//! the seam. A dataset that issued its own `SELECT` would see no non-stored
//! calculated field, would ignore ownership and row-level security, and could
//! not read a table a module provides at all — three ways for a fit to be
//! computed over rows that are not the rows the application has.
//!
//! Phase 2 adds [`install_models`], which is the other half a server owes this
//! crate: the two tables exist, and a fit that was running when the process died
//! is failed rather than left saying `fitting` for ever. The registry and the fit
//! runner join them in later phases.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use sc_api::rows::{RowQuery, count_rows_where, list_row_values};
use sc_catalog::Catalog;
use sc_error::{Context, Error, Result};
use sc_model::{
    Column, Dataset, DatasetSource, Frame, SPLIT_KEY, bootstrap_model_instances, bootstrap_models,
    canonical_key, reap_fitting_instances,
};
use sc_query::{Expr, Projection, Value};

/// The [`DatasetSource`] a running server has: the catalog, read through
/// `sc_api::rows`.
pub struct CatalogDatasetSource {
    catalog: Arc<Catalog>,
}

impl CatalogDatasetSource {
    /// A source over `catalog`.
    pub fn new(catalog: Arc<Catalog>) -> CatalogDatasetSource {
        CatalogDatasetSource { catalog }
    }
}

#[async_trait]
impl DatasetSource for CatalogDatasetSource {
    async fn materialise(&self, ds: &Dataset, cap: u64) -> Result<Frame> {
        let table = self.catalog.require(&ds.table)?;
        let shape = self.catalog.schema_shape()?;
        let filter = ds.filter_expr(&shape)?;

        // The count first, and on purpose: a dataset is a `SELECT` an admin
        // wrote and the server has to hold the answer in memory, so the refusal
        // costs one `COUNT(*)` rather than a partial read that has already
        // allocated most of what it would have refused.
        let count = count_rows_where(&self.catalog, &table, filter.clone(), None)
            .await
            .with_context(|| format!("counting the rows of dataset table `{}`", ds.table))?;
        if count > 0 && cap < count as u64 {
            return Err(Error::invalid(format!(
                "the dataset selects more than {cap} rows (it selects {count}); \
                 add a filter or raise `--model-max-rows`"
            )));
        }

        // The split key rides along as a reserved projection rather than as the
        // primary-key column's own name: a dataset column may legitimately be
        // *called* `id` while computing something else, and a split that hashed
        // that would be a split over the wrong thing. A table with a composite
        // or absent primary key simply has no key column — reads are unaffected,
        // and only the split refuses (§5).
        let mut projections = ds.projections(&shape)?;
        let key_column = ds.primary_key(&shape).ok();
        if let Some(pk) = &key_column {
            projections.push(Projection::expr_as(
                Expr::qcol(ds.table.clone(), pk.clone()),
                SPLIT_KEY,
            ));
        }

        let query = RowQuery::new().where_(filter).projecting(projections);
        let rows = list_row_values(&self.catalog, &table, &query, None)
            .await
            .with_context(|| format!("reading dataset table `{}`", ds.table))?;

        // A frame of no rows is a real answer — a filter that matched nothing —
        // and it keeps its columns: a frame with none would fail later as a
        // shape error rather than here as an empty fit. There is also nothing to
        // check translation against, since a column is "present" only by having
        // arrived on some row.
        if rows.is_empty() {
            return Frame::new(
                ds.columns
                    .iter()
                    .map(|c| (c.name.clone(), Column::Null(0)))
                    .collect(),
                Vec::new(),
            );
        }

        let mut columns = Vec::with_capacity(ds.columns.len());
        let mut arrived: BTreeSet<&str> = BTreeSet::new();
        for column in &ds.columns {
            // A column absent from every row is one whose formula did not
            // translate: `Dataset::projections` leaves those to the reified
            // evaluator, which the read path does not yet run.
            if rows.iter().all(|row| !row.contains_key(&column.name)) {
                continue;
            }
            arrived.insert(column.name.as_str());
            columns.push((
                column.name.clone(),
                Column::from_values(
                    rows.iter()
                        .map(|row| row.get(&column.name).cloned().unwrap_or(Value::Null))
                        .collect(),
                ),
            ));
        }
        // Saying which column and why, rather than handing back a frame that is
        // quietly missing it — a fit over the columns that happened to arrive
        // would not be the fit the model asked for.
        let missing = ds.missing(&arrived);
        if !missing.is_empty() {
            return Err(Error::invalid(format!(
                "dataset on `{}`: the formula for {} does not translate to SQL, and the \
                 dataset read has no reified fallback",
                ds.table,
                missing
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }

        let keys = match &key_column {
            Some(_) => rows
                .iter()
                .map(|row| canonical_key(row.get(SPLIT_KEY).unwrap_or(&Value::Null)))
                .collect(),
            None => Vec::new(),
        };
        Frame::new(columns, keys)
    }
}

/// Ensure the two model tables exist, and **reap every fit that was running
/// when this process last stopped** (TODO §8).
///
/// A fit is a job whose registry is its row: `fitModel` writes the instance
/// first, returns its id, and runs the work on a spawned task. Nothing survives
/// a restart, so an instance still saying `fitting` at boot is one nothing will
/// ever finish — and leaving it that way would show an admin a fit in progress
/// that is not. It is failed by name instead, with the sentence saying what
/// happened. Making a fit durable is the workflow engine's job and would mean
/// expressing a fit as a workflow, which is a bigger claim than this milestone
/// makes.
///
/// Runs before anything can read an instance, for that reason. It carries no
/// registry yet: there is nothing to fit with until the built-in providers land
/// (Phase 4) and nothing to fit from until the API does (Phase 5).
pub async fn install_models(catalog: &Arc<Catalog>) -> Result<()> {
    bootstrap_models(catalog)
        .await
        .context("ensuring the models table exists")?;
    bootstrap_model_instances(catalog)
        .await
        .context("ensuring the model instances table exists")?;
    let reaped = reap_fitting_instances(catalog)
        .await
        .context("failing the fits that were running at the last shutdown")?;
    if reaped > 0 {
        eprintln!(
            "feldspar: {reaped} model fit(s) were running when the server last stopped and \
             have been marked failed"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_split_key_is_a_reserved_alias_and_not_a_dataset_column_name() {
        // A dataset column called `id` computes whatever its formula says; the
        // key is projected separately so the two cannot be confused.
        assert!(SPLIT_KEY.starts_with("_sc_"));
        let ds = Dataset::new("houses").column("id", "bedrooms");
        assert!(ds.columns.iter().all(|c| c.name != SPLIT_KEY));
    }
}
