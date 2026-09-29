//! Turns that end without a reply to act on: one that failed, and one that
//! ran past its ceiling
//!
//! Each parks the work item on a ruling whose yes puts the turn back, and
//! whose no stops the work item.

use std::time::Duration;

use crate::ports::Timestamp;
use crate::runner::Runner;
use crate::runner::report::{Begin, StepReport};
use crate::runner::ruling::park;
use crate::state::{ProjectState, RulingKind, StateError};
use crate::work_item::Turn;

impl Runner {
    pub(super) fn turn_ceiling(&self) -> Duration {
        Duration::from_secs(u64::from(self.settings.worker.turn_timeout.get()) * 60)
    }

    pub(super) fn park_ceiling_passed(&mut self, now: Timestamp) -> Result<Begin, StateError> {
        let mut next = self.state.clone();
        if let Some(item) = next.work_item.as_mut() {
            item.turn = Turn::Ended { at: now };
        }
        let mut report = timed_out(self.project.as_str(), &mut next);
        self.save(next)?;
        self.fill_comment_failed(&mut report);
        Ok(Begin::Report(report))
    }
}

// Parks the work item on a turn-ceiling ruling and builds its report. Shared
// by a call that actually hit `ClaudeError::TimedOut` and by a restart that
// finds a turn already past its ceiling with no call spent. The caller sets
// `item.turn` beforehand: this only raises the ruling. `comment_failed` is
// filled in afterwards, once the ruling has actually been posted.
pub(super) fn timed_out(project: &str, next: &mut ProjectState) -> StepReport {
    let item = next
        .work_item
        .as_mut()
        .expect("a turn ceiling is about a work item");
    let (issue, session, pull_request) = (item.issue, item.session.clone(), item.pull_request);
    let phase = Some(item.phase.clone());
    let (_, id, question) = park(
        project,
        next,
        pull_request,
        RulingKind::TurnTimeout { phase },
    );
    StepReport::TimedOut {
        issue,
        session,
        pull_request,
        id,
        question,
        comment_failed: None,
    }
}

