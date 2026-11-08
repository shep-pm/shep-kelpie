//! A head no reviewer read goes back through a pass of the review, and a pass cut
//! short by a reviewer down waits at the gate, through the runner's stand-ins

use std::sync::Mutex;
use std::time::Instant;

use serde_json::json;

use crate::ports::{AgentError, CLEAN, Checks, Role, parse_findings};
use crate::runner::coderabbit::tests::now;
use crate::runner::gate::READ_AGAIN;
use crate::runner::review_bot::two_bots::listing;
use crate::runner::{Runner, StepReport, step};
use crate::settings::Harness;
use crate::test::{CALL_PATIENCE, Rig, Scripted, ScriptedRound, Seen};
use crate::work_item::ReviewPlan;

use super::ruling_report;

const NOTHING: &str = "{\"pick\": null}";
const FINDING: &str = "P2: `bleats` loses a stamped prefix. Strip only files shep stamped.";
const WAITED: &str = "no reviewer read its last review";
const KEPT_FAILING: &str = "claude was passed over after its calls kept failing";
const DOWN_FILE: &str = "qwen reviewed no file";

// What qwen-review.sh writes when the model behind it cannot be reached
const BLANK: &str = "LOW|work.txt:0|not reviewed: curl: (7) Failed to connect to gpu.box port \
                     8080|raw response kept at /tmp/qwen-review/raw/work.txt.txt\n";

// A reviewer card whose `paths` leave out the file the worker touched
const OPUS: &str = "---\nrole: reviewer\nharness: claude-code\nmodel: claude-opus-5-5\n\
                    effort: high\npaths: [\"src/**\"]\n---\nRead {{DIFF}}.\n";

// `n` Claude review rounds that fail, oldest first
fn down(n: usize) -> Vec<Scripted> {
    let broke = AgentError::Failed(Harness::ClaudeCode, "the model is down".into());
    (0..n).map(|_| Scripted::Fail(broke.clone())).collect()
}

// Queues a clean Claude round for the next pass, and `n` of them
fn read_clean(rig: &Rig, n: usize) {
    rig.claude
        .script_for(Role::Reviewer, (0..n).map(|_| Scripted::Text(CLEAN)));
}

// Queues each `say` as the project manager's next answers, oldest first
fn pm_says(rig: &Rig, says: impl IntoIterator<Item = &'static str>) {
    rig.claude
        .script_for(Role::Pm, says.into_iter().map(Scripted::Say));
}

