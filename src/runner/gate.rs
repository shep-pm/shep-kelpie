//! The gate between the worker's pull request and the merge ruling
//!
//! Each step looks once at the pull request's head. A label, ready or head
//! change kelpie did not make parks the worker before anything else. A
//! branch that changes agents' own files parks it next. A
//! branch without the latest `main` is rebased and pushed, or merged when
//! it is adopted or holds a merge already, and a conflict is
//! the worker's next turn, naming the files. A conflict the worker left
//! standing parks it on a ruling. A pending run, or none yet, waits for the
//! next step. A red run is the worker's next turn, naming the checks that
//! failed. A green run raises the merge ruling, or under `auto` merges. A
//! project without CI skips the checks.

use super::Runner;
use super::claude_files::Unchecked;
use super::report::{Begin, StepReport};
use super::rework::HUMAN;
use crate::board::READY;
use crate::ports::{Checks, PullRequestState, Timestamp};
use crate::settings::MergeAuthority;
use crate::skills::Step;
use crate::state::{RulingKind, StateError, Stuck};
use crate::work_item::{Phase, Turn, foreign_change};
use crate::worktree::{self, Base};

// GitHub registers the checks a push or a ready pull request starts within
// seconds, one at a time. Kelpie trusts a rollup once two minutes have passed.
pub(crate) const CHECKS_SETTLE: u64 = 120;

mod catch_up;

impl Runner {
    pub(super) fn check_ci(&mut self) -> Result<Begin, StateError> {
        let item = self.current().expect("CI runs on a work item");
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
            // The maintainer merged it by hand, which is their own ruling,
            // unless it is kelpie's `auto` merge whose answer was an error.
            PullRequestState::Merged => {
                let notice = item.merge_tried.as_deref() == Some(pr.head.as_str());
                return self.merged(item.issue, number, pr.head, notice);
            }
            PullRequestState::Closed => return self.raise(number, Stuck::Closed.into()),
        }
        let known = item.known.clone();
        let now = self.ports.clock.now();
        let since = if seen.as_deref() == Some(pr.head.as_str()) {
            since
        } else {
            let head = Some(pr.head.clone());
            self.update(|item| item.phase = Phase::Ci { head, since: now })?;
            now
        };
        let base = match self.base_of(&pr.head) {
            Ok(Base::Lagging) => return Ok(Begin::Idle),
            Ok(base) => base,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        // A late round that left the head it read pushed no fix, so a
        // catch-up of that head is not one.
        let head = Some(pr.head.as_str());
        if self
            .current()
            .is_some_and(|i| i.late_from.as_deref() == head)
        {
            self.update(|item| item.late_from = None)?;
        }
        // Past the lag, the forge's head is the branch on `origin`, not a
        // worker's push still arriving. A rework asked for mid-flight waits.
        let mut labels = pr.labels;
        labels.retain(|l| l != READY);
        if let Some((seen, description)) = foreign_change(&known, &labels, !pr.draft, &pr.head) {
            let kind = RulingKind::ForeignChange {
                description,
                known: seen,
            };
            return self.raise(number, kind);
        }
        if let Some(parked) = self.claude_files_changed(Unchecked::Stop)? {
            return Ok(parked);
        }
        if known.head.is_none() {
            if self.settings.merge_authority == MergeAuthority::Auto {
                return self.regate_unknown(number, &pr.head);
            }
            let head = Some(pr.head.clone());
            self.update(|item| item.known.head = head)?;
        }
        if base == Base::Behind {
            return self.rebase(number, &pr.head);
        }
        if !self.settings.ci {
            return self.passed(number, pr.head);
        }
        // A check set registers a check at a time, so a verdict waits until
        // it has had time to register whole.
        if !settled(since, now) {
            return Ok(Begin::Idle);
        }
        match pr.checks {
            Checks::None | Checks::Pending => Ok(Begin::Idle),
            Checks::Passed => self.passed(number, pr.head),
            Checks::Failed(checks) => self.ci_failed(number, pr.head, checks),
        }
    }

