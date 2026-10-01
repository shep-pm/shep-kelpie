//! A pull request in the forge's merge queue
//!
//! With a merge queue on `main`, the merge call only queues the pull
//! request. The forge tests it on top of the ones ahead of it and merges it,
//! or removes it. A removal goes back to the worker like red CI, with the
//! forge's reason, and a second one at the same head is a refused merge.

use super::super::Runner;
use super::super::gate::{settled, short};
use super::super::report::Begin;
use crate::skills::Step;
use crate::state::{RulingKind, StateError};
use crate::work_item::MergeQueued;

impl Runner {
    // Saved before anything else, so a restart goes on waiting on the queue.
    pub(super) fn queue_marked(&mut self, removals: u32) -> Result<Begin, StateError> {
        let since = self.ports.clock.now();
        self.update(|item| item.merge_queued = Some(MergeQueued { removals, since }))?;
        Ok(Begin::Idle)
    }

    // A pull request the forge shows open and in no queue, with no removal
    // since kelpie queued it, may only be slow to register, so it gets the
    // checks' settling time before it counts as gone.
    pub(super) fn queued(
        &mut self,
        number: u64,
        head: String,
        queued: MergeQueued,
    ) -> Result<Begin, StateError> {
        let repo = self.settings.forge.clone();
        let standing = match self.ports.forge.merge_queue(&repo, number) {
            Ok(standing) => standing,
            Err(e) => {
                return Ok(self.gate_failed(format!("cannot read #{number}'s queue: {e}")));
            }
        };
        if standing.queued {
            return Ok(Begin::Idle);
        }
        let removed = standing.removals > queued.removals;
        if !removed && !settled(queued.since, self.ports.clock.now()) {
            return Ok(Begin::Idle);
        }
        let reason = standing
            .reason
            .filter(|_| removed)
            .unwrap_or_else(|| "it is neither merged nor in the queue".to_owned());
        self.update(|item| item.merge_queued = None)?;
        let item = self.current().expect("a merge is of a work item");
        if item.red_head.as_deref() == Some(head.as_str()) {
            return self.raise(number, RulingKind::MergeRefused { head, reason });
        }
        let prompt = self
            .skills
            .invoke(Step::Ci, &queue_prompt(number, &head, &reason));
        self.back_to_worker(number, head, vec!["merge queue".to_owned()], prompt)
    }
}

