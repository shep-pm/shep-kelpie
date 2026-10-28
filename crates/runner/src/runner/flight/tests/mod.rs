//! Calls in flight, driven a pass at a time through the runner's stand-ins

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver};

use serde_json::json;

use super::*;
use crate::ports::{Cost, Session, Usage};
use crate::runner::answer;
use crate::runner::step;
use crate::test::ticket;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted};
use crate::ticket::TicketId;

// One hour, the rig's `turn_timeout`
pub(super) const CEILING: u64 = 3600;

// The runner's loop as a test drives it: one pass at a time, and each
// call's end recorded as it comes back.
pub(crate) struct Driven<'a> {
    runner: &'a Mutex<Runner>,
    woken: Receiver<()>,
}

impl<'a> Driven<'a> {
    pub(crate) fn new(runner: &'a Mutex<Runner>) -> Self {
        let (wake, woken) = mpsc::channel();
        lock(runner).wake_with(wake);
        Self { runner, woken }
    }

    pub(crate) fn pass(&self) -> Pass {
        advance(self.runner).unwrap()
    }

    pub(crate) fn started(&self) {
        let pass = self.pass();
        assert!(matches!(pass, Pass::Started), "no call started: {pass:?}");
    }

    // Waits for the next piece of news from a call in flight, and records it.
    pub(crate) fn heard(&self) -> Option<StepReport> {
        loop {
            let news = lock(self.runner).next_news();
            match news {
                Some(news) => return hear(self.runner, news).unwrap(),
                None => self
                    .woken
                    .recv_timeout(CALL_PATIENCE)
                    .expect("no call in flight had news"),
            }
        }
    }

    // Waits for the next call in flight to end, and records it.
    pub(crate) fn landed(&self) -> Option<StepReport> {
        loop {
            let news = lock(self.runner).next_news();
            match news {
                Some(news @ News::Ended { .. }) => return hear(self.runner, news).unwrap(),
                Some(news) => {
                    hear(self.runner, news).unwrap();
                }
                None => self
                    .woken
                    .recv_timeout(CALL_PATIENCE)
                    .expect("no call in flight ended"),
            }
        }
    }
}

// An implementer on a local model
pub(super) const CODER: &str = "---\nrole: implementer\nharness: stand-in\nmodel: qwen3-coder\n\
                     effort: low\nusage: none\n---\n";

// A running project with issue 7 open on `coder`, whose call queues at its
// model's lease until the returned hold is released, then does `then`
fn seven_queued(project: &str, then: Scripted) -> (Rig, Mutex<Runner>, Hold) {
    let rig = Rig::new(project);
    rig.write_agent("coder", CODER);
    rig.implementers(&["sonnet-high", "coder"]);
    let (rig, runner) = {
        let runner = rig.open().unwrap();
        rig.agent_board.ranks(&["coder"]);
        rig.ask(&runner, "add", Some("7"));
        rig.agent_board.ranks_listed();
        (rig, runner)
    };
    let queue = Hold::default();
    rig.claude
        .script([Scripted::Queued(queue.clone(), Box::new(then))]);
    (rig, runner, queue)
}

fn usage() -> Usage {
    Usage {
        input: 1,
        cache_write: 10,
        cache_write_5m: 0,
        cache_read: 100,
        output: 1000,
    }
}

// A running project with two slots, and issues 7 and 8 open in them
fn two_open(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "add", Some("8"));
    (rig, runner)
}

// A running project with issue 7 open
fn seven_open(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    (rig, runner)
}

fn turns(rig: &Rig, runner: &Mutex<Runner>) -> Vec<(u64, String)> {
    let status = rig.ask(runner, "status", None);
    let items = status["work_items"].as_array().unwrap();
    (items.iter())
        .map(|item| {
            let state = item["turn"]["state"].as_str().unwrap().to_owned();
            (
                item["issue"].as_str().unwrap().parse::<u64>().unwrap(),
                state,
            )
        })
        .collect()
}

