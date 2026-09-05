//! Predictive models (layer 6; TODO "Predictive models", which replaces the
//! technical design's §14.2).
//!
//! Everything else in this system *retrieves*. This crate is the half that
//! answers what the data **implies**: a [`Dataset`] is a saved question about a
//! table — which rows, and which derived values — and a model provider answers
//! it, leaving a fitted instance behind that is both inspected (the
//! coefficients, the test statistic) and applied (a predicted price on a row a
//! trigger just inserted).
//!
//! Phase 1 is the data half of that: what a dataset is, what it becomes as SQL,
//! what comes back ([`Frame`]), and how the rows divide into train, validation
//! and test ([`Split`]). The provider trait, the registry, the stores, the
//! encoding and the fit follow in later phases.
//!
//! ## Layering: why this is at layer 6 and not above the row layer
//!
//! Its data comes from `sc-api::rows`, which is layer 8, so the obvious place
//! for this crate is above it. It is here instead for the reason `sc-action` is
//! here: **a module supplies model providers** the way it supplies actions and
//! table providers, and `sc-module` (layer 6) can only implement a trait
//! declared *below* it. `TableProviderHost` — declared in `sc-catalog` at layer
//! 4, implemented in `sc-module` at layer 6 — is the same shape.
//!
//! The price is that this crate cannot read a row, and it does not pretend
//! otherwise: reading is a **seam** ([`DatasetSource`]) that somebody above the
//! row layer fills in (`sc_server::models::CatalogDatasetSource`), exactly as
//! `sc-agent` declares `ProviderConnector` and `sc-server` supplies it. Going
//! around the row layer instead would mean a dataset that ignored non-stored
//! calculated fields, ownership and row-level security, and that could not read
//! a provided table at all.
//!
//! ## The three decisions Phase 1 fixes
//!
//! - **A dataset is a list of formulas, and that is the whole of it.** There is
//!   no second vocabulary of "field / joinfield / aggregation" with three shapes
//!   in the JSON and three code paths behind it: a column is an `sc-expr`
//!   formula, validated against the same [`SchemaShape`](sc_expr::SchemaShape)
//!   as a calculated field and translated by the same `translate_value`. The
//!   admin UI's picker is sugar that *writes* one.
//! - **The split is a hash of the primary key, not a shuffle.** So a refit after
//!   new rows arrive keeps every old row on the side it was on, and the test
//!   metric of instance 7 is comparable with the test metric of instance 3 —
//!   which is the entire reason anybody looks at two instances of one model.
//!   See [`Split`].
//! - **The frame is columnar, and it is bounded.** Every consumer wants a
//!   column, and a dataset is a `SELECT` an admin wrote that the server has to
//!   hold in memory — so [`DatasetSource::materialise`] takes a cap and refuses
//!   by name rather than by the OOM killer. See [`DEFAULT_MAX_ROWS`].

mod dataset;
mod frame;
mod source;
mod split;

pub use dataset::{Dataset, DatasetColumn, DatasetColumnShape, DatasetShape, validate_dataset};
pub use frame::{Column, ColumnType, Frame, canonical_key};
pub use source::{DEFAULT_MAX_ROWS, DatasetSource, SPLIT_KEY};
pub use split::{Part, Split, SplitCounts, Splits};
