//! How a dataset becomes rows: the seam, and its bound (TODO §4, §9).
//!
//! This crate is at layer 6 so a module can supply a model provider (see the
//! crate docs), and the row layer is at layer 8 — so reading is a **seam** that
//! somebody above the row layer fills in. `sc_server::models::CatalogDatasetSource`
//! is that somebody, over `sc_api::rows`, which is what makes a dataset see the
//! non-stored calculated fields, the ownership rule, row-level security and a
//! provided table. Going around the row layer would mean a dataset that saw none
//! of those.
//!
//! The seam takes a **cap** rather than reading it from a configuration this
//! crate cannot see. A dataset is a `SELECT` an admin wrote and the server has
//! to hold the answer in memory, so a materialisation that would exceed the cap
//! is refused by name — "the dataset selects more than 200 000 rows; add a
//! filter or raise `--model-max-rows`" — rather than by the OOM killer. The
//! count is asked for **before** the rows, so the refusal costs one `COUNT(*)`
//! and not a partial read.
//!
//! ## Why a read is three things and not one
//!
//! A fit reads the whole dataset, and that was the only reader Phase 3 had. Two
//! more arrived with the API (Phase 5) and neither is that read:
//!
//! - **A prediction** wants the dataset's derived columns for *some* rows — the
//!   row a trigger just wrote, or the rows a filter selects. It cannot compute
//!   them itself: a join path and an aggregation are the row layer's answer, so
//!   the restriction has to go *into* the read rather than be applied to what
//!   comes back. Hence [`Read::restricted_to`], which the source ands into the
//!   `WHERE`. A prediction also reads [`unfiltered`](Read::unfiltered), and that
//!   is the point of the flag: a dataset's filter says which rows the model was
//!   **fitted from**, not which rows it may be asked about. `sold` is the
//!   motivating case — a model of what houses sell for is fitted on the sold
//!   ones and asked about the unsold one a trigger just inserted, and a read
//!   that kept the filter would answer "this row is not in the dataset" for
//!   every row anybody actually wants a prediction for.
//! - **A preview** wants the first few rows and their types, on a table that may
//!   be far over the cap — that is the whole point of previewing before fitting.
//!   Hence [`Read::first`], which is a `LIMIT` and therefore needs no count: the
//!   answer is bounded by construction, so refusing it for being over the cap
//!   would refuse the one screen that exists to say "your dataset is too big".

use async_trait::async_trait;
use sc_error::Result;
use sc_query::Expr;

use crate::dataset::Dataset;
use crate::frame::Frame;

/// The default ceiling on a dataset's rows (`--model-max-rows`).
pub const DEFAULT_MAX_ROWS: u64 = 200_000;

/// The alias a dataset read projects each row's primary key under, so the split
/// can hash it (§5).
///
/// A reserved name rather than the primary-key column's own, because a dataset
/// column may legitimately be *called* `id` while computing something else — and
/// a split that hashed that would be a split over the wrong thing.
pub const SPLIT_KEY: &str = "_fd_split_key";

/// What one read of a dataset asks for: the bound it must stay under, the rows
/// it is restricted to, and how many of them it wants.
///
/// An options value rather than three parameters because two of the three are
/// absent in the common case, and `materialise(ds, None, None, cap)` at every
/// call site would say nothing about which `None` was which.
#[derive(Debug, Clone, Copy)]
pub struct Read<'a> {
    /// The ceiling on the rows this read may return — `--model-max-rows`.
    pub cap: u64,
    /// An extra predicate, anded with the dataset's own filter.
    ///
    /// A `sc_query::Expr` rather than a formula, because the two callers build
    /// it differently and both already have what they need: a prediction over
    /// one row has the primary key's *value*, and a prediction over a filter has
    /// a formula it translated with
    /// [`translate_filter`](crate::translate_filter).
    pub restrict: Option<&'a Expr>,
    /// At most this many rows, and **no count**: a limited read is bounded by
    /// construction.
    pub limit: Option<u64>,
    /// Whether the dataset's **own** filter applies.
    ///
    /// True for a fit and a preview, which are looking at the sample the model
    /// is *about*. False for a prediction, which is looking at rows the caller
    /// named: the filter is a statement about what was fitted, and reusing it to
    /// decide what may be predicted would make a model fitted on `sold` houses
    /// unable to answer about an unsold one — which is the only question anybody
    /// asks it.
    pub filtered: bool,
}

impl<'a> Read<'a> {
    /// Every row the dataset selects, up to `cap`.
    pub fn all(cap: u64) -> Read<'a> {
        Read {
            cap,
            restrict: None,
            limit: None,
            filtered: true,
        }
    }

    /// The same read, **without** the dataset's own filter — what a prediction
    /// does. See [`filtered`](Read::filtered).
    pub fn unfiltered(mut self) -> Read<'a> {
        self.filtered = false;
        self
    }

    /// The same read, restricted to the rows `expr` selects.
    pub fn restricted_to(mut self, expr: &'a Expr) -> Read<'a> {
        self.restrict = Some(expr);
        self
    }

    /// The same read, stopping after `limit` rows.
    pub fn first(mut self, limit: u64) -> Read<'a> {
        self.limit = Some(limit);
        self
    }

    /// How many rows this read may return at most — the limit where there is
    /// one, and the cap otherwise.
    pub fn ceiling(&self) -> u64 {
        self.limit.map_or(self.cap, |n| n.min(self.cap))
    }
}

/// How a [`Dataset`] becomes a [`Frame`]. Implemented in `sc-server` over
/// `sc_api::rows`.
#[async_trait]
pub trait DatasetSource: Send + Sync {
    /// Read `ds` as `how` asks, refusing by name if an unlimited read would
    /// exceed [`Read::cap`].
    ///
    /// The frame's [`keys`](Frame::keys) are filled in when the dataset's table
    /// has a single primary key and left empty when it does not: reading is
    /// unaffected by that, and only [`Frame::split`](crate::Frame::split)
    /// refuses.
    async fn read(&self, ds: &Dataset, how: &Read<'_>) -> Result<Frame>;

    /// Read every row of `ds`, refusing by name if it selects more than `cap` —
    /// what a fit does.
    async fn materialise(&self, ds: &Dataset, cap: u64) -> Result<Frame> {
        self.read(ds, &Read::all(cap)).await
    }
}
