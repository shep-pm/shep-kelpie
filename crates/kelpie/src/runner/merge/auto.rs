//! Merge authority `auto` through the runner's stand-ins
//!
//! `auto` replaces only the merge ruling. Each other ruling is raised here
//! under `auto`, and each still parks the worker with nothing merged.

use std::sync::Mutex;

use serde_json::json;

use crate::ports::{Checks, ClaudeError, PullRequestState};
use crate::runner::coderabbit::tests::{fixed, hold_a_finding, now, reviewed_by_qwen};
use crate::runner::coderabbit::{ANSWER_WAIT, REVIEW_WAIT};
use crate::runner::rework::HUMAN;
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::test::{Rig, Scripted};

// The same project restarted under `auto`, as the maintainer would switch
// it: a runner reads its settings when it opens.
fn under_auto(rig: &Rig, runner: Mutex<Runner>) -> Mutex<Runner> {
    drop(runner);
    rig.merge_auto();
    rig.open().unwrap()
}

// A running project under `auto` with issue 7 added and no turn run yet
fn auto_with_issue_7(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    rig.merge_auto();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    (rig, runner)
}

// With CodeRabbit on, green CI, the draft marked ready and the summon
fn summoned_under_auto(project: &str) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = reviewed_by_qwen(project);
    let runner = under_auto(&rig, runner);
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned { .. })
    ));
    (rig, runner, head)
}

fn merged(report: Option<StepReport>) -> bool {
    matches!(
        report,
        Some(StepReport::Finished {
            issue: 7,
            pull_request: Some(71),
            merged: true,
            ..
        })
    )
}

// The id of the ruling a report raised, whichever report carries it
fn raised(report: Option<StepReport>) -> u64 {
    match report {
        Some(
            StepReport::Ruling { id, .. }
            | StepReport::Asked { id, .. }
            | StepReport::Failed { id, .. }
            | StepReport::TimedOut { id, .. },
        ) => id,
        other => panic!("no ruling was raised: {other:?}"),
    }
}

// Ruling `id`, of `kind`, parks the worker as it would under `ask`: it is
// posted, and the worker waits with nothing merged.
fn still_asks(rig: &Rig, runner: &Mutex<Runner>, id: u64, kind: &str) {
    let status = rig.ask(runner, "status", None);
    assert_eq!(status["merge_authority"], "auto");
    assert_eq!(status["rulings"][0]["id"], id);
    assert_eq!(status["rulings"][0]["kind"]["kind"], kind);
    assert_eq!(
        status["work_item"]["phase"],
        json!({ "state": "ruling", "id": id })
    );
    assert_eq!(step(runner).unwrap(), Some(StepReport::Alerted { id }));
    rig.clock.advance(CHECKS_SETTLE);
    assert_eq!(step(runner).unwrap(), None, "a parked worker waits");
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn green_gates_merge_with_no_ruling_and_one_notice_after() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    let runner = under_auto(&rig, runner);
    rig.relay.set_up(true);
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady {
            issue: 7,
            pull_request: 71,
        })
    );
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    rig.clock.advance(CHECKS_SETTLE);
    assert!(merged(step(&runner).unwrap()));
    assert_eq!(rig.forge.merges(), [(71, head.clone())]);
    assert_eq!(rig.forge.comments(), [], "no ruling was posted");
    let labelled = rig.forge.coderabbit.label_log();
    assert!(labelled.iter().all(|(_, l, _)| l != HUMAN), "{labelled:?}");

    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Noticed {
            issue: 7,
            pull_request: 71,
        })
    );
    let [(webhook, alert)] = rig.alerts.posts().try_into().unwrap();
    assert_eq!(webhook, rig.webhook());
    assert_eq!(alert.title, "kelpie: shep merged #71");
    assert_eq!(
        alert.text,
        format!(
            "Kelpie merged pull request #71 for issue #7 into main at {} on shep, \
             every gate passed. Nothing to answer.",
            &head[..7]
        )
    );
    assert_eq!(rig.relay.sent(), [], "a notice takes no answer");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        (&status["work_item"], &status["rulings"]),
        (&json!(null), &json!([]))
    );

    assert_eq!(step(&runner).unwrap(), None);
    drop(runner);
    let runner = rig.open().unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "a restart posts nothing again"
    );
    assert_eq!(rig.alerts.posts().len(), 1);
}

