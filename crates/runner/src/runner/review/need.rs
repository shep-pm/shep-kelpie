//! A pass runs `reviews.need` reviewers, each round the agent board's pick

use std::sync::Mutex;

use super::several::{at_review, reviewer_models, reviewers_until_ci, two_local};
use crate::agents::Tier;
use crate::lease::LeaseKind;
use crate::ports::{CLEAN, Checks, Role, Timestamp};
use crate::runner::coderabbit::tests::now;
use crate::runner::review_bot::SETTLE_LEAST;
use crate::runner::review_bot::late_reads::passed_over;
use crate::runner::review_bot::two_bots::{bot_reviewed, listing as with_bots};
use crate::runner::{Runner, StepReport, step};
use crate::settings::{AgentName, LocalRound};
use crate::test::{Rig, Scripted, Told};

// `two_local`'s project, whose candidates are `mine`, `other` and `claude`, and
// whose `reviews.need` is left out.
fn three_candidates() -> Rig {
    let rig = two_local();
    rig.edit_settings(|s| {
        s.replace(
            r#"reviewers = ["mine", "other"]"#,
            r#"reviewers = ["mine", "other", "claude"]"#,
        )
    });
    rig.reviews_need(None);
    rig
}

fn phase(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
    rig.ask(runner, "status", None)["work_item"]["phase"]["state"].clone()
}

// The command each local round ran, by file name, oldest first.
fn commands(rig: &Rig) -> Vec<String> {
    let name = |round: &crate::test::SeenRound| match &round.local {
        LocalRound::Command(c) => c
            .command
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        _ => unreachable!("every local reviewer here is a command"),
    };
    rig.reviewer.seen().iter().map(name).collect()
}

fn names(list: &[&str]) -> Vec<AgentName> {
    let name = |n: &&str| AgentName::try_from((*n).to_owned()).unwrap();
    list.iter().map(name).collect()
}

#[test]
fn two_reviewers_read_by_default_and_the_pass_goes_to_ci() {
    let rig = three_candidates();
    let runner = at_review(&rig, "work.txt");

    reviewers_until_ci(&rig, &runner);

    assert_eq!(rig.reviewer.seen().len(), 2, "mine and other read");
    assert!(reviewer_models(&rig).is_empty(), "claude was not needed");
    assert_eq!(phase(&rig, &runner), "ci");
}

#[test]
fn each_round_goes_to_the_boards_first_pick_the_pass_has_not_run() {
    let rig = three_candidates();
    let runner = at_review(&rig, "work.txt");
    rig.agent_board.ranks(&["other", "claude", "mine"]);
    rig.claude.script([Scripted::Text(CLEAN)]);

    reviewers_until_ci(&rig, &runner);

    assert_eq!(commands(&rig), ["other"]);
    assert_eq!(reviewer_models(&rig), ["claude-sonnet-5"]);
    let asks = rig.agent_board.asks();
    let mine = &names(&["mine"])[0];
    let reviews: Vec<_> = asks.iter().filter(|ask| ask.agents.contains(mine)).collect();
    assert_eq!(reviews[0].leave_out, []);
    assert_eq!(reviews[1].leave_out, names(&["other"]));
}

#[test]
fn the_ask_carries_the_items_kind_tier_and_slack() {
    let rig = three_candidates();
    let runner = at_review(&rig, "work.txt");
    {
        let mut runner = runner.lock().unwrap();
        let item = &mut runner.state.work_items[0];
        item.kind = Some("fix".to_owned());
        item.tier = Some(Tier::Heavy);
        item.slack = Some(15);
    }

    step(&runner).unwrap();

    let ask = rig.agent_board.asks().pop().unwrap();
    assert_eq!(ask.kind.as_deref(), Some("fix"));
    assert_eq!(ask.tier, Some(Tier::Heavy));
    assert_eq!(ask.slack, Some(15));
    assert_eq!(ask.pin, None);
    assert_eq!(ask.agents, names(&["mine", "other", "claude"]));
    assert_eq!(ask.leave_out, []);
}

