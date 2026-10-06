//! Calls in flight, driven a pass at a time through the runner's stand-ins

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::lease::gpu::{Attempt, Claim, GpuLock};
use crate::ports::{Cost, Session, Usage};
use crate::runner::step;
use crate::test::{Hold, Rig, Scripted};

// Real threads on real time, so every wait has this ceiling.
const PATIENCE: Duration = Duration::from_secs(30);

// One hour, the rig's `turn_timeout`
const CEILING: u64 = 3600;

// The runner's loop as a test drives it: one pass at a time, and each
// call's end recorded as it comes back.
struct Driven<'a> {
    runner: &'a Mutex<Runner>,
    woken: Receiver<()>,
}

impl<'a> Driven<'a> {
    fn new(runner: &'a Mutex<Runner>) -> Self {
        let (wake, woken) = mpsc::channel();
        lock(runner).wake_with(wake);
        Self { runner, woken }
    }

    fn pass(&self) -> Pass {
        advance(self.runner).unwrap()
    }

    fn started(&self) {
        let pass = self.pass();
        assert!(matches!(pass, Pass::Started), "no call started: {pass:?}");
    }

    // Waits for the next piece of news from a call in flight, and records it.
    fn heard(&self) -> Option<StepReport> {
        loop {
            let news = lock(self.runner).next_news();
            match news {
                Some(news) => return hear(self.runner, news).unwrap(),
                None => self
                    .woken
                    .recv_timeout(PATIENCE)
                    .expect("no call in flight had news"),
            }
        }
    }

    // Waits for the next call in flight to end, and records it.
    fn landed(&self) -> Option<StepReport> {
        loop {
            let news = lock(self.runner).next_news();
            match news {
                Some(news @ News::Ended { .. }) => return hear(self.runner, news).unwrap(),
                Some(news) => {
                    hear(self.runner, news).unwrap();
                }
                None => self
                    .woken
                    .recv_timeout(PATIENCE)
                    .expect("no call in flight ended"),
            }
        }
    }
}

// An implementer on the GPU, which waits for the `gpu` lease
const CODER: &str = "---\nrole: implementer\nharness: stand-in\nmodel: qwen3-coder\n\
                     effort: low\nusage: none\n---\n";

// A running project with issue 7 open on `coder`, and the GPU lock held by
// someone else, released when the returned claim is
fn seven_queued_for_the_gpu(project: &str) -> (Rig, Mutex<Runner>, GpuLock, Claim) {
    let rig = Rig::new(project);
    rig.write_agent("coder", CODER);
    rig.implementers(&["sonnet-high", "coder"]);
    rig.forge.label(7, "agent:coder");
    let (rig, runner) = {
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        (rig, runner)
    };
    let lock = GpuLock::under(&rig.home.path().join("tmp"));
    let round = Claim {
        pid: std::process::id(),
        what: "qwen-review round 1".into(),
    };
    assert_eq!(lock.try_take(&round).unwrap(), Attempt::Taken);
    (rig, runner, lock, round)
}

fn usage() -> Usage {
    Usage {
        input: 1,
        cache_write: 10,
        cache_read: 100,
        output: 1000,
    }
}

// A running project with two slots, and issues 7 and 8 open in them
fn two_open(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    rig.edit_settings(|s| s.replace("max_items = 1", "max_items = 2"));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "add", Some("8"));
    (rig, runner)
}

// A running project with issue 7 open
fn seven_open(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    (rig, runner)
}

fn turns(rig: &Rig, runner: &Mutex<Runner>) -> Vec<(u64, String)> {
    let status = rig.ask(runner, "status", None);
    let items = status["work_items"].as_array().unwrap();
    (items.iter())
        .map(|item| {
            let state = item["turn"]["state"].as_str().unwrap().to_owned();
            (item["issue"].as_u64().unwrap(), state)
        })
        .collect()
}

fn ended_issue(report: Option<StepReport>) -> u64 {
    match report {
        Some(StepReport::Ended { issue, .. }) => issue,
        other => panic!("no turn ended: {other:?}"),
    }
}

