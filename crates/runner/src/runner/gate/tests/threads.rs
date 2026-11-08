//! An open review thread holds the merge until the project manager rules on it,
//! through the runner's stand-ins

use std::sync::Mutex;
use std::time::Instant;

use serde_json::json;

use crate::board::READY;
use crate::ports::{CLEAN, Cost, Finding, Role, Severity, Standing};
use crate::runner::coderabbit::tests::now;
use crate::runner::flight::tests::Driven;
use crate::runner::review_bot::two_bots::listing;
use crate::runner::{Runner, StepReport, step};
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted, ScriptedRound, Seen, ticket};

use super::ruling_report;

const NIT: &str = "P3: The name could be shorter. It reads badly.";
const THREAD: &str = "PRRT_cubic_71_0";
const DOLLAR: u64 = 1_000_000_000;
const BACK_OFF: u64 = 30 * 60;

const THREADS_HELD: &str = "its open review threads are with the project manager, so the merge \
                            waits on its answer";
const PM_DOWN: &str = "the project manager is down, so its open review threads wait for it";
const PUSHED_BACK: &str = "the project manager pushed back on a review thread the maintainer \
                           wrote, so the merge waits for their reply";

const NOTHING: &str = "{\"pick\": null}";
const CLOSE: &str = "{\"threads\": [{\"item\": 7, \"thread\": 1, \"decision\": \"close\", \
                     \"why\": \"the caller checks it\"}]}";
const FIX: &str = "{\"threads\": [{\"item\": 7, \"thread\": 1, \"decision\": \"fix\", \
                   \"why\": \"the guard is missing\"}]}";
const FOLLOW_UP: &str = "{\"threads\": [{\"item\": 7, \"thread\": 1, \"decision\": \
                         \"follow-up\", \"why\": \"too big for this pull request\"}]}";
const PUSH_BACK: &str = "{\"threads\": [{\"item\": 7, \"thread\": 1, \"decision\": \
                         \"push-back\", \"why\": \"the race is real\"}]}";

// A project naming `agents.pm`, listing cubic after its qwen and Claude rounds
fn pm_project(project: &str) -> Rig {
    let rig = listing(project, &["cubic"]);
    rig.pm("pm");
    rig.allow(&["sonnet-high", "qwen", "claude", "cubic", "pm"]);
    rig
}

// #7's pull request 71, read clean by qwen and Claude, with cubic having reviewed
// the head on its own and left `titles` threads open on it. The item stands at the
// gate, which has not run yet, and the returned head is the pull request's.
fn at_the_gate(rig: Rig, titles: &[&str]) -> (Rig, Mutex<Runner>, String) {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    runner.lock().unwrap().plan_two_reads_for_the_rig();
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
    step(&runner).unwrap(); // the worker's first turn: opens the pull request
    step(&runner).unwrap(); // review round 1, qwen: clean by default
    step(&runner).unwrap(); // review round 2, Claude: scripted clean above
    let head = rig.forge.head_of("kelpie/7").expect("the worker pushed");
    open_bots(&rig, &head, titles);
    (rig, runner, head)
}

// cubic reads `head` on its own, opening one thread for each title
fn open_bots(rig: &Rig, head: &str, titles: &[&str]) {
    if titles.is_empty() {
        return;
    }
    rig.forge
        .coderabbit
        .cubic_review(71, head, now(rig) + 60, titles);
    rig.clock.advance(60);
}

// Steps until the item waits at the gate on `reason`, keeping what each step reported
fn waits_with(rig: &Rig, runner: &Mutex<Runner>, reason: &str) -> Vec<StepReport> {
    let deadline = Instant::now() + CALL_PATIENCE;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match step(runner).unwrap() {
            Some(report) => seen.push(report),
            None => std::thread::yield_now(),
        }
        let status = rig.ask(runner, "status", None);
        if status["work_item"]["held_up"]["because"].as_str() == Some(reason) {
            return seen;
        }
        assert!(
            status["rulings"].as_array().unwrap().is_empty(),
            "a ruling came while the thread waited: {status}"
        );
    }
    panic!("the item never waited on {reason}: {seen:?}");
}

