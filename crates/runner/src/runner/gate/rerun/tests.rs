use serde_json::json;

use super::super::CHECKS_SETTLE;
use super::super::tests::ruling_report;
use crate::ports::{Checks, Timestamp};
use crate::runner::retro::journal::{Entry, append};
use crate::runner::{Runner, StepReport, step};
use crate::test::ticket;
use crate::test::{Rig, Scripted};

// The worker's first turn changed `work.txt`, so that is the branch's one changed file.
const OTHER_FILE_PANIC: &str = "--- run 9\nthread 'sheep::tests::reconnects' panicked at \
                                src/sheep.rs:412:9:\nassertion failed: elapsed < limit";

fn red(rig: &Rig, runner: &std::sync::Mutex<Runner>, head: &str, log: &str) -> StepReport {
    rig.forge.set_ci_log(log);
    rig.forge
        .set_checks(head, Checks::Failed(vec!["test".to_owned()]));
    rig.verdict(runner).expect("the gate read the red run")
}

fn fix_prompt(rig: &Rig, runner: &std::sync::Mutex<Runner>) -> String {
    rig.claude.script([Scripted::Say("fixed")]);
    step(runner).unwrap();
    rig.claude.calls().last().unwrap().prompt.clone()
}

#[test]
fn a_red_run_whose_log_names_no_changed_file_is_rerun_once_and_sends_no_turn() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    let calls = rig.claude.calls().len();

    let report = red(&rig, &runner, &head, OTHER_FILE_PANIC);

    assert_eq!(
        report,
        StepReport::CiRerun {
            issue: ticket(7),
            pull_request: 71,
            head: head.clone(),
            checks: vec!["test".to_owned()],
        }
    );
    assert_eq!(rig.forge.reruns(), std::slice::from_ref(&head));
    assert_eq!(rig.claude.calls().len(), calls, "no fix turn went out");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["rerunning"], json!(head));

    // The rollup still reads red for a moment after the rerun starts.
    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(CHECKS_SETTLE);
    rig.forge.set_checks(&head, Checks::Passed);
    let (_, question) = ruling_report(step(&runner).unwrap());
    assert!(question.starts_with("Merge pull request #71"), "{question}");
    assert_eq!(rig.forge.reruns().len(), 1);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"].get("rerunning"), None);
}

#[test]
fn a_head_red_again_after_its_rerun_sends_the_fix_turn_saying_it_repeated() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    red(&rig, &runner, &head, OTHER_FILE_PANIC);
    rig.clock.advance(CHECKS_SETTLE);

    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::CiFailed { head: ref h, .. }) if *h == head),
        "{report:?}"
    );
    assert_eq!(rig.forge.reruns().len(), 1, "a head is rerun once");
    let prompt = fix_prompt(&rig, &runner);
    assert!(
        prompt.contains(
            "kelpie reran this head's failed jobs and they failed again: the failure repeats"
        ),
        "{prompt}"
    );
}

#[test]
fn a_red_run_whose_log_names_a_changed_file_sends_the_fix_turn() {
    let (rig, runner, head) = Rig::with_pull_request("koji");

    let log = "--- run 9\nerror[E0308]: mismatched types\n --> work.txt:3:9";
    let report = red(&rig, &runner, &head, log);

    assert!(matches!(report, StepReport::CiFailed { .. }), "{report:?}");
    assert_eq!(rig.forge.reruns(), Vec::<String>::new());
    let prompt = fix_prompt(&rig, &runner);
    assert!(!prompt.contains("failed again"), "{prompt}");
}

#[test]
fn an_unreadable_log_sends_the_fix_turn() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    rig.forge.set_ci_log_error("gh run list failed");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".to_owned()]));
    let report = rig.verdict(&runner).expect("the gate read the red run");
    assert!(matches!(report, StepReport::CiFailed { .. }), "{report:?}");
    assert_eq!(rig.forge.reruns(), Vec::<String>::new());

    // A log with nothing in it is as unreadable.
    let (rig, runner, head) = Rig::with_pull_request("rotom");
    let report = red(&rig, &runner, &head, "");
    assert!(matches!(report, StepReport::CiFailed { .. }), "{report:?}");
    assert_eq!(rig.forge.reruns(), Vec::<String>::new());
}

