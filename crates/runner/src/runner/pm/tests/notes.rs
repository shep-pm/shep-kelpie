//! The project manager's notes through the runner: the log kelpie rolls at a
//! wake, the standing notes it writes, and what a wake says of both

use super::{PICK_12, answered, pm_seen, step_until, with_pm};
use crate::ports::{AgentError, Role, Session};
use crate::runner::flight::advance;
use crate::runner::pm::notes::{NOTES_MAX, PM_ARCHIVE, PM_STANDING, STANDING_MAX};
use crate::runner::{Pass, StepReport, step};
use crate::settings::Harness;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted};

/// A log of `n` entries, each a `## wake <n>` heading over 20 lines of 64
/// bytes, so 40 of them are 51,631 bytes
fn log_of(n: usize) -> String {
    let line = "x".repeat(63) + "\n";
    (1..=n)
        .map(|i| format!("## wake {i}\n{}", line.repeat(20)))
        .collect()
}

/// What a wake says when the standing notes are past their limit
const CONDENSE: &str = "`pm-standing.md` is past 4 KiB. Condense it";

fn read(rig: &Rig, name: &str) -> String {
    std::fs::read_to_string(rig.paths().pm.join(name)).unwrap_or_default()
}

#[test]
fn a_wake_over_the_limit_rolls_the_file_before_its_call() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    std::fs::create_dir_all(rig.paths().pm).unwrap();
    let log = log_of(40);
    std::fs::write(rig.paths().pm.join(crate::ports::PM_NOTES), &log).unwrap();

    let hold = Hold::default();
    rig.claude
        .script([Scripted::HoldThenSay(hold.clone(), PICK_12)]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the wake's call never began");

    let kept = read(&rig, crate::ports::PM_NOTES);
    let rolled = read(&rig, PM_ARCHIVE);
    assert!(kept.len() <= NOTES_MAX, "the log is {} bytes", kept.len());
    assert_eq!(format!("{rolled}{kept}"), log, "every byte held once");

    hold.release();
    answered(Some(step_until(&runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));
    assert_eq!(pm_seen(&rig).len(), 1, "the roll is no wake of its own");
}

#[test]
fn a_line_with_no_end_or_an_unreadable_end_stands_as_plain_text() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.block(9, 900);
    std::fs::create_dir_all(rig.paths().pm).unwrap();
    let lines = "Hold #12 until the release\n\
         Hold #12 (until the release)\n\
         Hold #12 (until soon)\n\
         Hold #12 (until 2026-13-05)\n\
         Hold #12 (until #9) until the release\n\
         Hold #12 (until #0)\n";
    std::fs::write(rig.paths().pm.join(PM_STANDING), lines).unwrap();
    rig.claude.script([Scripted::Say("{\"pick\": null}")]);
    rig.ask(&runner, "tell", Some("first"));
    let (_, acted, dropped) = answered(step(&runner).unwrap());

    assert_eq!(dropped, Vec::<String>::new(), "no tell is refused for one");
    assert!(!acted.iter().any(|a| a.contains("standing")), "{acted:?}");
    assert_eq!(
        rig.forge.issue_reads(),
        0,
        "no line carries an end, so no ticket is read"
    );
    let standing = read(&rig, PM_STANDING);
    assert!(standing.starts_with(lines), "{standing}");
    let log = read(&rig, "pm-notes.md");
    assert!(!log.contains("standing ended:"), "{log}");
}

#[test]
fn a_removed_line_takes_its_tell_heading_when_it_is_the_only_line_under_it() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.block(9, 900);
    std::fs::create_dir_all(rig.paths().pm).unwrap();
    std::fs::write(
        rig.paths().pm.join(PM_STANDING),
        "Never merge a head with CI red\n\
         ## 2026-09-20 09:00\nHold the release notes (until #41)\n\
         ## 2026-09-21 14:13\nHold #12 until the release (until #41)\nReview every diff before a merge\n",
    )
    .unwrap();
    rig.forge.close_issue(41);
    rig.claude.script([Scripted::Say("{\"pick\": null}")]);
    rig.ask(&runner, "tell", Some("first"));
    answered(step(&runner).unwrap());

    let standing = read(&rig, PM_STANDING);
    assert!(
        standing.starts_with("Never merge a head with CI red\n## 2026-09-21 14:13\nReview every diff before a merge\n"),
        "the heading with a line left under it stays, the one whose only line went goes: {standing}"
    );
    assert!(!standing.contains("## 2026-09-20 09:00"), "{standing}");
    assert!(!standing.contains("Hold the release notes"), "{standing}");
    assert!(
        !standing.contains("Hold #12 until the release"),
        "{standing}"
    );
    let notes = read(&rig, "pm-notes.md");
    assert_eq!(notes.matches("standing ended:").count(), 2, "{notes}");
}

