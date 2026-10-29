//! A review bot the project leaves out of its picks, and when it comes back
//!
//! A bot passed over as not installed on the repo, or shut out by its
//! visibility, is left out of every pick for a week, so no work item waits on
//! it again. One seen reviewing comes off the list at once, and one passed
//! over for its window, its silence or its settings stays a candidate.

use std::sync::Mutex;

use super::super::review_bot::{ANSWER_WAIT, FAR, SETTLE_LEAST};
use crate::board::briefing::date_time;
use crate::coderabbit::LABEL;
use crate::lease::LeaseKind;
use crate::lease::door::PickAsk;
use crate::ports::{Checks, Timestamp, Visibility};
use crate::runner::coderabbit::tests::{now, reviewed_by_qwen, summoned};
use crate::runner::review_bot::two_bots::{bot_reviewed, item_reviewed};
use crate::runner::{Runner, StepReport, step};
use crate::settings::AgentName;
use crate::state::BOT_LEFT_OUT;
use crate::test::{Rig, Scripted, ticket};

const NOT_INSTALLED: &str =
    "it has never posted a review on this repo, so it may not be installed there";
const NOT_PUBLIC: &str =
    "the repo is not public, and CodeRabbit's free plan reviews public repos only";
const WINDOW: &str = "its window is closed";

fn coderabbit() -> AgentName {
    AgentName::kelpies("coderabbit")
}

fn skipped(reason: &str) -> Option<StepReport> {
    Some(StepReport::ReviewerSkipped {
        issue: ticket(7),
        pull_request: 71,
        round: 3,
        reviewer: coderabbit(),
        reason: reason.to_owned(),
    })
}

// CodeRabbit's round, its summon sent and its review never answered: the
// forge says it has never reviewed on the repo.
fn passed_over_as_not_installed(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner, _) = summoned(project);
    rig.forge.set_last_bot_review(None);
    rig.clock.advance(ANSWER_WAIT);
    assert_eq!(step(&runner).unwrap(), skipped(NOT_INSTALLED));
    (rig, runner)
}

// The board's asks for a reviewer, in order, oldest first
fn review_asks(rig: &Rig) -> Vec<PickAsk> {
    rig.agent_board
        .asks()
        .into_iter()
        .filter(|ask| ask.agents.contains(&coderabbit()))
        .collect()
}

// Whether any of them left CodeRabbit out
fn left_out_of_a_pick(asks: &[PickAsk]) -> bool {
    asks.iter().any(|ask| ask.leave_out.contains(&coderabbit()))
}

// The bots the project's state file has left out, by reviewer name
fn left_out(rig: &Rig) -> Vec<String> {
    let text = std::fs::read_to_string(rig.paths().state).unwrap();
    let state: serde_json::Value = serde_json::from_str(&text).unwrap();
    state["bots_left_out"]
        .as_array()
        .map(|bots| {
            bots.iter()
                .map(|bot| {
                    bot["reviewer"]
                        .as_str()
                        .expect("a reviewer name")
                        .to_owned()
                })
                .collect()
        })
        .unwrap_or_default()
}

// CodeRabbit's summon label on pull request 71, in the order kelpie changed it
fn labels(rig: &Rig) -> Vec<(u64, String, bool)> {
    rig.forge
        .coderabbit
        .label_log()
        .into_iter()
        .filter(|(_, label, _)| label == LABEL)
        .collect()
}

// The next work item's pass, up to the round after its two reviewers read
fn next_item(rig: &Rig, runner: &Mutex<Runner>) -> String {
    rig.ask(runner, "drop", None);
    item_reviewed(rig, runner, 8)
}