    // Green CI: `auto` merges by the path a yes takes, and `ask` raises the
    // merge ruling with the pull request handed back `ready-for-human`. A
    // pull request no reviewer read, one with a listed bot's threads
    // unaddressed, or a head no round read and no fix turn of kelpie's
    // pushed, gets the ruling under `auto` too, naming why; open nits are
    // named and hold nothing. One an older state file left owing the listed
    // bots a pass gets it first, and a bot the pass went on without that
    // has since reviewed the head gets its late round first.
    fn passed(&mut self, number: u64, head: String) -> Result<Begin, StateError> {
        if self.bots_before_merge()? {
            return self.review_step();
        }
        if let Some(begin) = self.late_review()? {
            return Ok(begin);
        }
        let unreviewed = self.current().and_then(|item| item.unreviewed.clone());
        let unread_head = self.current().is_some_and(|item| !item.vouches_for(&head));
        let open = match self.threads_open(number) {
            Ok(open) => open,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        // A merge ruling's `no`, or a bot's late round, sent its fix here
        // unread, so the maintainer decides on it again, even under `auto`,
        // and is told so. A head kelpie did not send unread itself, such as
        // someone else's push, is not the worker's fix.
        let item = self.current().expect("CI runs on a work item");
        let fix_from = |from: &Option<String>| {
            from.as_ref().is_some_and(|from| *from != head)
                && !item.reviewed_heads.contains(&head)
                && item.sent_unread.contains(&head)
        };
        let (note_fix, late_fix) = (fix_from(&item.noted_from), fix_from(&item.late_from));
        let vouched = unreviewed.is_none()
            && open.holding.is_none()
            && !unread_head
            && !note_fix
            && !late_fix;
        if self.settings.merge_authority == MergeAuthority::Auto && vouched {
            self.update(|item| {
                item.phase = Phase::Merge {
                    head,
                    readied: None,
                    auto: true,
                };
            })?;
            return self.merge();
        }
        if let Err(reason) = self.hand_back(number) {
            return Ok(self.gate_failed(reason));
        }
        self.update(|item| {
            item.known.labels.retain(|l| l != READY && l != HUMAN);
            item.known.labels.push(HUMAN.to_owned());
        })?;
        let kind = RulingKind::Merge {
            head,
            unreviewed,
            open_threads: open.holding,
            unread_head,
            note_fix,
            late_fix,
            nits: open.nits,
        };
        self.late_read_now();
        self.raise(number, kind)
    }

    // A worker that pushed nothing after its last red run would get the
    // same run again and again, so the second time the maintainer decides.
    fn ci_failed(
        &mut self,
        number: u64,
        head: String,
        checks: Vec<String>,
    ) -> Result<Begin, StateError> {
        let item = self.current().expect("CI runs on a work item");
        if item.red_head.as_deref() == Some(head.as_str()) {
            return self.raise(number, Stuck::StillRed { head, checks }.into());
        }
        let prompt = self
            .skills
            .invoke(Step::Ci, &red_prompt(number, &head, &checks));
        self.back_to_worker(number, head, checks, prompt)
    }

    // The failure becomes the worker's next turn, and this head is marked red
    // so a second failure at it goes to the maintainer instead.
    pub(super) fn back_to_worker(
        &mut self,
        number: u64,
        head: String,
        checks: Vec<String>,
        prompt: String,
    ) -> Result<Begin, StateError> {
        let issue = self.current().expect("CI runs on a work item").issue;
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

    /// Where `head` stands against `origin`, or why git could not say
    pub(super) fn base_of(&self, head: &str) -> Result<Base, String> {
        let item = self.current().expect("a base is of a work item");
        worktree::base_of(&self.settings.repo, &item.branch, head)
            .map_err(|e| format!("cannot fetch main: {e}"))
    }

    // A head no gate vouched for, such as one a yes under `ask` left before
    // a restart onto `auto`, goes through every gate before any merge.
    fn regate_unknown(&mut self, number: u64, head: &str) -> Result<Begin, StateError> {
        let item = self.current().expect("CI runs on a work item");
        let issue = item.issue;
        let at = worktree::head(&self.settings.repo, &item.worktree);
        let regated = at
            .map_err(|e| format!("cannot read the worktree's head: {e}"))
            .and_then(|at| self.regate(&at, head));
        if let Err(reason) = regated {
            return Ok(self.gate_failed(reason));
        }
        Ok(Begin::Report(StepReport::Regated {
            issue,
            pull_request: number,
            head: head.to_owned(),
        }))
    }

    pub(super) fn gate_failed(&self, reason: String) -> Begin {
        let issue = self.current().map_or(0, |item| item.issue);
        Begin::Report(StepReport::GateFailed { issue, reason })
    }
}

pub(super) fn settled(since: Timestamp, now: Timestamp) -> bool {
    now.0.saturating_sub(since.0) >= CHECKS_SETTLE
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
pub(super) mod tests {
    use serde_json::json;

    use super::*;
    use crate::ports::{Cost, Session, Usage};
    use crate::runner::step;
    use crate::test::{Rig, Scripted, git};

    pub(super) fn ruling_report(report: Option<StepReport>) -> (u64, String) {
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
        let (id, question) = ruling_report(rig.verdict(&runner));
        assert_eq!(id, 1);
        assert_eq!(
            question,
            format!(
                "Merge pull request #71 at {} into main? `shep kelpie rule 1 yes` \
                 merges it, `shep kelpie rule 1 no <note>` sends the worker your note for \
                 a fix that goes to CI and back to you, and `shep kelpie rule 1 rework \
                 <note>` sends it your note for a change the whole review reads again.",
                &head[..7]
            )
        );
        assert_eq!(rig.forge.comments(), [], "a merge ruling is not posted");
        let status = rig.ask(&runner, "status", None);
        assert_eq!(
            status["rulings"],
            json!([{
                "id": 1,
                "issue": 7,
                "question": question,
                "pull_request": 71,
                "kind": { "kind": "merge", "head": head },
                "alerted": false,
            }])
        );
        assert_eq!(
            status["work_item"]["phase"],
            json!({ "state": "ruling", "id": 1 })
        );
        assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
        assert_eq!(step(&runner).unwrap(), None, "a parked worker waits");
    }

    // The worker pushed a head past the red one, and green CI on it raises
    // the first merge ruling, which names it.
    fn assert_fix_landed(rig: &Rig, runner: &std::sync::Mutex<Runner>, red: &str) {
        let fixed = rig.forge.head_of("kelpie/7").unwrap();
        assert_ne!(fixed, red);
        rig.forge.set_checks(&fixed, Checks::Passed);
        let (id, question) = ruling_report(rig.verdict(runner));
        assert_eq!(id, 1);
        assert!(question.contains(&fixed[..7]), "{question}");
    }

    #[test]
    fn a_red_run_is_the_workers_next_turn_naming_the_failed_checks() {
        let (rig, runner, head) = Rig::with_pull_request("koji");
        let failed = vec!["lint".to_owned(), "test".to_owned()];
        rig.forge.set_checks(&head, Checks::Failed(failed.clone()));
        assert_eq!(
            rig.verdict(&runner),
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
            "\nCI failed on your pull request #71 at {}: lint, test. ",
            &head[..7]
        );
        assert!(fix.prompt.starts_with("/mattpocock:diagnosing-bugs "));
        assert!(fix.prompt.contains(&named), "{}", fix.prompt);

        assert_fix_landed(&rig, &runner, &head);
    }

    #[test]
    fn a_fix_left_uncommitted_is_sent_back_naming_its_files_before_the_worker_is_parked() {
        let (rig, runner, head) = Rig::with_pull_request("koji");
        rig.forge
            .set_checks(&head, Checks::Failed(vec!["test".into()]));
        rig.verdict(&runner);
        rig.claude.script([
            Scripted::Plant("fix.txt", "fixed\n"),
            Scripted::Push("fix.txt", "fixed\n"),
        ]);
        step(&runner).unwrap();
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Ended { issue: 7, .. })
        ));
        let [first, _, again] = rig.claude.calls().try_into().unwrap();
        assert_eq!(again.session, Session::Resume(first.session.id().clone()));
        assert!(
            again
                .prompt
                .starts_with("Your last turn ended with uncommitted changes in your worktree"),
            "{}",
            again.prompt
        );
        assert!(again.prompt.contains("fix.txt"), "{}", again.prompt);
        assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
        assert_fix_landed(&rig, &runner, &head);
    }

