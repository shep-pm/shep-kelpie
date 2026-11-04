//! What a Codex call's tokens would cost at OpenAI's API list prices
//!
//! A ChatGPT plan charges no price per call. Kelpie prices a call at list
//! all the same, so a Codex-built work item does not read as free
//! (`docs/facts/codex-as-an-agent.md`).

use crate::ports::{Cost, Usage};

/// One model's list prices, each in billionths of a dollar per token, which
/// is a dollar per million tokens times a thousand
struct Rates {
    model: &'static str,
    input: u64,
    cache_read: u64,
    cache_write: u64,
    output: u64,
}

/// OpenAI's standard prices, from <https://developers.openai.com/api/docs/pricing>
/// read 2026-11-04
///
/// The rates for a request of up to 272K input tokens. A turn's summed tokens
/// cannot tell which of its requests passed that.
const RATES: [Rates; 1] = [Rates {
    model: "gpt-6.1-sol",
    input: 2_000,
    cache_read: 100,
    cache_write: 2_500,
    output: 10_000,
}];

/// What `usage`, a call's or a session's tokens on `model`, costs at list, or
/// `None` for a model the table does not name
///
/// `Usage::input` counts uncached input only, so each kind is priced once.
pub(super) fn price(model: &str, usage: &Usage) -> Option<Cost> {
    let rates = RATES.iter().find(|rates| rates.model == model)?;
    let cost = [
        (usage.input, rates.input),
        (usage.cache_read, rates.cache_read),
        (usage.cache_write, rates.cache_write),
        (usage.output, rates.output),
    ]
    .into_iter()
    .fold(0_u64, |sum, (tokens, rate)| {
        sum.saturating_add(tokens.saturating_mul(rate))
    });
    Some(Cost(cost))
}