#[test]
fn a_reviewer_whose_plan_is_held_is_skipped_for_the_next_pick() {
    let rig = three_candidates();
    rig.reviews_need(Some(1));
    let runner = at_review(&rig, "work.txt");
    rig.agent_board.ranks(&["claude", "mine", "other"]);
    rig.meter.set(Rig::utilization(0, 60)); // Claude's 5-hour window is held

    reviewers_until_ci(&rig, &runner);

    assert_eq!(commands(&rig), ["review"], "mine read in claude's place");
    assert!(reviewer_models(&rig).is_empty(), "claude was not waited on");
    assert_eq!(phase(&rig, &runner), "ci");
}

// Pull request 71 for issue 7 with the worker's first turn done, on a project
// whose candidates are qwen, Claude and cubic, whose window is closed.
fn cubic_closed(need: usize) -> (Rig, Mutex<Runner>) {
    let rig = with_bots("shep", &["cubic"]);
    rig.reviews_need(Some(need));
    rig.leases.opens_at(
        &LeaseKind::try_from("cubic").unwrap(),
        Timestamp(Rig::EPOCH + 600),
    );
    let runner = at_review(&rig, "work.txt");
    rig.agent_board.ranks(&["cubic", "qwen", "claude"]);
    rig.claude.script([Scripted::Text(CLEAN)]);
    (rig, runner)
}

#[test]
fn a_review_bot_whose_window_is_closed_is_skipped_not_waited_on() {
    let (rig, runner) = cubic_closed(1);
    let before = now(&rig);

    let report = step(&runner).unwrap();

    let Some(StepReport::ReviewerSkipped {
        reviewer, round, ..
    }) = report
    else {
        panic!("cubic was not passed over: {report:?}");
    };
    assert_eq!((reviewer, round), (AgentName::kelpies("cubic"), 1));
    reviewers_until_ci(&rig, &runner);
    assert_eq!(rig.reviewer.seen().len(), 1, "qwen read in its place");
    assert_eq!(now(&rig), before, "no wait for the window");
    let cubic = Told::Want(LeaseKind::try_from("cubic").unwrap());
    assert!(!rig.leases.told().contains(&cubic), "cubic was never asked");
    let skipped = &rig.ask(&runner, "status", None)["work_item"]["bots_skipped"];
    assert_eq!(skipped[0]["why"], "window");
}

#[test]
fn a_bot_passed_over_does_not_count_toward_need() {
    let (rig, runner) = cubic_closed(2);

    reviewers_until_ci(&rig, &runner);

    assert_eq!(rig.reviewer.seen().len(), 1, "qwen read");
    assert_eq!(reviewer_models(&rig), ["claude-sonnet-5"], "and Claude");
}

#[test]
fn with_no_answer_from_the_board_the_round_asks_again_next_poll() {
    let rig = three_candidates();
    let runner = at_review(&rig, "work.txt");
    rig.agent_board.unreachable();

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(phase(&rig, &runner), "review");
    assert!(rig.reviewer.seen().is_empty(), "no fallback reviewer read");
    assert!(reviewer_models(&rig).is_empty());

    rig.agent_board.ranks_listed();
    reviewers_until_ci(&rig, &runner);
    assert_eq!(rig.reviewer.seen().len(), 2);
}

#[test]
fn a_late_bot_review_is_still_read_after_the_pass() {
    let rig = with_bots("shep", &["cubic"]);
    let (runner, head) = passed_over(&rig);
    assert_eq!(phase(&rig, &runner), "ci");
    let finding = "P2: `bleats` loses a stamped prefix. Strip only files shep stamped.";
    rig.forge
        .coderabbit
        .cubic_review(71, &head, now(&rig) + 60, &[finding]);
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(rig.verdict(&runner), None, "green CI reads cubic first");

    rig.clock.advance(SETTLE_LEAST);

    assert_eq!(step(&runner).unwrap(), bot_reviewed(4, "cubic", 1));
    let roles: Vec<Role> = rig.claude.all_calls().iter().map(|c| c.role).collect();
    assert_eq!(roles, [Role::Worker, Role::Reviewer], "no new pass");
}
