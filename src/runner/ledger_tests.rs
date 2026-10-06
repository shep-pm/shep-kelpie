//! The usage ledger through the runner's stand-ins: a line for every call
//! as it ends, a finished work item's summary, and a ledger that cannot be
//! written failing nothing

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};

use crate::ports::{AgentError, Clock, Cost, Usage};
use crate::runner::flight::advance;
use crate::runner::{CHECKS_SETTLE, Pass, Runner, StepReport, count_stopped, step};
use crate::settings::Harness;
use crate::test::{Hold, Rig, Scripted, ScriptedRound};
use crate::usage::{FILE, read};

// How long a test waits on a call in flight
const PATIENCE: Duration = Duration::from_secs(60);

// Every line of the rig's project's ledger, as JSON
fn ledger(rig: &Rig) -> Vec<Value> {
    let lines = read(&rig.paths().folder.join(FILE)).unwrap();
    lines
        .iter()
        .map(|line| serde_json::to_value(line).unwrap())
        .collect()
}

fn usd(dollars: f64) -> Cost {
    Cost::from_usd(dollars).unwrap()
}

#[test]
fn a_merged_work_item_leaves_a_line_for_each_call_and_one_for_itself() {
    let (rig, runner, _) = Rig::parked("koji");
    rig.ask(&runner, "rule", Some("1 yes"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);
    let finished = step(&runner).unwrap();
    assert!(
        matches!(finished, Some(StepReport::Finished { merged: true, .. })),
        "{finished:?}"
    );

    let lines = ledger(&rig);
    let [turn, round, session, item] = lines.as_slice() else {
        panic!("three calls and the item: {lines:#?}")
    };
    let session_id = rig.claude.all_seen()[0].call.session.id().0.clone();
    assert_eq!(
        turn,
        &json!({
            "line": "call",
            "at": Rig::EPOCH,
            "issue": 7,
            "pull_request": 71,
            "role": "worker",
            "kind": "turn",
            "agent": "sonnet-high",
            "harness": "claude-code",
            "model": "claude-sonnet-5-5",
            "effort": "high",
            "session": session_id,
            "usage": { "input": 0, "cache_write": 0, "cache_read": 0, "output": 0 },
            "units": 0,
            "cost_usd": 0.0,
            "session_cost_usd": 0.0,
            "seconds": 0,
            "ended": "answered",
            "pacer": {
                "claude": {
                    "at": Rig::EPOCH,
                    "session_pct": 0,
                    "session_resets_at": Rig::EPOCH + 5 * 3600,
                    "week_pct": 0,
                    "week_resets_at": Rig::EPOCH + 7 * Rig::DAY,
                },
            },
        })
    );
    assert_eq!(
        (&round["kind"], &round["agent"], &round["harness"]),
        (&json!("local-round"), &json!("qwen"), &json!("local"))
    );
    assert_eq!(
        (&round["gpu_seconds"], round.get("session"), &round["ended"]),
        (&json!(0), None, &json!("answered"))
    );
    assert_eq!(round.get("unpriced"), None, "a local round costs nothing");
    assert_eq!(
        (&session["role"], &session["kind"], &session["agent"]),
        (&json!("reviewer"), &json!("review"), &json!("claude"))
    );
    assert_eq!(session["pull_request"], 71);
    assert_eq!(
        (&item["line"], &item["issue"], &item["pull_request"]),
        (&json!("finished"), &json!(7), &json!(71))
    );
    assert_eq!(
        (&item["merged"], &item["review_rounds"], &item["rulings"]),
        (&json!(true), &json!(2), &json!(1))
    );
    assert_eq!(
        (&item["worker"]["calls"], &item["reviewer"]["calls"]),
        (&json!(1), &json!(1))
    );
    assert_eq!(item["qwen"], json!({ "rounds": 1, "seconds": 0 }));

    // The state file's record keeps the same.
    let state = std::fs::read_to_string(rig.paths().state).unwrap();
    let state: Value = serde_json::from_str(&state).unwrap();
    let spend = &state["history"][0]["spend"];
    assert_eq!(
        spend["counts"],
        json!({ "review_rounds": 2, "fix_turns": 0, "rulings": 1 })
    );
    assert_eq!(spend["reviewer"]["calls"], 1);
}

#[test]
fn a_turn_s_line_costs_the_change_in_its_session_and_carries_the_last_reading() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    let tokens = |n: u64| Usage {
        input: n,
        cache_write: 10 * n,
        cache_write_5m: 0,
        cache_read: 100 * n,
        output: n,
    };
    rig.claude.script([
        Scripted::Spend(Rig::utilization(30, 10), tokens(1), usd(1.0)),
        Scripted::Reply(tokens(2), usd(1.5)),
    ]);
    step(&runner).unwrap(); // the first turn: nothing pushed, so it is sent back
    rig.clock.advance(60);
    step(&runner).unwrap(); // the second turn, resuming the session

    let lines = ledger(&rig);
    let [first, second] = lines.as_slice() else {
        panic!("two turns: {lines:#?}")
    };
    assert_eq!(
        (
            &first["units"],
            &first["cost_usd"],
            &first["session_cost_usd"]
        ),
        (&json!(36), &json!(1.0), &json!(1.0))
    );
    assert_eq!(
        (
            &second["units"],
            &second["cost_usd"],
            &second["session_cost_usd"]
        ),
        (&json!(72), &json!(0.5), &json!(1.5))
    );
    assert_eq!(first["session"], second["session"]);
    assert_eq!(
        second["pacer"]["claude"],
        json!({
            "at": Rig::EPOCH + 60,
            "session_pct": 10,
            "session_resets_at": Rig::EPOCH + 5 * 3600,
            "week_pct": 30,
            "week_resets_at": Rig::EPOCH + 7 * Rig::DAY,
        }),
        "the reading taken before the second turn"
    );
}