// The project manager's wakes, in order
fn pm_seen(rig: &Rig) -> Vec<Seen> {
    rig.claude
        .all_seen()
        .into_iter()
        .filter(|s| s.call.role == Role::Pm)
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

// The current work item's plan of `reads` reads, as a project manager's pick leaves it
fn plan(runner: &Mutex<Runner>, reads: u32) {
    let mut runner = runner.lock().unwrap();
    let reads = std::num::NonZeroU32::new(reads).expect("a plan reads at least once");
    runner.state.work_items[0].review_plan = Some(ReviewPlan { reads, about: None });
}

// What the current work item keeps of its local reviewers that reviewed nothing
fn local_down(runner: &Mutex<Runner>) -> Vec<String> {
    let runner = runner.lock().unwrap();
    let item = runner.current().expect("the item is open");
    item.local_failures.keys().map(|n| n.to_string()).collect()
}

// A project naming `agents.pm`, whose pull request 71 its worker pushed, whose qwen
// round is clean by default, and whose Claude rounds are `claude`, oldest first, on a
// plan of two reads. `auto` is whether its `rulings.pm_decides.merge` is on. The item
// stands at the gate, which has not run yet, and the head is the pull request's.
fn at_the_gate(
    project: &str,
    auto: bool,
    claude: impl IntoIterator<Item = Scripted>,
) -> (Rig, Mutex<Runner>, String) {
    let rig = Rig::new(project);
    rig.pm("pm");
    rig.allow(&["sonnet-high", "qwen", "claude", "pm"]);
    if auto {
        rig.merge_auto();
    }
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    plan(&runner, 2);
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    rig.claude.script_for(Role::Reviewer, claude);
    steps(&runner, 3); // the worker's turn, the qwen round, and the Claude round
    let head = rig.forge.head_of("kelpie/7").expect("the worker pushed");
    (rig, runner, head)
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

// Whether `status` holds the item at the gate for a reviewer's read
fn waited(status: &serde_json::Value) -> bool {
    (status["work_item"]["held_up"]["because"].as_str())
        .is_some_and(|why| why.starts_with(WAITED))
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

// Steps until the pull request merged
fn merged(rig: &Rig, runner: &Mutex<Runner>) {
    let deadline = Instant::now() + CALL_PATIENCE;
    while Instant::now() < deadline {
        step(runner).unwrap();
        if !rig.forge.merges().is_empty() {
            return;
        }
    }
    panic!("nothing merged: {}", rig.ask(runner, "status", None));
}

// The item's status, and what the gate holds it for
fn held(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
    let status = rig.ask(runner, "status", None);
    assert!(waited(&status), "{status}");
    status
}

#[test]
fn an_unreviewed_pass_goes_back_to_a_reviewer_with_no_ruling() {
    let (rig, runner, head) = at_the_gate("unread-short", true, down(3));
    let status = waiting(&rig, &runner);
    assert_eq!(status["work_item"]["phase"]["state"], "ci", "it waits in CI");
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
    let (rig, runner, _) = at_the_gate("unread-head", false, [Scripted::Text(CLEAN)]);
    // A commit pushed off kelpie's calls: no round read it, and kelpie sent no fix
    // turn that pushed it
    let pushed = rig.push_by_hand("kelpie/7", "steer.txt");
    read_clean(&rig, 1);

    let question = merge_question(&runner);
    assert!(
        question.contains(&format!("Merge pull request #71 at {}", &pushed[..7])),
        "{question}"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"][0]["kind"],
        json!({ "kind": "merge", "head": pushed }),
        "a pass read the head before the gate asked"
    );
    assert_eq!(rounds(&rig), (2, 2), "one pass of the review for the head");
    assert_eq!(rig.forge.merges(), [], "nothing merged unread");
}

#[test]
fn a_note_fix_goes_back_to_a_reviewer() {
    let (rig, runner, _) = at_the_gate("unread-note", false, [Scripted::Text(CLEAN)]);
    until(&runner, |r| matches!(r, StepReport::Ruling { .. }));
    rig.ask(&runner, "rule", Some("71 no  rename the flag to --dry-run "));
    rig.claude
        .script_for(Role::Worker, [Scripted::Push("rename.txt", "renamed\n")]);
    read_clean(&rig, 1);

    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    let question = merge_question(&runner);
    assert!(
        question.contains(&format!("Merge pull request #71 at {}", &fixed[..7])),
        "{question}"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"][0]["kind"],
        json!({ "kind": "merge", "head": fixed }),
        "the fix was read, so the ruling says nothing about it"
    );
    assert_eq!(rounds(&rig), (2, 2), "the fix went through a pass");
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
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
    steps(&runner, 3); // the worker's turn, the qwen round, and the Claude round
    let head = rig.forge.head_of("kelpie/7").unwrap();
    // cubic reviewed the head after the pass went on without it, and left one finding
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
    until(&runner, |r| matches!(r, StepReport::FixPushed { .. }));
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_ne!(fixed, head, "the worker pushed the late review's fix");
    assert_eq!(rig.reviewer.seen().len(), 1, "no reviewer read the fix yet");

    // The fix goes back through a pass of the review, which reads it
    read_clean(&rig, 1);
    merged(&rig, &runner);
    assert_eq!(rig.forge.merges(), [(71, fixed)], "the read fix merged");
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
    until(&runner, |r| matches!(r, StepReport::CiFailed { .. }));
    rig.claude
        .script_for(Role::Worker, [Scripted::Push("lint.txt", "linted\n")]);
    steps(&runner, 2); // the fix turn's call, and its end
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&fixed, Checks::Passed);

    rig.verdict(&runner);
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
    step(&runner).unwrap(); // the worker's first turn

    // The pass has no reader: `opus` reviews only `src/**`. Kelpie sent it unread
    // by choice, so the gate takes the head.
    merged(&rig, &runner);
    assert_eq!(rig.forge.merges().len(), 1, "no ruling for a list that has no reader");
    assert_eq!(rounds(&rig), (0, 0), "no reviewer ran");
}

#[test]
fn a_pass_back_reads_what_the_plan_gives() {
    let (rig, runner, head) = at_the_gate("unread-plan", true, down(3));
    waiting(&rig, &runner);
    pm_says(&rig, [NOTHING]);
    read_clean(&rig, 1);
    rig.clock.advance(READ_AGAIN);

    // The new pass runs the two reads the plan gives, and no more
    let reports = until(&runner, |r| matches!(r, StepReport::ReviewFindingsSent { .. }));
    let sent: Vec<u32> = (reports.iter())
        .filter_map(|r| match r {
            StepReport::ReviewFindingsSent { round, .. } => Some(*round),
            _ => None,
        })
        .collect();
    assert_eq!(sent.last(), Some(&2), "{reports:?}: the plan's two reads");
    assert!(
        !reports.iter().any(|r| matches!(r, StepReport::Summoned { .. })),
        "{reports:?}: no third read was started"
    );
    assert_eq!(
        plan_of(&runner),
        Some(2),
        "sending it back raises no plan of its own"
    );
    merged(&rig, &runner);
    assert_eq!(rig.forge.merges(), [(71, head)]);
}

#[test]
fn a_reviewer_down_waits_says_why_in_status_and_runs_no_round() {
    let (rig, runner, _) = at_the_gate("unread-wait", true, down(3));
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
    let (rig, runner, _) = at_the_gate("unread-wake", true, down(9));
    pm_says(&rig, [NOTHING, NOTHING]);
    waiting(&rig, &runner);
    assert!(pm_seen(&rig).is_empty(), "the first wait wakes nothing");

    rig.clock.advance(READ_AGAIN);
    until(&runner, |r| matches!(r, StepReport::Unreviewed { .. }));
    let [wake] = pm_seen(&rig).try_into().expect("one wake for the wait that ended");
    assert!(
        wake.call.prompt.contains("no reviewer has read its pull request since"),
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
        pm_seen(&rig).len(),
        1,
        "a reviewer that stays down is not woken for again"
    );
}

#[test]
fn a_reviewer_down_raises_no_ruling_however_long_it_lasts() {
    let (rig, runner, _) = at_the_gate("unread-long", true, down(15));
    pm_says(&rig, [NOTHING, NOTHING, NOTHING]);
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
    let (rig, runner, head) = at_the_gate("unread-read", true, down(3));
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
        "one new pass, its two reviewers reading"
    );
}

#[test]
fn a_step_during_the_wait_reads_nothing() {
    let (rig, runner, _) = at_the_gate("unread-step", true, down(3));
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
    let (rig, runner, head) = at_the_gate("unread-local", true, [Scripted::Text(CLEAN)]);
    rig.reviewer
        .script([ScriptedRound::Findings(parse_findings(BLANK))]);
    let status = waiting(&rig, &runner);
    assert!(
        status["work_item"]["unreviewed"]
            .as_str()
            .is_some_and(|why| why.contains(DOWN_FILE)),
        "{status}"
    );
    assert_eq!(local_down(&runner), ["qwen".to_owned()], "qwen is down for it");

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
    let (rig, runner, head) = at_the_gate("unread-list", true, down(3));
    plan(&runner, 3);
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
    let (rig, runner, head) = Rig::with_pull_request_set("unread-no-pm", |rig| {
        rig.claude.script_for(Role::Reviewer, down(3));
    });
    until(&runner, |r| matches!(r, StepReport::Unreviewed { .. }));
    rig.forge.set_checks(&head, Checks::Passed);

    let (_, question) = ruling_report(rig.verdict(&runner));
    assert!(
        question.contains(&format!("No reviewer read it in its last review: 1 of 2 reviewers read it: {KEPT_FAILING}.")),
        "{question}"
    );
    assert!(
        question.contains("only because the project names none"),
        "{question}"
    );
    assert_eq!(rig.forge.merges(), [], "the ruling holds the merge");
    assert_eq!(rounds(&rig), (1, 3), "no new pass ran for it");
}

#[test]
fn an_adopted_contribution_keeps_its_warned_ruling() {
    let (rig, runner, head) = at_the_gate("unread-adopted", true, down(3));
    runner
        .lock()
        .unwrap()
        .update(|item| {
            item.contributor = Some("octocat".to_owned());
            item.adopted = true;
        })
        .unwrap();
    until(&runner, |r| matches!(r, StepReport::Unreviewed { .. }));

    let question = merge_question(&runner);
    assert!(
        question.contains(KEPT_FAILING),
        "the ruling still names what it warns of: {question}"
    );
    assert!(
        question.contains("or the pull request is a contribution's"),
        "{question}"
    );
    assert_eq!(rig.ask(&runner, "status", None)["rulings"][0]["kind"]["head"], json!(head));
    assert_eq!(rig.forge.merges(), [], "the ruling holds the merge");
    assert!(pm_seen(&rig).is_empty(), "nothing is asked of the project manager");
}

// Steps until the gate raises a merge ruling, and returns what it asks
fn merge_question(runner: &Mutex<Runner>) -> String {
    let seen = until(runner, |r| matches!(r, StepReport::Ruling { .. }));
    ruling_report(seen.last().cloned()).1
}

// The current work item's plan, as a count of reads
fn plan_of(runner: &Mutex<Runner>) -> Option<u32> {
    let runner = runner.lock().unwrap();
    runner
        .current()
        .and_then(|item| item.review_plan.as_ref())
        .map(|plan| u32::from(plan.reads.get()))
}

// A wait that ends without a read wakes the project manager once, in words that
// say kelpie keeps trying
#[test]
fn the_wake_says_kelpie_starts_a_new_pass_each_wait() {
    let (rig, runner, _) = at_the_gate("unread-words", true, down(6));
    pm_says(&rig, [NOTHING]);
    waiting(&rig, &runner);
    rig.clock.advance(READ_AGAIN);
    until(&runner, |r| matches!(r, StepReport::Unreviewed { .. }));

    let [wake] = pm_seen(&rig).try_into().expect("one wake");
    assert!(
        wake.call.prompt.contains("Kelpie starts one new review pass every 30 minutes"),
        "{}",
        wake.call.prompt
    );
}
