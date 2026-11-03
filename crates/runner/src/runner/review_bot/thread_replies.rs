//! The replies kelpie posts on the threads of a bot whose findings went to
//! a fix turn, through the runner's stand-ins

use std::sync::Mutex;

use crate::ports::{CLEAN, Checks};
use crate::runner::coderabbit::tests::now;
use crate::runner::review_bot::two_bots::{bot_reviewed, listing, reviewed, summoned};
use crate::runner::{Runner, StepReport, step};
use crate::state::RulingKind;
use crate::test::ticket;
use crate::test::{Rig, Scripted};

const FINDING: &str = "P2: `bleats` loses a stamped prefix. Strip only files shep stamped.";
const NIT: &str = "P3: The name could be shorter.";
const THREAD: &str = "PRRT_cubic_71_0";

// cubic, last after qwen and Claude, reads the head and leaves `threads`,
// which go to the worker's fix turn, the next step.
pub(in crate::runner) fn sent(threads: &[&str]) -> (Rig, Mutex<Runner>, String) {
    sent_in(listing("shep", &["cubic"]), threads)
}

fn sent_in(rig: Rig, threads: &[&str]) -> (Rig, Mutex<Runner>, String) {
    let (runner, head) = reviewed(&rig);
    step(&runner).unwrap(); // marks the draft ready
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    rig.forge
        .coderabbit
        .cubic_review(71, &head, now(&rig) + 300, threads);
    rig.clock.advance(300);
    assert_eq!(
        rig.threads_read(&runner),
        bot_reviewed(3, "cubic", threads.len())
    );
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent { .. })
    ));
    (rig, runner, head)
}

// Where kelpie's App covers the repo, the reply and the resolve are the
// App's, made with its token, not the maintainer's login through `gh`.
#[test]
fn where_the_app_covers_the_repo_the_replies_and_resolves_are_the_apps() {
    let rig = listing("shep", &["cubic"]);
    rig.with_app();
    let (rig, runner, _) = sent_in(rig, &[FINDING]);
    rig.claude.script([Scripted::PushSay(
        "strip.txt",
        "stripped\n",
        "thread 1: fixed",
    )]);
    step(&runner).unwrap(); // the worker's fix turn
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));

    assert_eq!(
        rig.forge.coderabbit.replies(),
        [(THREAD.to_owned(), "Fixed.".to_owned())]
    );
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
    let made = rig.github.thread_writes();
    assert_eq!(made.len(), 2, "{made:?}");
    for (token, _) in &made {
        assert!(token.starts_with("ghs_test"), "{token}");
    }
    assert_eq!(
        rig.forge.coderabbit.gh_writes(),
        0,
        "nothing went through gh as the maintainer"
    );
}

// With no App the threads are answered through `gh`, as before.
#[test]
fn with_no_app_the_replies_and_resolves_go_through_gh() {
    let (rig, runner, _) = sent(&[FINDING]);
    rig.claude.script([Scripted::PushSay(
        "strip.txt",
        "stripped\n",
        "thread 1: fixed",
    )]);
    step(&runner).unwrap(); // the worker's fix turn
    step(&runner).unwrap(); // the thread is answered

    assert_eq!(rig.forge.coderabbit.gh_writes(), 2);
    assert_eq!(rig.github.thread_writes(), []);
}

#[test]
fn a_thread_the_workers_reply_leaves_out_is_replied_to_as_not_addressed_and_stays_open() {
    let (rig, runner, head) = sent(&[FINDING]);
    rig.claude
        .script([Scripted::Push("strip.txt", "stripped\n")]);
    step(&runner).unwrap(); // the worker's fix turn, which says nothing of the thread
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    assert_eq!(
        rig.forge.coderabbit.replies(),
        [(
            THREAD.to_owned(),
            "Not addressed: the worker's reply did not mention this thread, so it stays open."
                .to_owned()
        )]
    );
    assert_eq!(rig.forge.coderabbit.resolved(), Vec::<String>::new());

    // Open on the forge and sent to no fix turn, the merge ruling counts it.
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_ne!(fixed, head);
    rig.forge.set_checks(&fixed, Checks::Passed);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(&runner) else {
        panic!("no merge ruling");
    };
    assert!(
        question.contains("cubic left 1 P2 thread open."),
        "{question}"
    );
}

