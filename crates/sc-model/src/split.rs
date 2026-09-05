//! Train, validation and test: **a hash of the primary key, not a shuffle**
//! (TODO §5).
//!
//! The obvious implementation shuffles a vector with a seeded RNG. This one
//! assigns each row by hashing its primary key with the fit's seed and taking
//! the fraction, which costs the same and buys three things:
//!
//! - **It does not depend on row order**, so a dataset materialised with a
//!   different `ORDER BY`, a different `LIMIT`, or off a table provider that
//!   answers in feed order splits identically.
//! - **A refit after new rows arrive keeps every old row on the side it was
//!   on.** The test metric of instance 7 is therefore comparable with the test
//!   metric of instance 3, which is the entire reason anybody looks at two
//!   instances of one model.
//! - **It is reproducible from the row, not from the run** — an instance records
//!   its seed and fractions, so "was this row in the training set" is answerable
//!   afterwards without storing a list of ids.
//!
//! The price is that the fractions are **approximate** on small datasets: 200
//! rows at 20% test is whatever the hash gives, not exactly 40. So the split
//! reports [`SplitCounts`] — what it actually got — and nobody has to guess.
//!
//! The hash is SHA-256 of the seed and the key. A *stable* hash on purpose:
//! `std::hash::DefaultHasher` is explicitly not stable across Rust releases, and
//! an instance whose train/test assignment moved under a toolchain upgrade would
//! undo the second bullet above.

use sc_error::{Error, Result};
use sha2::{Digest, Sha256};

use crate::frame::Frame;

/// Which side of the split a row is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    /// The rows the fit is computed from.
    Train,
    /// The rows a hyperparameter search scores its grid points on (§11). Empty
    /// in the common case, where there is no search.
    Validation,
    /// The rows the reported metrics are computed on, and which the fit never
    /// saw.
    Test,
}

/// The fractions a fit divides its rows by, and the seed the hash is salted
/// with.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Split {
    /// Fraction of rows fitted on.
    pub train: f64,
    /// Fraction of rows a hyperparameter search scores on — `0.0` when there is
    /// no search, which is the common case and must not pay for the uncommon
    /// one.
    pub validation: f64,
    /// Fraction of rows held out for the reported metrics.
    pub test: f64,
    /// The salt. Stored on the model, so a refit reproduces the same assignment.
    pub seed: u64,
}

impl Default for Split {
    /// Four fifths fitted, one fifth held out, and no validation rows — the
    /// shape of a fit with no hyperparameter search.
    fn default() -> Split {
        Split {
            train: 0.8,
            validation: 0.0,
            test: 0.2,
            seed: 0,
        }
    }
}

/// How many rows each side actually got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct SplitCounts {
    /// Rows assigned to [`Part::Train`].
    pub train: usize,
    /// Rows assigned to [`Part::Validation`].
    pub validation: usize,
    /// Rows assigned to [`Part::Test`].
    pub test: usize,
}

/// The three frames one split produces, and the counts they came to.
#[derive(Debug, Clone, PartialEq)]
pub struct Splits {
    /// The rows fitted on.
    pub train: Frame,
    /// The rows a hyperparameter search scores on.
    pub validation: Frame,
    /// The rows held out.
    pub test: Frame,
    /// What each side actually got — the fractions are approximate (see the
    /// module docs), so this is reported rather than recomputed.
    pub counts: SplitCounts,
}

/// The tolerance the fractions must sum to 1 within — floating-point slack, not
/// a licence to be approximately right.
const SUM_TOLERANCE: f64 = 1e-9;

impl Split {
    /// A split with these fractions and seed.
    pub fn new(train: f64, validation: f64, test: f64, seed: u64) -> Split {
        Split {
            train,
            validation,
            test,
            seed,
        }
    }

    /// This split with a different seed.
    pub fn seeded(mut self, seed: u64) -> Split {
        self.seed = seed;
        self
    }