#[test]
fn a_notice_the_webhook_refuses_is_tried_again_across_a_restart_and_posted_once() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    let runner = under_auto(&rig, runner);
    rig.forge.set_checks(&head, Checks::Passed);
    rig.verdict(&runner);
    rig.clock.advance(CHECKS_SETTLE);
    assert!(merged(step(&runner).unwrap()));

    rig.alerts.set_down(true);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::NoticeFailed {
            issue: 7,
            pull_request: 71,
            ..
        })
    ));
    assert_eq!(step(&runner).unwrap(), None, "not before its retry");
    drop(runner);
    rig.alerts.set_down(false);
    let runner = rig.open().unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Noticed {
            issue: 7,
            pull_request: 71,
        })
    );
    rig.clock.advance(3600);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.alerts.posts().len(), 2, "one failure and one post");
}

#[test]
fn with_coderabbit_on_the_merge_waits_until_it_is_satisfied() {
    let (rig, runner, head) = summoned_under_auto("golbat");
    assert_eq!(rig.forge.merges(), []);
    rig.forge.coderabbit.review(71, &head, now(&rig) + 60, &[]);
    rig.clock.advance(60);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitSatisfied { .. })
    ));
    assert!(merged(rig.verdict(&runner)));
    assert_eq!(rig.forge.merges(), [(71, head)]);
    assert_eq!(rig.forge.comments(), []);
}

#[test]
fn a_main_that_moved_before_the_merge_is_caught_up_and_merged_with_no_ruling() {
    let (rig, runner, head) = Rig::with_pull_request("rotom");
    let runner = under_auto(&rig, runner);
    rig.forge.set_checks(&head, Checks::Passed);
    rig.verdict(&runner);
    rig.land_on_origin("landed.txt");
    rig.clock.advance(CHECKS_SETTLE);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MergeWithdrawn { reason, .. }) if reason == "main moved since the gate passed"
    ));
    let Some(StepReport::Rebased { head: rebased, .. }) = step(&runner).unwrap() else {
        panic!("the branch was not rebased");
    };
    rig.forge.set_checks(&rebased, Checks::Passed);
    assert!(merged(rig.verdict(&runner)));
    assert_eq!(rig.forge.merges(), [(71, rebased)]);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn a_merge_the_forge_refuses_is_caught_up_once_then_parks_and_a_yes_looks_again() {
    let (rig, runner, head) = Rig::with_pull_request("reactmap");
    let runner = under_auto(&rig, runner);
    rig.forge.set_merges_down(true);
    rig.forge.set_checks(&head, Checks::Passed);
    rig.verdict(&runner);
    rig.clock.advance(CHECKS_SETTLE);
    let refused = "cannot merge #71: gh failed: merges are down";
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::MergeWithdrawn {
            issue: 7,
            pull_request: 71,
            reason: refused.into(),
        })
    );

    let id = raised(rig.verdict(&runner));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["rulings"][0]["question"],
        format!(
            "Kelpie could not merge pull request #71 at {} after catching it up: {refused}. \
             `shep trigger reactmap rule '{id} yes'` has kelpie look again and merge once \
             every gate passes, and `shep trigger reactmap rule '{id} no <note>'` sends \
             the worker your note.",
            &head[..7]
        )
    );
    still_asks(&rig, &runner, id, "merge-refused");

    // The answer gives the next refusal its catch-up again.
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MergeWithdrawn { reason, .. }) if reason == refused
    ));
    rig.forge.set_merges_down(false);
    assert!(merged(rig.verdict(&runner)));
    assert_eq!(rig.forge.merges(), [(71, head)]);
}

#[test]
fn a_label_added_outside_kelpie_still_parks_on_a_ruling() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    let runner = under_auto(&rig, runner);
    rig.forge.label_pull_request(71, "bug");
    rig.forge.set_checks(&head, Checks::Passed);
    let id = raised(step(&runner).unwrap());
    still_asks(&rig, &runner, id, "foreign-change");
}

#[test]
fn a_commit_pushed_by_hand_still_parks_on_a_ruling() {
    let (rig, runner, _) = Rig::with_pull_request("koji");
    let runner = under_auto(&rig, runner);
    let by_hand = rig.push_by_hand("kelpie/7", "by-hand.txt");
    rig.forge.set_checks(&by_hand, Checks::Passed);
    let id = raised(rig.verdict(&runner));
    still_asks(&rig, &runner, id, "foreign-change");
}

