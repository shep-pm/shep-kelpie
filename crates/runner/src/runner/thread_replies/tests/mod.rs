use std::sync::Mutex;

use crate::ports::{Clock, ForgeError, Timestamp};
use crate::runner::{BOARD_POLL, CHECKS_SETTLE, Runner, step};
use crate::state::{Heard, Ruling, RulingKind, Stuck};
use crate::test::ticket;
use crate::test::{Rig, Scripted, VIEWER};
use crate::ticket::TicketId;

mod marks;
mod settled;
mod triage_answer;

const ASKS: &str = "<kelpie-question>\nWhich flag?\n</kelpie-question>\n";

// A merge ruling for pull request 71 is up through the App, which mentions
// `octocat`.
fn a_merge_ruling_on_71_for(octocat_writes: bool) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = Rig::parked_set("shep", |rig| {
        rig.with_app();
        rig.maintainer("octocat");
    });
    if octocat_writes {
        rig.forge.threads.grant_write("octocat");
    }
    // The ruling went up some seconds ago.
    rig.clock.advance(5);
    (rig, runner, head)
}

// ... and octocat has write access to the repo.
fn a_merge_ruling_on_71() -> (Rig, Mutex<Runner>, String) {
    a_merge_ruling_on_71_for(true)
}

// The ids of the rulings that wait.
fn pending(rig: &Rig, runner: &Mutex<Runner>) -> Vec<TicketId> {
    let status = rig.ask(runner, "status", None);
    let rulings = status["rulings"].as_array().unwrap();
    rulings
        .iter()
        .map(|r| r["id"].as_str().unwrap().parse().unwrap())
        .collect()
}

// What kelpie posted on thread 71 after the ruling's own comment.
fn replies_on_71(rig: &Rig) -> Vec<String> {
    let comments = rig.github.comments();
    let after = comments
        .iter()
        .position(|(_, body)| body.starts_with("### Merge"));
    let comments = &comments[after.unwrap() + 1..];
    comments
        .iter()
        .map(|(thread, body)| {
            assert_eq!(*thread, 71);
            body.clone()
        })
        .collect()
}

// The ids of the rulings that wait, as saved: `status` shows a ruling's number.
fn saved_ids(runner: &Mutex<Runner>) -> Vec<TicketId> {
    let runner = runner.lock().unwrap();
    runner.state.rulings.iter().map(|r| r.id).collect()
}

fn none() -> Vec<TicketId> {
    Vec::new()
}

#[test]
fn a_yes_naming_the_ruling_from_the_login_it_mentions_merges_the_pull_request() {
    let (rig, runner, head) = a_merge_ruling_on_71();
    let comment = rig.writes_on(71, "octocat", "71 yes");

    rig.next_look();
    step(&runner).unwrap(); // takes the reply, and marks the pull request ready
    rig.clock.advance(CHECKS_SETTLE);
    step(&runner).unwrap(); // merges

    assert_eq!(rig.forge.merges(), [(71, head)]);
    assert_eq!(rig.github.reactions(), [comment], "kelpie took the reply");
}

#[test]
fn the_number_of_the_pull_request_or_its_issue_names_the_ruling_too() {
    for named in ["71 yes", "#71 yes", "7 yes"] {
        let (rig, runner, head) = a_merge_ruling_on_71();
        let comment = rig.writes_on(71, "octocat", named);

        rig.next_look();
        step(&runner).unwrap();
        rig.clock.advance(CHECKS_SETTLE);
        step(&runner).unwrap();

        assert_eq!(rig.forge.merges(), [(71, head)], "{named}");
        assert_eq!(rig.github.reactions(), [comment], "{named}");
    }
}

#[test]
fn a_reply_by_number_from_before_the_ruling_was_raised_is_told_it_changed() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    let written = rig.clock.now();
    rig.writes_on(71, "octocat", "71 yes");
    {
        // The ruling was raised again after the comment was written.
        let mut runner = runner.lock().unwrap();
        let mut next = runner.state.clone();
        next.rulings[0].raised = Some(Timestamp(written.0 + 1));
        runner.save(next).unwrap();
    }

    rig.next_look();
    step(&runner).unwrap();

    let [told] = replies_on_71(&rig).try_into().unwrap();
    assert!(
        told.starts_with("@octocat Ruling #71 was not answered: the ruling changed since that comment was written, and it asks now:"),
        "{told}"
    );
    assert!(rig.forge.merges().is_empty());
    assert!(rig.github.reactions().is_empty());
    assert_eq!(pending(&rig, &runner), [ticket(71)]);
}

