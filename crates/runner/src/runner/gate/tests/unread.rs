//! A head no reviewer read goes back through a pass of the review, and a pass cut
//! short by a reviewer down waits at the gate, through the runner's stand-ins

use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Value, json};

use crate::ports::{AgentError, CLEAN, Checks, Role};
use crate::runner::coderabbit::tests::now;
use crate::runner::gate::READ_AGAIN;
use crate::runner::pm::HOLD_FOR;
use crate::runner::review_bot::SETTLE_LEAST;
use crate::runner::review_bot::two_bots::listing;
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::settings::{AgentName, Harness};
use crate::test::{CALL_PATIENCE, Rig, Scripted, Seen, ticket};
use crate::work_item::{LOCAL_FAILURES_DOWN, ReviewPlan};

use super::ruling_report;

const NOTHING: &str = "{\"pick\": null}";
const FINDING: &str = "P2: `bleats` loses a stamped prefix. Strip only files shep stamped.";
const WAITED: &str = "no reviewer read its last review";
const KEPT_FAILING: &str = "claude was passed over after its calls kept failing";
const WAKE_WORDS: &str = "no reviewer has read its pull request since";
const EACH_WAIT: &str = "Kelpie starts one new review pass every 30 minutes";

// A reviewer card whose `paths` leave out the file the worker touched
const OPUS: &str = "---\nrole: reviewer\nharness: claude-code\nmodel: claude-opus-5-5\n\
                    effort: high\npaths: [\"src/**\"]\n---\nRead {{DIFF}}.\n";

// `n` Claude review rounds that fail, oldest first
fn down(n: usize) -> Vec<Scripted> {
    let broke = AgentError::Failed(Harness::ClaudeCode, "the model is down".into());
    (0..n).map(|_| Scripted::Fail(broke.clone())).collect()
}

// Queues a clean Claude round for each of the next `n` passes
fn read_clean(rig: &Rig, n: usize) {
    rig.claude
        .script_for(Role::Reviewer, (0..n).map(|_| Scripted::Text(CLEAN)));
}

// Queues each `say` as the project manager's next answers, oldest first
fn pm_says(rig: &Rig, says: impl IntoIterator<Item = &'static str>) {
    rig.claude
        .script_for(Role::Pm, says.into_iter().map(Scripted::Say));
}

// The project manager's calls, in order
fn pm_calls(rig: &Rig) -> Vec<Seen> {
    (rig.claude.all_seen())
        .into_iter()
        .filter(|s| s.call.role == Role::Pm)
        .collect()
}

// The wakes that say kelpie is waiting at the gate for a reviewer's read
fn read_wakes(rig: &Rig) -> Vec<Seen> {
    pm_calls(rig)
        .into_iter()
        .filter(|s| s.call.prompt.contains(WAKE_WORDS))
        .collect()
}

// How many rounds each kind of reviewer has run: local, then Claude
fn rounds(rig: &Rig) -> (usize, usize) {
    let claude = (rig.claude.all_seen())
        .into_iter()
        .filter(|s| s.call.role == Role::Reviewer)
        .count();
    (rig.reviewer.seen().len(), claude)
}

// The current work item's plan of `reads` reads
fn plan(runner: &Mutex<Runner>, reads: u32) {
    let mut runner = runner.lock().unwrap();
    let reads = std::num::NonZeroU32::new(reads).expect("a plan reads at least once");
    runner.state.work_items[0].review_plan = Some(ReviewPlan { reads, about: None });
}

// The current work item's plan, as a count of reads
fn plan_of(runner: &Mutex<Runner>) -> Option<u32> {
    let runner = runner.lock().unwrap();
    runner
        .current()
        .and_then(|item| item.review_plan.as_ref())
        .map(|plan| plan.reads.get())
}

// Records that local reviewer `name` has reviewed nothing for the item this many
// times in a row, which takes it out of every pick
fn local_down_for(runner: &Mutex<Runner>, name: &'static str) {
    runner
        .lock()
        .unwrap()
        .update(|item| {
            item.local_failures
                .insert(AgentName::kelpies(name), LOCAL_FAILURES_DOWN);
        })
        .unwrap();
}

// What the current work item keeps of its local reviewers that reviewed nothing
fn local_down(runner: &Mutex<Runner>) -> Vec<String> {
    let runner = runner.lock().unwrap();
    let item = runner.current().expect("the item is open");
    item.local_failures.keys().map(|n| n.to_string()).collect()
}