/// The project manager's wakes, in order
fn pm_seen(rig: &Rig) -> Vec<Seen> {
    rig.claude
        .all_seen()
        .into_iter()
        .filter(|s| s.call.role == Role::Pm)
        .collect()
}

// Waits until the project manager has been woken `n` times, and returns its wakes
fn woken(rig: &Rig, n: usize) -> Vec<Seen> {
    let deadline = Instant::now() + CALL_PATIENCE;
    loop {
        let seen = pm_seen(rig);
        if seen.len() >= n {
            return seen;
        }
        if Instant::now() >= deadline {
            panic!("the project manager was woken {} times, not {n}", seen.len());
        }
        std::thread::yield_now();
    }
}

/// The worker's turns, in order
fn worker(rig: &Rig) -> Vec<String> {
    rig.claude
        .all_calls()
        .into_iter()
        .filter(|c| c.role == Role::Worker)
        .map(|c| c.prompt)
        .collect()
}

/// Queues each `say` as the project manager's next answers, oldest first
fn pm_says(rig: &Rig, says: impl IntoIterator<Item = &'static str>) {
    rig.claude
        .script_for(Role::Pm, says.into_iter().map(Scripted::Say));
}

/// Queues `steps` as the worker's next turns
fn worker_says(rig: &Rig, steps: impl IntoIterator<Item = Scripted>) {
    rig.claude.script_for(Role::Worker, steps);
}

