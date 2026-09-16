//! What a model costs: [`Prices`], read from a model row, and
//! [`Usage::cost`](crate::Usage::cost) (TODO §4).
//!
//! **A blank price is unknown, never zero** (§3a). A cost computed from a price
//! nobody entered would be a number that looks like data. So every price is an
//! `Option`, and a step whose tokens include a class with no price has an
//! unknown cost.

use sc_types::Attrs;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::message::Usage;

/// Model setting: the price of an uncached input token, per million.
pub const CFG_PRICE_INPUT: &str = "price_input";
/// Model setting: the price of an input token read from the cache, per million.
pub const CFG_PRICE_CACHED_INPUT: &str = "price_cached_input";
/// Model setting: the price of an input token written to the cache, per
/// million (Anthropic's cache creation).
pub const CFG_PRICE_CACHE_WRITE: &str = "price_cache_write";
/// Model setting: the price of an output token, per million.
pub const CFG_PRICE_OUTPUT: &str = "price_output";

/// A model's prices, in currency units per million tokens. `None` is unknown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Prices {
    /// Uncached input.
    pub input: Option<f64>,
    /// Input read from the cache.
    pub cached_input: Option<f64>,
    /// Input written to the cache.
    pub cache_write: Option<f64>,
    /// Output, including reasoning.
    pub output: Option<f64>,
}

impl Prices {
    /// The prices set on a model row's `config`. A blank, missing or negative
    /// value is unknown.
    pub fn from_config(config: &Attrs) -> Prices {
        let price = |key: &str| {
            config
                .get(key)
                .and_then(Json::as_f64)
                .filter(|p| p.is_finite() && *p >= 0.0)
        };
        Prices {
            input: price(CFG_PRICE_INPUT),
            cached_input: price(CFG_PRICE_CACHED_INPUT),
            cache_write: price(CFG_PRICE_CACHE_WRITE),
            output: price(CFG_PRICE_OUTPUT),
        }
    }

    /// Whether a cost can be computed at all: the input and output prices are
    /// both known. A cost budget is refused for a model without them (§4).
    pub fn is_priced(&self) -> bool {
        self.input.is_some() && self.output.is_some()
    }
}

impl Usage {
    /// What this usage cost at `prices`, or `None` when a token class that was
    /// used has no price.
    ///
    /// [`input_tokens`](Usage::input_tokens) is the whole prompt, so the cached
    /// and cache-write tokens are taken out of it and priced on their own. A
    /// class with no tokens needs no price: a model with no cache price still has
    /// a known cost for a step that read nothing from the cache.
    pub fn cost(&self, prices: &Prices) -> Option<f64> {
        let per_million = |tokens: u64, price: Option<f64>| -> Option<f64> {
            if tokens == 0 {
                Some(0.0)
            } else {
                price.map(|p| tokens as f64 * p / 1_000_000.0)
            }
        };
        // Input and output prices are always required, even for a step that
        // reported no tokens: "free" and "unknown" must not look alike.
        prices.input?;
        prices.output?;
        let uncached = self
            .input_tokens
            .saturating_sub(self.cached_input_tokens)
            .saturating_sub(self.cache_write_input_tokens);
        Some(
            per_million(uncached, prices.input)?
                + per_million(self.cached_input_tokens, prices.cached_input)?
                + per_million(self.cache_write_input_tokens, prices.cache_write)?
                + per_million(self.output_tokens, prices.output)?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-9)
    }

    #[test]
    fn prices_are_read_from_a_row_and_blank_is_unknown() {
        let config = json!({
            CFG_PRICE_INPUT: 3.0,
            CFG_PRICE_OUTPUT: 15,
            CFG_PRICE_CACHED_INPUT: null,
            CFG_PRICE_CACHE_WRITE: -1.0,
        });
        let prices = Prices::from_config(config.as_object().unwrap());
        assert_eq!(prices.input, Some(3.0));
        assert_eq!(prices.output, Some(15.0));
        assert_eq!(prices.cached_input, None);
        assert_eq!(prices.cache_write, None);
        assert!(prices.is_priced());
        assert!(!Prices::default().is_priced());
    }

    #[test]
    fn cost_prices_each_token_class_on_its_own() {
        let prices = Prices {
            input: Some(3.0),
            cached_input: Some(0.3),
            cache_write: Some(3.75),
            output: Some(15.0),
        };
        let usage = Usage {
            input_tokens: 1_000_000,
            cached_input_tokens: 600_000,
            cache_write_input_tokens: 100_000,
            output_tokens: 10_000,
        };
        // 300k uncached × 3 + 600k × 0.3 + 100k × 3.75 + 10k × 15, per million.
        assert!(close(usage.cost(&prices), 0.9 + 0.18 + 0.375 + 0.15));
    }

    #[test]
    fn an_unknown_price_is_an_unknown_cost_never_zero() {
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 10,
            ..Usage::default()
        };
        assert_eq!(usage.cost(&Prices::default()), None);
        assert_eq!(
            usage.cost(&Prices {
                input: Some(1.0),
                ..Prices::default()
            }),
            None
        );

        // Cached tokens with no cached price: unknown.
        let cached = Usage {
            cached_input_tokens: 500,
            ..usage
        };
        let partial = Prices {
            input: Some(1.0),
            output: Some(2.0),
            ..Prices::default()
        };
        assert_eq!(cached.cost(&partial), None);
        // No cached tokens: the missing cached price is not needed.
        assert!(close(usage.cost(&partial), 1_000.0 / 1e6 + 20.0 / 1e6));
        // A step that used nothing is free only when the prices are known.
        assert!(close(Usage::default().cost(&partial), 0.0));
    }
}