#[test]
fn a_bare_yes_answers_nothing_so_a_conversational_yes_never_merges() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    rig.writes_on(71, "octocat", "yes");

    rig.next_look();
    step(&runner).unwrap();
    rig.clock.advance(CHECKS_SETTLE);
    step(&runner).unwrap();

    assert_eq!(rig.forge.merges(), []);
    assert_eq!(pending(&rig, &runner), [ticket(71)]);
    assert!(rig.github.reactions().is_empty());
    let told = "@octocat Ruling #71 was not answered: start your reply with its number, as `71 yes`, \
                `71 no <note>` or `71 rework <note>`. Reply again in this thread, starting \
                with `71`.";
    assert_eq!(replies_on_71(&rig), [told]);
}

// Only a merge ruling's thread has a worker to read a plain comment, so on
// any other ruling it is told how to reply, as before.
#[test]
fn a_plain_comment_under_a_ruling_other_than_a_merge_is_told_how_to_reply() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    {
        let mut runner = runner.lock().unwrap();
        let mut next = runner.state.clone();
        next.rulings[0].kind = RulingKind::from(Stuck::Closed);
        runner.save(next).unwrap();
    }
    rig.writes_on(71, "octocat", "go ahead and retry");

    rig.next_look();
    step(&runner).unwrap();

    let [told] = replies_on_71(&rig).try_into().unwrap();
    assert!(
        told.starts_with("@octocat Ruling #71 was not answered: start your reply with its number"),
        "{told}"
    );
    assert_eq!(pending(&rig, &runner), [ticket(71)]);
}

#[test]
fn a_reply_from_a_login_the_ruling_does_not_mention_changes_nothing_whatever_it_may_write() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    rig.forge.threads.grant_write("stranger");
    rig.writes_on(71, "stranger", "71 yes");

    rig.next_poll(&runner);
    step(&runner).unwrap();
    rig.clock.advance(CHECKS_SETTLE);
    step(&runner).unwrap();

    assert_eq!(rig.forge.merges(), []);
    assert_eq!(pending(&rig, &runner), [ticket(71)]);
    assert!(rig.github.reactions().is_empty(), "no reaction");
    assert_eq!(replies_on_71(&rig), Vec::<String>::new(), "no answer");
    assert_eq!(rig.forge.threads.asked(), Vec::<String>::new());
}

#[test]
fn a_reply_from_a_login_without_write_access_changes_nothing() {
    let (rig, runner, _) = a_merge_ruling_on_71_for(false);
    rig.writes_on(71, "octocat", "71 yes");

    rig.next_look();
    step(&runner).unwrap();
    rig.clock.advance(CHECKS_SETTLE);
    step(&runner).unwrap();

    assert_eq!(rig.forge.merges(), []);
    assert_eq!(pending(&rig, &runner), [ticket(71)]);
    assert!(rig.github.reactions().is_empty(), "no reaction");
    assert_eq!(replies_on_71(&rig), Vec::<String>::new(), "no answer");
    assert_eq!(
        rig.forge.threads.asked(),
        ["octocat"],
        "its access was asked"
    );
}

#[test]
fn a_bots_comment_is_left_alone_without_asking_what_it_may_do() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    let now = rig.clock.now();
    rig.forge
        .threads
        .bot_writes(71, "coderabbitai[bot]", "71 yes", now);

    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(pending(&rig, &runner), [ticket(71)]);
    assert_eq!(rig.forge.threads.asked(), Vec::<String>::new());
    assert_eq!(replies_on_71(&rig), Vec::<String>::new());
}

// The question before any pull request is open: its thread is issue 7.
fn a_question_on_7_mentioning(login: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.maintainer(login);
    rig.forge.threads.grant_write(login);
    // Issue 7 exists on the forge, so a comment on it can be posted.
    rig.forge.list_ready(7, false);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say(ASKS)]);
    step(&runner).unwrap();
    assert_eq!(pending(&rig, &runner), [ticket(7)]);
    rig.clock.advance(5);
    (rig, runner)
}

#[test]
fn a_comment_from_before_the_ruling_is_not_an_answer() {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.maintainer("octocat");
    rig.forge.threads.grant_write("octocat");
    let before = rig.clock.now();
    rig.clock.advance(60);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.threads.write(7, "octocat", "use --check", before);
    rig.claude.script([Scripted::Say(ASKS)]);
    step(&runner).unwrap();
    assert_eq!(pending(&rig, &runner), [ticket(7)]);

    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(
        pending(&rig, &runner),
        [ticket(7)],
        "the old comment answered nothing"
    );
    assert!(rig.github.reactions().is_empty());
}