// Steps until a step reports what `wanted` matches, and returns every report it saw
fn steps(runner: &Mutex<Runner>, wanted: impl Fn(&StepReport) -> bool) -> Vec<StepReport> {
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

// Steps until the project manager has answered `n` times, and returns every report it saw
fn answers(runner: &Mutex<Runner>, n: usize) -> Vec<StepReport> {
    let deadline = Instant::now() + CALL_PATIENCE;
    let mut seen = Vec::new();
    let mut answered = 0;
    while Instant::now() < deadline {
        match step(runner).unwrap() {
            Some(report) => {
                if matches!(report, StepReport::PmAnswered { .. }) {
                    answered += 1;
                }
                seen.push(report);
                if answered >= n {
                    return seen;
                }
            }
            None => std::thread::yield_now(),
        }
    }
    panic!("the project manager answered {answered} times, not {n}: {seen:?}")
}

// What the project manager's answer came to: what kelpie acted on and dropped
fn ruled(reports: &[StepReport]) -> (Vec<String>, Vec<String>) {
    for report in reports {
        if let StepReport::PmAnswered { acted, dropped, .. } = report {
            return (acted.clone(), dropped.clone());
        }
    }
    panic!("the project manager did not answer: {reports:?}")
}

// The gate's steps and the project manager's answer to them
fn ruled_by(rig: &Rig, runner: &Mutex<Runner>, say: &'static str) -> Vec<StepReport> {
    pm_says(rig, [say]);
    answers(runner, 1)
}

// The merge ruling the gate raises, and what it says
fn merge_ruling(runner: &Mutex<Runner>) -> String {
    let seen = steps(runner, |r| matches!(r, StepReport::Ruling { .. }));
    let (id, question) = ruling_report(seen.last().cloned());
    assert_eq!(id, ticket(71));
    question
}

// The maintainer opens a thread on the pull request, which the worker turns down,
// so it stays open: its id
fn declined_thread(rig: &Rig, runner: &Mutex<Runner>) -> String {
    let id = rig.forge.coderabbit.person_opens(
        71,
        "work.txt",
        "octocat",
        Standing::Writer,
        "Rename `bleats`. It reads badly.",
    );
    worker_says(
        rig,
        [Scripted::PushSay(
            "other.txt",
            "other\n",
            "thread 1: declined: the name is public API",
        )],
    );
    steps(runner, |r| matches!(r, StepReport::Ended { .. }));
    id
}

// What the worker writes to defer `found`, in the deferred findings file's own format
fn deferred(found: &[Finding]) -> String {
    let line = |f: &Finding| format!("HIGH|{}:{}|{}|{}\n", f.file, f.line, f.what, f.why);
    found.iter().map(line).collect()
}

// What kelpie replied on `THREAD`, last first
fn last_reply(rig: &Rig) -> (String, String) {
    rig.forge
        .coderabbit
        .replies()
        .last()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn an_open_nit_holds_the_merge_and_asks_the_project_manager() {
    let (rig, runner, head) = at_the_gate(pm_project("threads-nit"), &[NIT]);
    pm_says(&rig, [NOTHING]);

    waits_with(&rig, &runner, THREADS_HELD);
    let [wake] = woken(&rig, 1).try_into().unwrap();
    assert!(
        wake.call
            .prompt
            .contains("1. work.txt:1 (cubic-dev-ai[bot], LOW): The name could be shorter."),
        "{}",
        wake.call.prompt
    );
    assert!(wake.call.prompt.contains(&head[..7]), "{}", wake.call.prompt);

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci", "it waits in CI");
    assert_eq!(status["rulings"], json!([]), "no merge ruling yet");

    step(&runner).unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"],
        json!([]),
        "a wake that rules on nothing raises no merge ruling either"
    );
    assert_eq!(
        pm_seen(&rig).len(),
        1,
        "the next ask waits out the item's back-off"
    );
}

#[test]
fn an_open_writers_thread_holds_the_merge_and_asks_the_project_manager() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-writer"), &[]);
    declined_thread(&rig, &runner);
    pm_says(&rig, [NOTHING]);

    waits_with(&rig, &runner, THREADS_HELD);
    let [wake] = woken(&rig, 1).try_into().unwrap();
    assert!(
        wake.call.prompt.contains("1. work.txt:3 (octocat,"),
        "{}",
        wake.call.prompt
    );
    assert!(
        wake.call.prompt.contains("Rename `bleats`."),
        "{}",
        wake.call.prompt
    );
    let line = wake.call.prompt.lines().find(|l| l.contains("1. work.txt:3"));
    assert!(
        !line.expect("the thread's line").contains("public API"),
        "the worker's answer is not the thread: {}",
        wake.call.prompt
    );
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn the_project_manager_cannot_close_the_maintainers_thread() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-close-refused"), &[]);
    declined_thread(&rig, &runner);

    let (_, dropped) = ruled(&ruled_by(&rig, &runner, CLOSE));

    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert!(
        dropped[0].starts_with("close #7 thread 1: the maintainer wrote in it"),
        "{dropped:?}"
    );
    let said = rig.forge.coderabbit.replies();
    assert!(
        !said.iter().any(|(_, body)| body.starts_with("Closed as wrong")),
        "{said:?}"
    );
    assert_eq!(rig.forge.coderabbit.resolved(), Vec::<String>::new());
}

#[test]
fn a_push_back_replies_and_holds_the_merge_until_the_maintainer_answers() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-push"), &[]);
    let id = declined_thread(&rig, &runner);

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, PUSH_BACK));
    assert_eq!(
        acted,
        ["ruled thread 1 of #7 as push-back: the race is real"],
        "{acted:?}"
    );
    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(last_reply(&rig), (id.clone(), "Pushed back: the race is real".to_owned()));
    assert!(
        !rig.forge.coderabbit.resolved().contains(&id),
        "a pushed-back thread stays open"
    );

    waits_with(&rig, &runner, PUSHED_BACK);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    assert_eq!(pm_seen(&rig).len(), 1, "the push-back is its whole answer");
}

