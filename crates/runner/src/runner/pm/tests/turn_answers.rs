//! The project manager's answers to a timed-out or failed turn: resume it,
//! resume it after a note, or park it on the maintainer

use std::sync::Mutex;
use std::time::Instant;

use serde_json::Value;

use super::asked::{comments, pending, posted};
use super::name_pm;
use super::rulings::left_to_pm;
use crate::ports::{AgentError, Cost, Role, Usage};
use crate::runner::{Runner, step};
use crate::settings::Harness;
use crate::test::{CALL_PATIENCE, Rig, Scripted};

/// Where a turn's last change sits, past the ten minutes a turn may be quiet for
const STALE: u64 = Rig::EPOCH - 11 * 60;

const BOTH: &str = "turn-timeout = true, turn-failed = true";

/// The words an `ask` puts ahead of the resumed turn
const PROGRESS: &str = "what you have done so far";

/// The project manager's reply voting on ruling `ruling`: `choice`, and `extra` after `design`
fn votes_on(ruling: u64, choice: char, extra: &str) -> &'static str {
    let text = format!(
        "{{\"pick\": null, \"votes\": [{{\"ruling\": {ruling}, \"choice\": \"{choice}\", \
         \"why\": \"PM-WHY\", \"design\": false{extra}}}]}}"
    );
    Box::leak(text.into_boxed_str())
}

fn votes(choice: char, extra: &str) -> &'static str {
    votes_on(7, choice, extra)
}

const ASK: &str = ", \"ask\": true";

fn timed_out() -> Scripted {
    Scripted::TimeOutPlanting("stale.rs", "fn stale() {}\n", STALE)
}

fn failed() -> Scripted {
    Scripted::Fail(AgentError::Failed(Harness::ClaudeCode, "overloaded".into()))
}

/// A project with a project manager and `map` as its `rulings.pm_decides`
fn project(name: &str, map: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(name);
    rig.with_app();
    rig.maintainer("octocat");
    name_pm(&rig);
    rig.edit_settings(|s| format!("{s}\n[app.dogs.kelpie.rulings]\npm_decides = {{ {map} }}\n"));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    (rig, runner)
}

/// [`project`] whose worker's turn on #7 ends with `end`, and whose project
/// manager answers the ruling with `vote`, stepped until a comment is posted
fn answered(name: &str, map: &str, end: Scripted, vote: &'static str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = project(name, map);
    rig.claude.script([end]);
    rig.claude.script_for(Role::Pm, [Scripted::Say(vote)]);
    posted(&rig, &runner, |_| true);
    (rig, runner)
}

/// The prompt of the worker's second call, the resumed turn, once it takes it
fn resumed_prompt(rig: &Rig, runner: &Mutex<Runner>) -> String {
    let usage = Usage {
        input: 1,
        cache_write: 10,
        cache_write_5m: 0,
        cache_read: 100,
        output: 1000,
    };
    rig.claude.script([Scripted::Reply(usage, Cost(1))]);
    let deadline = Instant::now() + CALL_PATIENCE;
    while rig.claude.calls().len() < 2 {
        assert!(Instant::now() < deadline, "the turn was never resumed");
        step(runner).unwrap();
        std::thread::yield_now();
    }
    rig.claude.calls()[1].prompt.clone()
}

/// How the work item's phase reads in `status`
fn phase(rig: &Rig, runner: &Mutex<Runner>) -> Value {
    rig.ask(runner, "status", None)["work_item"]["phase"].clone()
}

/// The comment that asks the maintainer, if one was posted
fn card(rig: &Rig) -> Option<String> {
    (comments(rig).into_iter()).find(|c| c.starts_with("### "))
}

/// The comment that says the project manager answered, if one was posted
fn resumed(rig: &Rig) -> Option<String> {
    (comments(rig).into_iter()).find(|c| c.starts_with("**Answered by the project manager"))
}

#[test]
fn a_yes_on_a_timed_out_turn_resumes_it_with_its_usual_prompt() {
    let (rig, runner) = answered("turn-yes", BOTH, timed_out(), votes('A', ""));

    let prompt = resumed_prompt(&rig, &runner);

    assert!(
        prompt.starts_with("Kelpie stopped your last turn"),
        "{prompt}"
    );
    assert!(!prompt.contains(PROGRESS), "{prompt}");
}

#[test]
fn an_ask_on_a_timed_out_turn_puts_the_note_ahead_of_its_prompt() {
    let (rig, runner) = answered("turn-ask-timeout", BOTH, timed_out(), votes('A', ASK));

    let prompt = resumed_prompt(&rig, &runner);

    let (note, due) = prompt
        .split_once("Kelpie stopped your last turn")
        .expect(&prompt);
    assert!(note.contains(PROGRESS), "{prompt}");
    assert!(due.contains("carry on from where you left off"), "{prompt}");
}