#[test]
fn a_reply_the_forge_lists_late_is_still_answered_after_a_newer_comment_elsewhere() {
    let (rig, runner, head) = a_merge_ruling_on_71();
    let written = rig.clock.now();
    rig.forge
        .threads
        .write(9, "octocat", "unrelated", Timestamp(written.0 + 1));
    rig.next_look();
    step(&runner).unwrap(); // reads only the comment on #9

    // The forge lists the reply now, though it was written before that one.
    let comment = rig.forge.threads.write(71, "octocat", "71 yes", written);
    rig.next_look();
    step(&runner).unwrap(); // takes the reply, and marks the pull request ready
    rig.clock.advance(CHECKS_SETTLE);
    step(&runner).unwrap(); // merges

    assert_eq!(rig.forge.merges(), [(71, head)]);
    assert_eq!(rig.github.reactions(), [comment]);
}

#[test]
fn a_reply_acted_on_one_thread_never_drops_a_reply_the_forge_lists_late_on_another() {
    let (rig, runner, head) = a_merge_ruling_on_71();
    let written = rig.clock.now();
    with_a_second_ruling_on(&runner, &rig, 72);
    // A reply on #72 that kelpie acts on, written after the one on #71.
    rig.forge
        .threads
        .write(72, "octocat", "2 maybe", Timestamp(written.0 + 1));
    rig.next_look();
    step(&runner).unwrap();
    let told = rig.github.comments();
    assert!(
        told.iter()
            .any(|(thread, body)| *thread == 72 && body.contains("Ruling #72 was not answered")),
        "{told:?}"
    );

    // The forge lists the earlier reply on #71 only now.
    let comment = rig.forge.threads.write(71, "octocat", "71 yes", written);
    rig.next_look();
    step(&runner).unwrap();
    rig.clock.advance(CHECKS_SETTLE);
    step(&runner).unwrap();

    assert_eq!(rig.forge.merges(), [(71, head)]);
    assert_eq!(rig.github.reactions(), [comment]);
}

#[test]
fn an_unreadable_reply_gets_one_reply_saying_what_kelpie_expects_and_the_ruling_stays_open() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    rig.writes_on(71, "octocat", "71 maybe");

    rig.next_look();
    step(&runner).unwrap();

    let told = "@octocat Ruling #71 was not answered: it takes `yes`, `no <note>` or `rework \
                <note>`. Reply again in this thread, starting with `71`.";
    assert_eq!(replies_on_71(&rig), [told]);
    assert_eq!(pending(&rig, &runner), [ticket(71)]);
    assert!(rig.github.reactions().is_empty(), "nothing was taken");

    // The same comment is not read again, however long the ruling waits.
    for _ in 0..3 {
        rig.next_look();
        step(&runner).unwrap();
    }
    assert_eq!(replies_on_71(&rig).len(), 1, "told once");
}

#[test]
fn a_no_with_a_note_goes_to_the_worker_and_a_question_takes_any_text() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    let comment = rig.writes_on(71, "octocat", "71 no please add a test");
    rig.claude.script([Scripted::Say(ASKS)]);

    rig.next_look();
    step(&runner).unwrap(); // takes the no; the worker's fix turn asks

    assert_eq!(
        pending(&rig, &runner),
        [ticket(71)],
        "the worker asked in its place"
    );
    assert_eq!(rig.github.reactions(), [comment]);
    rig.clock.advance(1);
    let answer = rig.writes_on(71, "octocat", "use --check");
    rig.claude.script([Scripted::Say("Done.")]);
    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(pending(&rig, &runner), none());
    assert_eq!(rig.github.reactions(), [comment, answer]);
}

// A question takes any text, so a number that opens it is part of the answer.
#[test]
fn a_number_opening_a_question_s_answer_is_not_taken_for_its_name() {
    let (rig, runner) = a_question_on_7_mentioning("octocat");
    let answer = "7 is the wrong issue, patch 8 instead";
    let comment = rig.writes_on(7, "octocat", answer);
    rig.claude.script([Scripted::Say("Done.")]);

    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(pending(&rig, &runner), none());
    assert_eq!(rig.github.reactions(), [comment]);
    let calls = rig.claude.calls();
    let resumed = &calls.last().unwrap().prompt;
    assert!(resumed.contains(answer), "{resumed}");
}