#[test]
fn a_follow_up_files_an_issue_settles_the_thread_and_the_head_merges() {
    let rig = pm_project("threads-follow-up");
    rig.merge_auto();
    let (rig, runner, _) = at_the_gate(rig, &[NIT]);

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, FOLLOW_UP));
    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(
        acted,
        ["ruled thread 1 of #7 as follow-up: too big for this pull request"],
        "{acted:?}"
    );
    assert_eq!(
        last_reply(&rig),
        (
            THREAD.to_owned(),
            "Filed as follow-up #900: too big for this pull request".to_owned()
        )
    );
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(issue.labels, [READY], "the follow-up is ready for a worker");

    steps(&runner, |r| {
        matches!(
            r,
            StepReport::Finished {
                merged: true,
                pull_request: Some(71),
                ..
            }
        )
    });
}

#[test]
fn a_fix_runs_one_worker_turn_and_comes_back_to_the_gate() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-fix"), &[NIT]);
    worker_says(
        &rig,
        [Scripted::PushSay("shorter.txt", "shorter\n", "thread 1: fixed")],
    );

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, FIX));
    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(
        acted,
        ["ruled thread 1 of #7 as fix: the guard is missing"],
        "{acted:?}"
    );

    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert_eq!(item.threads_sent, [THREAD], "the ruled thread goes to the worker");
    assert_eq!(item.counts.fix_turns, 1);

    steps(&runner, |r| matches!(r, StepReport::FixPushed { .. }));

    let turns = worker(&rig);
    let [_, fix] = turns.as_slice() else {
        panic!("{turns:?}");
    };
    assert!(
        fix.contains("The project manager ruled on 1 open review thread(s) on your pull \
                      request #71"),
        "{fix}"
    );
    assert!(fix.contains("review-findings.md"), "{fix}");
    let file = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(file.contains("the guard is missing"), "{file}");

    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci",
        "the fix ends at the gate, with no new pass"
    );
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
    assert_eq!(
        rig.forge.coderabbit.replies(),
        [(THREAD.to_owned(), "Fixed.".to_owned())],
        "the worker's own answer is what kelpie posted"
    );
    assert_eq!(
        pm_seen(&rig).len(),
        1,
        "a fixed thread is not put to the project manager again"
    );
}

#[test]
fn a_close_replies_with_the_reason_settles_the_thread_and_the_head_merges() {
    let rig = pm_project("threads-close");
    rig.merge_auto();
    let (rig, runner, _) = at_the_gate(rig, &[NIT]);

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, CLOSE));
    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(
        acted,
        ["ruled thread 1 of #7 as close: the caller checks it"],
        "{acted:?}"
    );
    assert_eq!(
        rig.forge.coderabbit.replies(),
        [(
            THREAD.to_owned(),
            "Closed as wrong: the caller checks it".to_owned()
        )]
    );
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);

    steps(&runner, |r| {
        matches!(
            r,
            StepReport::Finished {
                merged: true,
                pull_request: Some(71),
                ..
            }
        )
    });
}

#[test]
fn a_close_files_nothing() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-close-nothing"), &[NIT]);
    ruled_by(&rig, &runner, CLOSE);

    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert_eq!(item.follow_ups, None, "a close files no follow-up");
    assert_eq!(item.threads_accepted, [THREAD]);
    assert_eq!(rig.forge.coderabbit.labels(900), Vec::<String>::new());

    let question = merge_ruling(&runner);
    assert!(question.contains("into main?"), "{question}");
}

#[test]
fn a_close_whose_reply_is_refused_still_settles_the_thread() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-close-guard"), &[NIT]);
    let say: &'static str = Box::leak(
        String::from(
            "{\"threads\": [{\"item\": 7, \"thread\": 1, \"decision\": \"close\", \
             \"why\": \"the guard sits in /Users/other/work.txt\"}]}",
        )
        .into_boxed_str(),
    );

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, say));

    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(acted.len(), 1, "{acted:?}");
    assert_eq!(
        rig.forge.coderabbit.replies(),
        [(
            THREAD.to_owned(),
            "The project manager's reason names something on the machine kelpie runs on, \
             so it is not posted."
                .to_owned()
        )]
    );
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert_eq!(item.threads_accepted, [THREAD]);
}

