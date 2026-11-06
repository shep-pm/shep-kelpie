//! A due worker turn that does not start says what it waits on

use serde_json::Value;

use super::*;
use crate::github::ApiError;
use crate::ports::Timestamp;
use crate::runner::flight::{Pass, advance};

// Three slots and issues 7 and 8 open
fn seven_and_eight(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    rig.edit_settings(|s| {
        assert!(
            s.contains("active_items = 1"),
            "the example's `active_items` moved"
        );
        s.replace("active_items = 1", "active_items = 3")
    });
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "add", Some("8"));
    (rig, runner)
}

// What `status` says issue 7 is held up on, if it says
fn held_up(rig: &Rig, runner: &Mutex<Runner>) -> Option<Value> {
    held_up_of(rig, runner, 7)
}

fn held_up_of(rig: &Rig, runner: &Mutex<Runner>, issue: u64) -> Option<Value> {
    let status = rig.ask(runner, "status", None);
    let item = status["work_items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["issue"] == ticket(issue).to_string())
        .unwrap()
        .clone();
    item.get("held_up").cloned()
}

fn because(rig: &Rig, runner: &Mutex<Runner>) -> String {
    let held = held_up(rig, runner).expect("status said nothing of the wait");
    held["because"].as_str().unwrap().to_owned()
}

fn idle(runner: &Mutex<Runner>) {
    let pass = advance(runner).unwrap();
    assert!(matches!(pass, Pass::Idle), "{pass:?}");
}

#[test]
fn a_second_item_starts_its_turn_beside_the_first_short_of_a_spent_window() {
    let (rig, runner) = seven_and_eight("acme");
    let (seven, eight) = (Hold::default(), Hold::default());
    rig.claude
        .script([Scripted::Hold(seven.clone()), Scripted::Hold(eight.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    // 90% of the window and of the week: neither is spent, so #8 starts too.
    rig.meter.set(Rig::utilization(90, 90));
    let pass = advance(&runner).unwrap();
    assert!(matches!(pass, Pass::Started), "{pass:?}");
    assert!(eight.entered(crate::test::CALL_PATIENCE));
    seven.release();
    eight.release();
}

#[test]
fn a_turn_waiting_for_a_github_token_says_so_in_status() {
    let rig = Rig::new("acme");
    rig.with_app();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    assert_eq!(held_up(&rig, &runner), None);

    rig.github.fail_next(ApiError::Refused(502));
    idle(&runner);
    let token = "waiting for a GitHub token, which GitHub is not giving just now";
    assert_eq!(because(&rig, &runner), token);

    // The pause before the token is asked for again holds it the same way.
    idle(&runner);
    assert_eq!(because(&rig, &runner), token);
}

#[test]
fn a_turn_held_back_by_a_draining_runner_says_so_in_status() {
    let (rig, runner) = with_issue_7("acme");
    rig.ask(&runner, "drain", None);
    idle(&runner);
    assert_eq!(
        because(&rig, &runner),
        "the runner is draining, so no turn starts"
    );
    assert_eq!(rig.claude.all_calls(), []);
}

// A process that runs until dropped, for an attach to hold the work item by:
// the runner lets go of one whose process `ps` does not find
struct Live(std::process::Child);

impl Live {
    fn new() -> Self {
        let child = std::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        Self(child)
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn attach(runner: &Mutex<Runner>, issue: u64, by: &Live) {
    let by = crate::runner::attach::running(by.0.id()).expect("`ps` names the process");
    runner
        .lock()
        .unwrap()
        .on(TicketId::from_issue(issue))
        .update(|item| {
            item.attached = Some(crate::work_item::Attached {
                by,
                session: None,
                since: Timestamp(Rig::EPOCH),
            });
        })
        .unwrap();
}

#[test]
fn a_turn_for_an_attached_work_item_says_so_in_status() {
    let (rig, runner) = with_issue_7("acme");
    let session = Live::new();
    attach(&runner, 7, &session);
    idle(&runner);
    assert_eq!(
        because(&rig, &runner),
        "attached by the maintainer, so no turn starts"
    );
}

#[test]
fn an_attach_replaces_the_reason_a_turn_was_held_up_for() {
    let (rig, runner) = with_issue_7("acme");
    rig.ask(&runner, "drain", None);
    idle(&runner);
    assert_eq!(
        because(&rig, &runner),
        "the runner is draining, so no turn starts"
    );

    let session = Live::new();
    attach(&runner, 7, &session);
    idle(&runner);
    assert_eq!(
        because(&rig, &runner),
        "attached by the maintainer, so no turn starts"
    );
}

#[test]
fn a_turn_for_a_work_item_with_no_slot_says_so_in_status() {
    let (rig, runner) = with_issue_7("acme");
    {
        // The board opens nothing past the one slot, but an item that left a
        // ruling waits for it: this is the seat that item has.
        let mut r = runner.lock().unwrap();
        let mut eight = r.state.work_items[0].clone();
        eight.issue = ticket(8);
        eight.seat = crate::work_item::Seat::Waiting;
        r.state.work_items.push(eight);
    }
    let seven = Hold::default();
    rig.claude.script([Scripted::Hold(seven.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    idle(&runner);
    let held = held_up_of(&rig, &runner, 8).expect("status said nothing of the wait");
    assert_eq!(
        held["because"],
        "waiting for a slot under `concurrency.active_items`"
    );
    assert_eq!(held_up_of(&rig, &runner, 7), None);
    seven.release();
}

#[test]
fn a_turn_the_pacer_still_holds_says_what_held_it_in_status() {
    let (rig, runner) = with_issue_7("acme");
    rig.meter.set(Rig::utilization(0, 100));
    let pass = advance(&runner).unwrap();
    let Pass::Report(StepReport::Held { reason, .. }) = pass else {
        panic!("the pacer held nothing: {pass:?}");
    };
    // The fresh hold is a report of its own, and the passes after it are silent.
    idle(&runner);
    assert_eq!(
        because(&rig, &runner),
        format!("held by the pacer: {reason}")
    );
    assert_eq!(rig.claude.all_calls(), []);
}

#[test]
fn a_turn_for_an_adopted_branch_pushed_to_by_someone_else_says_so_in_status() {
    let rig = Rig::new("rotom");
    rig.ci_on();
    rig.push_by_hand("fix/timeline", "work.txt");
    let red = rig.push_by_hand("fix/timeline", "more.txt");
    rig.forge.open_pull_request(80, "fix/timeline", &[5]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    rig.forge
        .set_checks(&red, crate::ports::Checks::Failed(vec!["test".into()]));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::CiFailed { .. })
    ));
    rig.push_by_hand("fix/timeline", "owner.txt");
    // The forge still reports the head it had, a moment behind the push.
    rig.forge.set_lagging(80, Some(&red));

    idle(&runner);

    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["held_up"]["because"],
        "its branch was pushed to by someone else, so the gate reads it first"
    );
    assert_eq!(rig.claude.all_calls(), []);
}

// The lines the log took for held-up turns since the last call
fn told(runner: &Mutex<Runner>) -> Vec<String> {
    let notes = runner.lock().unwrap().take_notes();
    notes
        .into_iter()
        .filter(|note| note.contains("has not started"))
        .collect()
}

#[test]
fn a_turn_held_up_five_minutes_is_logged_once_for_each_reason() {
    let (rig, runner) = with_issue_7("acme");
    rig.meter.set(Rig::utilization(0, 100));
    advance(&runner).unwrap();
    idle(&runner);
    rig.clock.advance(299);
    idle(&runner);
    assert_eq!(told(&runner), Vec::<String>::new());

    rig.clock.advance(1);
    idle(&runner);
    let [line] = told(&runner).try_into().unwrap();
    assert!(
        line.starts_with(
            "issue #7's turn is due and has not started for 5 minutes: held by the pacer: "
        ),
        "{line}"
    );
    rig.clock.advance(60);
    idle(&runner);
    assert_eq!(
        told(&runner),
        Vec::<String>::new(),
        "the same reason told twice"
    );

    // Another reason is told once, too: the pacer lets go of a draining runner.
    rig.ask(&runner, "drain", None);
    rig.meter.set(Rig::utilization(0, 0));
    rig.clock.advance(600);
    idle(&runner);
    let [line] = told(&runner).try_into().unwrap();
    assert!(
        line.ends_with("the runner is draining, so no turn starts"),
        "{line}"
    );
    idle(&runner);
    assert_eq!(told(&runner), Vec::<String>::new());
}

#[test]
fn a_turn_that_starts_clears_what_held_it_up() {
    let (rig, runner) = with_issue_7("acme");
    rig.meter.set(Rig::utilization(0, 100));
    advance(&runner).unwrap();
    idle(&runner);
    assert!(held_up(&rig, &runner).is_some());

    rig.meter.set(Rig::utilization(0, 0));
    rig.clock.advance(601);
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert_eq!(held_up(&rig, &runner), None);
    hold.release();
}