// Takes away every record of a round having read the item's heads, as a state
// file saved before heads were recorded has none
fn forget_reads(rig: &Rig) {
    let state = rig.paths().state;
    let mut saved: Value = serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    let item = saved["work_items"][0].as_object_mut().unwrap();
    assert!(
        item.remove("reviewed_heads").is_some(),
        "the rig records each read head"
    );
    std::fs::write(&state, saved.to_string()).unwrap();
}

// A project naming `agents.pm` as `pm`, whose pull request 71 its worker pushed,
// whose qwen round is clean by default and whose Claude rounds and reviewer list
// are what `setup` scripted, on a plan of `reads` reads, `auto` being whether
// `rulings.pm_decides.merge` is on. Its pass is over, the gate not run yet.
fn at_the_gate(
    project: &str,
    auto: bool,
    reads: u32,
    setup: impl FnOnce(&Rig),
) -> (Rig, Mutex<Runner>, String) {
    let rig = Rig::new(project);
    rig.pm("pm");
    rig.allow(&["sonnet-high", "qwen", "claude", "pm"]);
    if auto {
        rig.merge_auto();
    }
    setup(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    plan(&runner, reads);
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    through_review(&rig, &runner);
    let head = rig.forge.head_of("kelpie/7").expect("the worker pushed");
    (rig, runner, head)
}

// Steps the worker's first turn and the pass after it, until the pass is over,
// and returns what the steps reported
fn through_review(rig: &Rig, runner: &Mutex<Runner>) -> Vec<StepReport> {
    let mut seen = Vec::new();
    for _ in 0..20 {
        seen.extend(step(runner).unwrap());
        if rig.ask(runner, "status", None)["work_item"]["phase"]["state"] != "review" {
            return seen;
        }
    }
    panic!("the pass never ended: {seen:#?}");
}

// Steps `n` times
fn steps(runner: &Mutex<Runner>, n: usize) {
    for _ in 0..n {
        step(runner).unwrap();
    }
}

// Steps until a report matches `wanted`, and returns what the steps reported
fn until(runner: &Mutex<Runner>, wanted: impl Fn(&StepReport) -> bool) -> Vec<StepReport> {
    let deadline = Instant::now() + CALL_PATIENCE;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match step(runner).unwrap() {
            Some(report) => {
                let hit = wanted(&report);
                seen.push(report);
                if hit {
                    return seen;
                }
            }
            None => std::thread::yield_now(),
        }
    }
    panic!("no step reported what the test waited for: {seen:?}")
}

// Steps until a report matches `wanted`, moving the clock on by `every` between
// steps, for a wait that only a step's read can end
fn until_moving(
    rig: &Rig,
    runner: &Mutex<Runner>,
    every: u64,
    wanted: impl Fn(&StepReport) -> bool,
) -> Vec<StepReport> {
    let mut seen = Vec::new();
    for _ in 0..20 {
        match step(runner).unwrap() {
            Some(report) => {
                let hit = wanted(&report);
                seen.push(report);
                if hit {
                    return seen;
                }
            }
            None => std::thread::yield_now(),
        }
        rig.clock.advance(every);
    }
    panic!("no step reported what the test waited for: {seen:?}")
}

// Whether `status` holds the item at the gate for a reviewer's read
fn waited(status: &serde_json::Value) -> bool {
    (status["work_item"]["held_up"]["because"].as_str()).is_some_and(|why| why.starts_with(WAITED))
}

// Steps until the item waits at the gate for a reviewer, and returns its status
fn waiting(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
    let deadline = Instant::now() + CALL_PATIENCE;
    while Instant::now() < deadline {
        step(runner).unwrap();
        let status = rig.ask(runner, "status", None);
        if waited(&status) {
            return status;
        }
    }
    panic!(
        "the item never waited for a reviewer: {}",
        rig.ask(runner, "status", None)
    );
}

// The item's status, which must still be held for a reviewer's read
fn held(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
    let status = rig.ask(runner, "status", None);
    assert!(waited(&status), "{status}");
    status
}

// Steps until the pull request merged, and returns what the steps reported
fn merged(rig: &Rig, runner: &Mutex<Runner>) -> Vec<StepReport> {
    let mut seen = Vec::new();
    for _ in 0..20 {
        seen.extend(step(runner).unwrap());
        if !rig.forge.merges().is_empty() {
            return seen;
        }
        rig.clock.advance(CHECKS_SETTLE);
    }
    panic!("nothing merged: {}", rig.ask(runner, "status", None));
}

