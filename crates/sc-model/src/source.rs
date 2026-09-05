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

use async_trait::async_trait;
use sc_error::Result;

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
pub const SPLIT_KEY: &str = "_sc_split_key";

/// How a [`Dataset`] becomes a [`Frame`]. Implemented in `sc-server` over
/// `sc_api::rows`.
#[async_trait]
pub trait DatasetSource: Send + Sync {
    /// Read `ds` into a frame, refusing by name if it selects more than `cap`
    /// rows.
    ///
    /// The frame's [`keys`](Frame::keys) are filled in when the dataset's table
    /// has a single primary key and left empty when it does not: reading is
    /// unaffected by that, and only [`Frame::split`](crate::Frame::split)
    /// refuses.
    async fn materialise(&self, ds: &Dataset, cap: u64) -> Result<Frame>;
}
