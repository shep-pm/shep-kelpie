//! A bot's threads read until they settle, the threads of a bot the pass
//! went on without, and those the merge ruling names, through the runner's
//! stand-ins

use std::sync::Mutex;

use crate::ports::Checks;
use crate::review_bot::Thread;
use crate::runner::coderabbit::tests::now;
use crate::runner::review_bot::two_bots::{bot_reviewed, listing, reviewed, summoned};
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted};

const FINDING: &str = "P2: `bleats` loses a stamped prefix. Strip only files shep stamped.";

// Takes the bot's threads out of the forge's answers, as a forge that lists
// a review before the threads posted with it does.
fn hidden(rig: &Rig, login: &str) -> Vec<Thread> {
    let mut threads = Vec::new();
    rig.forge.coderabbit.post_as(71, login, |seen| {
        threads = std::mem::take(&mut seen.threads)
    });
    threads
}

fn shown(rig: &Rig, login: &str, threads: Vec<Thread>) {
    rig.forge
        .coderabbit
        .post_as(71, login, |seen| seen.threads.extend(threads));
}

fn findings_file(rig: &Rig) -> String {
    std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap()
}

// The settle's bounds, pinned as seconds: reads 20 apart, a landing no
// sooner than 60 after the review was seen.
#[test]
fn threads_the_forge_lists_after_the_review_still_reach_the_fix_turn() {
    let rig = listing("shep", &["coderabbit"]);
    let (runner, head) = reviewed(&rig);
    step(&runner).unwrap(); // marks the draft ready
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let login = crate::coderabbit::LOGIN.rest;
    rig.forge
        .coderabbit
        .review(71, &head, now(&rig) + 240, &["Name the flag."]);
    let late = hidden(&rig, login);
    rig.clock.advance(240);
    let reads = || rig.forge.coderabbit.logins().len();
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "the review, with no thread yet"
    );
    let first = reads();

    shown(&rig, login, late);
    rig.clock.advance(19);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(reads(), first, "no read 19s on");
    rig.clock.advance(1);
    assert_eq!(step(&runner).unwrap(), None, "one more thread than before");
    assert_eq!(reads(), first + 1, "a read 20s on");
    rig.clock.advance(20);
    assert_eq!(step(&runner).unwrap(), None, "agreed, 40s after the review");
    rig.clock.advance(19);
    assert_eq!(step(&runner).unwrap(), None, "59s after the review");
    rig.clock.advance(1);
    assert_eq!(step(&runner).unwrap(), bot_reviewed(3, "coderabbit", 1));
    let state = std::fs::read_to_string(rig.paths().state).unwrap();
    let state: serde_json::Value = serde_json::from_str(&state).unwrap();
    assert_eq!(
        state["work_items"][0]["counts"]["review_rounds"], 3,
        "qwen's, Claude's and CodeRabbit's, its settle reads counted once"
    );
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent {
            round: 3,
            held: 1,
            ..
        })
    ));
    assert!(findings_file(&rig).contains("Name the flag."));
}

#[test]
fn threads_that_keep_changing_are_taken_as_they_stand_120s_after_the_review() {
    let rig = listing("shep", &["cubic"]);
    let (runner, head) = reviewed(&rig);
    step(&runner).unwrap(); // marks the draft ready
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let at = now(&rig) + 300;
    rig.forge.coderabbit.cubic_review(71, &head, at, &[FINDING]);
    rig.clock.advance(300);
    assert_eq!(step(&runner).unwrap(), None);
    for seconds in [20, 40, 60, 80, 100] {
        rig.forge.coderabbit.cubic_review(71, &head, at, &[FINDING]);
        rig.clock.advance(20);
        assert_eq!(step(&runner).unwrap(), None, "still changing at {seconds}s");
    }
    rig.forge.coderabbit.cubic_review(71, &head, at, &[FINDING]);
    rig.clock.advance(20);
    assert_eq!(step(&runner).unwrap(), bot_reviewed(3, "cubic", 7));
}

