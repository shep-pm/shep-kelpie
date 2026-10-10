//! The account's rate limits, from `gh api rate_limit`, which counts
//! against none of them

use std::collections::BTreeMap;

use serde::Deserialize;

use super::{gh, unreadable};
use crate::ports::{ForgeError, Timestamp};

/// The latest reset of any limit with nothing left, or `None` when none is used up
pub(super) fn rate_limit_reset() -> Result<Option<Timestamp>, ForgeError> {
    parse_rate_limit(&gh(&["api", "rate_limit"])?)
}

fn parse_rate_limit(stdout: &[u8]) -> Result<Option<Timestamp>, ForgeError> {
    #[derive(Deserialize)]
    struct Limits {
        resources: BTreeMap<String, Limit>,
    }
    #[derive(Deserialize)]
    struct Limit {
        remaining: u64,
        reset: u64,
    }
    let limits: Limits = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    let used_up = limits.resources.values().filter(|l| l.remaining == 0);
    Ok(used_up.map(|l| Timestamp(l.reset)).max())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded 2026-10-10 with every limit to spare.
    const RECORDED: &str = include_str!("../../../fixtures/gh-api-rate-limit.json");

    #[test]
    fn a_limit_with_some_left_holds_nothing() {
        assert_eq!(parse_rate_limit(RECORDED.as_bytes()), Ok(None));
    }

    #[test]
    fn the_latest_reset_of_a_used_up_limit_is_when_calls_go_again() {
        let mut limits: serde_json::Value = serde_json::from_str(RECORDED).unwrap();
        let resources = &mut limits["resources"];
        resources["graphql"]["remaining"] = 0.into();
        resources["search"]["remaining"] = 0.into();
        let graphql = resources["graphql"]["reset"].as_u64().unwrap();
        let search = resources["search"]["reset"].as_u64().unwrap();
        let text = serde_json::to_vec(&limits).unwrap();

        let reset = parse_rate_limit(&text).unwrap();

        assert_eq!(reset, Some(Timestamp(graphql.max(search))));
    }

    #[test]
    fn an_answer_with_no_resources_is_unreadable() {
        assert!(matches!(
            parse_rate_limit(br#"{"rate":{}}"#),
            Err(ForgeError::Unreadable(_))
        ));
    }
}
