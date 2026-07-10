//! Query results: a [`Row`] of named [`Value`]s and the [`RowStream`] that
//! [`query`](crate::DatabaseDriver::query) yields.
//!
//! `RowStream` is an async stream so a driver can hand back rows as they arrive
//! from the backend (tokio-postgres row streaming maps straight onto it) without
//! buffering a whole result set. Callers that want everything at once use
//! [`RowStream::try_collect`].

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::Stream;
use futures::stream::{self, BoxStream, StreamExt};
use sc_error::Result;
use sc_query::Value;

/// One row of a result set: an ordered list of [`Value`]s addressable by column
/// name or position.
///
/// Every row of a given stream shares one column-name vector (held in an `Arc`),
/// so streaming a large result set does not re-allocate the column list per row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    columns: Arc<Vec<String>>,
    values: Vec<Value>,
}

impl Row {
    /// Build a row from a shared column list and its values. The two must have
    /// the same length; a mismatch is a programming error in the driver and
    /// yields an [`Error::msg`](sc_error::Error::msg).
    pub fn new(columns: Arc<Vec<String>>, values: Vec<Value>) -> Result<Self> {
        if columns.len() != values.len() {
            return Err(sc_error::Error::msg(format!(
                "row has {} values but {} columns",
                values.len(),
                columns.len()
            )));
        }
        Ok(Row { columns, values })
    }

    /// The column names, in order.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// The values, in column order.
    pub fn values(&self) -> &[Value] {
        &self.values
    }

    /// The value at a column position, if in range.
    pub fn get_index(&self, index: usize) -> Option<&Value> {
        self.values.get(index)
    }

    /// The value for a column by name, if present.
    pub fn get(&self, column: &str) -> Option<&Value> {
        let index = self.columns.iter().position(|c| c == column)?;
        self.values.get(index)
    }

    /// Consume the row, returning its values in column order.
    pub fn into_values(self) -> Vec<Value> {
        self.values
    }

    /// The number of columns.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether the row has no columns.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// An async stream of [`Row`]s, the result of running a query.
///
/// It implements [`Stream`], so callers can pull rows with
/// [`StreamExt::next`](futures::StreamExt::next), or materialise the whole set
/// with [`try_collect`](RowStream::try_collect).
pub struct RowStream {
    inner: BoxStream<'static, Result<Row>>,
}

impl RowStream {
    /// Wrap an owned boxed stream of fallible rows.
    pub fn new(inner: BoxStream<'static, Result<Row>>) -> Self {
        RowStream { inner }
    }

    /// An already-materialised set of rows presented as a stream — handy for
    /// providers that compute rows eagerly and for tests.
    pub fn from_rows(rows: Vec<Row>) -> Self {
        RowStream {
            inner: stream::iter(rows.into_iter().map(Ok)).boxed(),
        }
    }

    /// An empty result set.
    pub fn empty() -> Self {
        RowStream::from_rows(Vec::new())
    }

    /// Drain the stream into a vector, short-circuiting on the first error.
    pub async fn try_collect(mut self) -> Result<Vec<Row>> {
        let mut out = Vec::new();
        while let Some(row) = self.inner.next().await {
            out.push(row?);
        }
        Ok(out)
    }
}

impl Stream for RowStream {
    type Item = Result<Row>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}