#[test]
fn a_declined_thread_closed_as_wrong_is_not_asked_again() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-declined"), &[NIT]);
    worker_says(
        &rig,
        [Scripted::PushSay(
            "shorter.txt",
            "shorter\n",
            "thread 1: declined: the name is public API",
        )],
    );
    pm_says(&rig, [FIX, CLOSE]);

    answers(&runner, 2);

    assert_eq!(
        rig.forge.coderabbit.replies(),
        [
            (
                THREAD.to_owned(),
                "Declined: the name is public API".to_owned()
            ),
            (
                THREAD.to_owned(),
                "Closed as wrong: the caller checks it".to_owned()
            ),
        ]
    );
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert_eq!(item.threads_accepted, [THREAD], "the close settles it");
    assert_eq!(pm_seen(&rig).len(), 2, "the settled thread is asked for no more");
}

#[test]
fn a_thread_with_no_answer_stays_open_and_is_asked_again() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-no-answer"), &[NIT]);
    worker_says(&rig, [Scripted::Push("shorter.txt", "shorter\n")]);
    pm_says(&rig, [FIX, NOTHING]);

    answers(&runner, 2);

    let replies = rig.forge.coderabbit.replies();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert!(
        replies[0].1.starts_with("Not addressed:"),
        "kelpie says the worker left it: {replies:?}"
    );
    assert_eq!(
        rig.forge.coderabbit.resolved(),
        Vec::<String>::new(),
        "the thread stays open"
    );
    assert_eq!(pm_seen(&rig).len(), 2, "the open thread is put to it again");
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn the_item_waits_with_the_project_manager_down_and_raises_no_ruling() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-down"), &[NIT]);
    rig.claude.script_for(
        Role::Pm,
        [Scripted::Fail(crate::ports::AgentError::Failed(
            crate::settings::Harness::ClaudeCode,
            "overloaded".into(),
        ))],
    );

    steps(&runner, |r| matches!(r, StepReport::PmFailed { .. }));

    waits_with(&rig, &runner, PM_DOWN);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    assert_eq!(
        pm_seen(&rig).len(),
        1,
        "a project manager that is down is not woken again"
    );
}

#[test]
fn a_restart_with_the_ask_out_asks_again() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-restart"), &[NIT]);
    let hold = Hold::default();
    rig.claude
        .script_for(Role::Pm, [Scripted::Hold(hold.clone()), Scripted::Say(NOTHING)]);
    let driven = Driven::new(&runner);
    driven.started(); // the gate holds the merge and wakes the project manager

    assert!(hold.entered(CALL_PATIENCE), "the wake's call never began");
    assert!(
        runner.lock().unwrap().pm_threads_out(ticket(7)),
        "the ask is out"
    );

    drop(driven);
    drop(runner);
    rig.claude.stop();
    let runner = rig.open().unwrap();
    answers(&runner, 1);

    assert_eq!(pm_seen(&rig).len(), 2, "a restart asks about the thread again");
    step(&runner).unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["held_up"]["because"].as_str(),
        Some(THREADS_HELD),
        "the merge still waits on the project manager"
    );
}

#[test]
fn a_refused_filing_retries_and_raises_no_ruling() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-refused"), &[NIT]);
    rig.forge.set_issues_down(true);

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, FOLLOW_UP));

    assert_eq!(acted.len(), 1, "{acted:?}");
    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert!(dropped[0].contains("issues are down"), "{dropped:?}");
    let logged = runner.lock().unwrap().take_started();
    let Some(StepReport::GateFailed { issue, reason }) = logged.last() else {
        panic!("the refusal was not logged as a step: {logged:?}");
    };
    assert_eq!(*issue, ticket(7));
    assert!(reason.contains("issues are down"), "{reason}");
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"],
        json!([]),
        "a refusal before the merge raises no follow-up ruling"
    );
    assert_eq!(
        rig.forge.coderabbit.resolved(),
        Vec::<String>::new(),
        "the thread stays open"
    );

    rig.clock.advance(BACK_OFF);
    rig.forge.set_issues_down(false);
    ruled_by(&rig, &runner, FOLLOW_UP);

    assert_eq!(rig.forge.created().len(), 1, "the filing is tried again");
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
}