    #[test]
    fn a_second_red_run_on_a_head_the_worker_left_alone_parks_it() {
        let (rig, runner, head) = Rig::with_pull_request("rotom");
        rig.forge
            .set_checks(&head, Checks::Failed(vec!["lint".into()]));
        rig.verdict(&runner);
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();

        let (_, question) = ruling_report(rig.verdict(&runner));
        let parked = format!(
            "CI failed again on pull request #71 at {}, and the worker pushed no fix: lint.",
            &head[..7]
        );
        assert!(question.starts_with(&parked), "{question}");
        assert_eq!(rig.claude.calls().len(), 2, "the red run went out once");
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Alerted { .. })
        ));
        assert_eq!(step(&runner).unwrap(), None);

        // The maintainer's fix and yes vouch for the head: no foreign-change ruling.
        let fixed = rig.push_by_hand("kelpie/7", "lint.txt");
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.forge.set_checks(&fixed, Checks::Passed);
        let (_, question) = ruling_report(rig.verdict(&runner));
        assert!(question.starts_with("Merge pull request #71"), "{question}");
    }

    #[test]
    fn a_branch_behind_main_is_rebased_and_asked_about_only_after_ci_on_the_new_head() {
        let (rig, runner, head) = Rig::with_pull_request("shep");
        let landed = rig.land_on_origin("landed.txt");
        rig.forge.set_checks(&head, Checks::Passed);
        let Some(StepReport::Rebased { head: rebased, .. }) = step(&runner).unwrap() else {
            panic!("the branch was not rebased");
        };
        assert_eq!(rig.forge.head_of("kelpie/7").as_ref(), Some(&rebased));
        let worktree = rig.worktree_7();
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), rebased);
        assert_eq!(git(&worktree, &["rev-parse", "HEAD~1"]), landed);
        assert_eq!(git(&worktree, &["show", "HEAD:work.txt"]), "work");

        assert_eq!(
            step(&runner).unwrap(),
            None,
            "CI on the rebased head is pending"
        );
        assert_eq!(rig.forge.comments(), []);
        rig.forge.set_checks(&rebased, Checks::Passed);
        ruling_report(rig.verdict(&runner));
        assert_eq!(
            rig.ask(&runner, "status", None)["rulings"][0]["kind"],
            json!({ "kind": "merge", "head": rebased })
        );
    }

    // Seen live on the playground: the step after a rebase read the head
    // from before the push, and parked the worker on a false ruling.
    #[test]
    fn a_forge_still_showing_the_head_from_before_a_push_is_waited_out() {
        let (rig, runner, head) = Rig::with_pull_request("webapp");
        rig.land_on_origin("landed.txt");
        let Some(StepReport::Rebased { head: rebased, .. }) = step(&runner).unwrap() else {
            panic!("the branch was not rebased");
        };
        rig.forge.set_lagging(71, Some(&head));
        rig.forge.set_checks(&head, Checks::Passed);
        rig.clock.advance(CHECKS_SETTLE);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
        assert_eq!(rig.forge.head_of("kelpie/7"), Some(rebased.clone()));

        rig.forge.set_lagging(71, None);
        rig.forge.set_checks(&rebased, Checks::Passed);
        ruling_report(rig.verdict(&runner));
        assert_eq!(
            rig.ask(&runner, "status", None)["rulings"][0]["kind"],
            json!({ "kind": "merge", "head": rebased })
        );
    }

    #[test]
    fn with_ci_on_a_head_without_checks_waits_however_long_it_takes() {
        let (rig, runner, head) = Rig::with_pull_request("chelone");
        rig.forge.set_checks(&head, Checks::None);
        for _ in 0..5 {
            rig.clock.advance(3600);
            assert_eq!(step(&runner).unwrap(), None);
        }
        assert_eq!(rig.forge.comments(), []);
    }

    #[test]
    fn a_verdict_waits_for_the_check_set_to_settle() {
        let (rig, runner, head) = Rig::with_pull_request("acme");
        rig.forge.set_checks(&head, Checks::Passed);
        assert_eq!(step(&runner).unwrap(), None);
        rig.clock.advance(CHECKS_SETTLE - 1);
        assert_eq!(step(&runner).unwrap(), None);
        rig.clock.advance(1);
        ruling_report(step(&runner).unwrap());
    }

    #[test]
    fn with_ci_off_the_checks_are_never_read() {
        let rig = Rig::new("webapp");
        rig.edit_settings(|s| s.replace("ci = true", "ci = false"));
        let runner = rig.open().unwrap();
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([
            Scripted::Push("work.txt", "work\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the worker's first turn: opens the pull request
        step(&runner).unwrap(); // review round 1, qwen: clean by default
        step(&runner).unwrap(); // review round 2, claude: scripted clean above
        let head = rig.forge.head_of("kelpie/7").unwrap();
        rig.forge
            .set_checks(&head, Checks::Failed(vec!["lint".into()]));

        let (id, question) = ruling_report(step(&runner).unwrap());
        assert!(question.starts_with("Merge pull request #71"), "{question}");
        rig.ask(&runner, "rule", Some(&format!("{id} yes")));
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::MarkedReady { .. })
        ));
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Finished {
                issue: 7,
                pull_request: Some(71),
                merged: true,
                ..
            })
        ));
        assert_eq!(rig.forge.merges(), [(71, head)]);
    }

    #[test]
    fn a_ruling_whose_comment_fails_still_stands_in_status_and_the_log() {
        let (rig, runner, _) = Rig::with_pull_request("golbat");
        rig.forge.set_state(71, PullRequestState::Closed);
        rig.forge.set_comments_down(true);
        let Some(StepReport::Ruling { comment_failed, .. }) = rig.verdict(&runner) else {
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
        let (rig, runner, _) = Rig::with_pull_request("acme");
        rig.forge.set_state(71, PullRequestState::Merged);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Finished {
                issue: 7,
                pull_request: Some(71),
                merged: true,
                ..
            })
        ));
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
        assert_eq!(
            rig.forge.comments(),
            [(
                71,
                "This pull request was closed without merging.\n\nWaiting on the maintainer."
                    .to_owned()
            )]
        );
        rig.ask(&runner, "rule", Some(&format!("{id} yes")));
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Finished {
                issue: 7,
                pull_request: Some(71),
                merged: false,
                ..
            })
        ));
        assert_eq!(rig.forge.head_of("kelpie/7"), Some(head));
        assert!(!rig.worktree_7().exists());
        assert_eq!(git(&rig.repo(), &["branch", "--list", "kelpie/7"]), "");
    }

    #[test]
    fn a_label_added_outside_kelpie_parks_the_worker_naming_it() {
        let (rig, runner, head) = Rig::with_pull_request("shep");
        rig.forge.label_pull_request(71, "bug");
        let (id, question) = ruling_report(step(&runner).unwrap());
        assert_eq!(id, 1);
        assert_eq!(
            question,
            "Pull request #71 changed outside kelpie: the `bug` label was added. \
             `shep kelpie rule 1 yes` accepts it and kelpie carries on, and \
             `shep kelpie rule 1 no <note>` sends the worker your note."
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["rulings"][0]["kind"],
            json!({
                "kind": "foreign-change",
                "description": "the `bug` label was added",
                "known": { "labels": ["bug"], "ready": false, "head": head },
            })
        );

        // A yes accepts it, and kelpie carries on watching the same pull request.
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.forge.set_checks(&head, Checks::Passed);
        let (next, merge_question) = ruling_report(rig.verdict(&runner));
        assert_eq!(next, 2);
        assert!(merge_question.starts_with("Merge pull request #71"));
        // Kelpie's own record now matches, so the accepted label is not asked again.
        assert_eq!(rig.forge.merges(), []);
    }

    #[test]
    fn marking_a_pull_request_ready_outside_kelpie_parks_the_worker() {
        let (rig, runner, _) = Rig::with_pull_request("koji");
        rig.forge.ready_pull_request(71);
        let (_, question) = ruling_report(step(&runner).unwrap());
        assert!(
            question.starts_with(
                "Pull request #71 changed outside kelpie: it was marked ready for review."
            ),
            "{question}"
        );
    }

    #[test]
    fn a_no_on_a_foreign_change_sends_the_worker_a_note_and_asks_again_once_it_reaches_ci() {
        let (rig, runner, head) = Rig::with_pull_request("rotom");
        rig.forge.label_pull_request(71, "bug");
        ruling_report(step(&runner).unwrap());
        rig.ask(&runner, "rule", Some("1 no  remove that label yourself "));
        rig.claude.script([
            Scripted::Push("rename.txt", "renamed\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the noted turn: pushes, enters round 1
        let [first, noted] = rig.claude.calls().try_into().unwrap();
        assert_eq!(noted.session, Session::Resume(first.session.id().clone()));
        assert_eq!(
            noted.prompt,
            "The maintainer answered no on pull request #71, with this note:\n\n\
             remove that label yourself\n"
        );
        assert_ne!(rig.forge.head_of("kelpie/7"), Some(head));

        step(&runner).unwrap(); // review round 1, qwen: clean by default
        step(&runner).unwrap(); // review round 2, claude: scripted clean above
        // The label is still there, and unaccepted, so the next look parks again.
        let (id, _) = ruling_report(step(&runner).unwrap());
        assert_eq!(id, 2);
    }

    #[test]
    fn a_label_removed_after_a_yes_accepted_it_parks_the_worker_again() {
        let (rig, runner, _) = Rig::with_pull_request("chelone");
        rig.forge.label_pull_request(71, "bug");
        ruling_report(step(&runner).unwrap());
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.forge.unlabel_pull_request(71, "bug");
        let (id, question) = ruling_report(step(&runner).unwrap());
        assert_eq!(id, 2);
        assert!(
            question.starts_with(
                "Pull request #71 changed outside kelpie: the `bug` label was removed."
            ),
            "{question}"
        );
    }

    #[test]
    fn a_normal_gate_run_with_nothing_changed_outside_kelpie_asks_nothing_about_it() {
        let (rig, runner, head) = Rig::with_pull_request("acme");
        rig.forge.set_checks(&head, Checks::Passed);
        let (_, question) = ruling_report(rig.verdict(&runner));
        assert!(question.starts_with("Merge pull request #71"), "{question}");
    }

    #[test]
    fn a_work_item_saved_before_heads_were_known_parks_nothing_and_learns_its_head() {
        let (rig, runner, head) = Rig::with_pull_request("chelone");
        drop(runner);
        let state = rig.paths().state;
        let text = std::fs::read_to_string(&state).unwrap();
        let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        let known = saved["work_items"][0]["known"].as_object_mut().unwrap();
        assert!(known.remove("head").is_some());
        std::fs::write(&state, saved.to_string()).unwrap();

        let runner = rig.open().unwrap();
        rig.forge.set_checks(&head, Checks::Passed);
        let (_, question) = ruling_report(rig.verdict(&runner));
        assert!(question.starts_with("Merge pull request #71"), "{question}");
        let text = std::fs::read_to_string(&state).unwrap();
        let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(saved["work_items"][0]["known"]["head"], json!(head));
    }

    #[test]
    fn a_work_item_saved_before_the_gate_existed_is_left_alone() {
        let (rig, runner, head) = Rig::with_pull_request("xilriws");
        drop(runner);
        let state = rig.paths().state;
        let text = std::fs::read_to_string(&state).unwrap();
        let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        let item = saved["work_items"][0].as_object_mut().unwrap();
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