// Steps until the gate raises a merge ruling, and returns what it asks
fn merge_question(runner: &Mutex<Runner>) -> String {
    let seen = until(runner, |r| matches!(r, StepReport::Ruling { .. }));
    ruling_report(seen.last().cloned()).1
}

#[test]
fn an_unreviewed_pass_goes_back_to_a_reviewer_with_no_ruling() {
    let (rig, runner, head) = at_the_gate("unread-short", true, 2, |rig| {
        rig.claude.script_for(Role::Reviewer, down(3));
    });
    let status = waiting(&rig, &runner);
    assert_eq!(
        status["work_item"]["phase"]["state"], "ci",
        "it waits in CI"
    );
    assert_eq!(status["rulings"], json!([]), "the gate raises no ruling");
    assert!(
        status["work_item"]["unreviewed"]
            .as_str()
            .is_some_and(|why| why.contains(KEPT_FAILING)),
        "{status}"
    );

    // The wait ends with one new pass, which reads the head
    pm_says(&rig, [NOTHING]);
    read_clean(&rig, 1);
    rig.clock.advance(READ_AGAIN);
    merged(&rig, &runner);
    assert_eq!(rig.forge.merges(), [(71, head)], "the new pass read it");
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn an_unread_head_goes_back_to_a_reviewer_with_no_ruling() {
    let (rig, runner, head) = at_the_gate("unread-head", true, 2, |rig| {
        rig.claude
            .script_for(Role::Reviewer, [Scripted::Text(CLEAN)]);
    });
    // No record of a round reading this head, as a state file saved before
    // heads were recorded leaves a work item
    drop(runner);
    forget_reads(&rig);
    let runner = rig.open().unwrap();
    read_clean(&rig, 1);

    merged(&rig, &runner);
    assert_eq!(rig.forge.merges(), [(71, head)], "a pass read it first");
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"],
        json!([]),
        "no ruling for a head a pass can read"
    );
    assert_eq!(rounds(&rig), (2, 2), "one pass of the review for the head");
}

#[test]
fn a_note_fix_goes_back_to_a_reviewer() {
    let (rig, runner, head) = at_the_gate("unread-note", false, 2, |rig| {
        rig.claude
            .script_for(Role::Reviewer, [Scripted::Text(CLEAN)]);
    });
    let (id, _) = ruling_report(step(&runner).unwrap());
    assert_eq!(id, ticket(71));
    rig.ask(&runner, "rule", Some("71 no rename the flag"));
    rig.claude
        .script_for(Role::Worker, [Scripted::Push("rename.txt", "renamed\n")]);
    read_clean(&rig, 1);
    step(&runner).unwrap(); // the noted turn: pushes, and goes to CI
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_ne!(fixed, head, "the worker pushed the fix");

    // The fix goes back through a pass of the review, and the ruling after it
    // has nothing left to warn of
    let question = merge_question(&runner);
    assert!(
        question.contains(&format!("Merge pull request #71 at {}", &fixed[..7])),
        "{question}"
    );
    assert!(!question.contains("no reviewer read it"), "{question}");
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"][0]["kind"],
        json!({ "kind": "merge", "head": fixed }),
        "the fix was read, so the ruling says nothing about it"
    );
    assert_eq!(rounds(&rig), (2, 2), "the fix went through a pass");
}

#[test]
fn a_note_fix_the_list_left_with_no_reader_raises_a_ruling_after_one_pass() {
    let rig = {
        let rig = Rig::new("unread-note-nobody");
        rig.pm("pm");
        rig.allow(&["sonnet-high", "opus", "pm"]);
        rig.reviewers(&["opus"]);
        rig.write_agent("opus", OPUS);
        rig
    };
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);

    // The pass has no reader: `opus` reviews only `src/**`. With `merge = false`
    // the gate asks the maintainer, whose `no` sends the worker a note.
    through_review(&rig, &runner);
    let head = rig.forge.head_of("kelpie/7").unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["id"], json!("71"), "{status}");
    assert!(
        status["rulings"][0]["question"]
            .as_str()
            .is_some_and(|q| q.contains(&head[..7])),
        "{status}"
    );
    rig.ask(&runner, "rule", Some("71 no rename the flag"));
    rig.claude
        .script_for(Role::Worker, [Scripted::Push("rename.txt", "renamed\n")]);
    step(&runner).unwrap(); // the noted turn: pushes, and goes to CI
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_ne!(fixed, head, "the worker pushed the fix");

    // The fix goes back through one pass, which has no reader either. The gate
    // does not send it back again: it asks the maintainer about the fix.
    let question = merge_question(&runner);
    assert!(
        question.contains(&format!("Merge pull request #71 at {}", &fixed[..7])),
        "{question}"
    );
    assert!(!question.contains("no reviewer read it"), "{question}");
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"][0]["kind"],
        json!({ "kind": "merge", "head": fixed }),
        "the sent-back pass cleared the note"
    );
    assert_eq!(rounds(&rig), (0, 0), "no reviewer ran for the fix");
}