fn queue_prompt(number: u64, head: &str, reason: &str) -> String {
    format!(
        "The merge queue removed your pull request #{number} at {}: {reason}. The queue \
         tests a pull request on top of `main` and the pull requests ahead of it, so \
         this can come from the combination and not from your branch alone. \
         `gh run list --event merge_group` lists the queue's runs, and \
         `gh run view <run id> --log-failed` shows what failed. Fix it on this branch, \
         commit, and push with `git push origin HEAD`.",
        short(head)
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use crate::ports::{Cost, Session, Usage};
    use crate::runner::gate::CHECKS_SETTLE;
    use crate::runner::{Runner, StepReport, step};
    use crate::test::{Rig, Scripted};

    const REMOVAL: &str = "Required status check \"test\" failed.";

    // A yes with the repo's merge queue on, and the pass that queues the
    // pull request: it is open, its work item is still there, nothing reports.
    fn queued(project: &str) -> (Rig, Mutex<Runner>, String) {
        let (rig, runner, head) = Rig::parked(project);
        rig.forge.set_merge_queue(true);
        rig.ask(&runner, "rule", Some("1 yes"));
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::MarkedReady { .. })
        ));
        rig.clock.advance(CHECKS_SETTLE);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.forge.merges(), [(71, head.clone())]);
        (rig, runner, head)
    }

    #[test]
    fn a_queued_pull_request_is_recorded_merged_once_the_queue_lands_it() {
        let (rig, runner, _) = queued("koji");
        assert_eq!(step(&runner).unwrap(), None, "still in the queue");
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"]["issue"], json!(7));

        rig.forge.queue_merges(71);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Finished {
                issue: 7,
                merged: true,
                ..
            })
        ));
        assert_eq!(rig.forge.merges().len(), 1, "queued once, not again");
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"], json!(null));
    }

    #[test]
    fn main_moving_under_a_queued_pull_request_does_not_withdraw_it() {
        let (rig, runner, _) = queued("rotom");
        rig.land_on_origin("ahead-in-the-queue.txt");
        assert_eq!(step(&runner).unwrap(), None);
        rig.clock.advance(CHECKS_SETTLE);
        assert_eq!(step(&runner).unwrap(), None);
    }

    #[test]
    fn a_runner_restarted_while_the_pull_request_is_queued_keeps_waiting() {
        let (rig, runner, _) = queued("zeus");
        drop(runner);
        let runner = rig.open().unwrap();
        assert_eq!(step(&runner).unwrap(), None);
        rig.forge.queue_merges(71);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Finished { merged: true, .. })
        ));
        assert_eq!(rig.forge.merges().len(), 1);
    }

    #[test]
    fn a_queued_pull_request_found_after_a_lost_mark_is_not_merged_again() {
        let (rig, runner, _) = Rig::parked("hamster");
        rig.forge.set_merge_queue(true);
        rig.ask(&runner, "rule", Some("1 yes"));
        step(&runner).unwrap();
        rig.clock.advance(CHECKS_SETTLE);
        // The merge call queued it, and the runner never saved the mark.
        rig.forge.queue_enqueues(71);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.forge.merges(), [], "no second merge call");
        rig.forge.queue_merges(71);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Finished { merged: true, .. })
        ));
    }

    #[test]
    fn a_pull_request_the_queue_removes_goes_back_to_the_worker_like_red_ci() {
        let (rig, runner, head) = queued("chelone");
        rig.forge.queue_removes(71, REMOVAL);
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::CiFailed {
                issue: 7,
                pull_request: 71,
                head: head.clone(),
                checks: vec!["merge queue".into()],
            })
        );

        rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
        step(&runner).unwrap();
        let [first, fix] = rig.claude.calls().try_into().unwrap();
        assert_eq!(fix.session, Session::Resume(first.session.id().clone()));
        let named = format!("removed your pull request #71 at {}: {REMOVAL}", &head[..7]);
        assert!(fix.prompt.contains(&named), "{}", fix.prompt);
        assert!(
            !fix.prompt.to_lowercase().contains("budget"),
            "{}",
            fix.prompt
        );
    }

    #[test]
    fn a_second_removal_at_the_same_head_is_a_refused_merge_for_the_maintainer() {
        let (rig, runner, head) = queued("reactmap");
        rig.forge.queue_removes(71, REMOVAL);
        step(&runner).unwrap();
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();
        let Some(StepReport::Ruling { id: 2, .. }) = rig.verdict(&runner) else {
            panic!("the merge ruling was not raised again");
        };

        rig.ask(&runner, "rule", Some("2 yes"));
        step(&runner).unwrap();
        rig.clock.advance(CHECKS_SETTLE);
        step(&runner).unwrap();
        rig.forge.queue_removes(71, REMOVAL);
        let Some(StepReport::Ruling { question, .. }) = step(&runner).unwrap() else {
            panic!("no ruling for the second removal");
        };
        let named = format!("Kelpie could not merge pull request #71 at {}", &head[..7]);
        assert!(question.starts_with(&named), "{question}");
        assert!(question.contains(REMOVAL), "{question}");
    }

    #[test]
    fn a_pull_request_in_no_queue_with_no_removal_is_waited_on_then_treated_as_gone() {
        let (rig, runner, _) = queued("shep");
        rig.forge.queue_forgets(71);
        assert_eq!(step(&runner).unwrap(), None, "the forge may be slow");
        rig.clock.advance(CHECKS_SETTLE);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::CiFailed { .. })
        ));
    }
}