#[test]
fn two_work_items_calls_run_at_once_and_end_in_their_own_order() {
    let (rig, runner) = two_open("acme");
    let (seven, eight) = (Hold::default(), Hold::default());
    rig.claude
        .script([Scripted::Hold(seven.clone()), Scripted::Hold(eight.clone())]);
    let driven = Driven::new(&runner);
    driven.started();
    driven.started();
    assert!(seven.entered(PATIENCE), "#7's turn never began");
    assert!(eight.entered(PATIENCE), "#8's turn never began beside #7's");
    assert!(matches!(driven.pass(), Pass::Idle), "a third call started");
    assert_eq!(
        turns(&rig, &runner),
        [(7, "running".to_owned()), (8, "running".to_owned())]
    );

    eight.release();
    assert_eq!(ended_issue(driven.landed()), 8);
    assert!(!seven.returned(), "#8's end waited for #7's");
    assert_eq!(turns(&rig, &runner)[0], (7, "running".to_owned()));
    seven.release();
    assert_eq!(ended_issue(driven.landed()), 7);
    assert_eq!(rig.claude.calls().len(), 2);
}

#[test]
fn triggers_are_answered_while_calls_run() {
    let (rig, runner) = two_open("shep");
    let (seven, eight) = (Hold::default(), Hold::default());
    rig.claude
        .script([Scripted::Hold(seven.clone()), Scripted::Hold(eight.clone())]);
    let driven = Driven::new(&runner);
    driven.started();
    driven.started();
    assert!(seven.entered(PATIENCE) && eight.entered(PATIENCE));

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_items"].as_array().unwrap().len(), 2);
    assert_eq!(rig.ask(&runner, "pause", None)["run"], "paused");
    assert!(
        !seven.returned() && !eight.returned(),
        "a pause ended a call"
    );

    // A paused project starts nothing, and still records the calls it had.
    seven.release();
    eight.release();
    let mut ended = vec![ended_issue(driven.landed()), ended_issue(driven.landed())];
    ended.sort_unstable();
    assert_eq!(ended, [7, 8]);
    assert!(matches!(driven.pass(), Pass::Idle));
    assert_eq!(rig.claude.calls().len(), 2);
}

#[test]
fn the_pacer_reads_usage_at_each_calls_start() {
    let (rig, runner) = two_open("acme");
    let seven = Hold::default();
    rig.claude.script([Scripted::Hold(seven.clone())]);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(seven.entered(PATIENCE));

    // The 5-hour window passes half while #7's turn runs.
    rig.meter.set(Rig::utilization(0, 60));
    assert!(
        matches!(driven.pass(), Pass::Report(StepReport::Held { .. })),
        "#8's turn started past the pacer"
    );
    assert_eq!(rig.claude.calls().len(), 1);
    assert!(!seven.returned(), "the hold ended the call in flight");
    seven.release();
    assert_eq!(ended_issue(driven.landed()), 7);
}

#[test]
fn a_turn_past_its_ceiling_is_ended_and_parked() {
    let (rig, runner) = seven_open("acme");
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(hold.entered(PATIENCE));

    rig.clock.advance(CEILING - 1);
    assert!(matches!(driven.pass(), Pass::Idle));
    assert!(!hold.returned(), "the turn was ended before its ceiling");
    rig.clock.advance(1);
    assert!(matches!(driven.pass(), Pass::Idle));
    assert!(hold.answered(PATIENCE), "the turn ran on past its ceiling");
    let Some(StepReport::TimedOut { issue: 7, id, .. }) = driven.landed() else {
        panic!("a turn past its ceiling raised no ruling");
    };
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({ "state": "ruling", "id": id })
    );
}