#[test]
fn fix_after_fix_parks_on_over_budget_and_raises_no_merge_ruling() {
    let rig = pm_project("threads-budget");
    rig.edit_settings(|s| s.replace("per_item_usd = 10", "per_item_usd = 5"));
    let (rig, runner, _) = at_the_gate(rig, &[NIT]);
    worker_says(
        &rig,
        [Scripted::Billed(
            "thread 1: declined: the name is public API",
            Cost(6 * DOLLAR),
        )],
    );
    pm_says(&rig, [FIX, FIX]);

    answers(&runner, 2);
    let seen = steps(&runner, |r| matches!(r, StepReport::Failed { .. }));

    let StepReport::Failed { issue, id, question, .. } = seen.last().unwrap() else {
        panic!("{seen:?}");
    };
    assert_eq!(*issue, ticket(7));
    assert_eq!(*id, ticket(71), "the item parks on its own ruling");
    assert!(
        question.contains("has cost $6.00 against its cap of $5.00"),
        "{question}"
    );
    let status = rig.ask(&runner, "status", None);
    let rulings = status["rulings"].as_array().unwrap();
    assert_eq!(rulings.len(), 1, "{status}");
    assert_eq!(rulings[0]["kind"]["kind"], "stuck");
    assert_eq!(rulings[0]["kind"]["reason"], "over-budget");
}

#[test]
fn a_push_back_asks_nothing_more_until_the_maintainer_replies() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-push-wait"), &[]);
    let id = declined_thread(&rig, &runner);
    ruled_by(&rig, &runner, PUSH_BACK);

    waits_with(&rig, &runner, PUSHED_BACK);
    assert_eq!(pm_seen(&rig).len(), 1, "the push-back holds the ask back");
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));

    rig.forge
        .coderabbit
        .person_replies(&id, "octocat", Standing::Writer, "Fair. I will fix it myself.");
    worker_says(&rig, [Scripted::Push("theirs.txt", "theirs\n")]);
    pm_says(&rig, [NOTHING]);
    answers(&runner, 1);

    let seen = woken(&rig, 2);
    assert!(
        seen[1].call.prompt.contains("Rename `bleats`."),
        "the maintainer's reply puts the thread back to it: {}",
        seen[1].call.prompt
    );
    assert_eq!(
        rig.forge.coderabbit.resolved(),
        Vec::<String>::new(),
        "the thread stays open throughout"
    );
}

#[test]
fn each_wait_says_why_in_status() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-why-asked"), &[NIT]);
    pm_says(&rig, [NOTHING]);
    waits_with(&rig, &runner, THREADS_HELD);
    drop(runner);

    let (rig, runner, _) = at_the_gate(pm_project("threads-why-down"), &[NIT]);
    rig.claude.script_for(
        Role::Pm,
        [Scripted::Fail(crate::ports::AgentError::Failed(
            crate::settings::Harness::ClaudeCode,
            "overloaded".into(),
        ))],
    );
    steps(&runner, |r| matches!(r, StepReport::PmFailed { .. }));
    waits_with(&rig, &runner, PM_DOWN);
    drop(runner);

    let (rig, runner, _) = at_the_gate(pm_project("threads-why-back"), &[]);
    declined_thread(&rig, &runner);
    ruled_by(&rig, &runner, PUSH_BACK);
    waits_with(&rig, &runner, PUSHED_BACK);
}

#[test]
fn a_strangers_thread_holds_nothing_and_reaches_no_prompt() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-stranger"), &[]);
    rig.forge.coderabbit.person_opens(
        71,
        "work.txt",
        "passerby",
        Standing::Other,
        "This looks odd to me.",
    );

    let question = merge_ruling(&runner);
    assert!(
        !question.contains("looks odd"),
        "a stranger's thread is not the ruling's: {question}"
    );
    assert!(pm_seen(&rig).is_empty(), "no model is asked about it");
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes.iter().any(|n| n.contains("1 open thread on #71 held nothing")),
        "{notes:?}"
    );
}