// Marks the turn failed and parks the work item on a ruling carrying why,
// keeping the turn as it stood so a yes can put it back. `comment_failed` is
// filled in afterwards, once the ruling has actually been posted.
pub(super) fn failed(
    project: &str,
    next: &mut ProjectState,
    at: Timestamp,
    reason: String,
) -> StepReport {
    let item = next
        .work_item
        .as_mut()
        .expect("a failed turn is about a work item");
    let failure = Turn::Failed {
        at,
        reason: reason.clone(),
    };
    let retry = std::mem::replace(&mut item.turn, failure);
    let (issue, pull_request) = (item.issue, item.pull_request);
    let kind = RulingKind::TurnFailed {
        reason,
        phase: item.phase.clone(),
        retry,
    };
    let (_, id, question) = park(project, next, pull_request, kind);
    StepReport::Failed {
        issue,
        pull_request,
        id,
        question,
        comment_failed: None,
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::time::Duration;

    use serde_json::json;

    use super::super::tests::{usage, with_issue_7};
    use crate::ports::{ClaudeError, Cost, Session};
    use crate::runner::{StepReport, step};
    use crate::test::{Rig, Scripted};

    #[test]
    fn a_failed_turn_raises_a_ruling_carrying_why_and_alerts_like_the_rest() {
        let (rig, runner) = with_issue_7("zeus");
        rig.claude
            .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
        let Some(StepReport::Failed {
            issue,
            pull_request,
            id,
            question,
            ..
        }) = step(&runner).unwrap()
        else {
            panic!("the failed turn raised no ruling");
        };
        assert_eq!((issue, pull_request, id), (7, None, 1));
        assert!(
            question
                .starts_with("The worker's turn on issue #7 failed: claude failed: overloaded."),
            "{question}"
        );
        let item = &rig.ask(&runner, "status", None)["work_item"];
        assert_eq!(
            item["turn"],
            json!({ "state": "failed", "at": Rig::EPOCH, "reason": "claude failed: overloaded" })
        );
        assert_eq!(item["phase"], json!({ "state": "ruling", "id": 1 }));

        assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
        let [(_, alert)] = rig.alerts.posts().try_into().unwrap();
        assert_eq!(alert.text, question);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.claude.calls().len(), 1);
    }

    #[test]
    fn a_yes_on_a_failed_turn_resumes_its_session() {
        let (rig, runner) = with_issue_7("zeus");
        rig.claude
            .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
        step(&runner).unwrap();
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Ended { .. })
        ));
        let [failed, retried] = rig.claude.calls().try_into().unwrap();
        assert_eq!(
            retried.session,
            Session::Resume(failed.session.id().clone())
        );
        assert_eq!(
            retried.prompt,
            "Your last turn failed before it finished. \
             Carry on with the work item from where you left off."
        );
    }

    #[test]
    fn a_retry_whose_session_never_began_starts_it_over_from_the_issue() {
        let (rig, runner) = with_issue_7("zeus");
        rig.claude
            .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
        step(&runner).unwrap();
        let first = rig.claude.calls()[0].session.id().clone();
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.claude.script([
            Scripted::Fail(ClaudeError::NoSession(first.clone())),
            Scripted::Reply(usage(1), Cost(1)),
        ]);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Ended { .. })
        ));
        let [original, _, again] = rig.claude.calls().try_into().unwrap();
        assert_eq!(again.session, Session::New(first));
        assert_eq!(again.prompt, original.prompt);
    }

    #[test]
    fn a_retried_turn_gets_a_whole_ceiling_however_long_the_ruling_waited() {
        let (rig, runner) = with_issue_7("zeus");
        rig.claude
            .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
        step(&runner).unwrap();
        rig.clock.advance(2000);
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
        step(&runner).unwrap();
        let [_, retried] = rig.claude.calls().try_into().unwrap();
        assert_eq!(retried.timeout, Some(Duration::from_secs(3600)));
    }

    #[test]
    fn a_no_on_a_failed_turn_stops_the_work_item_the_way_a_timed_out_one_does() {
        let (rig, runner) = with_issue_7("rotom");
        rig.claude
            .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
        step(&runner).unwrap();
        rig.ask(&runner, "rule", Some("1 no not worth another go"));
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Finished {
                issue: 7,
                pull_request: None,
                merged: false,
                ..
            })
        ));
        assert!(!rig.worktree_7().exists());
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    }

    #[test]
    fn a_turn_past_its_ceiling_is_stopped_and_a_yes_resumes_its_session() {
        let (rig, runner) = with_issue_7("zeus");
        rig.claude.script([Scripted::Fail(ClaudeError::TimedOut)]);
        let Some(StepReport::TimedOut {
            issue,
            session,
            pull_request,
            id,
            question,
            ..
        }) = step(&runner).unwrap()
        else {
            panic!("the timed-out turn raised no ruling");
        };
        assert_eq!((issue, pull_request, id), (7, None, 1));
        assert!(
            question.starts_with(
                "The worker on issue #7 has been running past its turn's ceiling, \
                 and kelpie stopped it."
            ),
            "{question}"
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"],
            json!({ "state": "ruling", "id": 1 })
        );

        rig.ask(&runner, "rule", Some("1 yes"));
        rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Ended { .. })
        ));
        let [_, resumed] = rig.claude.calls().try_into().unwrap();
        assert_eq!(resumed.session, Session::Resume(session));
        assert_eq!(
            resumed.prompt,
            "Kelpie stopped your last turn: it ran past its ceiling. \
             Carry on with the work item from where you left off."
        );
    }

    #[test]
    fn a_no_on_a_timed_out_turn_stops_the_work_item_keeping_nothing_of_its_own() {
        let (rig, runner) = with_issue_7("rotom");
        rig.claude.script([Scripted::Fail(ClaudeError::TimedOut)]);
        step(&runner).unwrap();
        rig.ask(&runner, "rule", Some("1 no not worth waiting for"));
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Finished {
                issue: 7,
                pull_request: None,
                merged: false,
                ..
            })
        ));
        assert!(!rig.worktree_7().exists());
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    }

    #[test]
    fn a_worker_turn_carries_the_projects_timeout() {
        let (rig, runner) = with_issue_7("golbat");
        rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
        step(&runner).unwrap();
        let [seen] = rig.claude.calls().try_into().unwrap();
        assert_eq!(seen.timeout, Some(std::time::Duration::from_secs(3600)));
    }

    #[test]
    fn a_restart_before_the_ceiling_passes_resumes_with_only_the_time_left() {
        let (rig, runner) = with_issue_7("zeus");
        rig.claude.script([Scripted::Kill]);
        let _ = catch_unwind(AssertUnwindSafe(|| step(&runner)));
        drop(runner);

        rig.clock.advance(2000);
        let runner = rig.open().unwrap();
        rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
        step(&runner).unwrap();
        let [_, resumed] = rig.claude.calls().try_into().unwrap();
        assert_eq!(
            resumed.timeout,
            Some(std::time::Duration::from_secs(1600)),
            "the restart must not reset the ceiling to a fresh hour"
        );
    }

    #[test]
    fn a_restart_after_the_ceiling_passed_parks_it_with_no_call_spent() {
        let (rig, runner) = with_issue_7("chelone");
        rig.claude.script([Scripted::Kill]);
        let _ = catch_unwind(AssertUnwindSafe(|| step(&runner)));
        drop(runner);

        // The whole ceiling, and then some, passes while kelpie is down.
        rig.clock.advance(3601);
        let runner = rig.open().unwrap();
        let Some(StepReport::TimedOut { id, .. }) = step(&runner).unwrap() else {
            panic!("a turn found past its ceiling on restart raised no ruling");
        };
        assert_eq!(id, 1);
        assert_eq!(
            rig.claude.calls().len(),
            1,
            "the killed call, and no second one"
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"],
            json!({ "state": "ruling", "id": 1 })
        );
    }
}
