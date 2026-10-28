//! A turn that ran past its ceiling while its worktree was still changing
//!
//! The rig's clock is fake and stands at `Rig::EPOCH`, while a file's mtime and
//! a commit's date are real, so each test sets them against the clock.

use super::*;

const RESUMED: &str = "Kelpie stopped your last turn";

// How long before the ceiling the turn last wrote
const FIVE_MINUTES_AGO: u64 = Rig::EPOCH - 5 * 60;
const ELEVEN_MINUTES_AGO: u64 = Rig::EPOCH - 11 * 60;

fn phase(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
    rig.ask(runner, "status", None)["work_item"]["phase"].clone()
}

// The prompt of the second call, which resumes the first's session
fn resumed_prompt(rig: &Rig) -> String {
    let [first, resumed] = rig.claude.calls().try_into().unwrap();
    assert_eq!(resumed.session, Session::Resume(first.session.id().clone()));
    resumed.prompt
}

#[test]
fn a_turn_that_wrote_a_file_in_its_last_ten_minutes_resumes_with_no_ruling() {
    let (rig, runner) = with_issue_7("shep");
    rig.claude.script([Scripted::TimeOutPlanting(
        "half.rs",
        "fn half() {}\n",
        FIVE_MINUTES_AGO,
    )]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_ne!(phase(&rig, &runner)["state"], "ruling");
    assert_eq!(
        runner.lock().unwrap().take_notes(),
        ["issue #7's turn passed its ceiling while still working, resumed (1 of 3)"]
    );
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ended { .. })
    ));
    let prompt = resumed_prompt(&rig);
    assert!(prompt.starts_with(RESUMED), "{prompt}");
}

#[test]
fn a_turn_that_committed_in_its_last_ten_minutes_resumes_with_no_ruling() {
    let (rig, runner) = with_issue_7("shep");
    rig.claude.script([Scripted::TimeOutCommitting(
        "done.rs",
        "fn done() {}\n",
        FIVE_MINUTES_AGO,
    )]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_ne!(phase(&rig, &runner)["state"], "ruling");
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    step(&runner).unwrap();
    let prompt = resumed_prompt(&rig);
    assert!(prompt.starts_with(RESUMED), "{prompt}");
}

#[test]
fn a_turn_with_no_change_in_ten_minutes_raises_the_ruling_as_before() {
    let (rig, runner) = with_issue_7("shep");
    rig.claude.script([Scripted::TimeOutPlanting(
        "stale.rs",
        "fn stale() {}\n",
        ELEVEN_MINUTES_AGO,
    )]);

    let Some(StepReport::TimedOut { id, .. }) = step(&runner).unwrap() else {
        panic!("a turn that changed nothing in ten minutes raised no ruling");
    };

    assert_eq!(id, ticket(7));
    assert_eq!(
        phase(&rig, &runner),
        json!({ "state": "ruling", "id": "7" })
    );
    assert_eq!(runner.lock().unwrap().take_notes(), Vec::<String>::new());
}

#[test]
fn a_fourth_ceiling_raises_the_ruling_even_with_progress() {
    let (rig, runner) = with_issue_7("shep");
    rig.claude.script([
        Scripted::TimeOutPlanting("one.rs", "1\n", FIVE_MINUTES_AGO),
        Scripted::TimeOutPlanting("two.rs", "2\n", FIVE_MINUTES_AGO),
        Scripted::TimeOutPlanting("three.rs", "3\n", FIVE_MINUTES_AGO),
        Scripted::TimeOutPlanting("four.rs", "4\n", FIVE_MINUTES_AGO),
    ]);

    for _ in 0..3 {
        assert_eq!(step(&runner).unwrap(), None);
    }
    let Some(StepReport::TimedOut { id, .. }) = step(&runner).unwrap() else {
        panic!("a fourth ceiling raised no ruling");
    };

    assert_eq!(id, ticket(7));
    assert_eq!(
        runner.lock().unwrap().take_notes(),
        [
            "issue #7's turn passed its ceiling while still working, resumed (1 of 3)",
            "issue #7's turn passed its ceiling while still working, resumed (2 of 3)",
            "issue #7's turn passed its ceiling while still working, resumed (3 of 3)",
        ]
    );
}

#[test]
fn a_resumed_turn_gets_a_fresh_ceiling() {
    let (rig, runner) = with_issue_7("shep");
    rig.claude.script([Scripted::TimeOutPlanting(
        "half.rs",
        "fn half() {}\n",
        FIVE_MINUTES_AGO,
    )]);
    assert_eq!(step(&runner).unwrap(), None);
    // The runner stops, and the whole ceiling passes before it starts again.
    drop(runner);
    rig.clock.advance(3601);
    let runner = rig.open().unwrap();

    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    let resumed = step(&runner).unwrap();

    assert!(
        matches!(resumed, Some(StepReport::Ended { .. })),
        "a ceiling from before the resume parked it: {resumed:?}"
    );
    resumed_prompt(&rig);
}

#[test]
fn a_turn_past_its_ceiling_is_not_resumed_when_another_pull_request_closes_its_issue() {
    let rig = Rig::new("shep");
    rig.with_app();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.list_open_pull(80, "Fix seven", &[7]);
    rig.forge.set_author(80, "newcomer");
    rig.claude.script([Scripted::TimeOutPlanting(
        "half.rs",
        "fn half() {}\n",
        FIVE_MINUTES_AGO,
    )]);

    step(&runner).unwrap();
    let ended = step(&runner).unwrap();

    assert!(
        matches!(
            ended,
            Some(StepReport::Finished {
                merged: false,
                closed: false,
                ..
            })
        ),
        "{ended:?}"
    );
    assert_eq!(rig.claude.calls().len(), 1, "the turn was resumed");
    assert_eq!(
        rig.github.comments().len(),
        1,
        "{:?}",
        rig.github.comments()
    );
}