fn ended_issue(report: Option<StepReport>) -> TicketId {
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
    assert!(seven.entered(CALL_PATIENCE), "#7's turn never began");
    assert!(
        eight.entered(CALL_PATIENCE),
        "#8's turn never began beside #7's"
    );
    assert!(matches!(driven.pass(), Pass::Idle), "a third call started");
    assert_eq!(
        turns(&rig, &runner),
        [(7, "running".to_owned()), (8, "running".to_owned())]
    );

    eight.release();
    assert_eq!(ended_issue(driven.landed()), ticket(8));
    assert!(!seven.returned(), "#8's end waited for #7's");
    assert_eq!(turns(&rig, &runner)[0], (7, "running".to_owned()));
    seven.release();
    assert_eq!(ended_issue(driven.landed()), ticket(7));
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
    assert!(seven.entered(CALL_PATIENCE) && eight.entered(CALL_PATIENCE));

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_items"].as_array().unwrap().len(), 2);
    assert!(rig.ask(&runner, "drain", None)["draining"].is_object());
    assert!(
        !seven.returned() && !eight.returned(),
        "a drain ended a call"
    );

    // A draining runner starts nothing, and still records the calls it had.
    seven.release();
    eight.release();
    let mut ended = vec![ended_issue(driven.landed()), ended_issue(driven.landed())];
    ended.sort_unstable();
    assert_eq!(ended, [ticket(7), ticket(8)]);
    assert!(matches!(driven.pass(), Pass::Idle));
    assert_eq!(rig.claude.calls().len(), 2);
}

// A pass reads `main`'s workflow runs with the lock let go, so no hold `status` shows is half as
// long as the read's wait. A pass's own work holds the lock for a moment, so the bound is loose.
#[test]
fn a_pass_that_waits_on_a_held_read_of_main_shows_no_hold_as_long_as_the_wait() {
    const HELD_FOR: Duration = Duration::from_secs(3);
    let (rig, runner) = seven_open("koji");
    let runner = Arc::new(runner);
    let read = rig.forge.hold_next_main_runs_read();

    std::thread::scope(|scope| {
        let pass = scope.spawn(|| advance(&runner));
        assert!(
            read.waiting(CALL_PATIENCE),
            "the pass never reached the read of `main`'s workflows"
        );
        std::thread::sleep(HELD_FOR);
        drop(read);
        pass.join().expect("the pass thread").unwrap();
    });

    let status = rig.ask(&runner, "status", None);
    let longest = status["lock"]
        .as_object()
        .expect("the holds by site")
        .values()
        .filter_map(serde_json::Value::as_f64)
        .fold(0.0, f64::max);
    assert!(longest < HELD_FOR.as_secs_f64() / 2.0, "{longest}");
}

#[test]
fn the_pacer_reads_usage_at_each_calls_start() {
    let (rig, runner) = two_open("acme");
    let seven = Hold::default();
    rig.claude.script([Scripted::Hold(seven.clone())]);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(seven.entered(CALL_PATIENCE));

    // The 5-hour window passes half while #7's turn runs.
    rig.meter.set(Rig::utilization(0, 60));
    assert!(
        matches!(driven.pass(), Pass::Report(StepReport::Held { .. })),
        "#8's turn started past the pacer"
    );
    assert_eq!(rig.claude.calls().len(), 1);
    assert!(!seven.returned(), "the hold ended the call in flight");
    seven.release();
    assert_eq!(ended_issue(driven.landed()), ticket(7));
}