#[test]
fn a_question_s_answer_that_opens_with_pm_is_its_answer() {
    let (rig, runner) = a_question_on_7_mentioning("octocat");
    let answer = "PM is right, keep the old flag";
    let comment = rig.writes_on(7, "octocat", answer);
    rig.claude.script([Scripted::Say("Done.")]);

    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(pending(&rig, &runner), none());
    assert_eq!(rig.github.reactions(), [comment]);
    let calls = rig.claude.calls();
    let resumed = &calls.last().unwrap().prompt;
    assert!(resumed.contains(answer), "{resumed}");
}

#[test]
fn comment_reads_stay_within_one_per_board_poll_however_often_the_loop_wakes() {
    let (rig, runner, _) = a_merge_ruling_on_71();

    for _ in 0..5 {
        step(&runner).unwrap();
    }
    assert_eq!(rig.forge.threads.reads().len(), 1);

    rig.clock.advance(BOARD_POLL.as_secs() - 1);
    for _ in 0..5 {
        step(&runner).unwrap();
    }
    assert_eq!(
        rig.forge.threads.reads().len(),
        1,
        "a second short of the poll"
    );

    rig.clock.advance(1);
    for _ in 0..5 {
        step(&runner).unwrap();
    }
    assert_eq!(rig.forge.threads.reads().len(), 2, "a poll on");
}

#[test]
fn no_comments_are_read_where_no_ruling_waits_or_no_app_covers_the_repo() {
    let rig = Rig::new("shep");
    rig.with_app();
    let runner = rig.open().unwrap();
    rig.next_look();
    step(&runner).unwrap();
    assert_eq!(rig.forge.threads.reads().len(), 0, "no ruling waits");

    let (rig, runner, _) = Rig::parked("shep");
    rig.next_look();
    step(&runner).unwrap();
    assert_eq!(rig.forge.threads.reads().len(), 0, "no App covers the repo");
}

#[test]
fn a_ruling_that_mentions_no_one_is_answered_at_the_terminal_only() {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.forge.threads.grant_write(VIEWER);
    rig.forge
        .set_viewer_down(ForgeError::Failed("not logged in".into()));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say(ASKS)]);
    step(&runner).unwrap();
    rig.clock.advance(5);
    rig.writes_on(7, VIEWER, "use --check");

    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(
        pending(&rig, &runner),
        [ticket(7)],
        "a reply answers nothing"
    );
    assert_eq!(rig.forge.threads.reads().len(), 0, "and is not even read");
    let comments = rig.github.comments();
    let how = &comments[0].1;
    assert!(
        how.ends_with(
            "This ruling mentions no one, so a reaction, a tick or a reply here answers \
             nothing. Answer it at the terminal: `shep kelpie rule 7 <your answer>`."
        ),
        "{how}"
    );
    rig.ask(&runner, "rule", Some("7 answer use --check"));
    assert_eq!(pending(&rig, &runner), none());
}

#[test]
fn a_comment_kelpie_posted_as_the_maintainers_login_is_never_an_answer() {
    let (rig, runner) = a_question_on_7_mentioning(VIEWER);
    // A summons, as `gh` posts it: from the login that answers the ruling.
    runner
        .lock()
        .unwrap()
        .comment_as_gh(7, "@codex review")
        .unwrap();

    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(
        pending(&rig, &runner),
        [ticket(7)],
        "the summons answered nothing"
    );
    assert!(rig.github.reactions().is_empty());

    // The maintainer's own words, from the same login, are.
    rig.clock.advance(1);
    let answer = rig.writes_on(7, VIEWER, "use --check");
    rig.claude.script([Scripted::Say("Done.")]);
    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(pending(&rig, &runner), none());
    assert_eq!(rig.github.reactions(), [answer]);
}

#[test]
fn a_ruling_answered_at_the_terminal_first_is_told_the_reply_came_too_late() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    rig.writes_on(71, "octocat", "71 yes");
    rig.ask(&runner, "rule", Some("71 no not yet"));
    rig.claude.script([Scripted::Say(ASKS)]);

    rig.next_look();
    step(&runner).unwrap();

    assert!(rig.github.reactions().is_empty());
    // The worker's question, ruling 71 again, went up on the thread too.
    let told = "@octocat Ruling #71 is already answered at the terminal with `shep kelpie rule`: \
                no, not yet.";
    assert!(replies_on_71(&rig).contains(&told.to_owned()));
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn a_ruling_answered_at_the_terminal_between_the_read_and_applying_it_is_not_answered_twice() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    rig.writes_on(71, "octocat", "71 yes");
    rig.next_look();

    // The read as `read_threads` makes it, the forge asked with the lock let
    // go, and the terminal answering the ruling before the read is applied.
    let (now, job) = runner.lock().unwrap().thread_job().expect("a read is due");
    let read = job.run(now);
    rig.ask(&runner, "rule", Some("71 no not yet"));
    runner.lock().unwrap().apply_threads(read).unwrap();

    assert!(rig.github.reactions().is_empty(), "the reply took nothing");
    assert_eq!(rig.forge.merges(), []);
    assert_eq!(pending(&rig, &runner), none());
    let told = "@octocat Ruling #71 is already answered at the terminal with `shep kelpie rule`: \
                no, not yet.";
    assert_eq!(replies_on_71(&rig), [told]);
    // The terminal's no is the worker's next turn, once.
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["turn"]["state"],
        "next"
    );
}

