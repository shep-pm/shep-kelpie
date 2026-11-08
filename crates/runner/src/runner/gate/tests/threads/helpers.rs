//! The fixtures and steps the open-thread tests share

use std::sync::Mutex;
use std::time::Instant;

use crate::ports::{CLEAN, Finding, Role, Severity, Standing};
use crate::runner::coderabbit::tests::now;
use crate::runner::review_bot::two_bots::listing;
use crate::runner::{Runner, StepReport, step};
use crate::test::{CALL_PATIENCE, Rig, Scripted, ScriptedRound, Seen, ticket};

use super::super::ruling_report;

pub(super) const NIT: &str = "P3: The name could be shorter. It reads badly.";
pub(super) const THREAD: &str = "PRRT_cubic_71_0";
pub(super) const DOLLAR: u64 = 1_000_000_000;
pub(super) const BACK_OFF: u64 = 30 * 60;

pub(super) const THREADS_HELD: &str = "its open review threads are with the project manager, so the merge \
                            waits on its answer";
pub(super) const TALK_UNREAD: &str = "what people wrote on its pull request cannot be read, so the merge \
                          waits for a read";
pub(super) const PM_DOWN: &str =
    "the project manager is down, so its open review threads wait for it";
pub(super) const PUSHED_BACK: &str = "the project manager pushed back on a review thread the maintainer \
                           wrote, so the merge waits for their reply";

pub(super) const NOTHING: &str = "{\"pick\": null}";
pub(super) const CLOSE: &str = "{\"threads\": [{\"item\": 7, \"thread\": 1, \"decision\": \"close\", \
                     \"why\": \"the caller checks it\"}]}";
pub(super) const FIX: &str = "{\"threads\": [{\"item\": 7, \"thread\": 1, \"decision\": \"fix\", \
                   \"why\": \"the guard is missing\"}]}";
pub(super) const FOLLOW_UP: &str = "{\"threads\": [{\"item\": 7, \"thread\": 1, \"decision\": \
                         \"follow-up\", \"why\": \"too big for this pull request\"}]}";
pub(super) const PUSH_BACK: &str = "{\"threads\": [{\"item\": 7, \"thread\": 1, \"decision\": \
                         \"push-back\", \"why\": \"the race is real\"}]}";

// A project naming `agents.pm`, listing cubic after its qwen and Claude rounds
pub(super) fn pm_project(project: &str) -> Rig {
    let rig = listing(project, &["cubic"]);
    rig.pm("pm");
    rig.allow(&["sonnet-high", "qwen", "claude", "cubic", "pm"]);
    rig
}