#[test]
fn a_late_fix_goes_back_to_a_reviewer() {
    let rig = {
        let rig = listing("unread-late", &["cubic"]);
        rig.pm("pm");
        rig.allow(&["sonnet-high", "qwen", "claude", "cubic", "pm"]);
        rig.merge_auto();
        rig
    };
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    plan(&runner, 2);
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude
        .script_for(Role::Reviewer, [Scripted::Text(CLEAN)]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    through_review(&rig, &runner); // its plan's two reads, cubic never reached
    let head = rig.forge.head_of("kelpie/7").unwrap();
    // cubic reviewed the head on its own, and left one thread above a nit
    rig.forge
        .coderabbit
        .cubic_review(71, &head, now(&rig) + 60, &[FINDING]);
    rig.clock.advance(60);
    rig.claude.script_for(
        Role::Worker,
        [Scripted::PushSay(
            "strip.txt",
            "stripped\n",
            "thread 1: fixed",
        )],
    );

    // cubic's round reaches the worker, whose fix no reviewer read
    step(&runner).unwrap(); // the gate starts cubic's round for the head it read
    until_moving(&rig, &runner, SETTLE_LEAST, |r| {
        matches!(r, StepReport::FixPushed { .. })
    });
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_ne!(fixed, head, "the worker pushed the late round's fix");
    assert_eq!(rig.reviewer.seen().len(), 1, "no reviewer read the fix yet");

    // The fix goes back through a pass of the review, which reads it
    read_clean(&rig, 1);
    merged(&rig, &runner);
    assert_eq!(rig.forge.merges(), [(71, fixed)], "the read fix merged");
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"],
        json!([]),
        "no ruling for a fix a pass read"
    );
    assert_eq!(rig.reviewer.seen().len(), 2, "a round read the fix");
}

#[test]
fn a_head_sent_unread_on_purpose_merges() {
    let (rig, runner, head) = Rig::with_pull_request_set("unread-sent", |rig| {
        rig.pm("pm");
        rig.allow(&["sonnet-high", "qwen", "claude", "pm"]);
        rig.merge_auto();
    });
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["lint".to_owned()]));
    assert!(
        matches!(rig.verdict(&runner), Some(StepReport::CiFailed { .. })),
        "red CI is the worker's fix turn"
    );
    rig.claude
        .script_for(Role::Worker, [Scripted::Push("lint.txt", "linted\n")]);
    step(&runner).unwrap(); // the fix turn: pushes, and goes to CI
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&fixed, Checks::Passed);

    merged(&rig, &runner);
    assert_eq!(
        rig.forge.merges(),
        [(71, fixed)],
        "kelpie sent this fix unread on purpose, so the gate takes it"
    );
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn a_pass_the_list_left_with_no_reader_merges() {
    let rig = {
        let rig = Rig::new("unread-nobody");
        rig.pm("pm");
        rig.allow(&["sonnet-high", "opus", "pm"]);
        rig.merge_auto();
        rig.reviewers(&["opus"]);
        rig.write_agent("opus", OPUS);
        rig
    };
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);

    // The pass has no reader: `opus` reviews only `src/**`. Kelpie sent the head
    // unread by choice, so the gate takes it.
    through_review(&rig, &runner);
    merged(&rig, &runner);
    assert_eq!(
        rig.forge.merges().len(),
        1,
        "no ruling for a list with no reader"
    );
    assert_eq!(rounds(&rig), (0, 0), "no reviewer ran");
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn a_pass_back_reads_what_the_plan_gives() {
    // The project's project manager would ask for one read on its own. Its plan
    // asks for two, and the pass the gate sends the head back through runs two.
    let (rig, runner, head) = at_the_gate("unread-plan", true, 2, |rig| {
        rig.claude.script_for(Role::Reviewer, down(3));
    });
    waiting(&rig, &runner);
    assert_eq!(
        rounds(&rig),
        (1, 3),
        "one read of the two the plan asks for"
    );

    pm_says(&rig, [NOTHING]);
    read_clean(&rig, 1);
    rig.clock.advance(READ_AGAIN);
    step(&runner).unwrap(); // the wait ends with one new pass
    assert_eq!(
        plan_of(&runner),
        Some(2),
        "sending it back raises no plan of its own"
    );

    let reports = merged(&rig, &runner);
    assert!(
        !reports
            .iter()
            .any(|r| matches!(r, StepReport::Summoned { .. })),
        "{reports:?}: no third read was started"
    );
    assert_eq!(
        rounds(&rig),
        (2, 4),
        "the new pass ran the plan's two reads"
    );
    assert_eq!(rig.forge.merges(), [(71, head)]);
}

