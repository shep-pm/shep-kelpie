//! Where the whole-issue check before the merge stands for a work item

use serde::{Deserialize, Serialize};

/// What the whole-issue check has found on a work item so far
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Audit {
    /// The head the check found nothing wrong with, which is not checked again
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed: Option<String>,
    /// How many times it sent the worker back since the last ruling on its
    /// findings. At [`SENDS_BACK`] the next gap goes to the maintainer.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sent_back: u32,
}

/// How many times the check sends the worker back before it asks the maintainer
pub const SENDS_BACK: u32 = 2;

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_that_found_nothing_yet_saves_as_an_empty_object() {
        assert_eq!(
            serde_json::to_value(Audit::default()).unwrap(),
            serde_json::json!({})
        );
        let kept = Audit {
            passed: Some("abc".into()),
            sent_back: 2,
        };
        let back: Audit = serde_json::from_value(serde_json::to_value(&kept).unwrap()).unwrap();
        assert_eq!(back, kept);
    }
}