#[test]
fn a_standing_line_whose_ticket_read_fails_stands_and_is_named_as_unsettled() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.block(9, 900);
    std::fs::create_dir_all(rig.paths().pm).unwrap();
    std::fs::write(
        rig.paths().pm.join(PM_STANDING),
        "Hold the release notes (until #41)\nNever merge a head with CI red\n",
    )
    .unwrap();
    // The source cannot say how #41 stands
    rig.forge.remove_issue(41);
    rig.claude.script([Scripted::Say("{\"pick\": null}")]);
    rig.ask(&runner, "tell", Some("first"));
    answered(step(&runner).unwrap());

    let standing = read(&rig, PM_STANDING);
    assert!(
        standing.contains("Hold the release notes (until #41)"),
        "an unanswered end keeps its line: {standing}"
    );
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("kelpie could not read #41")
                && n.ends_with("a standing line stands")),
        "{notes:?}"
    );
    let [wake] = pm_seen(&rig).try_into().unwrap();
    let prompt = &wake.call.prompt;
    assert!(
        prompt.contains(
            "standing unsettled: Hold the release notes (until #41): kelpie could not read #41"
        ),
        "{prompt}"
    );
    let log = read(&rig, "pm-notes.md");
    assert!(!log.contains("standing ended:"), "{log}");
}

#[test]
fn a_standing_line_the_board_cannot_answer_is_read_once_a_wake_through_the_ticket_port() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.block(9, 900);
    std::fs::create_dir_all(rig.paths().pm).unwrap();
    std::fs::write(
        rig.paths().pm.join(PM_STANDING),
        "Never merge a head with CI red (until #9)\nHold the release notes (until #41)\nEvery fix carries a changelog line, such as #42\n",
    )
    .unwrap();
    // #41 is open and off the board, so only its source can answer it. The gate
    // a read waits at is the tickets port's own, so a read reaching it says
    // which port asked.
    rig.claude.script([
        Scripted::Say("{\"pick\": null}"),
        Scripted::Say("{\"pick\": null}"),
    ]);
    rig.ask(&runner, "tell", Some("first"));
    let before = rig.forge.issue_reads();
    let held = rig.forge.hold_next_issue_read();
    let (reply, pass) = rig.asked_while_held(&runner, held, "detach", Some("7 1"));
    assert!(
        reply.is_some(),
        "the trigger waited for the lock the read held"
    );
    assert!(matches!(pass, Ok(Pass::Started)), "{pass:?}");
    answered(Some(step_until(&runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));

    assert_eq!(
        rig.forge.issue_reads(),
        before + 1,
        "one read for #41: the board answers #9, and #42 carries no end"
    );
    let standing = read(&rig, PM_STANDING);
    assert!(
        standing.contains("Hold the release notes (until #41)"),
        "#41 is open, so its line stands: {standing}"
    );

    // The line is read again on the next wake, and as often as the board cannot answer it
    rig.ask(&runner, "tell", Some("second"));
    answered(step(&runner).unwrap());
    assert_eq!(rig.forge.issue_reads(), before + 2);
    assert!(
        read(&rig, PM_STANDING).contains("Hold the release notes (until #41)"),
        "{standing}"
    );
    assert_eq!(pm_seen(&rig).len(), 2, "two wakes, two reads");
}