    /// Refuse fractions that are negative or that do not sum to 1 — a split that
    /// silently normalised them would hold out a different fraction than the
    /// instance says it did.
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("train", self.train),
            ("validation", self.validation),
            ("test", self.test),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(Error::invalid(format!(
                    "split: the `{name}` fraction must be a number between 0 and 1, not {value}"
                )));
            }
        }
        let sum = self.train + self.validation + self.test;
        if (sum - 1.0).abs() > SUM_TOLERANCE {
            return Err(Error::invalid(format!(
                "split: the train, validation and test fractions must sum to 1, not {sum}"
            )));
        }
        if self.train <= 0.0 {
            return Err(Error::invalid(
                "split: the `train` fraction must be greater than 0 — a fit with no rows \
                 to fit on is not a fit",
            ));
        }
        Ok(())
    }

    /// Whether this split holds anything back at all.
    ///
    /// A split that is all train holds nothing out, so there is nothing to
    /// assign and **no primary key is needed** — which is what lets an
    /// unsupervised fit run over a table with a composite or absent one (§5).
    pub fn holds_out(&self) -> bool {
        self.validation > 0.0 || self.test > 0.0
    }

    /// Which side `key` — a row's [`canonical_key`](crate::canonical_key) — is
    /// on.
    ///
    /// A pure function of the key and the seed, which is the whole point: it
    /// does not know how many rows there are, what order they came in, or which
    /// fit is asking.
    pub fn assign(&self, key: &str) -> Part {
        let u = fraction(self.seed, key);
        if u < self.train {
            Part::Train
        } else if u < self.train + self.validation {
            Part::Validation
        } else {
            Part::Test
        }
    }
}

/// Where `key` falls in `[0, 1)`, salted with `seed`.
fn fraction(seed: u64, key: &str) -> f64 {
    let mut hasher = Sha256::new();
    // The seed is fixed-width, so the two cannot run together and no separator
    // is needed: seed 1 with key "23" and seed 12 with key "3" hash different
    // bytes.
    hasher.update(seed.to_be_bytes());
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    // 53 bits is what an f64 represents exactly, so the top 53 of the digest are
    // taken and the rest dropped: a wider numerator would round, and rounding at
    // the boundary is exactly where an assignment could move.
    let bits = u64::from_be_bytes(head) >> 11;
    bits as f64 / (1u64 << 53) as f64
}

