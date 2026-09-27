//! The gate between the worker's pull request and the merge ruling
//!
//! Each step looks once at the pull request's head. A pending run waits for
//! the next step. A red run is the worker's next turn, naming the checks
//! that failed. A green run, on a branch that already has the latest
//! `main`, raises the merge ruling.

use super::Runner;
use super::turn::{Begin, StepReport};
use crate::ports::{Checks, PullRequestState, Timestamp};
use crate::state::{RulingKind, StateError};
use crate::work_item::{Phase, Turn};

// GitHub registers a push's checks within seconds. A head that still has
// none two minutes after kelpie first saw it is taken to have no CI.
pub(super) const NO_CHECKS_GRACE: u64 = 120;

impl Runner {
    pub(super) fn check_ci(&mut self) -> Result<Begin, StateError> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("CI runs on a work item");
        let Some(number) = item.pull_request else {
            return Ok(Begin::Idle);
        };
        let Phase::Ci { head: seen, since } = item.phase.clone() else {
            return Ok(Begin::Idle);
        };
        let pr = match self.ports.forge.pull_request(&self.settings.forge, number) {
            Ok(pr) => pr,
            Err(e) => return Ok(self.gate_failed(format!("cannot read #{number}: {e}"))),
        };
        match pr.state {
            PullRequestState::Open => {}
            // The maintainer merged it by hand, which is their own ruling.
            PullRequestState::Merged => {
                self.update(|item| item.phase = Phase::Done { merged: true })?;
                return self.finish(true);
            }
            PullRequestState::Closed => return self.raise(RulingKind::Closed),
        }
        let now = self.ports.clock.now();
        let since = if seen.as_deref() == Some(pr.head.as_str()) {
            since
        } else {
            let head = Some(pr.head.clone());
            self.update(|item| item.phase = Phase::Ci { head, since: now })?;
            now
        };
        match pr.checks {
            Checks::Pending => Ok(Begin::Idle),
            Checks::None if !grace_over(since, now) => Ok(Begin::Idle),
            Checks::None | Checks::Passed => self.raise(RulingKind::Merge { head: pr.head }),
            Checks::Failed(checks) => self.ci_failed(number, pr.head, checks),
        }
    }

    // A worker that pushed nothing after its last red run would get the
    // same run again and again, so the second time the maintainer decides.
    fn ci_failed(
        &mut self,
        number: u64,
        head: String,
        checks: Vec<String>,
    ) -> Result<Begin, StateError> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("CI runs on a work item");
        if item.red_head.as_deref() == Some(head.as_str()) {
            return self.raise(RulingKind::StillRed { head, checks });
        }
        let issue = item.issue;
        let prompt = red_prompt(number, &head, &checks);
        let red = head.clone();
        self.update(|item| {
            item.red_head = Some(red);
            item.turn = Turn::Next { prompt };
            item.phase = Phase::Implement;
        })?;
        Ok(Begin::Report(StepReport::CiFailed {
            issue,
            pull_request: number,
            head,
            checks,
        }))
    }

    pub(super) fn gate_failed(&self, reason: String) -> Begin {
        let issue = self.state.work_item.as_ref().map_or(0, |item| item.issue);
        Begin::Report(StepReport::GateFailed { issue, reason })
    }
}

fn grace_over(since: Timestamp, now: Timestamp) -> bool {
    now.0.saturating_sub(since.0) >= NO_CHECKS_GRACE
}

/// The first seven characters of a commit hash, as git shows it
pub(super) fn short(head: &str) -> &str {
    head.get(..7).unwrap_or(head)
}

