//! Closing a ready issue once every one of its sub-issues is closed
//!
//! The board never dispatches an issue with sub-issues: they are worked in
//! its place. Once the forge shows them all closed, a step closes the issue
//! too. A close the forge keeps refusing is passed over, so the board goes on.

use std::collections::BTreeMap;

use super::Runner;
use super::report::{Begin, StepReport};
use crate::board::ReadyIssue;

/// Refusals in a row before this runner stops closing an issue, until it
/// restarts, so one the forge will never close cannot hold up the board
const TRIES: u32 = 3;

/// What an issue kelpie closes, once its sub-issues are all closed, says
pub(super) const PARENT_CLOSED: &str =
    "Every sub-issue of this issue is closed, so kelpie closes it too.";

/// The forge's refusals in a row to close each issue, kept in memory only
pub(super) type Refused = BTreeMap<u64, u32>;

impl Runner {
    /// Closes a ready issue whose sub-issues are all closed, if the board
    /// lists one the forge has not refused too often
    pub(super) fn close_done_parent(&mut self, ready: &[ReadyIssue]) -> Option<Begin> {
        let refused = &self.close_refused;
        let done = ready.iter().find(|i| {
            i.sub_issues.all_closed() && refused.get(&i.number).is_none_or(|n| *n < TRIES)
        });
        let issue = done?.number;
        let closed = (self.ports.forge).close_issue(&self.settings.forge, issue, PARENT_CLOSED);
        let report = match closed {
            Ok(()) => {
                self.close_refused.remove(&issue);
                StepReport::ParentClosed { issue }
            }
            Err(e) => {
                let failures = self.close_refused.entry(issue).or_default();
                *failures += 1;
                StepReport::ParentCloseFailed {
                    issue,
                    reason: e.to_string(),
                    given_up: *failures >= TRIES,
                }
            }
        };
        Some(Begin::Report(report))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::runner::step;
    use crate::test::Rig;

    fn running(project: &str) -> (Rig, Mutex<Runner>) {
        let rig = Rig::new(project);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        (rig, runner)
    }

    #[test]
    fn the_parent_closes_when_its_last_sub_issue_does() {
        let (rig, runner) = running("xilriws");
        rig.forge.list_ready(5, false);
        rig.forge.list_ready(6, false);
        rig.forge.link_sub_issue(5, 6);
        rig.forge.link_sub_issue(5, 7);
        rig.forge.close_issue(7);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Dispatched { issue: 6, .. })
        ));
        assert!(rig.forge.closings().is_empty());

        rig.forge.close_issue(6);
        rig.ask(&runner, "drop", Some("6"));
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::ParentClosed { issue: 5 })
        );
        let [(closed, why)] = rig.forge.closings().try_into().unwrap();
        assert_eq!((closed, why.as_str()), (5, PARENT_CLOSED));
        assert_eq!(step(&runner).unwrap(), None);
    }

    #[test]
    fn a_parent_with_sub_issues_is_never_dispatched() {
        let (rig, runner) = running("chelone");
        rig.forge.list_ready(5, false);
        rig.forge.list_ready(8, false);
        rig.forge.link_sub_issue(5, 8);
        rig.forge.block(8, 40);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(
            rig.ask(&runner, "status", None)["skipped"],
            json!([
                { "reason": "split", "issue": 5, "open": 1 },
                { "reason": "blocked", "issue": 8, "by": [40] },
            ])
        );
    }

    #[test]
    fn a_parent_the_forge_will_not_close_is_passed_over_and_the_board_goes_on() {
        let (rig, runner) = running("koji");
        rig.forge.list_ready(5, false);
        rig.forge.list_ready(6, false);
        rig.forge.link_sub_issue(5, 8);
        rig.forge.close_issue(8);
        rig.forge.set_closes_down(true);
        let refused = |given_up| StepReport::ParentCloseFailed {
            issue: 5,
            reason: "gh failed: closing is refused".into(),
            given_up,
        };
        assert_eq!(step(&runner).unwrap(), Some(refused(false)));
        assert_eq!(step(&runner).unwrap(), Some(refused(false)));
        assert_eq!(step(&runner).unwrap(), Some(refused(true)));
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Dispatched { issue: 6, .. })
        ));
        assert!(rig.forge.closings().is_empty());

        // A restarted runner tries it again.
        rig.forge.set_closes_down(false);
        rig.ask(&runner, "drop", Some("6"));
        drop(runner);
        let runner = rig.open().unwrap();
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::ParentClosed { issue: 5 })
        );
    }
}
