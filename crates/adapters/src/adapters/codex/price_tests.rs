//! What a Codex model's tokens cost at OpenAI's list prices

use super::price::price;
use crate::ports::{Cost, Usage};

#[test]
fn gpt_6_1_sol_is_priced_at_its_list_rates() {
    let million_of_each = Usage {
        input: 1_000_000,
        cache_write: 1_000_000,
        cache_write_5m: 0,
        cache_read: 1_000_000,
        output: 1_000_000,
    };
    // $2.00 input + $2.50 cache write + $0.10 cached input + $10.00 output.
    assert_eq!(
        price("gpt-6.1-sol", &million_of_each),
        Cost::from_usd(14.60)
    );
}

#[test]
fn a_model_the_table_does_not_name_is_unpriced() {
    let spent = Usage {
        input: 1_000,
        ..Usage::default()
    };
    assert_eq!(price("gpt-6-luna", &spent), None);
    assert_eq!(price("", &spent), None);
}

#[test]
fn cached_input_is_priced_once_at_the_cached_rate() {
    // 1M input tokens of which 900K were read from the cache, as `Usage` counts them.
    let session = Usage {
        input: 100_000,
        cache_read: 900_000,
        ..Usage::default()
    };
    assert_eq!(price("gpt-6.1-sol", &session), Cost::from_usd(0.29));
}