// A running project whose settings name the project manager `pm`
fn with_pm(rig: &Rig) -> Mutex<Runner> {
    rig.edit_settings(|s| {
        let listed = "\nimplementers = [\"sonnet-high\"]\n";
        assert!(s.contains(listed), "the example's implementers moved");
        s.replace(listed, "\nimplementers = [\"sonnet-high\"]\npm = \"pm\"\n")
    });
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    runner
}

#[test]
fn the_project_manager_s_wakes_and_compaction_cost_what_its_session_grew_across_a_restart() {
    let rig = Rig::new("reactmap");
    let runner = with_pm(&rig);
    rig.ask(&runner, "tell", Some("first"));
    rig.claude.script([
        Scripted::SayAt("{\"pick\": null}", 100_001),
        Scripted::Billed("compacted", usd(0.4)),
        Scripted::Billed("{\"pick\": null}", usd(0.5)),
    ]);
    step(&runner).unwrap(); // a wake
    let compacted = step(&runner).unwrap();
    assert!(
        matches!(compacted, Some(StepReport::PmCompacted { .. })),
        "{compacted:?}"
    );
    drop(runner);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "tell", Some("second"));
    step(&runner).unwrap(); // a wake, resuming the same session after the restart

    let lines = ledger(&rig);
    let read = |line: &Value| {
        (
            line["kind"].clone(),
            line["cost_usd"].clone(),
            line["agent"].clone(),
            line["issue"].clone(),
        )
    };
    assert_eq!(
        lines.iter().map(read).collect::<Vec<_>>(),
        [
            (json!("wake"), json!(0.0), json!("pm"), Value::Null),
            (json!("compact"), json!(0.4), json!("pm"), Value::Null),
            (json!("wake"), json!(0.1), json!("pm"), Value::Null),
        ]
    );
    assert!(lines.iter().all(|line| line["role"] == "pm"));
}

#[test]
fn a_reviewer_s_session_keeps_how_it_ended_and_one_that_never_started_has_no_line() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Fail(AgentError::TimedOut(Harness::ClaudeCode)),
        Scripted::Fail(AgentError::Unreadable(Harness::ClaudeCode, "{".into())),
        Scripted::Fail(AgentError::Spawn(Harness::ClaudeCode, "no claude".into())),
    ]);
    step(&runner).unwrap(); // the worker's turn
    step(&runner).unwrap(); // qwen: clean by default
    step(&runner).unwrap(); // claude: timed out
    step(&runner).unwrap(); // claude again: unreadable
    step(&runner).unwrap(); // claude again: never started, so no line

    let ended: Vec<_> = ledger(&rig).iter().map(|l| l["ended"].clone()).collect();
    assert_eq!(ended, ["answered", "answered", "timed-out", "unreadable"]);
    let state = std::fs::read_to_string(rig.paths().state).unwrap();
    let state: Value = serde_json::from_str(&state).unwrap();
    assert_eq!(
        state["work_items"][0]["counts"]["reviewer_unreported"], 2,
        "the two that reached a model and reported nothing"
    );
}

