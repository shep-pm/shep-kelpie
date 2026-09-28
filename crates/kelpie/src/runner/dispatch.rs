//! Dispatch: the board's next issue becomes the work item in flight
//!
//! A running project with nothing in flight asks the board on every step.
//! One work item is in flight at a time, so a queued issue waits until the
//! one in flight is gone.

use super::Runner;
use super::report::{Begin, StepReport};
use crate::board::{self, Skip};
use crate::pacer::Scope;
use crate::state::StateError;

impl Runner {
    pub(super) fn dispatch(&mut self) -> Result<Begin, StateError> {
        let forge = &self.ports.forge;
        let repo = &self.settings.forge;
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
        let mut failed: Vec<Skip> = Vec::new();
        let mut paced = false;
        loop {
            let pick = board::pick(&ready, &open, &self.state.finished);
            let mut skipped = pick.skipped;
            skipped.extend(failed.iter().cloned());
            skipped.sort_by_key(Skip::issue);
            let Some(issue) = pick.issue else {
                let reason = failed
                    .iter()
                    .filter_map(|skip| match skip {
                        Skip::Failed { issue, error } => {
                            Some(format!("cannot dispatch #{issue}: {error}"))
                        }
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
                if let Some(held) = self.pace(Scope::Dispatch)?.holds() {
                    self.skipped = skipped;
                    return Ok(held);
                }
                paced = true;
            }
            match self.add(issue) {
                Ok(worker) => {
                    self.skipped.clone_from(&skipped);
                    return Ok(Begin::Report(StepReport::Dispatched {
                        issue,
                        worker,
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::board::{Skip, WorkerModel};
    use crate::ports::{ClaudeError, Cost, Usage};
    use crate::runner::{StepReport, step};
    use crate::settings::Effort;
    use crate::test::{Rig, Scripted};

    fn running(project: &str) -> (Rig, Mutex<Runner>) {
        let rig = Rig::new(project);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        (rig, runner)
    }

    fn sonnet_medium() -> WorkerModel {
        WorkerModel {
            model: "claude-sonnet-5".into(),
            effort: Effort::Medium,
        }
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
                worker: sonnet_medium(),
                skipped: vec![],
            })
        );
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();
        let [call] = rig.claude.calls().try_into().unwrap();
        assert!(call.prompt.starts_with("Your work item is issue #9: "));

        // The turn ended, and #9 is still in flight, so #12 waits.
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.ask(&runner, "status", None)["work_item"]["issue"], 9);
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
                worker: sonnet_medium(),
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

    #[test]
    fn a_worker_label_runs_that_items_worker_on_its_model_and_effort() {
        let (rig, runner) = running("golbat");
        rig.forge.list_ready(6, false);
        rig.forge.label(6, "worker:opus-medium");
        step(&runner).unwrap();
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();

        let [call] = rig.claude.calls().try_into().unwrap();
        assert_eq!(
            (call.model.as_str(), call.effort),
            ("claude-opus-5-5", Effort::Medium)
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["worker"],
            json!({ "model": "claude-opus-5-5", "effort": "medium" })
        );
    }

    #[test]
    fn a_worker_label_added_after_dispatch_changes_nothing() {
        let (rig, runner) = running("chelone");
        rig.forge.list_ready(6, false);
        step(&runner).unwrap();
        rig.forge.label(6, "worker:opus-high");
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();
        assert_eq!(rig.claude.calls()[0].model, "claude-sonnet-5");
    }

    #[test]
    fn add_takes_the_worker_label_too_and_refuses_one_it_cannot_read() {
        let (rig, runner) = running("xilriws");
        rig.forge.label(8, "worker:gpt-high");
        assert_eq!(
            rig.ask(&runner, "add", Some("8")),
            json!({ "error": "label `worker:gpt-high` is not `worker:<model>-<effort>` \
                              with a model from opus, sonnet, haiku, fable" })
        );
        rig.forge.label(9, "worker:haiku-low");
        let item = &rig.ask(&runner, "add", Some("9"))["work_item"];
        assert_eq!(
            item["worker"],
            json!({ "model": "claude-haiku-4-5-20251001", "effort": "low" })
        );
    }

    #[test]
    fn a_paused_project_dispatches_nothing() {
        let rig = Rig::new("zeus");
        let runner = rig.open().unwrap();
        rig.forge.list_ready(2, false);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));

        rig.ask(&runner, "start", None);
        rig.ask(&runner, "pause", None);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
        assert_eq!(rig.claude.calls(), []);
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
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Dispatched { issue: 2, .. })
        ));
    }

    #[test]
    fn a_dispatch_that_cannot_be_saved_is_an_error_and_takes_nothing() {
        let (rig, runner) = running("rotom");
        rig.forge.list_ready(4, false);
        std::fs::remove_dir_all(rig.paths().state.parent().unwrap()).unwrap();
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
                worker: sonnet_medium(),
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
        rig.claude
            .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
        step(&runner).unwrap();
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["pull_request"],
            json!(null)
        );
    }
}