fn red_prompt(number: u64, head: &str, checks: &[String]) -> String {
    format!(
        "CI failed on your pull request #{number} at {}: {}. `gh pr checks {number}` \
         lists the checks, and `gh run view <run id> --log-failed` shows what failed. \
         Fix it on this branch, commit, and push with `git push origin HEAD`.",
        short(head),
        checks.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ports::{Cost, Session, Usage};
    use crate::runner::step;
    use crate::test::{Rig, Scripted, git};

    fn ruling_report(report: Option<StepReport>) -> (u64, String) {
        match report {
            Some(StepReport::Ruling { id, question, .. }) => (id, question),
            other => panic!("no ruling was raised: {other:?}"),
        }
    }

    #[test]
    fn a_pending_run_waits_and_a_green_one_raises_the_merge_ruling() {
        let (rig, runner, head) = Rig::with_pull_request("shep");
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));

        rig.forge.set_checks(&head, Checks::Passed);
        let (id, question) = ruling_report(step(&runner).unwrap());
        assert_eq!(id, 1);
        assert_eq!(
            question,
            format!(
                "Merge pull request #71 at {} into main? `shep trigger shep rule '1 yes'` \
                 merges it, and `shep trigger shep rule '1 no <note>'` sends the worker your note.",
                &head[..7]
            )
        );
        assert_eq!(rig.forge.comments(), [(71, question.clone())]);
        let status = rig.ask(&runner, "status", None);
        assert_eq!(
            status["rulings"],
            json!([{
                "id": 1,
                "question": question,
                "pull_request": 71,
                "kind": { "kind": "merge", "head": head },
            }])
        );
        assert_eq!(
            status["work_item"]["phase"],
            json!({ "state": "ruling", "id": 1 })
        );
        assert_eq!(step(&runner).unwrap(), None, "a parked worker waits");
    }

    #[test]
    fn a_red_run_is_the_workers_next_turn_naming_the_failed_checks() {
        let (rig, runner, head) = Rig::with_pull_request("koji");
        let failed = vec!["lint".to_owned(), "test".to_owned()];
        rig.forge.set_checks(&head, Checks::Failed(failed.clone()));
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::CiFailed {
                issue: 7,
                pull_request: 71,
                head: head.clone(),
                checks: failed,
            })
        );

        rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
        step(&runner).unwrap();
        let [first, fix] = rig.claude.calls().try_into().unwrap();
        assert_eq!(fix.session, Session::Resume(first.session.id().clone()));
        let named = format!(
            "CI failed on your pull request #71 at {}: lint, test. ",
            &head[..7]
        );
        assert!(fix.prompt.starts_with(&named), "{}", fix.prompt);

        let fixed = rig.forge.head_of("kelpie/7").unwrap();
        assert_ne!(fixed, head);
        rig.forge.set_checks(&fixed, Checks::Passed);
        let (id, question) = ruling_report(step(&runner).unwrap());
        assert_eq!(id, 1);
        assert!(question.contains(&fixed[..7]), "{question}");
    }

    #[test]
    fn a_second_red_run_on_a_head_the_worker_left_alone_parks_it() {
        let (rig, runner, head) = Rig::with_pull_request("rotom");
        rig.forge
            .set_checks(&head, Checks::Failed(vec!["lint".into()]));
        step(&runner).unwrap();
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();

        let (_, question) = ruling_report(step(&runner).unwrap());
        let parked = format!(
            "CI failed again on pull request #71 at {}, and the worker pushed no fix: lint.",
            &head[..7]
        );
        assert!(question.starts_with(&parked), "{question}");
        assert_eq!(rig.claude.calls().len(), 2, "the red run went out once");
        assert_eq!(step(&runner).unwrap(), None);
    }

    #[test]
    fn a_head_without_checks_waits_out_the_grace_then_counts_as_green() {
        let (rig, runner, head) = Rig::with_pull_request("chelone");
        rig.forge.set_checks(&head, Checks::None);
        assert_eq!(step(&runner).unwrap(), None);
        rig.clock.advance(NO_CHECKS_GRACE - 1);
        assert_eq!(step(&runner).unwrap(), None);
        rig.clock.advance(1);
        ruling_report(step(&runner).unwrap());
    }

    #[test]
    fn a_ruling_whose_comment_fails_still_stands_in_status_and_the_log() {
        let (rig, runner, head) = Rig::with_pull_request("golbat");
        rig.forge.set_checks(&head, Checks::Passed);
        rig.forge.set_comments_down(true);
        let Some(StepReport::Ruling { comment_failed, .. }) = step(&runner).unwrap() else {
            panic!("no ruling was raised");
        };
        assert_eq!(
            comment_failed.as_deref(),
            Some("gh failed: comments are down")
        );
        assert_eq!(rig.ask(&runner, "status", None)["rulings"][0]["id"], 1);
    }

    #[test]
    fn a_pull_request_merged_by_hand_ends_the_work_item_with_no_merge_by_kelpie() {
        let (rig, runner, _) = Rig::with_pull_request("zeus");
        rig.forge.set_state(71, PullRequestState::Merged);
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::Finished {
                issue: 7,
                pull_request: Some(71),
                merged: true
            })
        );
        assert_eq!(rig.forge.merges(), []);
        assert!(!rig.worktree_7().exists());
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    }

    #[test]
    fn a_closed_pull_request_parks_the_worker_and_a_yes_keeps_its_branch() {
        let (rig, runner, head) = Rig::with_pull_request("reactmap");
        rig.forge.set_state(71, PullRequestState::Closed);
        let (id, question) = ruling_report(step(&runner).unwrap());
        assert!(
            question.starts_with("Pull request #71 was closed without merging."),
            "{question}"
        );
        rig.ask(&runner, "rule", Some(&format!("{id} yes")));
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::Finished {
                issue: 7,
                pull_request: Some(71),
                merged: false
            })
        );
        assert_eq!(rig.forge.head_of("kelpie/7"), Some(head));
        assert!(!rig.worktree_7().exists());
        assert_eq!(git(&rig.repo(), &["branch", "--list", "kelpie/7"]), "");
    }

    #[test]
    fn a_work_item_saved_before_the_gate_existed_is_left_alone() {
        let (rig, runner, head) = Rig::with_pull_request("xilriws");
        drop(runner);
        let state = rig.paths().state;
        let text = std::fs::read_to_string(&state).unwrap();
        let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        let item = saved["work_item"].as_object_mut().unwrap();
        item.remove("phase");
        item.remove("red_head");
        std::fs::write(&state, saved.to_string()).unwrap();

        let runner = rig.open().unwrap();
        rig.forge.set_checks(&head, Checks::Passed);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.forge.comments(), []);
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"],
            json!({ "state": "implement" })
        );
    }
}