// A project whose only reviewer is CodeRabbit, whose worker opened pull
// request 71, and whose CodeRabbit round is next
fn coderabbit_only(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    rig.reviewers(&["coderabbit"]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn: opens the pull request
    (rig, runner)
}

// When the state file says the left-out bot comes back
fn back_in(rig: &Rig) -> Timestamp {
    let text = std::fs::read_to_string(rig.paths().state).unwrap();
    let state: serde_json::Value = serde_json::from_str(&text).unwrap();
    let at = state["bots_left_out"][0]["at"].as_u64().expect("when");
    Timestamp(at + BOT_LEFT_OUT)
}

// The round after the next item's two reviewers read: the board is asked, and
// no round of CodeRabbit's runs
fn assert_no_bot_round(runner: &Mutex<Runner>) {
    let report = step(runner).unwrap();
    assert!(
        !matches!(
            report,
            Some(StepReport::MarkedReady { .. } | StepReport::Summoned { .. })
        ),
        "CodeRabbit read #81 anyway: {report:?}"
    );
}

#[test]
fn a_bot_passed_over_as_not_installed_is_not_asked_for_again() {
    let (rig, runner) = passed_over_as_not_installed("shep");
    assert_eq!(left_out(&rig), ["coderabbit"]);

    next_item(&rig, &runner);
    assert_no_bot_round(&runner);
    let asks = review_asks(&rig);
    assert!(
        !left_out_of_a_pick(&asks[..3]),
        "the pass that found it not installed asked it: {asks:#?}"
    );
    assert!(
        left_out_of_a_pick(&asks[3..]),
        "the next pass leaves it out of its pick: {asks:#?}"
    );
    assert_eq!(rig.forge.readied(), [71], "nothing marks #81 ready for it");
    assert_eq!(
        labels(&rig),
        [(71, LABEL.to_owned(), true), (71, LABEL.to_owned(), false)]
    );
}

#[test]
fn a_bot_passed_over_as_not_public_is_not_asked_for_again() {
    let (rig, runner, _) = reviewed_by_qwen("acme");
    rig.forge.set_visibility(Visibility::Private);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    assert_eq!(step(&runner).unwrap(), skipped(NOT_PUBLIC));
    assert_eq!(left_out(&rig), ["coderabbit"]);

    next_item(&rig, &runner);
    assert_no_bot_round(&runner);
    let asks = review_asks(&rig);
    assert!(
        !left_out_of_a_pick(&asks[..3]),
        "the pass that found the repo private asked it: {asks:#?}"
    );
    assert!(
        left_out_of_a_pick(&asks[3..]),
        "the next pass leaves it out of its pick: {asks:#?}"
    );
    assert_eq!(
        rig.forge.readied(),
        [71],
        "it marked #71 ready before it found the repo private"
    );
    assert_eq!(labels(&rig), [], "it never put its label on");
}

#[test]
fn a_bot_left_out_is_asked_for_again_after_seven_days() {
    let (rig, runner) = passed_over_as_not_installed("shep");
    assert_eq!(left_out(&rig), ["coderabbit"]);

    rig.clock.advance(BOT_LEFT_OUT);
    next_item(&rig, &runner);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady {
            issue: ticket(8),
            pull_request: 81,
        }),
        "its round runs again"
    );
    assert_eq!(rig.forge.readied(), [71, 81]);
    assert_eq!(left_out(&rig), ["coderabbit"], "its round is not over yet");
}

#[test]
fn a_bot_seen_reviewing_comes_off_the_list() {
    let (rig, runner, head) = summoned("shep");
    rig.forge.set_last_bot_review(None);
    rig.clock.advance(ANSWER_WAIT);
    assert_eq!(step(&runner).unwrap(), skipped(NOT_INSTALLED));
    assert_eq!(left_out(&rig), ["coderabbit"]);

    // It reviews the head anyway, and its late round reads it.
    rig.ci_on_now(&runner);
    rig.forge.coderabbit.review(71, &head, now(&rig) + 60, &[]);
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(rig.verdict(&runner), None, "green CI reads it first");
    rig.clock.advance(SETTLE_LEAST);
    assert_eq!(step(&runner).unwrap(), bot_reviewed(4, "coderabbit", 0));
    assert_eq!(
        left_out(&rig),
        Vec::<String>::new(),
        "a bot seen reviewing is asked again"
    );
}

#[test]
fn a_bot_passed_over_for_its_window_stays_a_candidate() {
    let (rig, runner, _) = reviewed_by_qwen("koji");
    rig.leases
        .opens_at(&LeaseKind::coderabbit(), Timestamp(now(&rig) + FAR + 1));
    assert_eq!(step(&runner).unwrap(), skipped(WINDOW));
    assert!(
        left_out(&rig).is_empty(),
        "its window closing is no reason to leave it out"
    );

    next_item(&rig, &runner);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped {
            issue: ticket(8),
            pull_request: 81,
            round: 3,
            reviewer: coderabbit(),
            reason: WINDOW.to_owned(),
        }),
        "the board picked it again for the next item"
    );
    assert!(left_out(&rig).is_empty());
    assert!(
        !left_out_of_a_pick(&review_asks(&rig)),
        "no pick left it out: {:?}",
        review_asks(&rig)
    );
    assert!(
        rig.forge.readied().is_empty(),
        "no round of its own ran for either item"
    );
}

// CodeRabbit's round on a project that lists it alone: its summon is sent,
// nothing answers, and the pass goes on without it.
fn passed_over_alone(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = coderabbit_only(project);
    rig.forge.set_last_bot_review(None);
    for _ in 0..4 {
        rig.clock.advance(ANSWER_WAIT);
        let report = step(&runner).unwrap();
        if let Some(StepReport::ReviewerSkipped { reason, round, .. }) = &report {
            assert_eq!((reason.as_str(), *round), (NOT_INSTALLED, 1), "{report:?}");
            return (rig, runner);
        }
    }
    panic!("CodeRabbit was never passed over");
}

#[test]
fn a_pass_with_nobody_but_a_left_out_bot_says_it_is_left_out_and_when() {
    let (rig, runner) = passed_over_alone("shep");
    assert_eq!(left_out(&rig), ["coderabbit"]);

    // The next work item has nobody to read it but the bot left out.
    rig.ask(&runner, "drop", None);
    rig.ask(&runner, "add", Some("8"));
    rig.forge.open_pull_request(81, "kelpie/8", &[8]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn: opens the pull request

    let until = date_time(back_in(&rig));
    let reason = format!("coderabbit is left out of every pick until {until}: {NOT_INSTALLED}");
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Unreviewed {
            issue: ticket(8),
            pull_request: 81,
            reason: reason.clone(),
        }),
        "not the board refusing a candidate"
    );
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(item["unreviewed"], reason.as_str());
    assert_eq!(item["phase"]["state"], "ci");
}
