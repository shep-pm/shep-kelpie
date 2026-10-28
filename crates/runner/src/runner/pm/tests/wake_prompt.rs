//! A wake's prompt and the notes kelpie writes for the wake it answered:
//! the board carried in the prompt, and one line of what the wake was for,
//! what kelpie did, and the answer's `why`

use super::rulings::{answers, left_to_pm};
use super::{PICK_12, answered, pm_seen, step_until, with_pm};
use crate::ports::{AgentError, Role};
use crate::runner::pm::notes::PM_STANDING;
use crate::runner::pm::prompt::BOARD_INLINE;
use crate::runner::{StepReport, step};
use crate::settings::{Harness, Merging};
use crate::test::{Rig, Scripted};

fn read(rig: &Rig, name: &str) -> String {
    std::fs::read_to_string(rig.paths().pm.join(name)).unwrap_or_default()
}

// The board's events section of a prompt that carries the board
fn events(prompt: &str) -> String {
    let at = ["## Recent events", "## Since your last wake"]
        .iter()
        .find_map(|heading| prompt.find(heading))
        .expect("the board in the prompt holds its events");
    let rest = &prompt[at + 3..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    rest[..end].to_owned()
}

// The first `most` characters of `text`, for an assertion that names a prompt
fn head(text: &str, most: usize) -> &str {
    match text.char_indices().nth(most) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}

#[test]
fn the_first_wake_carries_the_board_and_does_not_tell_it_to_read_it() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude.script([Scripted::Say(PICK_12)]);
    answered(step(&runner).unwrap());

    let [wake] = pm_seen(&rig).try_into().unwrap();
    let prompt = &wake.call.prompt;
    let board = read(&rig, "board.md");
    assert!(board.contains("#12"), "{board}");
    assert!(
        prompt.contains(&board),
        "the prompt holds the board `board.md` holds: {prompt}"
    );
    assert!(prompt.contains("## The board"), "{prompt}");
    assert!(
        prompt.contains("`board.md` holds the same text"),
        "{prompt}"
    );
    assert!(!prompt.contains("Read `board.md`"), "{prompt}");
}