#[test]
fn a_second_red_run_still_parks_on_a_ruling() {
    let (rig, runner, head) = Rig::with_pull_request("chelone");
    let runner = under_auto(&rig, runner);
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["lint".into()]));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::CiFailed { .. })
    ));
    rig.claude
        .script([Scripted::Say("Looked, changed nothing.")]);
    step(&runner).unwrap();
    let id = raised(rig.verdict(&runner));
    still_asks(&rig, &runner, id, "still-red");
}

#[test]
fn a_rebase_that_fails_still_parks_on_a_ruling() {
    let (rig, runner, _) = Rig::with_pull_request("xilriws");
    let runner = under_auto(&rig, runner);
    rig.land_on_origin("work.txt");
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Conflicted { .. })
    ));
    rig.claude.script([Scripted::Say("Could not resolve it.")]);
    step(&runner).unwrap();
    let id = raised(step(&runner).unwrap());
    still_asks(&rig, &runner, id, "rebase");
}

#[test]
fn a_closed_pull_request_still_parks_on_a_ruling() {
    let (rig, runner, _) = Rig::with_pull_request("zeus");
    let runner = under_auto(&rig, runner);
    rig.forge.set_state(71, PullRequestState::Closed);
    let id = raised(step(&runner).unwrap());
    still_asks(&rig, &runner, id, "closed");
}

#[test]
fn a_workers_question_still_parks_on_a_ruling() {
    let (rig, runner) = auto_with_issue_7("shep");
    rig.claude.script([Scripted::Say(
        "<kelpie-question>Which name?</kelpie-question>",
    )]);
    let id = raised(step(&runner).unwrap());
    still_asks(&rig, &runner, id, "question");
}

#[test]
fn a_failed_turn_still_parks_on_a_ruling() {
    let (rig, runner) = auto_with_issue_7("golbat");
    rig.claude
        .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
    let id = raised(step(&runner).unwrap());
    still_asks(&rig, &runner, id, "turn-failed");
}

#[test]
fn a_turn_past_its_ceiling_still_parks_on_a_ruling() {
    let (rig, runner) = auto_with_issue_7("rotom");
    rig.claude.script([Scripted::Fail(ClaudeError::TimedOut)]);
    let id = raised(step(&runner).unwrap());
    still_asks(&rig, &runner, id, "turn-timeout");
}

#[test]
fn the_review_loops_round_guard_still_parks_on_a_ruling() {
    let (rig, runner, _) = Rig::with_pull_request("reactmap");
    drop(runner);
    let state = rig.paths().state;
    let text = std::fs::read_to_string(&state).unwrap();
    let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    saved["work_item"]["phase"] = json!({
        "state": "review",
        "round": 9,
        "consecutive_clean": 0,
        "guard_cleared": false,
        "stage": { "stage": "round" },
    });
    std::fs::write(&state, saved.to_string()).unwrap();
    rig.merge_auto();
    let runner = rig.open().unwrap();
    let id = raised(step(&runner).unwrap());
    still_asks(&rig, &runner, id, "review-guard");
}

#[test]
fn a_fix_that_pushes_nothing_still_parks_on_a_ruling() {
    let (rig, runner, head) = summoned_under_auto("shep");
    assert!(matches!(
        hold_a_finding(&rig, &runner, &head, "Name the flag."),
        Some(StepReport::CodeRabbitJudged { round: 1, .. })
    ));
    rig.claude.script([Scripted::Say("Nothing to change.")]);
    step(&runner).unwrap();
    let id = raised(step(&runner).unwrap());
    still_asks(&rig, &runner, id, "fix-not-pushed");
}

#[test]
fn coderabbits_cap_still_parks_on_a_ruling() {
    let (rig, runner, head) = summoned_under_auto("koji");
    assert!(matches!(
        hold_a_finding(&rig, &runner, &head, "First."),
        Some(StepReport::CodeRabbitJudged { round: 1, .. })
    ));
    let head = fixed(&rig, &runner, "one.txt");
    rig.forge.coderabbit.settle("PRRT_71_0");
    let id = raised(hold_a_finding(&rig, &runner, &head, "Second."));
    still_asks(&rig, &runner, id, "coderabbit-cap");
}

#[test]
fn a_summon_coderabbit_never_answers_still_parks_on_a_ruling() {
    let (rig, runner, _) = summoned_under_auto("chelone");
    rig.clock.advance(ANSWER_WAIT);
    step(&runner).unwrap();
    rig.clock.advance(REVIEW_WAIT - ANSWER_WAIT);
    let id = raised(step(&runner).unwrap());
    still_asks(&rig, &runner, id, "coderabbit-silent");
}
