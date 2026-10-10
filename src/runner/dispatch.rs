//! Dispatch: the board's next issue opens a work item in a free slot
//!
//! A running project with fewer than `concurrency.active_items` working and fewer than
//! `concurrency.pending_rulings` parked asks the board on its steps, after starting any pull
//! request it adopted and checking its open pull requests for one asking
//! for a rework. A queued issue waits until a slot is free, an issue, or a
//! pull request, whose work item is open waits for it to end, and an issue
//! naming a file a parked item's branch changes waits for that item. With a
//! project manager and two or more issues the board could take, its pick
//! goes in place of the board rule's, and a ready issue it holds waits.

use super::Runner;
use super::pm::Choice;
use super::report::{Begin, StepReport};
use crate::board::{self, ReadyIssue, Skip};
use crate::state::StateError;

impl Runner {
    // The step runs this only while the board may open a work item, and it
    // opens at most one, so nothing here checks `concurrency.active_items` again.
    pub(super) fn dispatch(&mut self) -> Result<Begin, StateError> {
        let forge = &self.ports.forge;
        let repo = &self.remote;
        let listed = forge
            .ready_issues(repo)
            .and_then(|ready| forge.open_pull_requests(repo).map(|open| (ready, open)));
        let (mut ready, open) = match listed {
            Ok(listed) => listed,
            Err(e) => {
                return Ok(Begin::Report(StepReport::BoardFailed {
                    reason: format!("cannot read the board: {e}"),
                }));
            }
        };
        self.trim_finished(&ready)?;
        // An issue in flight is left out, and not listed in `skipped`: it is
        // being worked on, not passed over.
        ready.retain(|issue| self.state.item(issue.number).is_none());
        self.ready_read(&ready)?;
        if let Some(begin) = self.close_done_parent(&ready) {
            return Ok(begin);
        }
        // An adopted pull request, then one asking for a rework, goes before
        // any ready issue, and one that cannot start is passed over like one.
        let (begin, mut failed) = self.adopt_waiting(&open)?;
        if let Some(begin) = begin {
            self.skipped = failed;
            return Ok(begin);
        }
        let (begin, reworks) = self.rework_asked(&open)?;
        failed.extend(reworks);
        if let Some(begin) = begin {
            self.skipped = failed;
            return Ok(begin);
        }
        // One whose paths a parked item's branch touches would build on code
        // that may yet change, and one whose paths are unread may be one.
        let overlaps = self.parked_overlap(&ready);
        ready.retain(|issue| !overlaps.iter().any(|s| s.issue() == issue.number));
        let implementers = self.agents.implementer_names();
        let all = takeable(&ready, &open, &self.state.finished, &implementers);
        let held = self.pm_holds(&all);
        ready.retain(|issue| !held.contains(&issue.number));
        let mut paced = false;
        let mut unlabelled: Vec<Skip> = Vec::new();
        loop {
            let implementers = self.agents.implementer_names();
            let finished = &self.state.finished;
            let pick = board::pick(&ready, &open, finished, &implementers);
            let mut skipped = pick.skipped;
            skipped.extend(failed.iter().cloned());
            skipped.extend(overlaps.iter().cloned());
            skipped.extend(unlabelled.iter().cloned());
            skipped.sort_by_key(Skip::issue);
            let takeable = takeable(&ready, &open, finished, &implementers);
            let Some(issue) = pick.issue else {
                let reason = failed
                    .iter()
                    .filter_map(|skip| match skip {
                        Skip::Failed { issue, error } => {
                            Some(format!("cannot dispatch #{issue}: {error}"))
                        }
                        Skip::Rework {
                            pull_request,
                            error,
                            ..
                        } => Some(format!("cannot rework #{pull_request}: {error}")),
                        Skip::Adopt {
                            pull_request,
                            error,
                        } => Some(format!("cannot adopt #{pull_request}: {error}")),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                self.skipped = skipped;
                return Ok(if failed.is_empty() {
                    Begin::Idle
                } else {
                    Begin::Report(StepReport::BoardFailed { reason })
                });
            };
            if !paced {
                if let Some(held) = self.pace_dispatch()?.holds() {
                    self.skipped = skipped;
                    return Ok(held);
                }
                paced = true;
            }
            let issue = match self.pm_pick(&takeable) {
                Choice::Take(n) => n,
                Choice::Rule => issue,
                Choice::Wait => {
                    self.skipped = skipped;
                    return Ok(Begin::Idle);
                }
            };
            // An issue the issue writer is labelling waits for it, even one
            // labelled by hand meanwhile.
            let labelling = self.flights.labelling() == Some(issue);
            if let Some(found) = ready.iter().find(|i| i.number == issue)
                && (labelling || self.needs_label(found))
            {
                let ruling = self.unlabelled_ruling(issue);
                if labelling
                    || ruling.is_some()
                    || self.draining
                    || self.flights.labelling().is_some()
                {
                    unlabelled.push(Skip::Unlabelled { issue, ruling });
                    ready.retain(|i| i.number != issue);
                    continue;
                }
                if let Some(held) = self.pace_writer()?.holds() {
                    self.skipped = skipped;
                    return Ok(held);
                }
                let found = found.clone();
                self.skipped = skipped;
                return Ok(Begin::Label(found));
            }
            match self.add(issue) {
                Ok(agent) => {
                    self.skipped.clone_from(&skipped);
                    return Ok(Begin::Report(StepReport::Dispatched {
                        issue,
                        agent,
                        skipped,
                    }));
                }
                Err(e @ (super::AddError::Forge(_) | super::AddError::Label(_))) => {
                    failed.push(Skip::Failed {
                        issue,
                        error: e.to_string(),
                    });
                    ready.retain(|i| i.number != issue);
                }
                Err(super::AddError::State(e)) => return Err(e),
                Err(e) => {
                    self.skipped = skipped;
                    return Ok(Begin::Report(StepReport::BoardFailed {
                        reason: format!("cannot dispatch #{issue}: {e}"),
                    }));
                }
            }
        }
    }
}

// The ready issues the board could take now, each on its own.
fn takeable(
    ready: &[ReadyIssue],
    open: &[board::OpenPullRequest],
    finished: &[u64],
    implementers: &[crate::settings::AgentName],
) -> Vec<u64> {
    (ready.iter())
        .filter(|i| {
            let one = std::slice::from_ref(*i);
            board::pick(one, open, finished, implementers)
                .issue
                .is_some()
        })
        .map(|i| i.number)
        .collect()
}

impl Runner {
    // The board skips a finished issue until the forge closes it, so an entry
    // goes only once its issue reads as closed. One the forge lists ready is
    // open, and one it cannot answer for stays.
    fn trim_finished(&mut self, ready: &[ReadyIssue]) -> Result<(), StateError> {
        let repo = &self.remote;
        let closed: Vec<u64> = self
            .state
            .finished
            .iter()
            .copied()
            .filter(|&n| !ready.iter().any(|r| r.number == n))
            .filter(|&n| self.ports.forge.issue(repo, n).is_ok_and(|i| !i.open))
            .collect();
        if closed.is_empty() {
            return Ok(());
        }
        let mut next = self.state.clone();
        next.finished.retain(|n| !closed.contains(n));
        self.save(next)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::board::Skip;
    use crate::ports::{AgentError, Cost, PullRequestState, Usage};
    use crate::runner::{StepReport, step};
    use crate::settings::{AgentName, Effort};
    use crate::test::{Rig, Scripted};

    fn running(project: &str) -> (Rig, Mutex<Runner>) {
        let rig = Rig::new(project);
        let runner = rig.open().unwrap();
        (rig, runner)
    }

    fn sonnet_high() -> AgentName {
        AgentName::try_from("sonnet-high".to_owned()).unwrap()
    }

    #[test]
    fn the_older_of_two_ready_issues_is_dispatched_and_the_other_waits() {
        let (rig, runner) = running("shep");
        rig.forge.list_ready(12, false);
        rig.forge.list_ready(9, false);

        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::Dispatched {
                issue: 9,
                agent: sonnet_high(),
                skipped: vec![],
            })
        );
        rig.claude.script([Scripted::Say(
            "<kelpie-question>\nWhich flag?\n</kelpie-question>\n",
        )]);
        step(&runner).unwrap();
        let [call] = rig.claude.calls().try_into().unwrap();
        assert!(call.prompt.starts_with("/mattpocock:implement "));
        assert!(call.prompt.contains("\nYour work item is issue #9: "));

        // #9 is parked on its question, which frees its slot for #12.
        assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::Dispatched {
                issue: 12,
                agent: sonnet_high(),
                skipped: vec![],
            })
        );
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"]["issue"], 9);
        assert_eq!(
            (&status["working"], &status["parked"]),
            (&json!([12]), &json!([9]))
        );
        assert_eq!(rig.claude.calls().len(), 1);
    }

    #[test]
    fn an_issue_with_an_open_pull_request_or_an_assignee_is_skipped() {
        let (rig, runner) = running("koji");
        rig.forge.list_ready(3, false);
        rig.forge.list_ready(4, true);
        rig.forge.list_ready(5, false);
        rig.forge.open_pull_request(30, "fix/three", &[3]);

        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::Dispatched {
                issue: 5,
                agent: sonnet_high(),
                skipped: vec![
                    Skip::PullRequest {
                        issue: 3,
                        pull_request: 30
                    },
                    Skip::Assigned { issue: 4 },
                ],
            })
        );
    }

    #[test]
    fn a_board_with_only_taken_issues_dispatches_nothing() {
        let (rig, runner) = running("rotom");
        rig.forge.list_ready(3, true);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    }

    // A project listing both of kelpie's own implementers.
    fn listing_opus(project: &str) -> (Rig, Mutex<Runner>) {
        let rig = Rig::new(project);
        rig.implementers(&["sonnet-high", "opus-high"]);
        let runner = rig.open().unwrap();
        (rig, runner)
    }

    #[test]
    fn an_agent_label_runs_that_items_worker_on_the_agent_it_names() {
        let (rig, runner) = listing_opus("golbat");
        rig.forge.list_ready(6, false);
        rig.forge.label(6, "agent:opus-high");
        step(&runner).unwrap();
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();

        let [call] = rig.claude.calls().try_into().unwrap();
        assert_eq!(
            (call.model.as_str(), call.effort),
            ("claude-opus-5-5", Effort::High)
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["agent"],
            json!("opus-high")
        );
    }

    #[test]
    fn an_agent_label_added_after_dispatch_changes_nothing() {
        let (rig, runner) = listing_opus("chelone");
        rig.forge.list_ready(6, false);
        rig.forge.label(6, "agent:sonnet-high");
        step(&runner).unwrap();
        rig.forge.label(6, "agent:opus-high");
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();
        assert_eq!(rig.claude.calls()[0].model, "claude-sonnet-5-5");
    }

    #[test]
    fn add_takes_the_agent_label_too_and_refuses_one_naming_no_listed_implementer() {
        let (rig, runner) = running("xilriws");
        rig.forge.label(8, "agent:opus-high");
        assert_eq!(
            rig.ask(&runner, "add", Some("8")),
            json!({ "error": "label `agent:opus-high` names no agent the project lists in \
                              `agents.implementers`, which are sonnet-high" })
        );
        rig.forge.label(9, "worker:haiku-low");
        let item = &rig.ask(&runner, "add", Some("9"))["work_item"];
        assert_eq!(item["agent"], json!("sonnet-high"));
        let notes = runner.lock().unwrap().take_notes();
        assert_eq!(
            notes,
            [
                "issue #9 is labelled `worker:haiku-low`, which kelpie no longer reads, so it \
              runs on the default implementer, sonnet-high: an `agent:<name>` label picks \
              another that `agents.implementers` lists"
            ]
        );
    }

    // A runner runs from its start: no trigger comes before its first look.
    #[test]
    fn a_runner_reads_the_board_on_its_first_pass() {
        let rig = Rig::new("acme");
        let runner = rig.open().unwrap();
        rig.forge.list_ready(2, false);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Dispatched { issue: 2, .. })
        ));
        assert_eq!(rig.ask(&runner, "status", None)["work_item"]["issue"], 2);
    }

    #[test]
    fn a_board_that_cannot_be_read_is_reported_and_dispatches_nothing() {
        let (rig, runner) = running("reactmap");
        rig.forge.list_ready(2, false);
        rig.forge.set_board_down(true);
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::BoardFailed {
                reason: "cannot read the board: gh failed: the board is down".into()
            })
        );
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));

        rig.forge.set_board_down(false);
        rig.next_look();
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Dispatched { issue: 2, .. })
        ));
    }

    #[test]
    fn a_dispatch_that_cannot_be_saved_is_an_error_and_takes_nothing() {
        let (rig, runner) = running("rotom");
        rig.forge.list_ready(4, false);
        let folder = rig.paths().state.parent().unwrap().to_owned();
        std::fs::remove_dir_all(&folder).unwrap();
        // A file where the folder was, which a save cannot make a folder of.
        std::fs::write(&folder, "").unwrap();
        let err = step(&runner).unwrap_err();
        assert!(
            err.to_string().starts_with("cannot write state file"),
            "{err}"
        );
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
        assert_eq!(rig.claude.calls(), []);
    }

    #[test]
    fn a_ready_issue_the_forge_cannot_show_is_skipped_for_the_next_oldest() {
        let (rig, runner) = running("golbat");
        rig.forge.list_ready(4, false);
        rig.forge.list_ready(6, false);
        rig.forge.remove_issue(4);
        let skip = Skip::Failed {
            issue: 4,
            error: "cannot read the issue: gh failed: no issue #4".into(),
        };
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::Dispatched {
                issue: 6,
                agent: sonnet_high(),
                skipped: vec![skip],
            })
        );
        assert_eq!(rig.meter.reads(), 1);
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"]["issue"], 6);
        assert_eq!(
            status["skipped"],
            json!([{
                "reason": "failed",
                "issue": 4,
                "error": "cannot read the issue: gh failed: no issue #4"
            }])
        );
    }

    #[test]
    fn a_board_of_issues_the_forge_cannot_show_reports_each_and_takes_nothing() {
        let (rig, runner) = running("golbat");
        rig.forge.list_ready(4, false);
        rig.forge.list_ready(5, false);
        rig.forge.remove_issue(4);
        rig.forge.remove_issue(5);
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::BoardFailed {
                reason: "cannot dispatch #4: cannot read the issue: gh failed: no issue #4; \
                         cannot dispatch #5: cannot read the issue: gh failed: no issue #5"
                    .into()
            })
        );
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"], json!(null));
        assert_eq!(status["skipped"].as_array().unwrap().len(), 2);
        assert_eq!(rig.meter.reads(), 1);
        assert_eq!(rig.claude.calls(), []);
    }

    #[test]
    fn an_issue_with_an_open_blocker_is_skipped_for_the_next_oldest() {
        let (rig, runner) = running("eevee");
        rig.forge.list_ready(8, false);
        rig.forge.list_ready(9, false);
        rig.forge.block(8, 32);
        rig.forge.block(8, 12);
        rig.forge.close_issue(12);
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::Dispatched {
                issue: 9,
                agent: sonnet_high(),
                skipped: vec![Skip::Blocked {
                    issue: 8,
                    by: vec![32],
                    unlisted: 0
                }],
            })
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["skipped"],
            json!([{ "reason": "blocked", "issue": 8, "by": [32] }])
        );
    }

    #[test]
    fn a_blocked_issue_waits_through_its_blockers_pull_request_until_the_blocker_closes() {
        let (rig, runner) = running("ditto");
        rig.forge.list_ready(8, false);
        rig.forge.block(8, 32);
        rig.forge.open_pull_request(40, "kelpie/32", &[32]);
        assert_eq!(step(&runner).unwrap(), None);
        rig.forge.set_state(40, PullRequestState::Merged);
        rig.next_look();
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(
            rig.ask(&runner, "status", None)["skipped"],
            json!([{ "reason": "blocked", "issue": 8, "by": [32] }])
        );

        rig.forge.close_issue(32);
        rig.next_look();
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Dispatched { issue: 8, .. })
        ));
        assert_eq!(rig.ask(&runner, "status", None)["skipped"], json!([]));
    }

    #[test]
    fn the_workers_draft_pull_request_is_recorded_when_its_turn_ends() {
        let (rig, runner) = running("shep");
        rig.forge.list_ready(7, false);
        step(&runner).unwrap();
        rig.forge.open_pull_request(70, "someone-else", &[]);
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        let Some(StepReport::Ended { pull_request, .. }) = step(&runner).unwrap() else {
            panic!("the turn did not end");
        };
        assert_eq!(pull_request, Some(71));
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["pull_request"],
            71
        );
    }

    #[test]
    fn a_turn_that_opened_no_pull_request_records_none() {
        let (rig, runner) = running("koji");
        rig.forge.list_ready(7, false);
        step(&runner).unwrap();
        rig.claude.script([Scripted::Fail(AgentError::Failed(
            crate::settings::Harness::ClaudeCode,
            "overloaded".into(),
        ))]);
        step(&runner).unwrap();
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["pull_request"],
            json!(null)
        );
    }
}
