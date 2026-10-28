//! What the project manager does about a workflow red on `main`: it reads the
//! failed log kelpie wrote, and a head still red after kelpie's rerun gets a
//! fix work item at the front of the board or a question to the maintainer

use std::sync::Mutex;

use serde_json::json;

use super::{answered, dispatched, pm_on, pm_seen, step_until};
use crate::ports::{BranchRun, Timestamp};
use crate::runner::{BOARD_POLL, MAIN_CI_POLL, Runner, StepReport, step};
use crate::test::{Rig, Scripted, ticket};

pub(super) const LOG: &str = "--- run 91\nFAIL e2e::checkout times out after 3 tries\n";

fn run(id: u64, created: u64) -> BranchRun {
    BranchRun {
        workflow: "test".into(),
        id,
        head: format!("head{id}"),
        event: "push".into(),
        conclusion: "failure".into(),
        created: Timestamp(created),
    }
}

fn ask(rig: &Rig, runner: &Mutex<Runner>, key: &str) -> serde_json::Value {
    rig.ask(runner, "status", None)[key].clone()
}

/// A project manager and a runner that has read `test` red in run 91 and
/// reran it, with no ready issue yet
fn rerun() -> (Rig, Mutex<Runner>) {
    let (rig, runner) = pm_on(Rig::new("acme"));
    rig.forge.set_run_log_tail(91, LOG);
    rig.forge.set_branch_runs("main", vec![run(91, Rig::EPOCH)]);
    step(&runner).unwrap();
    assert_eq!(rig.forge.run_reruns(), [91]);
    (rig, runner)
}

/// ... and read run 91 red again after its rerun, which wakes the project
/// manager once, and it makes no call
pub(super) fn red_again() -> (Rig, Mutex<Runner>) {
    let (rig, runner) = rerun();
    rig.claude.script([Scripted::Say("{\"pick\": null}")]);
    rig.clock.advance(MAIN_CI_POLL.as_secs());
    step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    (rig, runner)
}

/// Lists #9 and #12, and has the project manager answer the free slot's wake
fn answers(rig: &Rig, runner: &Mutex<Runner>, say: &'static str) -> StepReport {
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude.script([Scripted::Say(say)]);
    rig.clock.advance(BOARD_POLL.as_secs());
    step_until(runner, |r| matches!(r, StepReport::PmAnswered { .. }))
}

const FIX: &str = "{\"pick\": null, \"red\": {\"workflow\": \"test\", \"call\": \"fix\", \
    \"title\": \"Fix e2e::checkout on main\", \"body\": \"`test` fails in e2e::checkout: \
    `times out after 3 tries`. Suspect: the merge of #323.\", \"why\": \"a real break\"}}";

#[test]
fn a_wake_while_main_is_red_carries_the_failed_log_and_how_to_clear_it() {
    let (rig, runner) = rerun();
    answers(&rig, &runner, "{\"pick\": null}");

    let pm = rig.paths().pm;
    assert_eq!(
        std::fs::read_to_string(pm.join("ci/main-91.log")).unwrap(),
        LOG
    );
    let board = std::fs::read_to_string(pm.join("board.md")).unwrap();
    assert!(
        board.contains("- main, test: CI log `ci/main-91.log`\n"),
        "{board}"
    );
    assert!(
        board.contains("at head91; kelpie reran its failed jobs and waits for the result\n"),
        "{board}"
    );
    let [wake] = pm_seen(&rig).try_into().unwrap();
    assert!(wake.call.prompt.contains("\"red\": null or"), "fresh key");
    assert!(
        wake.call.prompt.contains("`ci/main-<run>.log`"),
        "{}",
        wake.call.prompt
    );
    assert!(
        !wake.call.prompt.contains("A pick of null"),
        "{}",
        wake.call.prompt
    );
}

#[test]
fn a_pick_of_null_while_the_rerun_is_out_acts_on_nothing_and_drops_nothing() {
    let (rig, runner) = rerun();

    let (_, acted, dropped) = answered(Some(answers(&rig, &runner, "{\"pick\": null}")));

    assert_eq!(acted, [] as [String; 0]);
    assert_eq!(dropped, [] as [String; 0]);
}

#[test]
fn a_pick_of_null_on_a_head_red_after_its_rerun_is_no_preference_and_the_rule_picks() {
    let (rig, runner) = red_again();

    let (_, acted, dropped) = answered(Some(answers(&rig, &runner, "{\"pick\": null}")));

    assert_eq!(acted, [] as [String; 0]);
    assert_eq!(dropped, [] as [String; 0]);
    assert_eq!(dispatched(step(&runner).unwrap()), ticket(9));
}

#[test]
fn a_pick_that_avoids_the_failing_area_still_starts_while_main_is_red() {
    let (rig, runner) = red_again();

    let (_, acted, _) = answered(Some(answers(
        &rig,
        &runner,
        "{\"pick\": 12, \"why\": \"#12 is docs only, so the e2e failure cannot touch it.\"}",
    )));

    assert_eq!(acted, ["picked #12"]);
    assert_eq!(dispatched(step(&runner).unwrap()), ticket(12));
}