// Under `auto` a yes merges past the threads that held the ruling, so a
// catch-up with `main` that withdraws the yes does not ask about them again.
#[test]
fn a_yes_merges_past_its_open_threads_after_main_moves() {
    yes_after_main_moves(false);
}

// A ruling a version 24 file left pending names its threads and holds no
// ids, so the yes asks the forge which hold it.
#[test]
fn a_yes_to_a_ruling_kept_without_ids_merges_past_its_open_threads_too() {
    yes_after_main_moves(true);
}

fn yes_after_main_moves(without_ids: bool) {
    let rig = listing("shep", &["cubic"]);
    rig.merge_auto();
    let (rig, runner, _) = sent_in(rig, &[FINDING]);
    rig.claude
        .script([Scripted::Push("strip.txt", "stripped\n")]);
    step(&runner).unwrap(); // the worker's fix turn, which leaves the thread open
    step(&runner).unwrap(); // the thread is replied to as not addressed
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&fixed, Checks::Passed);
    let Some(StepReport::Ruling { id, question, .. }) = rig.verdict(&runner) else {
        panic!("no merge ruling");
    };
    assert!(
        question.contains("cubic left 1 P2 thread open"),
        "{question}"
    );
    if without_ids {
        let mut runner = runner.lock().unwrap();
        for ruling in &mut runner.state.rulings {
            if let RulingKind::Merge { held_by, .. } = &mut ruling.kind {
                held_by.clear();
            }
        }
    }

    rig.land_on_origin("landed.txt");
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::YesWithdrawn { reason, .. }) if reason == "main moved since the question"
    ));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Rebased { .. })
    ));
    let rebased = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&rebased, Checks::Passed);
    assert!(
        matches!(
            rig.verdict(&runner),
            Some(StepReport::Finished { merged: true, .. })
        ),
        "the open thread asked again"
    );
    assert_eq!(rig.forge.coderabbit.resolved(), Vec::<String>::new());
}

// A reply the parser cannot read says nothing of any thread, so each is
// replied to as not addressed and none is resolved, whatever it pushed.
#[test]
fn a_reply_the_parser_cannot_read_leaves_every_thread_open_as_not_addressed() {
    let (rig, runner, _) = sent(&[FINDING, NIT]);
    rig.claude.script([Scripted::PushSay(
        "strip.txt",
        "stripped\n",
        "All fixed!\nThread one: done\nthreads 1 and 2 are fixed\n**1.** fixed\nthread 2 - fixed",
    )]);
    step(&runner).unwrap(); // the worker's fix turn
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    let not_addressed =
        "Not addressed: the worker's reply did not mention this thread, so it stays open.";
    assert_eq!(
        rig.forge.coderabbit.replies(),
        [
            (THREAD.to_owned(), not_addressed.to_owned()),
            ("PRRT_cubic_71_1".to_owned(), not_addressed.to_owned()),
        ]
    );
    assert_eq!(rig.forge.coderabbit.resolved(), Vec::<String>::new());
}

// ADR 0007: a fix turn sent only nits may decline them all and push nothing.
#[test]
fn a_nit_fix_that_pushes_nothing_gets_the_same_replies() {
    let (rig, runner, _) = sent(&[NIT]);
    rig.claude.script([Scripted::Say(
        "Nothing to change.\nthread 1: declined: the longer name says what it holds",
    )]);
    step(&runner).unwrap(); // the worker's fix turn, which pushes nothing
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::NitsDeclined { nits: 1, .. })
    ));
    assert_eq!(
        rig.forge.coderabbit.replies(),
        [(
            THREAD.to_owned(),
            "Declined: the longer name says what it holds".to_owned()
        )]
    );
    assert_eq!(rig.forge.coderabbit.resolved(), Vec::<String>::new());
}