impl Frame {
    /// This frame divided into train, validation and test by `split`.
    ///
    /// Refuses a frame with no [`keys`](Frame::keys): a table with a composite
    /// or absent primary key has nothing stable to hash, and a fit over it would
    /// have to fall back to row order — which is the one thing §5 exists to
    /// avoid.
    pub fn split(&self, split: &Split) -> Result<Splits> {
        split.validate()?;
        if !split.holds_out() {
            // Nothing is held out, so nothing has to be assigned — and a table
            // with no single primary key can still be fitted over (§5).
            return Ok(Splits {
                counts: SplitCounts {
                    train: self.rows,
                    validation: 0,
                    test: 0,
                },
                train: self.clone(),
                validation: self.take_rows(&[])?,
                test: self.take_rows(&[])?,
            });
        }
        if self.rows > 0 && self.keys.len() != self.rows {
            return Err(Error::invalid(
                "this dataset's rows have no primary key, so they cannot be assigned to a \
                 train/validation/test split: there is nothing stable to hash",
            ));
        }
        let (mut train, mut validation, mut test) = (Vec::new(), Vec::new(), Vec::new());
        for (i, key) in self.keys.iter().enumerate() {
            match split.assign(key) {
                Part::Train => train.push(i),
                Part::Validation => validation.push(i),
                Part::Test => test.push(i),
            }
        }
        Ok(Splits {
            counts: SplitCounts {
                train: train.len(),
                validation: validation.len(),
                test: test.len(),
            },
            train: self.take_rows(&train)?,
            validation: self.take_rows(&validation)?,
            test: self.take_rows(&test)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Column;

    /// A frame of `n` rows whose key is the row's index and whose one column is
    /// the same number.
    fn frame(keys: impl IntoIterator<Item = i64>) -> Frame {
        let keys: Vec<i64> = keys.into_iter().collect();
        Frame::new(
            vec![(
                "x".to_owned(),
                Column::Int(keys.iter().copied().map(Some).collect()),
            )],
            keys.iter().map(|k| format!("int:{k}")).collect(),
        )
        .expect("frame")
    }

    /// Which key ended up on which side, as a sorted list per part.
    fn sides(splits: &Splits) -> (Vec<i64>, Vec<i64>) {
        let of = |f: &Frame| match f.column("x") {
            Some(Column::Int(v)) => {
                let mut out: Vec<i64> = v.iter().flatten().copied().collect();
                out.sort_unstable();
                out
            }
            _ => Vec::new(),
        };
        (of(&splits.train), of(&splits.test))
    }

    #[test]
    fn the_split_does_not_depend_on_row_order() {
        let split = Split::default().seeded(7);
        let forward = frame(0..200).split(&split).expect("split");
        let backward = frame((0..200).rev()).split(&split).expect("split");
        assert_eq!(sides(&forward), sides(&backward));
        assert_eq!(forward.counts, backward.counts);
    }

    #[test]
    fn appending_rows_leaves_every_old_row_on_the_side_it_was_on() {
        let split = Split::default().seeded(7);
        let before = frame(0..200).split(&split).expect("split");
        let after = frame(0..400).split(&split).expect("split");
        let (train_before, test_before) = sides(&before);
        let (train_after, test_after) = sides(&after);
        for k in train_before {
            assert!(train_after.contains(&k), "row {k} moved out of train");
        }
        for k in test_before {
            assert!(test_after.contains(&k), "row {k} moved out of test");
        }
    }

    #[test]
    fn the_fractions_are_approximate_and_the_counts_say_what_they_came_to() {
        let splits = frame(0..1000)
            .split(&Split::new(0.6, 0.2, 0.2, 3))
            .expect("split");
        let counts = splits.counts;
        assert_eq!(counts.train + counts.validation + counts.test, 1000);
        // Approximate, not exact — that is the price §5 names.
        assert!((counts.train as i64 - 600).abs() < 60, "{counts:?}");
        assert!((counts.test as i64 - 200).abs() < 60, "{counts:?}");
        assert_eq!(splits.train.rows, counts.train);
        assert_eq!(splits.validation.rows, counts.validation);
    }

    #[test]
    fn a_different_seed_is_a_different_assignment() {
        let a = frame(0..200).split(&Split::default().seeded(1)).expect("a");
        let b = frame(0..200).split(&Split::default().seeded(2)).expect("b");
        assert_ne!(sides(&a), sides(&b));
    }

    #[test]
    fn no_validation_fraction_means_no_validation_rows() {
        let splits = frame(0..200).split(&Split::default()).expect("split");
        assert_eq!(splits.counts.validation, 0);
        assert_eq!(splits.validation.rows, 0);
    }

    #[test]
    fn a_split_that_holds_nothing_out_needs_no_primary_key() {
        // §5's parenthetical: the restriction is the *split's*, so an
        // unsupervised fit over a keyless table is still allowed.
        let frame = Frame::new(
            vec![("x".to_owned(), Column::Int(vec![Some(1), Some(2)]))],
            Vec::new(),
        )
        .expect("frame");
        let all_train = Split::new(1.0, 0.0, 0.0, 0);
        assert!(!all_train.holds_out());
        let splits = frame.split(&all_train).expect("split");
        assert_eq!(splits.counts.train, 2);
        assert_eq!(splits.test.rows, 0);
        assert_eq!(splits.test.names(), vec!["x"]);
    }

    #[test]
    fn a_frame_with_no_keys_cannot_be_split() {
        let frame = Frame::new(
            vec![("x".to_owned(), Column::Int(vec![Some(1), Some(2)]))],
            Vec::new(),
        )
        .expect("frame");
        let err = frame.split(&Split::default()).expect_err("no keys");
        assert!(err.to_string().contains("nothing stable to hash"), "{err}");
    }

    #[test]
    fn fractions_that_do_not_sum_to_one_are_refused() {
        let err = Split::new(0.6, 0.2, 0.4, 0).validate().expect_err("sum");
        assert!(err.to_string().contains("must sum to 1"), "{err}");
        let err = Split::new(-0.1, 0.2, 0.9, 0)
            .validate()
            .expect_err("negative");
        assert!(err.to_string().contains("between 0 and 1"), "{err}");
        let err = Split::new(0.0, 0.5, 0.5, 0)
            .validate()
            .expect_err("no train");
        assert!(err.to_string().contains("not a fit"), "{err}");
    }

    #[test]
    fn the_seed_is_fixed_width_so_it_cannot_run_into_the_key() {
        assert_ne!(fraction(1, "23"), fraction(12, "3"));
    }
}
