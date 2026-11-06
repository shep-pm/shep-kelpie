//! A pass runs the project manager's plan, and a round's finding above a nit
//! asks it for one more read

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::super::several::{at_review, reviewer_models, reviewers_until_ci, two_local};
    use crate::agents::Tier;
    use crate::runner::{Runner, step};
    use crate::test::Rig;
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
}