// A fix that moved no head fixed nothing, whatever the worker says.
#[test]
fn a_fixed_line_on_a_turn_that_pushed_nothing_resolves_nothing() {
    let (rig, runner, _) = sent(&[NIT]);
    rig.claude
        .script([Scripted::Say("All done.\nthread 1: fixed")]);
    step(&runner).unwrap();
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::NitsDeclined { nits: 1, .. })
    ));
    assert_eq!(
        rig.forge.coderabbit.replies(),
        [(
            THREAD.to_owned(),
            "Not fixed: the worker said it was, but pushed nothing, so it stays open.".to_owned()
        )]
    );
    assert_eq!(rig.forge.coderabbit.resolved(), Vec::<String>::new());
}

// A merge ruling that warns of open threads cannot be cleared by a fix sent
// straight to CI, so its `no <note>` starts a new pass, and the threads that
// pass finds go to a fix turn like any round's.
#[test]
fn the_threads_a_new_pass_after_a_no_finds_are_answered_like_any_rounds() {
    let (rig, runner, head) = sent(&[FINDING]);
    rig.claude.script([Scripted::PushSay(
        "strip.txt",
        "stripped\n",
        "thread 1: fixed",
    )]);
    step(&runner).unwrap(); // the worker's fix turn
    step(&runner).unwrap(); // the thread is answered
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_ne!(fixed, head);
    // cubic reviews every push, and leaves the same finding on the fix.
    rig.forge
        .coderabbit
        .cubic_review(71, &fixed, now(&rig) + 60, &[FINDING]);
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. })
     if id == ticket(71)));
    rig.ask(&runner, "rule", Some("71 no strip only stamped files"));
    rig.claude
        .script([Scripted::Push("stamped.txt", "stamped only\n")]);
    step(&runner).unwrap(); // the noted turn: pushes, and a pass begins
    rig.claude.script([Scripted::Text(CLEAN)]);
    step(&runner).unwrap(); // round 1, qwen: clean by default
    step(&runner).unwrap(); // round 2, claude: scripted clean above
    let stamped = rig.forge.head_of("kelpie/7").unwrap();
    assert_eq!(step(&runner).unwrap(), summoned(&stamped));
    rig.forge
        .coderabbit
        .cubic_review(71, &stamped, now(&rig) + 300, &[FINDING]);
    rig.clock.advance(300);
    assert_eq!(rig.threads_read(&runner), bot_reviewed(3, "cubic", 2));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent { held: 2, .. })
    ));
    rig.claude.script([Scripted::PushSay(
        "again.txt",
        "again\n",
        "thread 1: declined: stamped files are the only ones it strips\nthread 2: fixed",
    )]);
    step(&runner).unwrap(); // the worker's fix turn
    step(&runner).unwrap(); // the threads are answered
    let replies = rig.forge.coderabbit.replies();
    assert_eq!(replies.len(), 3, "{replies:?}");
    assert_eq!(
        replies[1..],
        [
            (
                "PRRT_cubic_71_1".to_owned(),
                "Declined: stamped files are the only ones it strips".to_owned()
            ),
            ("PRRT_cubic_71_2".to_owned(), "Fixed.".to_owned()),
        ]
    );
    assert_eq!(
        rig.forge.coderabbit.resolved(),
        [THREAD, "PRRT_cubic_71_2"],
        "the declined thread stays open"
    );
}

#[test]
fn the_fix_prompt_asks_for_a_line_per_thread() {
    let (rig, runner, _) = sent(&[FINDING]);
    rig.claude.script([Scripted::Say("Not now.")]);
    step(&runner).unwrap();
    let prompt = rig.claude.calls().pop().unwrap().prompt;
    assert!(
        prompt.ends_with(
            " Each finding is a review bot's thread, and its line in the file starts with its \
             number, `thread <n>|`. Kelpie replies on each thread for you and resolves the \
             ones you fixed, so end your final reply with one line per finding, using the \
             number its line starts with: `thread <n>: fixed` or `thread <n>: declined: \
             <the reason>`. A finding with no line is replied to as not addressed, and \
             stays open."
        ),
        "{prompt}"
    );
}
