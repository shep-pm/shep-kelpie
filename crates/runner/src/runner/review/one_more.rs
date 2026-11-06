//! A pass runs the project manager's plan, and a round's finding above a nit
//! asks it for one more read

use super::lineup::changed_lines;
use crate::ports::Finding;
use crate::runner::Runner;
use crate::runner::pm::Wake;
use crate::work_item::Review;

impl Runner {
    /// What to ask the project manager once `review`'s round, which sent
    /// `sent` to the worker, is fixed, or None when its pass asks nothing
    ///
    /// A pass asks when a finding above a nit was sent, every read the plan
    /// calls for is in, and a candidate is left to read once more. A bot's
    /// pass asks nothing, since a bot reads on its own rounds.
    pub(super) fn findings_wake(&self, review: &Review, sent: &[Finding]) -> Option<Wake> {
        let item = self.current().filter(|_| self.agents.pm.is_some())?;
        let above: Vec<Finding> = sent.iter().filter(|f| !f.is_nit()).cloned().collect();
        if review.bots_only || above.is_empty() {
            return None;
        }
        let next = Review {
            ran: self.ran_of(review),
            ..review.clone()
        }
        .next_round();
        let reads = self.reads_in(item, &next);
        if reads < self.need_of(item) {
            return None;
        }
        let base = item.review_base(self.item_base(item));
        let left = self.due(&next, &item.worktree, &base).ok()?;
        let lines = changed_lines(&self.settings.git.checkout, &item.worktree, &base).ok();
        let pull_request = item.pull_request?;
        (!left.is_empty()).then(|| Wake::Findings {
            ticket: item.issue,
            pull_request,
            reads,
            findings: above,
            weight: item.weight.clone(),
            lines: lines.flatten(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::super::several::{at_review, reviewer_models, reviewers_until_ci, two_local};
    use crate::agents::Tier;
    use crate::ports::{AgentError, Finding, Role, Severity};
    use crate::runner::{Runner, StepReport, step};
    use crate::settings::Harness;
    use crate::test::{CALL_PATIENCE, Rig, Scripted, ScriptedRound};
    use crate::work_item::{Heavier, Lighter, ReviewPlan, ReviewWeight};

    // `two_local`'s candidates `mine`, `other` and `claude` on a project that names `agents.pm`,
    // with `reviews.need` left out.
    fn pm_project() -> Rig {
        let rig = two_local();
        rig.reviewers(&["mine", "other", "claude"]);
        rig.allow(&["sonnet-high", "mine", "other", "claude", "pm"]);
        rig.reviews_need(None);
        rig.edit_settings(|s| {
            let listed = "[app.dogs.kelpie.agents]\n";
            assert!(s.contains(listed), "the example's agents table moved");
            s.replace(listed, "[app.dogs.kelpie.agents]\npm = \"pm\"\n")
        });
        rig
    }

    fn phase(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
        rig.ask(runner, "status", None)["work_item"]["phase"]["state"].clone()
    }

    #[test]
    fn an_item_with_no_plan_reads_once_and_goes_to_ci() {
        let rig = pm_project();
        let runner = at_review(&rig, "work.txt");

        reviewers_until_ci(&rig, &runner);

        assert_eq!(rig.reviewer.seen().len(), 1, "one reviewer read");
        assert!(reviewer_models(&rig).is_empty(), "claude was not needed");
        assert_eq!(phase(&rig, &runner), "ci");
    }

    // The pass reads under a plan of `reads` and what they are for.
    fn plan(runner: &Mutex<Runner>, reads: u32, about: Option<&str>) {
        let mut runner = runner.lock().unwrap();
        runner.state.work_items[0].review_plan = Some(ReviewPlan {
            reads: reads.try_into().unwrap(),
            about: about.map(str::to_owned),
        });
    }

    #[test]
    fn a_plan_of_two_reads_twice_and_goes_to_ci() {
        let rig = pm_project();
        let runner = at_review(&rig, "work.txt");
        plan(&runner, 2, None);

        reviewers_until_ci(&rig, &runner);

        assert_eq!(rig.reviewer.seen().len(), 2, "mine and other read");
        assert!(reviewer_models(&rig).is_empty(), "claude was not needed");
        assert_eq!(phase(&rig, &runner), "ci");
    }

    #[test]
    fn an_adopted_item_with_no_plan_keeps_the_full_count() {
        let rig = pm_project();
        let runner = at_review(&rig, "work.txt");
        runner.lock().unwrap().state.work_items[0].adopted = true;

        reviewers_until_ci(&rig, &runner);

        assert_eq!(rig.reviewer.seen().len(), 2, "reviews.need is 2");
        assert_eq!(phase(&rig, &runner), "ci");
    }

    #[test]
    fn a_project_with_no_project_manager_keeps_its_count() {
        let rig = pm_project();
        rig.edit_settings(|s| s.replace("pm = \"pm\"\n", ""));
        let runner = at_review(&rig, "work.txt");

        reviewers_until_ci(&rig, &runner);

        assert_eq!(rig.reviewer.seen().len(), 2, "reviews.need is 2");
        assert_eq!(phase(&rig, &runner), "ci");
    }

    #[test]
    fn a_project_managers_project_weighs_with_no_light_key() {
        let rig = pm_project();
        let runner = at_review(&rig, "docs/guide.md");
        step(&runner).unwrap();
        let docs = runner.lock().unwrap().current().unwrap().weight.clone();
        assert_eq!(docs, Some(ReviewWeight::Light(Lighter::Docs)));

        let rig = pm_project();
        let runner = at_review(&rig, "src/lib.rs");
        step(&runner).unwrap();
        let code = runner.lock().unwrap().current().unwrap().weight.clone();
        let touched = Heavier::Touched("src/lib.rs".into());
        assert_eq!(code, Some(ReviewWeight::Full(touched)));
    }

    #[test]
    fn a_light_weight_leaves_the_asks_tier_on_a_project_manager_project() {
        let rig = pm_project();
        let runner = at_review(&rig, "docs/guide.md");
        runner.lock().unwrap().state.work_items[0].tier = Some(Tier::Heavy);

        step(&runner).unwrap();

        let ask = rig.agent_board.asks().pop().unwrap();
        assert_eq!(ask.tier, Some(Tier::Heavy));
    }

    fn found(severity: Severity) -> Vec<Finding> {
        vec![Finding::new(severity, "work.txt", 1, "wrong", "it is")]
    }

    // The pass's first reader reads with `found`, and the worker fixes what it
    // sent, so the round is over.
    fn read_and_fixed(rig: &Rig, runner: &Mutex<Runner>, found: Vec<Finding>) {
        let fixed = !found.is_empty();
        rig.reviewer.script([ScriptedRound::Findings(found)]);
        rig.claude.script([Scripted::Push("fixed.txt", "fixed\n")]);
        step(runner).unwrap(); // the reviewer reads
        let deadline = std::time::Instant::now() + CALL_PATIENCE;
        while fixed && std::time::Instant::now() < deadline {
            // The worker's fix turn runs on its own thread.
            match step(runner).unwrap() {
                Some(StepReport::FixPushed { .. } | StepReport::NitsDeclined { .. }) => break,
                _ => std::thread::yield_now(),
            }
        }
    }

    // What the project manager was asked about a round's findings, per call.
    fn asked(rig: &Rig) -> Vec<String> {
        let calls = rig.claude.all_calls();
        let pm = calls.iter().filter(|c| c.role == Role::Pm);
        let about = |c: &&crate::ports::AgentCall| c.prompt.contains("above a nit");
        pm.filter(about).map(|c| c.prompt.clone()).collect()
    }

    // Steps until the project manager's answer is reported.
    fn until_answered(runner: &Mutex<Runner>) -> StepReport {
        let deadline = std::time::Instant::now() + CALL_PATIENCE;
        while std::time::Instant::now() < deadline {
            match step(runner).unwrap() {
                Some(report @ StepReport::PmAnswered { .. }) => return report,
                _ => std::thread::yield_now(),
            }
        }
        panic!("the project manager never answered");
    }

    // Steps a few times, as a runner left alone does.
    fn left_alone(runner: &Mutex<Runner>) {
        for _ in 0..5 {
            step(runner).unwrap();
        }
    }

    #[test]
    fn a_clean_round_ends_the_pass_with_no_wake() {
        let rig = pm_project();
        let runner = at_review(&rig, "work.txt");
        read_and_fixed(&rig, &runner, Vec::new());

        assert_eq!(phase(&rig, &runner), "ci");
        left_alone(&runner);
        assert!(asked(&rig).is_empty());
    }

    #[test]
    fn a_round_of_nits_ends_the_pass_with_no_wake() {
        let rig = pm_project();
        let runner = at_review(&rig, "work.txt");
        read_and_fixed(&rig, &runner, found(Severity::Low));

        assert_eq!(phase(&rig, &runner), "ci");
        left_alone(&runner);
        assert!(asked(&rig).is_empty());
    }

    #[test]
    fn a_medium_finding_on_the_last_read_wakes_the_project_manager() {
        let rig = pm_project();
        let runner = at_review(&rig, "work.txt");
        rig.claude
            .script_for(Role::Pm, [Scripted::Say("{\"pick\": null}")]);
        read_and_fixed(&rig, &runner, found(Severity::Medium));

        until_answered(&runner);

        let [prompt] = asked(&rig).try_into().unwrap();
        assert!(prompt.contains("MEDIUM work.txt:1 wrong"), "{prompt}");
        assert!(prompt.contains("has had 1 read"), "{prompt}");
    }

    #[test]
    fn a_high_finding_on_the_last_read_wakes_the_project_manager() {
        let rig = pm_project();
        let runner = at_review(&rig, "work.txt");
        rig.claude
            .script_for(Role::Pm, [Scripted::Say("{\"pick\": null}")]);
        read_and_fixed(&rig, &runner, found(Severity::High));

        until_answered(&runner);

        let [prompt] = asked(&rig).try_into().unwrap();
        assert!(prompt.contains("HIGH work.txt:1 wrong"), "{prompt}");
    }

    // The pass is over: CI went on, and the merge ruling after it is asked,
    // as nothing is left to hold the item.
    fn past_review(rig: &Rig, runner: &Mutex<Runner>) {
        let state = phase(rig, runner);
        assert!(state == "ci" || state == "ruling", "{state}");
    }

    fn answer(read: &'static str) -> Scripted {
        Scripted::Say(read)
    }

    const YES: &str =
        "{\"read\": [{\"item\": 7, \"more\": true, \"why\": \"the lock is still racy\"}]}";
    const NO: &str = "{\"read\": [{\"item\": 7, \"more\": false, \"why\": \"small and fixed\"}]}";

    // Item 7's pass has one reader, who found a Medium the worker fixed, and
    // the project manager answers `said`, which is read before this returns.
    fn asked_and_told(said: &'static str) -> (Rig, Mutex<Runner>, StepReport) {
        let rig = pm_project();
        let runner = at_review(&rig, "work.txt");
        rig.claude.script_for(Role::Pm, [answer(said)]);
        read_and_fixed(&rig, &runner, found(Severity::Medium));
        let report = until_answered(&runner);
        (rig, runner, report)
    }

    #[test]
    fn a_yes_runs_one_more_reviewer_and_saves_the_plan() {
        let (rig, runner, _) = asked_and_told(YES);
        plan_is(&runner, Some((2, None)));

        reviewers_until_ci(&rig, &runner);

        assert_eq!(rig.reviewer.seen().len(), 2, "mine and other read");
        assert_eq!(phase(&rig, &runner), "ci");
    }

    // The plan the work item holds, as reads and what they are about.
    fn plan_is(runner: &Mutex<Runner>, want: Option<(u32, Option<&str>)>) {
        let runner = runner.lock().unwrap();
        let plan = runner.state.work_items[0].review_plan.as_ref();
        let held = plan.map(|p| (p.reads.get(), p.about.as_deref()));
        assert_eq!(held, want);
    }

    #[test]
    fn a_yes_keeps_what_the_plan_says_the_reads_are_about() {
        let rig = pm_project();
        let runner = at_review(&rig, "work.txt");
        plan(&runner, 1, Some("the lock"));
        rig.claude.script_for(Role::Pm, [answer(YES)]);
        read_and_fixed(&rig, &runner, found(Severity::High));

        until_answered(&runner);

        plan_is(&runner, Some((2, Some("the lock"))));
    }

    #[test]
    fn a_no_ends_the_pass_at_ci() {
        let (rig, runner, _) = asked_and_told(NO);
        plan_is(&runner, None);

        reviewers_until_ci(&rig, &runner);

        assert_eq!(rig.reviewer.seen().len(), 1, "no one read again");
        past_review(&rig, &runner);
    }

    #[test]
    fn an_answer_that_names_no_read_ends_the_pass_at_ci() {
        let (rig, runner, _) = asked_and_told("{\"pick\": null}");

        reviewers_until_ci(&rig, &runner);

        assert_eq!(rig.reviewer.seen().len(), 1, "no one read again");
        past_review(&rig, &runner);
    }

    #[test]
    fn a_failed_wake_ends_the_pass_at_ci() {
        let rig = pm_project();
        let runner = at_review(&rig, "work.txt");
        let down = AgentError::Failed(Harness::ClaudeCode, "the model is down".into());
        rig.claude.script_for(Role::Pm, [Scripted::Fail(down)]);
        read_and_fixed(&rig, &runner, found(Severity::Medium));

        reviewers_until_ci(&rig, &runner);

        assert_eq!(rig.reviewer.seen().len(), 1, "no one read again");
        past_review(&rig, &runner);
    }

    #[test]
    fn the_calls_why_reaches_the_journal() {
        let (_, _, report) = asked_and_told(YES);

        let StepReport::PmAnswered { acted, .. } = report else {
            unreachable!("until_answered returns an answer")
        };
        let [line] = acted.try_into().unwrap();
        assert_eq!(
            line,
            "let one more reviewer read #7, which makes 2 reads: the lock is still racy"
        );
    }
}