#[test]
fn a_reviewer_down_waits_says_why_in_status_and_runs_no_round() {
    let (rig, runner, _) = at_the_gate("unread-wait", true, 2, |rig| {
        rig.claude.script_for(Role::Reviewer, down(3));
    });
    let status = waiting(&rig, &runner);
    let because = status["work_item"]["held_up"]["because"].as_str().unwrap();
    assert!(because.starts_with(WAITED), "{because}");
    assert!(because.contains(KEPT_FAILING), "{because}");
    assert!(
        because.contains("the merge waits for one to read it"),
        "{because}"
    );

    let before = rounds(&rig);
    steps(&runner, 5);
    assert_eq!(rounds(&rig), before, "no round runs during the wait");
    let status = held(&rig, &runner);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    assert_eq!(status["rulings"], json!([]), "the wait raises no ruling");
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn a_reviewer_that_stays_down_wakes_the_project_manager_once() {
    let (rig, runner, _) = at_the_gate("unread-wake", true, 2, |rig| {
        rig.claude.script_for(Role::Reviewer, down(9));
    });
    pm_says(&rig, [NOTHING]);
    waiting(&rig, &runner);
    assert!(read_wakes(&rig).is_empty(), "the first wait wakes nothing");

    // The wait ends with one new pass, which is cut short again. Its wake waits
    // out the window a held wake has, and then goes out.
    rig.clock.advance(READ_AGAIN);
    until(&runner, |r| matches!(r, StepReport::Unreviewed { .. }));
    rig.clock.advance(HOLD_FOR);
    until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let [wake] = read_wakes(&rig)
        .try_into()
        .expect("one wake for the wait that ended");
    assert!(
        wake.call.prompt.contains(WAKE_WORDS),
        "{}",
        wake.call.prompt
    );
    assert!(
        wake.call.prompt.contains(KEPT_FAILING),
        "{}",
        wake.call.prompt
    );

    rig.clock.advance(READ_AGAIN);
    until(&runner, |r| matches!(r, StepReport::Unreviewed { .. }));
    assert_eq!(
        read_wakes(&rig).len(),
        1,
        "a reviewer that stays down is not woken for again"
    );
}

#[test]
fn the_wake_says_kelpie_starts_a_new_pass_each_wait() {
    let (rig, runner, _) = at_the_gate("unread-words", true, 2, |rig| {
        rig.claude.script_for(Role::Reviewer, down(6));
    });
    pm_says(&rig, [NOTHING]);
    waiting(&rig, &runner);
    rig.clock.advance(READ_AGAIN);
    until(&runner, |r| matches!(r, StepReport::Unreviewed { .. }));
    rig.clock.advance(HOLD_FOR);
    until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));

    let [wake] = read_wakes(&rig).try_into().expect("one wake");
    assert!(wake.call.prompt.contains(EACH_WAIT), "{}", wake.call.prompt);
}

#[test]
fn a_reviewer_down_raises_no_ruling_however_long_it_lasts() {
    let (rig, runner, _) = at_the_gate("unread-long", true, 2, |rig| {
        rig.claude.script_for(Role::Reviewer, down(15));
    });
    pm_says(&rig, [NOTHING]);
    waiting(&rig, &runner);
    for _ in 0..4 {
        rig.clock.advance(READ_AGAIN);
        steps(&runner, 12);
        let status = held(&rig, &runner);
        assert_eq!(status["rulings"], json!([]), "{status}");
    }
    assert_eq!(rig.forge.merges(), [], "nothing merged unread");
}