#[test]
fn a_rerun_the_forge_refuses_sends_the_fix_turn() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    rig.forge.set_rerun_error("gh failed: no admin rights");

    let report = red(&rig, &runner, &head, OTHER_FILE_PANIC);

    assert!(matches!(report, StepReport::CiFailed { .. }), "{report:?}");
    assert_eq!(rig.forge.reruns(), Vec::<String>::new());
}

#[test]
fn a_head_red_while_the_review_waits_for_it_is_rerun_and_the_review_waits_on_the_rerun() {
    let rig = Rig::new("shep");
    rig.ci_on();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    let head = rig.forge.head_of("kelpie/7").unwrap();
    let calls = rig.claude.all_seen().len();

    let report = red(&rig, &runner, &head, OTHER_FILE_PANIC);

    assert!(matches!(report, StepReport::CiRerun { .. }), "{report:?}");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "review");
    assert_eq!(status["work_item"]["rerunning"], json!(head));
    assert_eq!(step(&runner).unwrap(), None, "the rerun is waited out");
    assert_eq!(rig.claude.all_seen().len(), calls, "no turn, no read");

    rig.forge.set_checks(&head, Checks::Passed);
    rig.clock.advance(CHECKS_SETTLE);
    assert!(step(&runner).unwrap().is_some(), "the review goes on");
    assert_eq!(rig.forge.reruns().len(), 1);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"].get("rerunning"),
        None,
        "a rerun that passed is no longer running"
    );
}

// A red run on work item 3, journaled as the rerun would, a day before the rig's clock
fn flaked_on_three(rig: &Rig, test: &str) {
    let journal = rig.paths().journal;
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    let entry = Entry::CiOutsideDiff {
        issue: ticket(3),
        tests: vec![test.to_owned()],
    };
    append(&journal, Timestamp(Rig::EPOCH - 86_400), entry).unwrap();
}

#[test]
fn a_test_failing_outside_two_items_diffs_is_carried_into_the_fix_turns_prompt() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    flaked_on_three(&rig, "sheep::tests::reconnects");
    red(&rig, &runner, &head, OTHER_FILE_PANIC);
    rig.clock.advance(CHECKS_SETTLE);
    step(&runner).unwrap();

    let prompt = fix_prompt(&rig, &runner);

    assert!(
        prompt.contains("Known flakes:") && prompt.contains("sheep::tests::reconnects"),
        "{prompt}"
    );
}

#[test]
fn a_test_that_failed_outside_one_items_diff_alone_is_no_known_flake() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    flaked_on_three(&rig, "another::test");
    red(&rig, &runner, &head, OTHER_FILE_PANIC);
    rig.clock.advance(CHECKS_SETTLE);
    step(&runner).unwrap();

    let prompt = fix_prompt(&rig, &runner);

    assert!(!prompt.contains("Known flakes"), "{prompt}");
}

#[test]
fn a_flake_not_seen_for_a_week_is_let_go() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    let journal = rig.paths().journal;
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    let old = Entry::CiOutsideDiff {
        issue: ticket(3),
        tests: vec!["sheep::tests::reconnects".to_owned()],
    };
    append(&journal, Timestamp(Rig::EPOCH - 8 * 86_400), old).unwrap();
    red(&rig, &runner, &head, OTHER_FILE_PANIC);
    rig.clock.advance(CHECKS_SETTLE);
    step(&runner).unwrap();

    let prompt = fix_prompt(&rig, &runner);

    assert!(!prompt.contains("Known flakes"), "{prompt}");
}

// Kelpie's one rerun of `head` failed again, and no fix turn is allowed, so
// the second red run is the maintainer's ruling.
fn still_red_after_the_rerun(log: &str) -> (Rig, std::sync::Mutex<Runner>, String, String) {
    let (rig, runner, head) = Rig::with_pull_request_set("koji", |rig| rig.fix_attempts(0));
    let report = match red(&rig, &runner, &head, log) {
        StepReport::CiRerun { .. } => {
            rig.clock.advance(CHECKS_SETTLE);
            step(&runner).unwrap()
        }
        ruling => Some(ruling),
    };
    let (_, question) = ruling_report(report);
    (rig, runner, head, question)
}