#[test]
fn a_retried_turn_gets_a_whole_ceiling_however_long_the_ruling_waited() {
    let (rig, runner) = seven_open("shep");
    rig.claude.script([Scripted::Fail(AgentError::Failed(
        crate::settings::Harness::ClaudeCode,
        "overloaded".into(),
    ))]);
    step(&runner).unwrap();
    rig.clock.advance(2000);
    rig.ask(&runner, "rule", Some("1 yes"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(hold.entered(PATIENCE));

    rig.clock.advance(CEILING - 1);
    driven.pass();
    assert!(!hold.returned(), "the retry got less than a whole ceiling");
    rig.clock.advance(1);
    driven.pass();
    assert!(matches!(
        driven.landed(),
        Some(StepReport::TimedOut { issue: 7, .. })
    ));
}

#[test]
fn a_restart_before_the_ceiling_passes_resumes_with_only_the_time_left() {
    let (rig, runner) = seven_open("acme");
    rig.claude.script([Scripted::Kill]);
    let _ = catch_unwind(AssertUnwindSafe(|| step(&runner)));
    drop(runner);

    rig.clock.advance(2000);
    let runner = rig.open().unwrap();
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(hold.entered(PATIENCE));
    rig.clock.advance(CEILING - 2000 - 1);
    driven.pass();
    assert!(!hold.returned(), "ended before the ceiling");
    rig.clock.advance(1);
    driven.pass();
    assert!(
        hold.answered(PATIENCE),
        "the restart reset the ceiling to a fresh hour"
    );
    assert!(matches!(
        driven.landed(),
        Some(StepReport::TimedOut { issue: 7, .. })
    ));
}

#[test]
fn a_restart_with_calls_in_flight_resumes_each_work_item_from_its_session() {
    let (rig, runner) = two_open("shep");
    let (seven, eight) = (Hold::default(), Hold::default());
    rig.claude
        .script([Scripted::Hold(seven.clone()), Scripted::Hold(eight.clone())]);
    let driven = Driven::new(&runner);
    driven.started();
    driven.started();
    assert!(seven.entered(PATIENCE) && eight.entered(PATIENCE));
    // The runner goes with both calls in flight, as a killed one would.
    drop(driven);
    drop(runner);

    let runner = rig.open().unwrap();
    assert_eq!(
        turns(&rig, &runner),
        [(7, "running".to_owned()), (8, "running".to_owned())]
    );
    rig.claude.script([
        Scripted::Reply(usage(), Cost(1)),
        Scripted::Reply(usage(), Cost(1)),
    ]);
    let mut ended = vec![
        ended_issue(step(&runner).unwrap()),
        ended_issue(step(&runner).unwrap()),
    ];
    ended.sort_unstable();
    assert_eq!(ended, [7, 8]);
    let calls = rig.claude.calls();
    for issue in [7, 8] {
        let mine: Vec<_> = calls.iter().filter(|c| c.issue == issue).collect();
        let [first, again] = mine.as_slice() else {
            panic!("#{issue} made {} calls, not two", mine.len());
        };
        assert_eq!(again.session, Session::Resume(first.session.id().clone()));
        assert_eq!(again.prompt, crate::runner::turn::CONTINUE);
        assert_eq!(again.cwd, first.cwd);
    }
    seven.release();
    eight.release();
}

#[test]
fn a_turn_queued_for_its_lease_gets_its_whole_ceiling_from_the_grant() {
    let (rig, runner, lock, round) = seven_queued_for_the_gpu("acme");
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    let driven = Driven::new(&runner);
    driven.started();
    rig.clock.advance(3000);
    driven.pass();
    assert_eq!(rig.claude.calls(), [], "the turn ran before its lease");

    lock.release(round.pid).unwrap();
    assert_eq!(driven.heard(), None, "the grant is not a report");
    assert!(hold.entered(PATIENCE), "the turn never began");
    rig.clock.advance(CEILING - 1);
    driven.pass();
    assert!(
        !hold.returned(),
        "the queue's wait counted against the turn"
    );
    rig.clock.advance(1);
    driven.pass();
    assert!(matches!(
        driven.landed(),
        Some(StepReport::TimedOut { issue: 7, .. })
    ));
}

#[test]
fn a_turn_still_queued_at_its_ceiling_leaves_the_queue_without_running() {
    let (rig, runner, lock, round) = seven_queued_for_the_gpu("shep");
    let driven = Driven::new(&runner);
    driven.started();
    rig.clock.advance(CEILING);
    driven.pass();
    assert!(matches!(
        driven.landed(),
        Some(StepReport::TimedOut { issue: 7, .. })
    ));
    assert_eq!(rig.claude.calls(), [], "it spawned only to be ended");
    let holder = lock.holder().expect("the other claim still holds the GPU");
    assert_eq!(holder.what, round.what);
    lock.release(round.pid).unwrap();
}