#[test]
fn a_turn_past_its_ceiling_is_ended_and_parked() {
    let (rig, runner) = seven_open("acme");
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(hold.entered(CALL_PATIENCE));

    rig.clock.advance(CEILING - 1);
    assert!(matches!(driven.pass(), Pass::Idle));
    assert!(!hold.returned(), "the turn was ended before its ceiling");
    rig.clock.advance(1);
    assert!(matches!(driven.pass(), Pass::Idle));
    assert!(
        hold.answered(CALL_PATIENCE),
        "the turn ran on past its ceiling"
    );
    let Some(StepReport::TimedOut { issue, id, .. }) = driven.landed() else {
        panic!("a turn past its ceiling raised no ruling");
    };
    assert_eq!(issue, ticket(7));
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
    rig.ask(&runner, "rule", Some("7 yes"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(hold.entered(CALL_PATIENCE));

    rig.clock.advance(CEILING - 1);
    driven.pass();
    assert!(!hold.returned(), "the retry got less than a whole ceiling");
    rig.clock.advance(1);
    driven.pass();
    assert!(matches!(
       driven.landed(),
       Some(StepReport::TimedOut { issue, .. }) if issue == ticket(7)
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
    assert!(hold.entered(CALL_PATIENCE));
    rig.clock.advance(CEILING - 2000 - 1);
    driven.pass();
    assert!(!hold.returned(), "ended before the ceiling");
    rig.clock.advance(1);
    driven.pass();
    assert!(
        hold.answered(CALL_PATIENCE),
        "the restart reset the ceiling to a fresh hour"
    );
    assert!(matches!(
       driven.landed(),
       Some(StepReport::TimedOut { issue, .. }) if issue == ticket(7)
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
    assert!(seven.entered(CALL_PATIENCE) && eight.entered(CALL_PATIENCE));
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
    assert_eq!(ended, [ticket(7), ticket(8)]);
    let calls = rig.claude.calls();
    for issue in [7, 8] {
        let mine: Vec<_> = calls
            .iter()
            .filter(|c| c.issue == Some(crate::test::ticket(issue)))
            .collect();
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
    let hold = Hold::default();
    let (rig, runner, queue) = seven_queued("acme", Scripted::Hold(hold.clone()));
    let driven = Driven::new(&runner);
    driven.started();
    rig.clock.advance(3000);
    driven.pass();
    assert_eq!(rig.claude.calls(), [], "the turn ran before its lease");

    queue.release();
    assert_eq!(driven.heard(), None, "the grant is not a report");
    assert!(hold.entered(CALL_PATIENCE), "the turn never began");
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
       Some(StepReport::TimedOut { issue, .. }) if issue == ticket(7)
    ));
}

#[test]
fn a_turn_still_queued_at_its_ceiling_leaves_the_queue_without_running() {
    let (rig, runner, _queue) = seven_queued("shep", Scripted::Say("done"));
    let driven = Driven::new(&runner);
    driven.started();
    rig.clock.advance(CEILING);
    driven.pass();
    assert!(matches!(
       driven.landed(),
       Some(StepReport::TimedOut { issue, .. }) if issue == ticket(7)
    ));
    assert_eq!(rig.claude.calls(), [], "it spawned only to be ended");
}

#[test]
fn a_turn_carries_its_ceiling_and_where_it_stands_for_its_record() {
    let (rig, runner) = seven_open("acme");
    let started = crate::ports::Clock::now(&rig.clock);
    rig.claude.script([Scripted::Say("done")]);
    let driven = Driven::new(&runner);
    driven.started();
    driven.landed();

    let [seen] = rig.claude.seen().try_into().unwrap();
    assert_eq!(
        seen.carried,
        Some(Flying {
            left: CEILING,
            began: started,
            again: false,
            first: true,
            compact: false,
        })
    );
}

#[test]
fn a_turn_queued_for_its_lease_carries_the_grant_as_when_it_began() {
    let hold = Hold::default();
    let (rig, runner, queue) = seven_queued("acme", Scripted::Hold(hold.clone()));
    let started = crate::ports::Clock::now(&rig.clock);
    let driven = Driven::new(&runner);
    driven.started();
    rig.clock.advance(3000);
    driven.pass();

    queue.release();
    assert_eq!(driven.heard(), None);
    assert!(hold.entered(CALL_PATIENCE), "the turn never began");

    let [seen] = rig.claude.seen().try_into().unwrap();
    let carried = seen.carried.unwrap();
    assert_eq!(carried.began, Timestamp(started.0 + 3000));
    assert_eq!(carried.left, CEILING);
    hold.release();
}

// A step posts the call it starts in the lock scope that starts it, and a trigger
// refused the lock answers from that copy. The copy lists the call the pass just
// started, so `drain` never answers that nothing runs while one has.
#[test]
fn a_call_the_pass_started_is_in_the_copy_a_refused_trigger_reads() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let postings = runner.lock().unwrap().postings();
    let pass = rotation::begin_rotation(&runner).unwrap();
    assert!(matches!(pass, Pass::Started), "no call started: {pass:?}");
    // The lock, held as a step holds it, so the trigger is refused it.
    let held = runner.lock().unwrap();

    let reply = answer(&runner, &postings, "drain", None);
    drop(held);

    let reply: serde_json::Value = serde_json::from_str(&reply).expect("a JSON reply");
    assert_eq!(
        reply["draining"]["calls"],
        json!([{ "issue": "7", "role": "worker" }]),
        "the copy a refused trigger reads left the call the pass started out: {reply}"
    );
}

mod builds;
mod ended;
mod reads;