#[test]
fn a_standing_line_ended_by_a_closed_ticket_is_removed_at_the_next_wake() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    std::fs::create_dir_all(rig.paths().pm).unwrap();
    std::fs::write(
        rig.paths().pm.join(PM_STANDING),
        "Never merge a head with CI red (until #9)\nHold the release notes (until #12)\n",
    )
    .unwrap();
    // #9 closed, so the board no longer shows it. #12 stays on the ready list,
    // held off dispatch by a blocker, and a read of it would fail, so a line
    // named unsettled would say it had been read.
    rig.forge.close_issue(9);
    rig.forge.block(12, 900);
    rig.forge.remove_issue(12);

    rig.ask(&runner, "tell", Some("first"));
    rig.claude.script([Scripted::Say("{\"pick\": null}")]);
    let (_, acted, dropped) = answered(step(&runner).unwrap());
    assert_eq!(dropped, Vec::<String>::new());
    assert!(!acted.iter().any(|a| a.contains("standing")), "{acted:?}");

    let standing = read(&rig, PM_STANDING);
    assert!(
        !standing.contains("Never merge a head with CI red"),
        "its ticket closed, so the line is out of the file: {standing}"
    );
    assert!(
        standing.contains("Hold the release notes (until #12)"),
        "the board shows #12 open, so its line stands unread: {standing}"
    );

    let notes = read(&rig, "pm-notes.md");
    assert_eq!(notes.matches("standing ended:").count(), 1, "{notes}");
    assert!(
        notes.contains("standing ended: Never merge a head with CI red (until #9)"),
        "{notes}"
    );

    let [wake] = pm_seen(&rig).try_into().unwrap();
    let prompt = &wake.call.prompt;
    assert!(
        prompt.contains("standing ended: Never merge a head with CI red (until #9)"),
        "{prompt}"
    );
    assert!(!prompt.contains("standing unsettled"), "{prompt}");
}

#[test]
fn a_standing_line_ended_by_a_date_goes_at_the_first_wake_after_that_day() {
    let (rig, runner) = with_pm("koji");
    std::fs::create_dir_all(rig.paths().pm).unwrap();
    std::fs::write(
        rig.paths().pm.join(PM_STANDING),
        "Hold the release notes (until 2026-09-21)\nNever merge a head with CI red.\n",
    )
    .unwrap();
    rig.ask(&runner, "tell", Some("first"));
    rig.claude.script([
        Scripted::Say("{\"pick\": null}"),
        Scripted::Say("{\"pick\": null}"),
    ]);
    answered(step(&runner).unwrap());

    let standing = read(&rig, PM_STANDING);
    assert!(
        standing.contains("Hold the release notes (until 2026-09-21)"),
        "the line holds through the day it names: {standing}"
    );

    rig.clock.advance(24 * 60 * 60);
    rig.ask(&runner, "tell", Some("second"));
    answered(step(&runner).unwrap());

    let standing = read(&rig, PM_STANDING);
    assert!(
        !standing.contains("Hold the release notes"),
        "the first wake after the day it names ends it: {standing}"
    );
    assert!(
        standing.contains("Never merge a head with CI red."),
        "a line with no end stands: {standing}"
    );
    let notes = read(&rig, "pm-notes.md");
    assert!(
        notes.contains("standing ended: Hold the release notes (until 2026-09-21)"),
        "{notes}"
    );
    assert_eq!(notes.matches("standing ended:").count(), 1, "{notes}");
}

#[test]
fn a_new_session_is_given_the_standing_notes_inline_and_told_where_the_archive_is() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    std::fs::create_dir_all(rig.paths().pm).unwrap();
    std::fs::write(
        rig.paths().pm.join(PM_STANDING),
        "Hold #12 until the release.\nNever merge a head with CI red.\n",
    )
    .unwrap();
    rig.claude.script([Scripted::Say(PICK_12)]);
    answered(step(&runner).unwrap());

    let [wake] = pm_seen(&rig).try_into().unwrap();
    let prompt = &wake.call.prompt;
    assert!(
        prompt.contains("> Hold #12 until the release.\n> Never merge a head with CI red."),
        "{prompt}"
    );
    assert!(
        prompt.contains("`pm-notes.md`: your recent log"),
        "{prompt}"
    );
    assert!(
        prompt.contains("`pm-archive.md`: your older log"),
        "{prompt}"
    );
    assert!(prompt.contains("never read it whole"), "{prompt}");
    assert!(prompt.contains("\"standing\": null or"), "{prompt}");
}