#[test]
fn the_wait_ends_with_one_new_pass_and_a_read_merges() {
    let (rig, runner, head) = at_the_gate("unread-read", true, 2, |rig| {
        rig.claude.script_for(Role::Reviewer, down(3));
    });
    waiting(&rig, &runner);
    let before = rounds(&rig);
    pm_says(&rig, [NOTHING]);
    read_clean(&rig, 1);
    rig.clock.advance(READ_AGAIN);

    merged(&rig, &runner);
    assert_eq!(rig.forge.merges(), [(71, head)]);
    assert_eq!(
        rounds(&rig),
        (before.0 + 1, before.1 + 1),
        "one new pass, its reviewers reading"
    );
}

#[test]
fn a_step_during_the_wait_reads_nothing() {
    let (rig, runner, _) = at_the_gate("unread-step", true, 2, |rig| {
        rig.claude.script_for(Role::Reviewer, down(3));
    });
    waiting(&rig, &runner);
    let before = rounds(&rig);
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "the step gives Idle before it reads anything"
    );
    assert_eq!(rounds(&rig), before, "the step ran no round");
    held(&rig, &runner);
}

#[test]
fn a_local_reviewer_down_for_the_item_is_tried_again_after_the_wait() {
    let (rig, runner, head) = at_the_gate("unread-local", true, 2, |rig| {
        rig.claude.script_for(Role::Reviewer, down(3));
    });
    waiting(&rig, &runner);
    // qwen has reviewed nothing for this item twice in a row, so kelpie has
    // taken it out of its picks. Nothing else puts that count back.
    local_down_for(&runner, "qwen");
    assert_eq!(local_down(&runner), ["qwen".to_owned()]);

    pm_says(&rig, [NOTHING]);
    read_clean(&rig, 1);
    rig.clock.advance(READ_AGAIN);
    step(&runner).unwrap(); // the wait ends with one new pass
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "review"
    );
    assert_eq!(local_down(&runner), Vec::<String>::new(), "its count went");

    merged(&rig, &runner);
    assert_eq!(rig.forge.merges(), [(71, head)]);
    assert_eq!(rig.reviewer.seen().len(), 2, "the down reviewer ran again");
}

#[test]
fn a_pass_the_list_left_short_after_the_wait_merges() {
    let (rig, runner, head) = at_the_gate("unread-list", true, 3, |rig| {
        rig.claude.script_for(Role::Reviewer, down(3));
    });
    waiting(&rig, &runner);
    pm_says(&rig, [NOTHING]);
    read_clean(&rig, 1);
    rig.clock.advance(READ_AGAIN);

    // The new pass ran the two readers the project lists, short of the three the
    // plan asks for, with nobody down: kelpie marks nothing and merges
    merged(&rig, &runner);
    assert_eq!(rig.forge.merges(), [(71, head)]);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn a_project_with_no_project_manager_keeps_its_warned_ruling() {
    let rig = {
        let rig = Rig::new("unread-no-pm");
        rig.merge_auto();
        rig.allow(&["sonnet-high", "qwen", "claude"]);
        rig.reviews_need(Some(2));
        rig.claude.script_for(Role::Reviewer, down(3));
        rig
    };
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    through_review(&rig, &runner);
    let head = rig.forge.head_of("kelpie/7").unwrap();

    let question = merge_question(&runner);
    assert!(
        question.contains(&format!(
            "No reviewer read it in its last review: 1 of 2 reviewers read it: {KEPT_FAILING}."
        )),
        "{question}"
    );
    assert!(
        question.contains("only because the project names none"),
        "{question}"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"][0]["kind"],
        json!({ "kind": "merge", "head": head, "unreviewed": format!(
            "1 of 2 reviewers read it: {KEPT_FAILING}"
        ) })
    );
    assert_eq!(rig.forge.merges(), [], "the ruling holds the merge");
    assert_eq!(rounds(&rig), (1, 3), "no new pass ran for it");
}

#[test]
fn an_adopted_contribution_keeps_its_warned_ruling() {
    let (rig, runner, head) = at_the_gate("unread-adopted", true, 2, |rig| {
        rig.claude.script_for(Role::Reviewer, down(3));
    });
    runner
        .lock()
        .unwrap()
        .update(|item| {
            item.contributor = Some("octocat".to_owned());
            item.adopted = true;
        })
        .unwrap();

    let question = merge_question(&runner);
    assert!(
        question.contains(KEPT_FAILING),
        "the ruling warns of what it did: {question}"
    );
    assert!(
        question.contains("or the pull request is a contribution's"),
        "{question}"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"][0]["kind"]["head"],
        json!(head)
    );
    assert_eq!(rig.forge.merges(), [], "the ruling holds the merge");
    assert!(
        pm_calls(&rig).is_empty(),
        "nothing is asked of the project manager"
    );
}