#[test]
fn a_call_still_in_flight_as_the_runner_stops_is_written_as_stopped() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(PATIENCE), "the turn never began");
    rig.clock.advance(30);

    // The stop writes it without the runner's lock, as when a pass holds it.
    let stopping = runner.lock().unwrap().stopping();
    let held = runner.lock().unwrap();
    rig.claude.stop();
    let stopped = stopping.record(crate::ports::Clock::now(&rig.clock));
    drop(held);
    assert_eq!(stopped, [(7, crate::ports::Role::Worker)]);
    count_stopped(&runner, &stopped);
    assert_eq!(
        stopping.record(crate::ports::Clock::now(&rig.clock)),
        [],
        "each call is written once"
    );

    let lines = ledger(&rig);
    let [line] = lines.as_slice() else {
        panic!("one line for the turn cut short: {lines:#?}")
    };
    assert_eq!(
        (
            &line["kind"],
            &line["ended"],
            &line["seconds"],
            &line["units"]
        ),
        (&json!("turn"), &json!("stopped"), &json!(30), &json!(0))
    );
    assert_eq!(
        (&line["issue"], &line["agent"]),
        (&json!(7), &json!("sonnet-high"))
    );
    let state = std::fs::read_to_string(rig.paths().state).unwrap();
    let state: Value = serde_json::from_str(&state).unwrap();
    assert_eq!(
        state["work_items"][0]["counts"]["worker_unreported"], 1,
        "the finished tally will count it"
    );
}

#[test]
fn a_local_round_cut_short_by_the_stop_keeps_its_seconds_on_the_gpu() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's turn
    let stopping = runner.lock().unwrap().stopping();
    let (queued, running) = (Hold::default(), Hold::default());
    rig.reviewer.script([ScriptedRound::Queued {
        queued: queued.clone(),
        running: running.clone(),
    }]);
    std::thread::scope(|scope| {
        let round = scope.spawn(|| step(&runner));
        assert!(queued.entered(PATIENCE), "the round never queued");
        rig.clock.advance(120);
        queued.release();
        assert!(running.entered(PATIENCE), "the round never ran");
        rig.clock.advance(45);
        assert_eq!(stopping.record(Clock::now(&rig.clock)), []);
        running.release();
        round.join().unwrap().unwrap();
    });
    let lines = ledger(&rig);
    let [_, round] = lines.as_slice() else {
        panic!("the turn and the round, once: {lines:#?}")
    };
    assert_eq!(
        (
            &round["kind"],
            &round["ended"],
            &round["seconds"],
            &round["gpu_seconds"]
        ),
        (
            &json!("local-round"),
            &json!("stopped"),
            &json!(165),
            &json!(45)
        )
    );
}

#[test]
fn a_call_that_never_started_is_not_written_by_the_stop_either() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    let stopping = runner.lock().unwrap().stopping();
    let (wake, woken) = std::sync::mpsc::channel();
    runner.lock().unwrap().wake_with(wake);
    let no_claude = AgentError::Spawn(Harness::ClaudeCode, "no claude".into());
    rig.claude.script([Scripted::Fail(no_claude)]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    woken
        .recv_timeout(PATIENCE)
        .expect("the call's end, unheard");
    assert_eq!(stopping.record(Clock::now(&rig.clock)), []);
    assert_eq!(ledger(&rig), Vec::<Value>::new());
}

#[test]
fn a_call_whose_thread_panicked_is_written_as_failed() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Kill]);
    let panicked = catch_unwind(AssertUnwindSafe(|| step(&runner)));
    assert!(panicked.is_err(), "the call's panic reaches the runner");
    let lines = ledger(&rig);
    let [line] = lines.as_slice() else {
        panic!("one line: {lines:#?}")
    };
    assert_eq!(
        (&line["kind"], &line["ended"]),
        (&json!("turn"), &json!("failed"))
    );
}

#[test]
fn a_ledger_that_cannot_be_written_leaves_the_turn_recorded() {
    let rig = Rig::new("koji");
    // A folder where the ledger would be, so no line can be added.
    std::fs::create_dir_all(rig.paths().folder.join(FILE)).unwrap();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.claude
        .script([Scripted::Reply(Usage::default(), usd(1.0))]);
    step(&runner).unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["by_role"]["worker"]["calls"], 1);
}