#[test]
fn a_resumed_wake_sends_it_back_at_no_notes_file() {
    let (rig, runner) = with_pm("koji");
    rig.ask(&runner, "tell", Some("first"));
    rig.claude.script([
        Scripted::Say("{\"pick\": null}"),
        Scripted::Say("{\"pick\": null}"),
        Scripted::Say("{\"pick\": null}"),
    ]);
    answered(step(&runner).unwrap());
    rig.ask(&runner, "tell", Some("second"));
    answered(step(&runner).unwrap());

    let [_, second] = pm_seen(&rig).try_into().unwrap();
    let prompt = &second.call.prompt;
    assert!(
        prompt.starts_with("Kelpie woke you again because:"),
        "{prompt}"
    );
    for file in ["pm-standing.md", "pm-notes.md", "pm-archive.md"] {
        assert!(!prompt.contains(file), "{prompt}");
    }
}

#[test]
fn the_first_roll_forces_a_full_prompt_that_sends_it_to_the_archive() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say("{\"pick\": null}")]);
    answered(step(&runner).unwrap());
    assert!(
        !rig.paths().pm.join(PM_STANDING).exists(),
        "no roll has moved anything yet"
    );

    std::fs::write(rig.paths().pm.join(crate::ports::PM_NOTES), log_of(40)).unwrap();
    rig.ask(&runner, "tell", Some("Release is out"));
    let down = AgentError::Failed(Harness::ClaudeCode, "overloaded".into());
    rig.claude.script_for(Role::Pm, [Scripted::Fail(down)]);
    step_until(&runner, |r| matches!(r, StepReport::PmFailed { .. }));

    let [first, second] = pm_seen(&rig).try_into().unwrap();
    assert!(matches!(second.call.session, Session::Resume(_)));
    assert_ne!(first.call.session, second.call.session);
    let prompt = &second.call.prompt;
    assert!(
        prompt.starts_with("You are the project manager for koji."),
        "the wake after the first roll is a full one: {prompt}"
    );
    assert!(
        prompt.contains(
            "Kelpie moved the older part of `pm-notes.md` to the end of `pm-archive.md`."
        ),
        "{prompt}"
    );
    assert!(
        prompt.contains("Grep `pm-archive.md` for what the maintainer told you"),
        "{prompt}"
    );
    assert_eq!(
        read(&rig, PM_STANDING),
        "",
        "the standing notes start empty"
    );
    assert!(!read(&rig, PM_ARCHIVE).is_empty());
}

#[test]
fn a_standing_answer_replaces_the_file_and_null_leaves_it() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude.script_for(
        Role::Pm,
        [
            Scripted::Say(
                "{\"pick\": null, \"standing\": \"Hold #12 until the release.\\nNever merge a \
                 head with CI red.\"}",
            ),
            Scripted::Say("{\"pick\": null, \"standing\": null}"),
        ],
    );
    let (_, acted, dropped) = answered(step(&runner).unwrap());
    assert_eq!(dropped, Vec::<String>::new());
    assert!(
        acted
            .iter()
            .any(|a| a.starts_with("set the standing notes")),
        "{acted:?}"
    );
    let held = "Hold #12 until the release.\nNever merge a head with CI red.\n";
    assert_eq!(read(&rig, PM_STANDING), held, "the file ends a line");

    rig.ask(&runner, "tell", Some("Release is out"));
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, _) = answered(Some(report));
    assert!(!acted.iter().any(|a| a.contains("standing")), "{acted:?}");
    let standing = read(&rig, PM_STANDING);
    assert!(
        standing.starts_with(held) && standing.ends_with("\nRelease is out\n"),
        "{standing}"
    );
    assert!(
        standing.contains("CI red.\n## "),
        "the dated tell starts on its own line: {standing}"
    );
}

#[test]
fn a_standing_answer_over_4_kib_is_dropped_and_named() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    std::fs::create_dir_all(rig.paths().pm).unwrap();
    std::fs::write(
        rig.paths().pm.join(PM_STANDING),
        "Hold #12 until the release.\n",
    )
    .unwrap();

    let long = "x".repeat(STANDING_MAX + 1);
    let said = format!("{{\"pick\": null, \"standing\": \"{long}\"}}");
    let said: &'static str = Box::leak(said.into_boxed_str());
    rig.claude.script([Scripted::Say(said)]);

    let (_, acted, dropped) = answered(step(&runner).unwrap());
    assert_eq!(
        dropped,
        ["`standing`: 4098 bytes, over its 4 KiB limit, so pm-standing.md stays as it is"]
    );
    assert!(!acted.iter().any(|a| a.contains("standing")), "{acted:?}");
    assert_eq!(
        read(&rig, PM_STANDING),
        "Hold #12 until the release.\n",
        "the file stays as it was"
    );
}

