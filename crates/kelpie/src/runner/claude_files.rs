//! Claude Code's own files, checked at the gate and before each Claude call
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

impl Runner {
    /// Parks the worker before a gate step that may call Claude in the worktree
    pub(super) fn fence_gate(&mut self) -> Result<Option<Begin>, StateError> {
        if let Some(parked) = self.claude_files_changed(false)? {
            return Ok(Some(parked));
        }
        self.refuse_differing()
    }

    /// Parks the worker if the branch on `origin` changes Claude Code's own files
    ///
    /// When the check itself fails, a `strict` caller stops the step, and
    /// any other carries on: the worktree check still guards its calls.
    pub(super) fn claude_files_changed(
        &mut self,
        strict: bool,
    ) -> Result<Option<Begin>, StateError> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("the gate runs on a work item");
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
            Err(e) if strict => Ok(Some(self.gate_failed(format!(
                "cannot check #{number} for changes to Claude Code's own files: {e}"
            )))),
            Err(_) => Ok(None),
        }
    }

    /// Why no Claude call may run in the work item's worktree, if one may not
    pub(super) fn claude_files_differ(&self) -> Option<String> {
        let item = self.state.work_item.as_ref()?;
        let accepted = item.claude_files_accepted.as_deref();
        match fence::differ(&self.settings.repo, &item.worktree, accepted) {
            Ok(files) if files.is_empty() => None,
            Ok(files) => Some(format!(
                "Claude Code's own files in the worktree differ from main's: {}",
                files.join(", ")
            )),
            Err(e) => Some(format!(
                "cannot check the worktree's Claude Code files: {e}"
            )),
        }
    }

    /// Fails the step, parking the worker, when [`Self::claude_files_differ`] says so
    pub(super) fn refuse_differing(&mut self) -> Result<Option<Begin>, StateError> {
        let Some(reason) = self.claude_files_differ() else {
            return Ok(None);
        };
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let mut report = failed(self.project.as_str(), &mut next, now, reason);
        self.save(next)?;
        self.fill_comment_failed(&mut report);
        Ok(Some(Begin::Report(report)))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;

    use crate::ports::{Checks, Cost, Usage};
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
                "Pull request #71 at {} changes Claude Code's own files, which run outside \
                 the worker's sandbox: .claude/settings.json. `shep trigger shep rule '1 yes'` \
                 accepts them at that head and kelpie carries on, and \
                 `shep trigger shep rule '1 no <note>'` stops the work item, keeping its \
                 branch and pull request on the forge.",
                short(&head)
            )
        );
        assert_eq!(
            rig.forge.comments(),
            [(
                71,
                "This pull request changes Claude Code's own files: .claude/settings.json.\
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
                "Claude Code's own files in the worktree differ from main's: \
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