// #7's pull request 71, read clean by qwen and Claude, with cubic having reviewed
// the head on its own and left `titles` threads open on it. The item stands at the
// gate, which has not run yet, and the returned head is the pull request's.
pub(super) fn at_the_gate(rig: Rig, titles: &[&str]) -> (Rig, Mutex<Runner>, String) {
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
pub(super) fn open_bots(rig: &Rig, head: &str, titles: &[&str]) {
    if titles.is_empty() {
        return;
    }
    rig.forge
        .coderabbit
        .cubic_review(71, head, now(rig) + 60, titles);
    rig.clock.advance(60);
}

// Steps until the item waits at the gate on `reason`, keeping what each step reported
pub(super) fn waits_with(rig: &Rig, runner: &Mutex<Runner>, reason: &str) -> Vec<StepReport> {
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
pub(super) fn pm_seen(rig: &Rig) -> Vec<Seen> {
    rig.claude
        .all_seen()
        .into_iter()
        .filter(|s| s.call.role == Role::Pm)
        .collect()
}

// Waits until the project manager has been woken `n` times, and returns its wakes
pub(super) fn woken(rig: &Rig, n: usize) -> Vec<Seen> {
    let deadline = Instant::now() + CALL_PATIENCE;
    loop {
        let seen = pm_seen(rig);
        if seen.len() >= n {
            return seen;
        }
        if Instant::now() >= deadline {
            panic!(
                "the project manager was woken {} times, not {n}",
                seen.len()
            );
        }
        std::thread::yield_now();
    }
}

/// The worker's turns, in order
pub(super) fn worker(rig: &Rig) -> Vec<String> {
    rig.claude
        .all_calls()
        .into_iter()
        .filter(|c| c.role == Role::Worker)
        .map(|c| c.prompt)
        .collect()
}

/// Queues each `say` as the project manager's next answers, oldest first
pub(super) fn pm_says(rig: &Rig, says: impl IntoIterator<Item = &'static str>) {
    rig.claude
        .script_for(Role::Pm, says.into_iter().map(Scripted::Say));
}

/// Queues `steps` as the worker's next turns
pub(super) fn worker_says(rig: &Rig, steps: impl IntoIterator<Item = Scripted>) {
    rig.claude.script_for(Role::Worker, steps);
}

// Steps until a step reports what `wanted` matches, and returns every report it saw
pub(super) fn steps(
    runner: &Mutex<Runner>,
    wanted: impl Fn(&StepReport) -> bool,
) -> Vec<StepReport> {
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
pub(super) fn answers(runner: &Mutex<Runner>, n: usize) -> Vec<StepReport> {
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
pub(super) fn ruled(reports: &[StepReport]) -> (Vec<String>, Vec<String>) {
    for report in reports {
        if let StepReport::PmAnswered { acted, dropped, .. } = report {
            return (acted.clone(), dropped.clone());
        }
    }
    panic!("the project manager did not answer: {reports:?}")
}

// The gate's steps and the project manager's answer to them
pub(super) fn ruled_by(rig: &Rig, runner: &Mutex<Runner>, say: &'static str) -> Vec<StepReport> {
    pm_says(rig, [say]);
    answers(runner, 1)
}

// The merge ruling the gate raises, and what it says
pub(super) fn merge_ruling(runner: &Mutex<Runner>) -> String {
    let seen = steps(runner, |r| matches!(r, StepReport::Ruling { .. }));
    let (id, question) = ruling_report(seen.last().cloned());
    assert_eq!(id, ticket(71));
    question
}

// The maintainer opens a thread on the pull request, which the worker turns down,
// so it stays open: its id
pub(super) fn declined_thread(rig: &Rig, runner: &Mutex<Runner>) -> String {
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
pub(super) fn deferred(found: &[Finding]) -> String {
    let line = |f: &Finding| format!("HIGH|{}:{}|{}|{}\n", f.file, f.line, f.what, f.why);
    found.iter().map(line).collect()
}

// What kelpie replied on `THREAD`, last first
pub(super) fn last_reply(rig: &Rig) -> (String, String) {
    rig.forge
        .coderabbit
        .replies()
        .last()
        .cloned()
        .unwrap_or_default()
}

// #7's pull request, whose qwen round found `found` and whose worker fixed it,
// then read clean by Claude. The item stands at the gate with `found` still held.
pub(super) fn at_the_gate_found(rig: Rig, found: &[Finding]) -> (Rig, Mutex<Runner>, String) {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    runner.lock().unwrap().plan_two_reads_for_the_rig();
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.reviewer
        .script([ScriptedRound::Findings(found.to_vec())]);
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

// The finding the worker defers in the tests below
pub(super) fn racy() -> Finding {
    Finding::new(
        Severity::High,
        "src/lib.rs",
        9,
        "looks racy",
        "two threads write the same field",
    )
}

// The worker leaves `found` in its deferred findings file
pub(super) fn defers(rig: &Rig, found: &[Finding]) {
    std::fs::write(rig.build_7().join("deferred-findings.md"), deferred(found)).unwrap();
}

// The project manager's `fix` of the first two threads of a wake
pub(super) const FIX_BOTH: &str = "{\"threads\": [\
    {\"item\": 7, \"thread\": 1, \"decision\": \"fix\", \"why\": \"the guard is missing\"}, \
    {\"item\": 7, \"thread\": 2, \"decision\": \"fix\", \"why\": \"the race is real\"}]}";