#[test]
fn a_dropped_standing_keeps_its_tells_and_the_next_wake_asks_again() {
    let (rig, runner) = with_pm("koji");
    rig.ask(&runner, "tell", Some("Hold #12 until the release"));
    let long = "x".repeat(STANDING_MAX + 1);
    let said = format!("{{\"pick\": null, \"standing\": \"{long}\"}}");
    let said: &'static str = Box::leak(said.into_boxed_str());
    rig.claude
        .script([Scripted::Say(said), Scripted::Say("{\"pick\": null}")]);
    answered(step(&runner).unwrap());
    assert_eq!(
        read(&rig, PM_STANDING),
        "## 2026-09-21 14:13\nHold #12 until the release\n",
        "the tell is kept though its `standing` was not"
    );

    rig.ask(&runner, "tell", Some("Release is out"));
    answered(step(&runner).unwrap());
    let [_, next] = pm_seen(&rig).try_into().unwrap();
    let prompt = &next.call.prompt;
    assert!(
        prompt.contains(
            "kelpie did not save your `standing`: it was 4098 bytes, over its 4 KiB limit, \
             so pm-standing.md stays as it was. Condense it, and give the whole shorter file \
             in `standing`."
        ),
        "{prompt}"
    );
    assert!(prompt.contains("\"standing\": null or"), "{prompt}");
}

#[test]
fn a_tell_is_appended_to_the_standing_notes_once_on_the_wake_that_answers_it() {
    let (rig, runner) = with_pm("xilriws");
    rig.ask(&runner, "tell", Some("Hold #12 until the release"));
    rig.claude.script([
        Scripted::Say("{\"pick\": null}"),
        Scripted::Say("{\"pick\": null}"),
    ]);
    answered(step(&runner).unwrap());

    let standing = read(&rig, PM_STANDING);
    assert_eq!(
        standing, "## 2026-09-21 14:13\nHold #12 until the release\n",
        "dated, and verbatim"
    );

    // A wake that carried no tell adds nothing, and the answered tell is gone
    // from the state file, so it is never added again.
    rig.ask(&runner, "tell", Some("Release is out"));
    answered(step(&runner).unwrap());
    let after = read(&rig, PM_STANDING);
    assert_eq!(
        after.matches("Hold #12 until the release").count(),
        1,
        "{after}"
    );
    assert!(after.contains("Release is out\n"), "{after}");
}

#[test]
fn standing_notes_past_their_limit_are_condensed_on_the_next_wake() {
    let (rig, runner) = with_pm("koji");
    std::fs::create_dir_all(rig.paths().pm).unwrap();
    std::fs::write(
        rig.paths().pm.join(PM_STANDING),
        "y".repeat(STANDING_MAX + 1),
    )
    .unwrap();
    rig.ask(&runner, "tell", Some("first"));
    rig.claude.script([
        Scripted::Say("{\"pick\": null}"),
        Scripted::Say("{\"pick\": null}"),
    ]);
    answered(step(&runner).unwrap());
    let [first] = pm_seen(&rig).try_into().unwrap();
    assert!(
        first.call.prompt.contains(CONDENSE),
        "{}",
        first.call.prompt
    );

    rig.ask(&runner, "tell", Some("second"));
    answered(step(&runner).unwrap());
    let [_, second] = pm_seen(&rig).try_into().unwrap();
    let prompt = &second.call.prompt;
    assert!(
        prompt.starts_with("Kelpie woke you again because:"),
        "{prompt}"
    );
    assert!(prompt.contains(CONDENSE), "{prompt}");
    assert!(prompt.contains("\"standing\": null or"), "{prompt}");
}