#[test]
fn a_strangers_reply_in_a_writers_thread_reaches_no_prompt() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-stranger-reply"), &[NIT, NIT]);
    rig.forge.coderabbit.person_replies(
        THREAD,
        "passerby",
        Standing::Other,
        "I agree, this is wrong.",
    );
    pm_says(&rig, [CLOSE]);

    answers(&runner, 1);

    let [wake] = woken(&rig, 1).try_into().unwrap();
    assert!(
        wake.call.prompt.contains("The name could be shorter."),
        "{}",
        wake.call.prompt
    );
    assert!(
        !wake.call.prompt.contains("I agree"),
        "the stranger's words reach no prompt: {}",
        wake.call.prompt
    );
    let case = std::fs::read_to_string(rig.paths().pm.join("cases/7.md")).expect("a case file");
    assert!(!case.contains("I agree, this is wrong."), "{case}");
}

#[test]
fn a_project_with_no_project_manager_keeps_its_ruling_and_its_nits_hold_nothing() {
    let rig = {
        let rig = listing("threads-no-pm", &["cubic"]);
        rig.reviews_need(Some(2));
        rig
    };
    let (rig, runner, _) = at_the_gate(rig, &[NIT]);

    let question = merge_ruling(&runner);
    assert!(
        question.contains("1 nit left open (cubic)"),
        "the ruling names the nit and merges anyway: {question}"
    );
    assert!(pm_seen(&rig).is_empty(), "no project manager is woken");
}

#[test]
fn an_adopted_contribution_keeps_its_ruling_and_asks_nothing() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-adopted"), &[NIT]);
    runner
        .lock()
        .unwrap()
        .update(|item| item.contributor = Some("octocat".to_owned()))
        .unwrap();

    let question = merge_ruling(&runner);
    assert!(
        question.contains("1 nit left open (cubic)"),
        "its threads stay the maintainer's to read: {question}"
    );
    assert!(pm_seen(&rig).is_empty(), "nothing is asked of the project manager");
}

#[test]
fn deferred_findings_are_still_filed_at_the_merge_after_a_follow_up_before_it() {
    let found = Finding::new(
        Severity::High,
        "src/lib.rs",
        9,
        "looks racy",
        "two threads write the same field",
    );
    let rig = pm_project("threads-deferred");
    rig.merge_auto();
    let (rig, runner, head) = at_the_gate_found(rig, &[found.clone()]);
    std::fs::write(
        rig.build_7().join("deferred-findings.md"),
        deferred(&[found.clone()]),
    )
    .unwrap();
    open_bots(&rig, &head, &[NIT]);

    ruled_by(&rig, &runner, FOLLOW_UP);
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(issue.labels, [READY], "the follow-up is ready for a worker");

    let question = merge_ruling(&runner);
    assert_eq!(rig.forge.merges(), [(71, head)], "the head merged");
    assert!(
        question.contains("looks racy"),
        "the deferred finding is still there to file: {question}"
    );
}

// #7's pull request, whose qwen round found `found` and whose worker fixed it,
// then read clean by Claude. The item stands at the gate with `found` still held.
fn at_the_gate_found(rig: Rig, found: &[Finding]) -> (Rig, Mutex<Runner>, String) {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    runner.lock().unwrap().plan_two_reads_for_the_rig();
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.reviewer.script([ScriptedRound::Findings(found.to_vec())]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text(CLEAN),
    ]);
    for _ in 0..20 {
        step(&runner).unwrap();
        if rig.ask(&runner, "status", None)["work_item"]["phase"]["state"] == "ci" {
            break;
        }
    }
    let head = rig.forge.head_of("kelpie/7").expect("the worker pushed");
    (rig, runner, head)
}