#[test]
fn an_ask_on_a_failed_turn_puts_the_note_ahead_of_the_turn_it_retries() {
    let (rig, runner) = answered("turn-ask-failed", BOTH, failed(), votes('A', ASK));

    let prompt = resumed_prompt(&rig, &runner);

    let (note, due) = prompt
        .split_once("Your last turn failed before it finished")
        .expect(&prompt);
    assert!(note.contains(PROGRESS), "{prompt}");
    assert!(due.contains("Carry on with the work item"), "{prompt}");
}

#[test]
fn an_ask_on_a_first_turn_with_no_session_sends_no_note_and_says_so() {
    let (rig, runner) = project("turn-ask-first", BOTH);
    // A worktree folder that is no worktree fails the turn before its session begins.
    std::fs::create_dir_all(rig.worktree_7()).unwrap();
    std::fs::write(rig.worktree_7().join("stray.txt"), "not a worktree\n").unwrap();
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(votes('A', ASK))]);
    posted(&rig, &runner, |_| true);

    assert!(
        (rig.claude.calls().iter()).all(|c| !c.prompt.contains(PROGRESS)),
        "a worker was sent the note"
    );
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes.iter().any(|n| n.contains("session has not begun")),
        "{notes:?}"
    );
}

#[test]
fn an_ask_on_another_kind_is_ignored_and_says_so() {
    let (rig, runner) = left_to_pm("turn-ask-merge");
    let vote = votes_on(71, 'A', ASK);
    rig.claude.script_for(Role::Pm, [Scripted::Say(vote)]);

    super::rulings::settled(&rig, &runner);

    assert!(
        super::rulings::answered(&rig).is_some(),
        "the yes was applied"
    );
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("`ask` is for a timed-out or failed turn")),
        "{notes:?}"
    );
}

#[test]
fn a_no_on_a_timed_out_turn_parks_it_with_the_pick_on_its_card() {
    let (rig, runner) = answered("turn-park-timeout", BOTH, timed_out(), votes('B', ""));

    let card = card(&rig).expect("the maintainer is asked");
    assert!(card.contains("**Project manager: "), "{card}");
    assert!(card.contains("PM-WHY"), "{card}");
    assert_eq!(resumed(&rig), None, "nothing was resumed");
    assert!(pending(&rig), "the ruling waits");
    assert_eq!(phase(&rig, &runner)["state"], "ruling");
}

#[test]
fn a_no_on_a_failed_turn_parks_it_with_the_pick_on_its_card() {
    let (rig, runner) = answered("turn-park-failed", BOTH, failed(), votes('B', ""));

    let card = card(&rig).expect("the maintainer is asked");
    assert!(card.contains("PM-WHY"), "{card}");
    assert_eq!(resumed(&rig), None);
    assert!(pending(&rig));
    assert_eq!(phase(&rig, &runner)["state"], "ruling");
}

#[test]
fn a_no_on_another_kind_is_applied_as_before() {
    let (rig, runner) = left_to_pm("turn-no-merge");
    let vote = votes_on(71, 'B', "");
    rig.claude.script_for(Role::Pm, [Scripted::Say(vote)]);

    super::rulings::settled(&rig, &runner);

    let said = super::rulings::answered(&rig).expect("the no was applied and posted");
    assert!(
        said.starts_with("**Answered by the project manager: B, "),
        "{said}"
    );
    assert!(!pending(&rig));
    drop(runner);
}

#[test]
fn the_maintainers_no_on_a_parked_turn_still_stops_the_work_item() {
    let (rig, runner) = answered("turn-park-then-no", BOTH, timed_out(), votes('B', ""));
    assert!(pending(&rig));

    let reply = rig.ask(&runner, "rule", Some("7 no stop"));
    assert_eq!(reply.get("error"), None, "{reply}");

    assert!(!pending(&rig));
    assert_eq!(phase(&rig, &runner)["state"], "done");
}

#[test]
fn a_turn_kind_set_false_goes_to_the_maintainer_and_wakes_no_answer() {
    let (rig, runner) = project("turn-false", "turn-timeout = false, turn-failed = true");
    rig.claude.script([timed_out()]);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(votes('A', ""))]);

    let card = posted(&rig, &runner, |_| true);

    assert!(card.starts_with("### "), "{card}");
    let woken = (super::pm_seen(&rig).iter())
        .filter(|s| s.call.prompt.contains("waits on your answer"))
        .count();
    assert_eq!(woken, 0);
    assert!(pending(&rig));
}

#[test]
fn a_resume_reaches_the_maintainer_as_no_ruling_and_mentions_no_one() {
    let (rig, runner) = answered("turn-resume-quiet", BOTH, timed_out(), votes('A', ASK));

    let said = resumed(&rig).expect("the resume is posted");

    assert!(said.contains("Reply `7 no <note>` to reopen it."), "{said}");
    assert!(!said.contains('@'), "{said}");
    assert_eq!(card(&rig), None, "the maintainer is never asked");
    assert!(!pending(&rig));
    assert_ne!(phase(&rig, &runner)["state"], "ruling");
}