#[test]
fn a_bot_passed_over_that_reviews_the_head_anyway_gets_a_round_before_the_next_reviewer() {
    let rig = listing("shep", &["cubic", "coderabbit"]);
    let (runner, head) = reviewed(&rig);
    step(&runner).unwrap(); // marks the draft ready
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let summon = now(&rig);
    rig.forge.coderabbit.cubic_refuse(71, summon + 20);
    rig.clock.advance(20);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped { round: 3, .. })
    ));

    rig.forge
        .coderabbit
        .cubic_review(71, &head, summon + 60, &[FINDING]);
    rig.clock.advance(40);
    assert_eq!(rig.threads_read(&runner), bot_reviewed(4, "cubic", 1));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["bots_skipped"], serde_json::json!(null));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent {
            round: 4,
            held: 1,
            ..
        })
    ));
    assert!(findings_file(&rig).contains("`bleats` loses a stamped prefix."));
}

// cubic, last in the pass, reads the head and leaves FINDING, whose fix
// goes to CI. cubic, which reviews every push, then leaves `findings` on the
// fix's head, and CI is green on it: the next step is the merge ruling's.
fn read_then_reviewed_again(rig: &Rig, findings: &[&str]) -> Mutex<Runner> {
    let (runner, head) = reviewed(rig);
    step(&runner).unwrap(); // marks the draft ready
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    rig.forge
        .coderabbit
        .cubic_review(71, &head, now(rig) + 300, &[FINDING]);
    rig.clock.advance(300);
    assert_eq!(rig.threads_read(&runner), bot_reviewed(3, "cubic", 1));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent { held: 1, .. })
    ));
    rig.claude
        .script([Scripted::Push("strip.txt", "stripped\n")]);
    step(&runner).unwrap(); // the worker's fix turn
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge
        .coderabbit
        .cubic_review(71, &fixed, now(rig) + 60, findings);
    rig.forge.set_checks(&fixed, Checks::Passed);
    runner
}

// cubic read once this pass, so its thread on a later head is named in the
// merge ruling and sent to no fix turn, and under `auto` it holds the merge.
#[test]
fn under_auto_a_bots_unaddressed_threads_raise_the_merge_ruling_instead_of_merging() {
    let rig = listing("shep", &["cubic"]);
    rig.merge_auto();
    let runner = read_then_reviewed_again(&rig, &[FINDING]);
    let worker_turns = rig.claude.calls().len();
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(&runner) else {
        panic!("merged with a thread nothing addressed");
    };
    assert!(
        question.contains("Review bot threads are still open on it: 1 from cubic."),
        "{question}"
    );
    assert_eq!(rig.claude.calls().len(), worker_turns, "no fix turn");
    assert_eq!(rig.forge.merges(), []);
}

// Open nits are named apart from the threads that hold a merge, and hold none.
#[test]
fn the_merge_ruling_names_open_nits_and_under_auto_they_hold_nothing() {
    let nits = [
        "P3: The name could be shorter.",
        "P3: The comment restates the code.",
    ];
    let rig = listing("shep", &["cubic"]);
    let runner = read_then_reviewed_again(&rig, &nits);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(&runner) else {
        panic!("no merge ruling");
    };
    assert!(
        question.contains(" into main? 2 nits left open (cubic). `shep kelpie rule 1 yes`"),
        "{question}"
    );
    assert!(
        question.contains("your note for a fix that goes to CI and back to you"),
        "a no is a fix turn: {question}"
    );

    let rig = listing("shep", &["cubic"]);
    rig.merge_auto();
    let runner = read_then_reviewed_again(&rig, &nits);
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    let merged = rig.verdict(&runner);
    assert!(
        matches!(merged, Some(StepReport::Finished { merged: true, .. })),
        "{merged:?}"
    );
    assert_eq!(rig.forge.merges(), [(71, fixed)]);
}

// A fix sent straight to CI could not clear the threads the ruling warns
// of, so a no on it starts a new pass, as a rework does, and says so.
#[test]
fn a_no_on_a_merge_ruling_that_warns_of_open_threads_starts_a_new_pass() {
    let rig = listing("shep", &["cubic"]);
    let runner = read_then_reviewed_again(&rig, &[FINDING]);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(&runner) else {
        panic!("no merge ruling");
    };
    assert!(
        question.ends_with(
            "`shep kelpie rule 1 yes` merges it, and `shep kelpie rule 1 no <note>` or \
             `shep kelpie rule 1 rework <note>` sends the worker your note for a change the \
             whole review reads again, since only a new pass clears that."
        ),
        "{question}"
    );
    rig.ask(&runner, "rule", Some("1 no strip only stamped files"));
    rig.claude
        .script([Scripted::Push("stamped.txt", "stamped only\n")]);
    step(&runner).unwrap(); // the noted turn: pushes, and a pass begins
    let noted = rig.claude.calls().pop().unwrap();
    assert!(
        noted
            .prompt
            .ends_with("Once you push it, the whole review reads your change again.\n"),
        "{}",
        noted.prompt
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "review");
    assert_eq!(status["work_item"]["phase"]["round"], 1);
    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert_eq!(
        item.noted_from, None,
        "the next ruling will not call its head the note's fix"
    );
}

