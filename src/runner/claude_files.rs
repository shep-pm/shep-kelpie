//! Agents' own files, checked at the gate and before each Claude call
//!
//! A pull request that changes them parks the worker on a ruling before any
//! review or CodeRabbit step, and before CI, since a change there is the
//! maintainer's call. A worktree whose copies differ from `main`'s, and from
//! any head the maintainer accepted, runs no Claude call: the turn fails.

use super::Runner;
use super::report::Begin;
use super::turn::failed;
use crate::fence;
use crate::state::{RulingKind, StateError};

/// What a step does when the branch cannot be checked
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Unchecked {
    /// Stops the step: CI, which leads to the merge
    Stop,
    /// Carries on: the worktree check still guards every call it makes
    CarryOn,
}

impl Runner {
    /// Parks the worker before a gate step that may call Claude in the worktree
    pub(super) fn fence_gate(&mut self) -> Result<Option<Begin>, StateError> {
        if let Some(parked) = self.claude_files_changed(Unchecked::CarryOn)? {
            return Ok(Some(parked));
        }
        self.refuse_differing()
    }

    /// Parks the worker if the branch on `origin` changes agents' own files
    pub(super) fn claude_files_changed(
        &mut self,
        unchecked: Unchecked,
    ) -> Result<Option<Begin>, StateError> {
        let item = self.current().expect("the gate runs on a work item");
        let Some(number) = item.pull_request else {
            return Ok(None);
        };
        let accepted = item.claude_files_accepted.as_deref();
        let phase = item.phase.clone();
        match fence::changed(&self.settings.repo, &item.branch, accepted) {
            Ok((_, files)) if files.is_empty() => Ok(None),
            Ok((head, files)) => {
                let kind = RulingKind::ClaudeFiles { head, files, phase };
                self.raise(number, kind).map(Some)
            }
            Err(e) if unchecked == Unchecked::Stop => Ok(Some(self.gate_failed(format!(
                "cannot check #{number} for changes to agents' own files: {e}"
            )))),
            Err(_) => Ok(None),
        }
    }

    /// Why no Claude call may run in the work item's worktree, if one may not
    pub(super) fn claude_files_refusal(&self) -> Option<String> {
        let item = self.current()?;
        let accepted = item.claude_files_accepted.as_deref();
        match fence::differ(&self.settings.repo, &item.worktree, accepted) {
            Ok(files) if files.is_empty() => None,
            Ok(files) => Some(format!(
                "agents' own files in the worktree differ from main's: {}",
                files.join(", ")
            )),
            Err(e) => Some(format!(
                "cannot check the worktree's agents' own files: {e}"
            )),
        }
    }