#[test]
fn choosing_rerun_on_a_still_red_ruling_reruns_the_failed_jobs_once_more() {
    let (rig, runner, head, question) = still_red_after_the_rerun(OTHER_FILE_PANIC);
    assert!(
        question.contains("`shep kelpie rule 71 rerun` reruns"),
        "{question}"
    );
    assert_eq!(
        rig.forge.reruns().len(),
        1,
        "only kelpie's own rerun so far"
    );

    let reply = rig.ask(&runner, "rule", Some("71 rerun"));

    assert_eq!(reply.get("error"), None, "{reply}");
    assert_eq!(rig.forge.reruns(), [head.clone(), head.clone()]);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    assert_eq!(status["rulings"], json!([]));
    let events = runner.lock().unwrap().state.events.clone();
    let shown = format!(
        "#7: ruling 71 option C (Rerun) chosen by the maintainer: reran the failed jobs of {}",
        &head[..7]
    );
    assert!(events.iter().any(|e| e.what == shown), "{events:?}");

    // The rerun's checks read red for a moment, then green: the merge ruling.
    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(CHECKS_SETTLE);
    rig.forge.set_checks(&head, Checks::Passed);
    let (_, question) = ruling_report(step(&runner).unwrap());
    assert!(question.starts_with("Merge pull request #71"), "{question}");
}

#[test]
fn rerun_is_not_offered_when_the_failed_log_names_a_changed_file() {
    let log = "--- run 9\nerror[E0308]: mismatched types\n --> work.txt:3:9";
    let (rig, runner, _, question) = still_red_after_the_rerun(log);
    assert!(!question.contains("rerun"), "{question}");

    let reply = rig.ask(&runner, "rule", Some("71 rerun"));

    let error = reply["error"].as_str().expect("the rerun was refused");
    assert!(error.contains("does not offer"), "{error}");
    assert_eq!(rig.forge.reruns(), Vec::<String>::new());
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ruling");
}

#[test]
fn a_rerun_the_forge_refuses_leaves_the_ruling_waiting() {
    let (rig, runner, _, _) = still_red_after_the_rerun(OTHER_FILE_PANIC);
    rig.forge.set_rerun_error("gh failed: no admin rights");

    let reply = rig.ask(&runner, "rule", Some("71 rerun"));

    let error = reply["error"].as_str().expect("the rerun was refused");
    assert!(error.contains("no admin rights"), "{error}");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ruling");
}

// Run 38275563600's job-log tail as `failed_run_logs` hands it: a note that a job's
// timeout cancelled it, and no file the branch changed
const TIMED_OUT: &str = concat!(
    "--- run 9\n",
    "note: job `test-macos` was cancelled in step `Run cargo test --locked` after 20m27s: ",
    "its `timeout-minutes` ended it, so this is a slow job and not a failing test\n",
    "test-macos\tUNKNOWN STEP\ttest sheep::tests::reconnects ... ok\n",
);

fn timed_out(rig: &Rig, runner: &std::sync::Mutex<Runner>, head: &str) -> StepReport {
    rig.forge.set_ci_log_timed_out(TIMED_OUT, 1);
    rig.forge
        .set_checks(head, Checks::Failed(vec!["test".to_owned()]));
    rig.verdict(runner).expect("the gate read the red run")
}

#[test]
fn a_head_whose_run_its_timeout_ended_is_not_rerun() {
    let (rig, runner, head) = Rig::with_pull_request("koji");

    let report = timed_out(&rig, &runner, &head);

    assert!(matches!(report, StepReport::CiFailed { .. }), "{report:?}");
    assert_eq!(rig.forge.reruns(), Vec::<String>::new());
    let prompt = fix_prompt(&rig, &runner);
    assert!(!prompt.contains("kelpie reran"), "{prompt}");
}

#[test]
fn rerun_is_not_offered_for_a_run_its_timeout_ended() {
    let (rig, runner, head) = Rig::with_pull_request_set("koji", |rig| rig.fix_attempts(0));
    let (_, question) = ruling_report(Some(timed_out(&rig, &runner, &head)));
    assert!(!question.contains("rerun"), "{question}");

    let reply = rig.ask(&runner, "rule", Some("71 rerun"));

    let error = reply["error"].as_str().expect("the rerun was refused");
    assert!(error.contains("does not offer"), "{error}");
    assert_eq!(rig.forge.reruns(), Vec::<String>::new());
}