#[test]
fn a_nit_sent_to_its_fix_and_an_outdated_thread_raise_no_warning() {
    let rig = listing("shep", &["cubic"]);
    let (runner, head) = reviewed(&rig);
    step(&runner).unwrap(); // marks the draft ready
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let at = now(&rig) + 300;
    let nit = "P3: The name could be shorter.";
    rig.forge
        .coderabbit
        .cubic_review(71, &head, at, &[nit, FINDING]);
    rig.forge
        .coderabbit
        .post_as(71, crate::cubic::LOGIN.rest, |seen| {
            seen.threads[1].outdated = true
        });
    rig.clock.advance(300);
    assert_eq!(rig.threads_read(&runner), bot_reviewed(3, "cubic", 1));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent { held: 1, .. })
    ));
    rig.claude
        .script([Scripted::Push("shorter.txt", "shorter\n")]);
    step(&runner).unwrap(); // the worker's fix turn
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&fixed, Checks::Passed);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(&runner) else {
        panic!("no merge ruling");
    };
    assert!(!question.contains("Review bot threads"), "{question}");
}

// cubic, listed first, refuses and is passed over: the pass goes on to
// CodeRabbit's round, which is next.
fn cubic_passed_over(rig: &Rig) -> (std::sync::Mutex<crate::runner::Runner>, String, u64) {
    let (runner, head) = reviewed(rig);
    step(&runner).unwrap(); // marks the draft ready
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let summon = now(rig);
    rig.forge.coderabbit.cubic_refuse(71, summon + 20);
    rig.clock.advance(20);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped { round: 3, .. })
    ));
    (runner, head, summon)
}

#[test]
fn a_late_review_whose_threads_show_only_after_its_first_read_still_reaches_the_fix_turn() {
    let rig = listing("shep", &["cubic", "coderabbit"]);
    let (runner, head, summon) = cubic_passed_over(&rig);
    let login = crate::cubic::LOGIN.rest;
    rig.forge
        .coderabbit
        .cubic_review(71, &head, summon + 60, &[FINDING]);
    let late = hidden(&rig, login);
    rig.clock.advance(40);
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "its review, with no thread yet"
    );
    shown(&rig, login, late);
    rig.clock.advance(20);
    assert_eq!(step(&runner).unwrap(), None, "one more thread than before");
    rig.clock.advance(40);
    assert_eq!(step(&runner).unwrap(), bot_reviewed(4, "cubic", 1));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent {
            round: 4,
            held: 1,
            ..
        })
    ));
}

#[test]
fn a_bot_passed_over_that_reviews_while_the_last_reviewer_runs_gets_a_round_before_ci() {
    let rig = Rig::new("shep");
    rig.reviewers(&["qwen", "cubic", "claude"]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn: opens the pull request
    step(&runner).unwrap(); // review round 1, qwen: clean by default
    let head = rig.forge.head_of("kelpie/7").unwrap();
    step(&runner).unwrap(); // marks the draft ready
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let summon = now(&rig);
    rig.forge.coderabbit.cubic_refuse(71, summon + 20);
    rig.clock.advance(20);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped { round: 2, .. })
    ));

    step(&runner).unwrap(); // review round 3, claude: scripted clean above
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["phase"]["state"], "review",
        "the last reviewer is done, and the pass not yet ended"
    );
    rig.forge
        .coderabbit
        .cubic_review(71, &head, summon + 30, &[FINDING]);
    assert_eq!(rig.threads_read(&runner), bot_reviewed(4, "cubic", 1));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent {
            round: 4,
            held: 1,
            ..
        })
    ));
}