    /// Fails the step, parking the worker, when [`Self::claude_files_refusal`] says so
    pub(super) fn refuse_differing(&mut self) -> Result<Option<Begin>, StateError> {
        let Some(reason) = self.claude_files_refusal() else {
            return Ok(None);
        };
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let issue = self.current().expect("the gate runs on a work item").issue;
        let mut report = failed(self.names(), &mut next, issue, now, reason);
        self.save(next)?;
        self.fill_comment_failed(&mut report);
        Ok(Some(Begin::Report(report)))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;

    use crate::ports::{Checks, Cost, MaintainerReview, Usage};
    use crate::runner::Runner;
    use crate::runner::gate::short;
    use crate::runner::report::StepReport;
    use crate::runner::step;
    use crate::test::{Rig, Scripted};

    const HOOK: &str =
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"touch /tmp/x"}]}]}}"#;

    // A worker whose first turn pushes `file` on pull request 71.
    fn pushed(file: &'static str) -> (Rig, Mutex<Runner>) {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([Scripted::Push(file, HOOK)]);
        step(&runner).unwrap();
        (rig, runner)
    }

    fn ruling(report: Option<StepReport>) -> (u64, String) {
        match report {
            Some(StepReport::Ruling { id, question, .. }) => (id, question),
            other => panic!("no ruling was raised: {other:?}"),
        }
    }

    fn failed(report: Option<StepReport>) -> String {
        match report {
            Some(StepReport::Failed { question, .. }) => question,
            other => panic!("no failed turn: {other:?}"),
        }
    }

    #[test]
    fn a_pull_request_changing_claudes_settings_parks_before_any_review() {
        let (rig, runner) = pushed(".claude/settings.json");
        let head = rig.forge.head_of("kelpie/7").unwrap();
        let (id, question) = ruling(step(&runner).unwrap());
        assert_eq!(
            question,
            format!(
                "Pull request #71 at {} changes agents' own files, which run outside \
                 the worker's sandbox: .claude/settings.json. `shep kelpie rule 1 yes` \
                 accepts them at that head and kelpie carries on, and \
                 `shep kelpie rule 1 no <note>` stops the work item, keeping its \
                 branch and pull request on the forge.",
                short(&head)
            )
        );
        assert_eq!(
            rig.forge.comments(),
            [(
                71,
                "This pull request changes agents' own files: .claude/settings.json.\
                 \n\nWaiting on the maintainer."
                    .to_owned()
            )]
        );
        assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id }));
        assert_eq!(step(&runner).unwrap(), None);
        assert!(rig.reviewer.seen().is_empty());
        assert_eq!(rig.claude.all_calls().len(), 1);
    }

    #[test]
    fn a_yes_accepts_the_change_and_the_review_runs_in_that_worktree() {
        let (rig, runner) = pushed(".claude/settings.json");
        ruling(step(&runner).unwrap());
        step(&runner).unwrap(); // the alert
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.claude.script([Scripted::Text("CLEAN")]);
        step(&runner).unwrap(); // review round 1, qwen
        step(&runner).unwrap(); // review round 2, claude
        assert_eq!(rig.reviewer.seen().len(), 1);
        assert_eq!(rig.claude.all_calls().len(), 2);
    }

    #[test]
    fn under_auto_a_change_to_claudes_settings_merges_only_after_a_yes() {
        let rig = Rig::new("shep");
        rig.merge_auto();
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([
            Scripted::Push(".claude/agents/helper.md", "an agent\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap();
        let (id, _) = ruling(step(&runner).unwrap());
        step(&runner).unwrap(); // the alert
        let head = rig.forge.head_of("kelpie/7").unwrap();
        rig.forge.set_checks(&head, Checks::Passed);
        assert_eq!(step(&runner).unwrap(), None);
        assert!(rig.forge.merges().is_empty());

        rig.ask(&runner, "rule", Some(&format!("{id} yes")));
        step(&runner).unwrap(); // review round 1, qwen
        step(&runner).unwrap(); // review round 2, claude
        // CI, then marking the draft ready and waiting out its checks.
        for _ in 0..4 {
            rig.verdict(&runner);
        }
        assert_eq!(rig.forge.merges(), [(71, head)]);
    }

    #[test]
    fn a_rework_of_a_branch_that_changes_them_asks_before_its_first_turn() {
        let rig = Rig::new("shep");
        rig.push_by_hand("kelpie/7", ".mcp.json");
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.forge.review(
            71,
            MaintainerReview {
                id: "PRR_71".into(),
                changes_requested: false,
                body: "Tidy it up.".into(),
                comments: vec![],
            },
        );
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "rework", Some("71"));
        let (id, question) = ruling(step(&runner).unwrap());
        assert!(question.contains("sandbox: .mcp.json."), "{question}");
        assert!(rig.claude.all_calls().is_empty());

        step(&runner).unwrap(); // the alert
        rig.ask(&runner, "rule", Some(&format!("{id} yes")));
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(0))]);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Ended { .. })
        ));
    }

    // An adoption goes straight to CI, with no review step in front of it.
    #[test]
    fn an_adopted_branch_that_changes_them_parks_at_ci() {
        let rig = Rig::new("shep");
        rig.push_by_hand("fix/tools", ".mcp.json");
        rig.forge.open_pull_request(80, "fix/tools", &[5]);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "adopt", Some("80"));
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Adopted { .. })
        ));
        let (_, question) = ruling(step(&runner).unwrap());
        assert!(question.contains("sandbox: .mcp.json."), "{question}");
        assert_eq!(rig.claude.all_calls().len(), 0);
    }

    #[test]
    fn a_settings_file_left_in_the_worktree_stops_a_coderabbit_step() {
        let rig = Rig::new("shep");
        rig.coderabbit_on();
        let head = rig.push_by_hand("fix/timeline", "work.txt");
        rig.forge.open_pull_request(80, "fix/timeline", &[5]);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "adopt", Some("80"));
        step(&runner).unwrap(); // the adoption
        rig.forge.set_checks(&head, Checks::Passed);
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::MarkedReady { .. })
        ));
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Summoned { .. })
        ));
        let planted = rig.paths().worktree(5).join(".claude/settings.local.json");
        fs::create_dir_all(planted.parent().unwrap()).unwrap();
        fs::write(&planted, HOOK).unwrap();
        let question = failed(step(&runner).unwrap());
        assert!(
            question.contains(".claude/settings.local.json"),
            "{question}"
        );
        assert_eq!(rig.claude.all_calls().len(), 0);
    }

    #[test]
    fn a_no_on_a_change_to_claudes_settings_stops_the_work_item() {
        let (rig, runner) = pushed(".mcp.json");
        let (_, question) = ruling(step(&runner).unwrap());
        assert!(question.contains("sandbox: .mcp.json."), "{question}");
        step(&runner).unwrap(); // the alert
        rig.ask(&runner, "rule", Some("1 no revert it"));
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Finished {
                issue: 7,
                merged: false,
                ..
            })
        ));
        assert_eq!(rig.claude.all_calls().len(), 1);
    }

    #[test]
    fn a_settings_file_left_in_the_worktree_runs_no_review_call() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude
            .script([Scripted::Plant(".claude/settings.local.json", HOOK)]);
        step(&runner).unwrap();
        let question = failed(step(&runner).unwrap());
        assert!(
            question.contains(
                "agents' own files in the worktree differ from main's: \
                 .claude/settings.local.json."
            ),
            "{question}"
        );
        assert!(rig.reviewer.seen().is_empty());
        assert_eq!(rig.claude.all_calls().len(), 1);
    }

    #[test]
    fn a_worktree_with_a_changed_settings_file_refuses_the_turn_until_it_is_put_back() {
        let (rig, runner, head) = Rig::with_pull_request("shep");
        let planted = rig.worktree_7().join(".claude/settings.local.json");
        fs::create_dir_all(planted.parent().unwrap()).unwrap();
        fs::write(&planted, HOOK).unwrap();
        rig.forge
            .set_checks(&head, Checks::Failed(vec!["test".into()]));
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::CiFailed { .. })
        ));
        let calls = rig.claude.calls().len();
        let question = failed(step(&runner).unwrap());
        assert!(
            question.contains(".claude/settings.local.json"),
            "{question}"
        );
        assert_eq!(rig.claude.calls().len(), calls);
        step(&runner).unwrap(); // the alert

        fs::remove_file(&planted).unwrap();
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(0))]);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Ended { .. })
        ));
        assert_eq!(rig.claude.calls().len(), calls + 1);
    }
}
