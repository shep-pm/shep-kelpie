//! Where a work item's deep review round stands
//!
//! The round is two reads. Each is kept in state, so a restart resumes at the
//! read it was on. What the two hold then goes on as any round's findings do.

use serde::{Deserialize, Serialize};

use crate::ports::Finding;

/// A read of the deep round
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Deep {
    /// The first reader is about to read the whole pull request
    Read,
    /// A second fresh session is about to read it, shown what the first found
    Missed {
        /// The first reader's findings
        first: Vec<Finding>,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ports::Severity;

    #[test]
    fn every_step_is_pinned() {
        let value = |d: Deep| serde_json::to_value(d).unwrap();
        assert_eq!(value(Deep::Read), json!({ "step": "read" }));
        let missed = Deep::Missed {
            first: vec![Finding {
                severity: Severity::High,
                file: "src/lib.rs".into(),
                line: 3,
                what: "w".into(),
                why: "y".into(),
            }],
        };
        let saved = value(missed.clone());
        assert_eq!(
            saved,
            json!({
                "step": "missed",
                "first": [{ "severity": "high", "file": "src/lib.rs", "line": 3, "what": "w", "why": "y" }],
            })
        );
        assert_eq!(serde_json::from_value::<Deep>(saved).unwrap(), missed);
    }
}