#[test]
fn a_resumed_wake_carries_the_board_with_only_the_events_since_its_last_answer() {
    let (rig, runner) = with_pm("koji");
    // Each read of the ready queue after the first is an event the board tells.
    rig.forge.list_ready(9, true);
    step(&runner).unwrap();
    rig.forge.list_ready(12, true);
    rig.clock.advance(61);
    step(&runner).unwrap();

    rig.ask(&runner, "tell", Some("first"));
    rig.claude.script([
        Scripted::Say("{\"pick\": null}"),
        Scripted::Say("{\"pick\": null}"),
    ]);
    answered(Some(step_until(&runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));

    rig.forge.list_ready(15, true);
    rig.clock.advance(61);
    step(&runner).unwrap();
    rig.ask(&runner, "tell", Some("second"));
    answered(Some(step_until(&runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));

    let [first, second] = pm_seen(&rig).try_into().unwrap();
    let before = events(&first.call.prompt);
    assert!(before.starts_with("Recent events"), "{before}");
    assert!(before.contains("#12 joined the ready queue"), "{before}");

    let after = events(&second.call.prompt);
    assert!(
        after.starts_with("Since your last wake"),
        "the board the resumed wake carries is the one written after its answer: {after}"
    );
    assert!(after.contains("#15 joined the ready queue"), "{after}");
    assert!(
        !after.contains("#12 joined the ready queue"),
        "its cursor holds the events it has read: {after}"
    );
    assert!(
        second
            .call
            .prompt
            .contains("`board.md` holds the same text"),
        "{}",
        &second.call.prompt
    );
}

#[test]
fn a_board_over_64_kib_keeps_the_read_instruction() {
    let (rig, runner) = with_pm("koji");
    // A ready issue the board passes over for a blocker keeps its body quoted
    // on it, so enough of them make a board no prompt should hold.
    let body = "word ".repeat(150);
    for n in 1..=110 {
        rig.forge.list_ready(n, false);
        rig.forge.set_ready_body(n, &body);
        rig.forge.block(n, 900);
    }
    rig.ask(&runner, "tell", Some("first"));
    rig.claude.script([Scripted::Say("{\"pick\": null}")]);
    answered(Some(step_until(&runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));

    let [wake] = pm_seen(&rig).try_into().unwrap();
    let board = read(&rig, "board.md");
    assert!(
        board.len() > BOARD_INLINE,
        "the test's board is {} bytes",
        board.len()
    );
    let prompt = &wake.call.prompt;
    assert!(
        prompt.len() < board.len(),
        "the board is no part of the prompt: {} bytes against the board's {}",
        prompt.len(),
        board.len()
    );
    assert!(!prompt.contains("## The board"), "{}", head(prompt, 400));
    assert!(
        prompt.contains("Read `board.md`, which kelpie wrote as the board stood when it woke you."),
        "{}",
        head(prompt, 400)
    );
    assert!(
        board.contains("#110"),
        "the board is written for it to read"
    );
}

#[test]
fn an_answered_wake_adds_one_line_with_its_reason_its_action_and_its_why() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    let long = "x".repeat(400);
    let home = rig.paths().pm.display().to_string();
    let picked = format!("{{\"pick\": null, \"hold\": [12], \"why\": \"{long}\"}}");
    let told = format!("{{\"pick\": null, \"hold\": [12], \"why\": \"{home}\"}}");
    let (picked, told): (&'static str, &'static str) = (
        Box::leak(picked.into_boxed_str()),
        Box::leak(told.into_boxed_str()),
    );
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(picked), Scripted::Say(told)]);
    answered(step(&runner).unwrap());
    rig.ask(&runner, "tell", Some("Release is out"));
    answered(Some(step_until(&runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));

    let notes = read(&rig, "pm-notes.md");
    let lines: Vec<&str> = notes.lines().collect();
    assert_eq!(lines.len(), 2, "{notes}");
    let [pick, tell] = lines.as_slice().try_into().unwrap();

    assert!(pick.starts_with("- [2026-09-21 14:13] "), "{pick}");
    assert!(
        pick.contains("woke for: a slot is free and 2 ready issues could fill it"),
        "{pick}"
    );
    assert!(!pick.contains("starts nothing"), "{pick}");
    let (_, why) = pick.split_once("; why: ").expect("{pick}");
    assert_eq!(
        why.chars().count(),
        300 + " …".chars().count(),
        "its `why` is cut at 300 characters: {why}"
    );

    assert!(
        tell.contains("woke for: the maintainer told you something"),
        "{tell}"
    );
    assert!(tell.contains("holds #12"), "{tell}");
    assert!(!tell.contains(&home), "{tell}");
    assert!(
        tell.contains("(withheld: it names a path on this machine, which names its user)"),
        "{tell}"
    );
}

#[test]
fn a_wake_that_failed_or_was_unread_adds_nothing_to_the_notes() {
    let (rig, runner) = with_pm("koji");
    rig.ask(&runner, "tell", Some("first"));
    let down = AgentError::Failed(Harness::ClaudeCode, "overloaded".into());
    rig.claude.script([
        Scripted::Fail(down),
        Scripted::Say("I thought about the board and wrote nothing else."),
    ]);
    assert!(
        matches!(step(&runner).unwrap(), Some(StepReport::PmFailed { .. })),
        "the wake failed"
    );

    rig.clock.advance(super::super::DOWN_FOR + 1);
    assert!(
        matches!(
            step_until(&runner, |r| matches!(r, StepReport::PmFailed { .. })),
            StepReport::PmFailed { .. }
        ),
        "its reply held no answer kelpie reads"
    );

    assert_eq!(
        read(&rig, "pm-notes.md"),
        "",
        "a wake that failed, and one whose reply kelpie could not read, wrote nothing"
    );
    assert_eq!(pm_seen(&rig).len(), 2, "both wakes ran");
}

#[test]
fn a_tell_reaches_the_notes_once_verbatim_on_the_wake_that_answers_it() {
    let (rig, runner) = with_pm("xilriws");
    rig.ask(&runner, "tell", Some("Hold #12 until the release"));
    rig.claude.script([Scripted::Say("{\"pick\": null}")]);
    answered(step(&runner).unwrap());

    let standing = read(&rig, PM_STANDING);
    assert_eq!(
        standing, "## 2026-09-21 14:13\nHold #12 until the release\n",
        "dated, and verbatim, whatever the project manager wrote"
    );
    let notes = read(&rig, "pm-notes.md");
    assert!(
        notes.contains("woke for: the maintainer told you something"),
        "{notes}"
    );
    assert_eq!(
        format!("{standing}{notes}")
            .matches("Hold #12 until the release")
            .count(),
        1,
        "the tell is in the notes once: {notes}"
    );
}

#[test]
fn a_wake_for_a_ruling_records_its_answer_and_not_nothing() {
    let (rig, runner) = left_to_pm("w-answer", Merging::Auto);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(answers('A', false))]);

    let (_, acted, _) = answered(Some(step_until(&runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));
    assert!(
        acted.contains(&"answered ruling 71 with option A".to_owned()),
        "{acted:?}"
    );

    let notes = read(&rig, "pm-notes.md");
    let lines: Vec<&str> = notes.lines().collect();
    assert_eq!(lines.len(), 1, "{notes}");
    assert!(
        lines[0].contains("answered ruling 71 with option A"),
        "the answer the wake was for is the action it records: {notes}"
    );
    assert!(!lines[0].contains("kelpie: nothing"), "{notes}");
}

#[test]
fn a_wake_for_a_ruling_it_gave_no_answer_on_records_that() {
    let (rig, runner) = left_to_pm("w-none", Merging::Auto);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say("{\"pick\": null}")]);

    let (_, acted, _) = answered(Some(step_until(&runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));
    let none = "gave no answer to ruling 71: its reply held no answer to it";
    assert!(acted.contains(&none.to_owned()), "{acted:?}");

    let notes = read(&rig, "pm-notes.md");
    let lines: Vec<&str> = notes.lines().collect();
    assert_eq!(lines.len(), 1, "{notes}");
    assert!(lines[0].contains(none), "{notes}");
}