#[test]
fn a_tell_answered_before_the_first_roll_does_not_hide_the_first_roll() {
    let (rig, runner) = with_pm("koji");
    rig.ask(&runner, "tell", Some("Never merge a head with CI red"));
    rig.claude.script([
        Scripted::Say("{\"pick\": null}"),
        Scripted::Say("{\"pick\": null}"),
    ]);
    answered(step(&runner).unwrap());
    assert!(
        read(&rig, PM_STANDING).contains("Never merge a head with CI red"),
        "the answered tell created the standing notes, which is no proof that nothing has \
         been rolled away yet"
    );

    std::fs::write(rig.paths().pm.join(crate::ports::PM_NOTES), log_of(40)).unwrap();
    rig.ask(&runner, "tell", Some("Release is out"));
    answered(step(&runner).unwrap());

    let [_, second] = pm_seen(&rig).try_into().unwrap();
    let prompt = &second.call.prompt;
    assert!(
        prompt.starts_with("You are the project manager for koji."),
        "the wake that rolls for the first time is still a full one: {prompt}"
    );
    assert!(
        prompt.contains("Grep `pm-archive.md` for what the maintainer told you"),
        "{prompt}"
    );
    assert!(!read(&rig, PM_ARCHIVE).is_empty());
    let standing = read(&rig, PM_STANDING);
    assert!(
        standing.contains("Never merge a head with CI red"),
        "what a tell already kept stays kept: {standing}"
    );
}

#[test]
fn an_empty_standing_answer_clears_the_file() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    std::fs::create_dir_all(rig.paths().pm).unwrap();
    std::fs::write(
        rig.paths().pm.join(PM_STANDING),
        "Hold #12 until the release.\n",
    )
    .unwrap();
    rig.claude
        .script([Scripted::Say("{\"pick\": null, \"standing\": \"\"}")]);

    let (_, acted, dropped) = answered(step(&runner).unwrap());
    assert_eq!(dropped, Vec::<String>::new());
    assert!(
        acted.iter().any(|a| a == "set the standing notes, 0 bytes"),
        "{acted:?}"
    );
    assert_eq!(
        read(&rig, PM_STANDING),
        "",
        "every standing decision had become obsolete"
    );
}

#[test]
fn a_standing_answer_that_carried_the_tell_is_not_written_twice() {
    let (rig, runner) = with_pm("xilriws");
    rig.ask(&runner, "tell", Some("Hold #12 until the release"));
    rig.claude.script([Scripted::Say(
        "{\"pick\": null, \"standing\": \"Hold #12 until the release\"}",
    )]);

    let (_, acted, dropped) = answered(step(&runner).unwrap());
    assert_eq!(dropped, Vec::<String>::new());
    assert!(
        acted
            .iter()
            .any(|a| a.starts_with("set the standing notes")),
        "{acted:?}"
    );
    assert_eq!(
        read(&rig, PM_STANDING),
        "Hold #12 until the release\n",
        "the answer wrote the file, and kelpie added the tell it carried to it once"
    );
}

#[test]
fn a_wake_that_rolled_and_then_failed_still_asks_for_the_archive() {
    let (rig, runner) = with_pm("xilriws");
    rig.ask(&runner, "tell", Some("first"));
    rig.claude.script([Scripted::Say("{\"pick\": null}")]);
    answered(step(&runner).unwrap());

    // The wake that rolls for the first time answers, and gives no `standing`,
    // so the archive still holds notes nothing has read
    std::fs::write(rig.paths().pm.join(crate::ports::PM_NOTES), log_of(40)).unwrap();
    rig.ask(&runner, "tell", Some("Release is out"));
    rig.claude.script([Scripted::Say("{\"pick\": null}")]);
    answered(step(&runner).unwrap());
    assert!(!read(&rig, PM_ARCHIVE).is_empty(), "the log rolled");

    // The next wake fails, and its retry resumes the same session
    rig.ask(&runner, "tell", Some("Never merge a head with CI red"));
    let down = AgentError::Failed(Harness::ClaudeCode, "overloaded".into());
    rig.claude.script([Scripted::Fail(down)]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::PmFailed { .. })
    ));

    rig.clock.advance(super::super::DOWN_FOR + 1);
    rig.claude.script([Scripted::Say("{\"pick\": null}")]);
    answered(step(&runner).unwrap());

    let [_, _, failed, retry] = pm_seen(&rig).try_into().unwrap();
    assert!(matches!(failed.call.session, Session::Resume(_)));
    assert!(matches!(retry.call.session, Session::Resume(_)));
    let prompt = &retry.call.prompt;
    assert!(
        prompt.starts_with("Kelpie woke you again because:"),
        "the retry is a resumed wake: {prompt}"
    );
    assert!(
        prompt.contains("Grep `pm-archive.md` for what the maintainer told you"),
        "the retry still owes the archive: {prompt}"
    );
    assert!(prompt.contains("\"standing\": null or"), "{prompt}");
}