#[test]
fn a_fix_call_opens_an_issue_with_what_it_read_and_it_starts_ahead_of_the_board() {
    let (rig, runner) = red_again();

    let (_, acted, dropped) = answered(Some(answers(&rig, &runner, FIX)));

    assert_eq!(
        acted,
        ["opened fix work item #900 at the front of the board for test"]
    );
    assert_eq!(dropped, [] as [String; 0]);
    let [fix] = rig.forge.created().try_into().unwrap();
    assert_eq!(fix.title, "Fix e2e::checkout on main");
    assert_eq!(fix.labels, ["ready-for-agent"]);
    assert!(
        fix.body.contains("`times out after 3 tries`"),
        "{}",
        fix.body
    );
    assert!(
        fix.body.contains("Suspect: the merge of #323."),
        "{}",
        fix.body
    );
    assert!(
        fix.body
            .contains("https://github.com/shep-pm/shep/actions/runs/91"),
        "{}",
        fix.body
    );
    // The forge lists it ready at the end of the queue, and the board starts it first.
    rig.forge.list_ready(900, false);
    assert_eq!(dispatched(step(&runner).unwrap()), ticket(900));
}

#[test]
fn status_shows_what_is_being_done_about_each_red_workflow() {
    let (rig, runner) = rerun();
    let doing = || ask(&rig, &runner, "red_workflows")[0]["doing"].clone();
    assert_eq!(
        doing(),
        json!("kelpie reran its failed jobs and waits for the result")
    );

    rig.claude.script([Scripted::Say("{\"pick\": null}")]);
    rig.clock.advance(MAIN_CI_POLL.as_secs());
    step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    assert_eq!(
        doing(),
        json!("red again after its rerun; the project manager opens a fix or asks")
    );

    answers(&rig, &runner, FIX);
    assert_eq!(
        doing(),
        json!("the project manager opened fix work item #900 at the front of the board")
    );
}

#[test]
fn an_ask_carries_the_log_lines_it_read_and_settles_the_head() {
    let (rig, runner) = red_again();

    let report = answers(
        &rig,
        &runner,
        "{\"pick\": null, \"red\": {\"workflow\": \"test\", \"call\": \"ask\", \
         \"log\": \"FAIL e2e::checkout times out after 3 tries\", \"why\": \"it passed on the PR\"}}",
    );

    let StepReport::PmAnswered { acted, reply, .. } = report else {
        panic!("not an answer: {report:?}");
    };
    assert_eq!(acted, ["asked the maintainer about test red on main"]);
    let reply = reply.expect("the question went to the maintainer");
    assert!(
        reply.contains("The log lines I read:\nFAIL e2e::checkout times out after 3 tries"),
        "{reply}"
    );
    assert!(
        reply.contains("cannot tell a flake from a break"),
        "{reply}"
    );
    assert_eq!(
        ask(&rig, &runner, "red_workflows")[0]["doing"],
        json!("the project manager asked the maintainer, with the log lines it read")
    );
}

#[test]
fn a_call_before_the_rerun_has_come_back_red_is_dropped() {
    let (rig, runner) = rerun();

    let (_, _, dropped) = answered(Some(answers(&rig, &runner, FIX)));

    assert_eq!(
        dropped,
        [
            "red test: kelpie's rerun of head91 has not come back red yet, so there is nothing to \
          fix or ask about"
        ]
    );
    assert!(rig.forge.created().is_empty());
}

#[test]
fn a_head_is_settled_once_and_a_new_red_head_is_the_project_managers_again() {
    let (rig, runner) = red_again();
    let call = &FIX[FIX.find("{\"workflow\"").unwrap()..FIX.len() - 1];
    let twice: &'static str =
        Box::leak(format!("{{\"pick\": null, \"red\": [{call}, {call}]}}").into_boxed_str());
    let (_, _, dropped) = answered(Some(answers(&rig, &runner, twice)));
    assert_eq!(
        dropped,
        [
            "red test: head91 already has the call the project manager opened fix work item #900 \
          at the front of the board"
        ]
    );
    assert_eq!(rig.forge.created().len(), 1);

    rig.forge
        .set_branch_runs("main", vec![run(95, Rig::EPOCH + 3600)]);
    rig.clock.advance(MAIN_CI_POLL.as_secs());
    step(&runner).unwrap();
    assert_eq!(
        ask(&rig, &runner, "red_workflows")[0]["doing"],
        json!("kelpie reran its failed jobs and waits for the result")
    );
}

#[test]
fn a_head_red_again_wakes_it_in_a_quiet_project_with_no_issue_to_pick() {
    let (rig, runner) = rerun();
    assert!(pm_seen(&rig).is_empty());

    rig.claude.script([Scripted::Say(FIX)]);
    rig.clock.advance(MAIN_CI_POLL.as_secs());
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));

    let StepReport::PmAnswered {
        woke_for, acted, ..
    } = report
    else {
        panic!("not an answer: {report:?}");
    };
    assert_eq!(
        woke_for,
        [
            "test is still red on main at head91 after kelpie reran its failed jobs: read its log, \
          then fix it or ask the maintainer with the `red` key"
        ]
    );
    assert_eq!(
        acted,
        ["opened fix work item #900 at the front of the board for test"]
    );
}