#[test]
fn a_summons_recorded_before_a_restart_is_still_skipped_after_it() {
    let (rig, runner) = a_question_on_7_mentioning(VIEWER);
    runner
        .lock()
        .unwrap()
        .comment_as_gh(7, "@codex review")
        .unwrap();
    // Nothing else is saved before the runner stops.
    drop(runner);

    let runner = rig.open().unwrap();
    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(
        pending(&rig, &runner),
        [ticket(7)],
        "the summons answered nothing"
    );
    assert!(rig.github.reactions().is_empty());
    assert_eq!(rig.forge.threads.reads().len(), 1, "it was read");
}

#[test]
fn a_ruling_answered_in_its_thread_is_already_answered_at_the_terminal() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    rig.writes_on(71, "octocat", "71 no not yet");
    rig.claude.script([Scripted::Say("Fixed.")]);
    rig.next_look();
    step(&runner).unwrap();

    let told = rig.ask(&runner, "rule", Some("71 yes"));

    assert_eq!(
        told["error"].as_str().unwrap(),
        "ruling 71 is already answered, or was never raised"
    );
}

// A second pending ruling on thread `thread`, mentioning octocat too, as an
// older kelpie or a second source would leave it.
fn with_a_second_ruling_on(runner: &Mutex<Runner>, rig: &Rig, thread: u64) {
    let mut runner = runner.lock().unwrap();
    let mut next = runner.state.clone();
    next.rulings.push(Ruling {
        issue: Some(ticket(8)),
        pull_request: Some(thread),
        alerted: true,
        ..Ruling::new(ticket(8), "Is it closed?", RulingKind::from(Stuck::Closed))
    });
    next.heard.insert(
        ticket(8),
        Heard {
            mentions: "octocat".into(),
            time: rig.clock.now(),
            comment: 0,
            ruling_comment: 0,
        },
    );
    runner.save(next).unwrap();
}

#[test]
fn with_several_rulings_on_one_thread_a_reply_names_the_one_it_answers() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    with_a_second_ruling_on(&runner, &rig, 71);
    rig.clock.advance(1);
    rig.writes_on(71, "octocat", "yes");

    rig.next_look();
    step(&runner).unwrap();

    let told = "@octocat Rulings 8, 71 wait on this thread, so start your reply with the id of \
                the one you answer, such as `8 yes`.";
    assert_eq!(replies_on_71(&rig), [told]);
    assert_eq!(saved_ids(&runner), [ticket(71), ticket(8)]);
    assert!(rig.github.reactions().is_empty());

    rig.clock.advance(1);
    let named = rig.writes_on(71, "octocat", "8 yes");
    rig.claude.script([Scripted::Say("Done.")]);
    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(rig.github.reactions(), [named]);
    assert_eq!(saved_ids(&runner), [ticket(71)], "ruling 8 was answered");
}

#[test]
fn a_ruling_with_no_place_read_to_in_its_thread_is_answered_at_the_terminal_only() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    {
        // As a file from before replies in a thread leaves it.
        let mut runner = runner.lock().unwrap();
        let mut next = runner.state.clone();
        next.heard.clear();
        runner.save(next).unwrap();
    }
    rig.writes_on(71, "octocat", "71 yes");

    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(pending(&rig, &runner), [ticket(71)]);
    assert_eq!(rig.forge.threads.reads().len(), 0);
    assert!(rig.github.reactions().is_empty());
}

#[test]
fn a_no_note_is_read_in_any_case_and_a_bare_no_is_none() {
    assert_eq!(
        super::no_note(" No use B instead "),
        Some("use B instead".into())
    );
    assert_eq!(super::no_note("no"), None);
    assert_eq!(super::no_note("no   "), None);
    assert_eq!(super::no_note("nope, use B"), None);
    assert_eq!(super::no_note("yes"), None);
}
