use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use shep_handover::HandOverError;

use super::super::{Hooks, Worker};
use super::*;
use crate::ports::{Adopted, Flying, Kept, KeptServer, SessionId, Timestamp};
use crate::runner::{CARRY_WAIT, NotCarried, Pass, advance};
use crate::settings::Harness;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted, ticket};
use crate::ticket::TicketId;

// What a handover that did not happen leaves to look at
struct Tried {
    execs: Arc<AtomicUsize>,
    carried: Arc<Mutex<Vec<TicketId>>>,
}

// A handing whose exec refuses with `why` and notes what it was to carry,
// while `feeding` says whether a child's stdin is being written.
fn refusing(why: &'static str, feeding: &Arc<AtomicBool>) -> (Arc<Handing>, Tried) {
    let tried = Tried {
        execs: Arc::default(),
        carried: Arc::default(),
    };
    let (execs, carried) = (Arc::clone(&tried.execs), Arc::clone(&tried.carried));
    let feeding = Arc::clone(feeding);
    let handing = Handing::with(
        Box::new(move |carry| {
            execs.fetch_add(1, Ordering::SeqCst);
            *carried.lock().unwrap() = carry.turns.iter().map(|t| t.ticket()).collect();
            why.to_owned()
        }),
        Box::new(|| Some(Box::new(()))),
        Box::new(move || feeding.load(Ordering::SeqCst)),
    );
    (handing, tried)
}

// A worker whose first turn on issue 7 is held open by `hold`.
fn in_a_turn(
    rig: &Rig,
    hold: &Hold,
    handover: Arc<Handing>,
) -> (Arc<Mutex<Runner>>, Worker, Sender<()>) {
    let runner = Arc::new(rig.open().unwrap());
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Hold(hold.clone())]);
    let (wake, woken) = mpsc::channel();
    let (died, _) = mpsc::channel();
    let hooks = Hooks {
        on_wake: Box::new(|_| {}),
        finished: Box::new(|_| {}),
        handover,
    };
    let worker = Worker::spawn(Arc::clone(&runner), hooks, wake.clone(), woken, died);
    assert!(hold.entered(CALL_PATIENCE), "the worker's turn never began");
    (runner, worker, wake)
}

fn turn_of_7(rig: &Rig, runner: &Mutex<Runner>) -> String {
    let status = rig.ask(runner, "status", None);
    let item = &status["work_items"][0];
    assert_eq!(item["issue"], "7");
    item["turn"]["state"].as_str().unwrap().to_owned()
}

fn calls_of_7(rig: &Rig, runner: &Mutex<Runner>) -> u64 {
    rig.ask(runner, "status", None)["work_items"][0]["calls"]
        .as_u64()
        .unwrap()
}

// Waits for a call on #7 to end after `before` were counted. A turn reads
// `ended` only until the worker moves #7 on, which a poll can miss.
fn see_the_turn_end(rig: &Rig, runner: &Mutex<Runner>, before: u64) {
    eventually("the turn's end", || calls_of_7(rig, runner) > before);
}

fn eventually(what: &str, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + CALL_PATIENCE;
    while !done() {
        assert!(std::time::Instant::now() < deadline, "never saw {what}");
        std::thread::yield_now();
    }
}

#[test]
fn a_refused_handover_leaves_the_runner_and_its_calls_as_they_were() {
    let rig = Rig::new("shep");
    let hold = Hold::default();
    let (handing, tried) = refusing("the build reads another state format", &Arc::default());
    let (runner, worker, wake) = in_a_turn(&rig, &hold, Arc::clone(&handing));

    let answer: serde_json::Value = serde_json::from_str(&handing.ask(&wake)).unwrap();

    assert_eq!(
        answer["error"],
        "handover refused: the build reads another state format"
    );
    assert_eq!(tried.execs.load(Ordering::SeqCst), 1);
    assert_eq!(
        *tried.carried.lock().unwrap(),
        [ticket(7)],
        "the turn was carried"
    );
    assert!(!handing.pending(), "the refused request stays queued");
    assert_eq!(turn_of_7(&rig, &runner), "running");
    assert!(!hold.returned(), "the call was ended");
    let before = calls_of_7(&rig, &runner);
    hold.release();
    see_the_turn_end(&rig, &runner, before);
    assert!(worker.stop(CALL_PATIENCE, || {}));
}

#[test]
fn a_handover_asked_while_passes_keep_starting_calls_is_tried_at_the_next_quiet_point() {
    let rig = Rig::new("shep");
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    let runner = Arc::new(rig.open().unwrap());
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "add", Some("8"));
    let (seven, eight) = (Hold::default(), Hold::default());
    rig.claude
        .script([Scripted::Hold(seven.clone()), Scripted::Hold(eight.clone())]);
    let (handing, tried) = refusing("stand-in", &Arc::default());
    let hooks = Hooks {
        on_wake: Box::new(|_| {}),
        finished: Box::new(|_| {}),
        handover: Arc::clone(&handing),
    };
    let (wake, woken) = mpsc::channel();
    let (died, _) = mpsc::channel();
    let held = rig.forge.hold_next_call();
    let worker = Worker::spawn(Arc::clone(&runner), hooks, wake, woken, died);
    assert!(held.waiting(CALL_PATIENCE), "the first pass never began");

    // The request carries a wake of its own, so the loop is not woken by it.
    let (elsewhere, _unheard) = mpsc::channel();
    let asking = Arc::clone(&handing);
    std::thread::spawn(move || asking.ask(&elsewhere));
    eventually("the request", || handing.pending());
    drop(held);

    assert!(seven.entered(CALL_PATIENCE), "#7's turn never began");
    assert!(eight.entered(CALL_PATIENCE), "#8's turn never began");
    eventually("the handover tried", || {
        tried.execs.load(Ordering::SeqCst) == 1
    });
    // Tried after the pass that started #7, before the one that started #8.
    assert_eq!(*tried.carried.lock().unwrap(), [ticket(7)]);
    assert!(!handing.pending());
    seven.release();
    eight.release();
    assert!(worker.stop(CALL_PATIENCE, || {}));
}

#[test]
fn a_handover_with_nothing_in_flight_carries_nothing() {
    let rig = Rig::new("shep");
    let runner = Arc::new(rig.open().unwrap());
    let (handing, tried) = refusing("unused", &Arc::default());
    let (wake, woken) = mpsc::channel();
    let (died, _) = mpsc::channel();
    let hooks = Hooks {
        on_wake: Box::new(|_| {}),
        finished: Box::new(|_| {}),
        handover: Arc::clone(&handing),
    };
    let worker = Worker::spawn(Arc::clone(&runner), hooks, wake.clone(), woken, died);

    // No call is in flight, so the carry is empty and the exec is tried.
    let answer: serde_json::Value = serde_json::from_str(&handing.ask(&wake)).unwrap();

    assert_eq!(answer["error"], "handover refused: unused");
    assert_eq!(tried.execs.load(Ordering::SeqCst), 1);
    assert!(tried.carried.lock().unwrap().is_empty());
    assert!(worker.stop(CALL_PATIENCE, || {}));
}

#[test]
fn a_handover_waits_out_a_call_whose_stdin_is_being_fed() {
    let rig = Rig::new("shep");
    let hold = Hold::default();
    let feeding = Arc::new(AtomicBool::new(true));
    let (handing, tried) = refusing("stand-in", &feeding);
    let (_runner, worker, wake) = in_a_turn(&rig, &hold, Arc::clone(&handing));

    // The loop comes by its quiet point many times in the time the trigger
    // waits, and is held each time.
    let answer: serde_json::Value = serde_json::from_str(&handing.ask(&wake)).unwrap();

    assert_eq!(answer["handover"], "waiting for the runner's quiet point");
    assert_eq!(tried.execs.load(Ordering::SeqCst), 0, "it did not wait");
    assert!(handing.pending());
    feeding.store(false, Ordering::SeqCst);
    eventually("the handover once the feed ended", || {
        tried.execs.load(Ordering::SeqCst) == 1
    });
    assert!(!handing.pending());
    hold.release();
    assert!(worker.stop(CALL_PATIENCE, || {}));
}

// A runner whose turn on issue 7 ended in the stand-in, so its record is gone, and
// whose loop has not heard the end. The handover is asked on a thread of its own
// and answers there.
fn with_an_unheard_end() -> (
    Rig,
    Arc<Mutex<Runner>>,
    Arc<Handing>,
    Tried,
    std::thread::JoinHandle<String>,
) {
    let rig = Rig::new("shep");
    let runner = Arc::new(rig.open().unwrap());
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the call never began");
    let (handing, tried) = refusing("stand-in", &Arc::default());
    let (wake, _unheard) = mpsc::channel();
    let asking = Arc::clone(&handing);
    let asked = std::thread::spawn(move || asking.ask(&wake));
    eventually("the request", || handing.pending());
    hold.release();
    eventually("the record gone", || {
        matches!(
            runner.lock().unwrap().carry_calls(),
            Err(NotCarried::Waiting(_))
        )
    });
    (rig, runner, handing, tried, asked)
}

#[test]
fn a_handover_waits_for_the_end_of_a_call_whose_record_is_gone_and_goes_once_it_is_heard() {
    let (_rig, runner, handing, tried, asked) = with_an_unheard_end();

    handing.attempt(&runner);

    assert_eq!(tried.execs.load(Ordering::SeqCst), 0, "it did not wait");
    assert!(handing.pending(), "the request was dropped");
    crate::runner::step(&runner).unwrap();
    handing.attempt(&runner);
    assert_eq!(tried.execs.load(Ordering::SeqCst), 1);
    assert!(tried.carried.lock().unwrap().is_empty());
    assert!(!handing.pending());
    let answer: serde_json::Value = serde_json::from_str(&asked.join().unwrap()).unwrap();
    assert_eq!(answer["error"], "handover refused: stand-in");
}

#[test]
fn a_handover_waiting_for_a_gone_record_can_be_withdrawn_and_stays_withdrawn() {
    let (_rig, runner, handing, tried, _asked) = with_an_unheard_end();
    handing.attempt(&runner);
    assert!(handing.pending(), "the request did not wait");

    let answer: serde_json::Value = serde_json::from_str(&handing.withdraw()).unwrap();

    assert_eq!(answer["handover"], "withdrawn");
    crate::runner::step(&runner).unwrap();
    handing.attempt(&runner);
    assert_eq!(
        tried.execs.load(Ordering::SeqCst),
        0,
        "a withdrawn handover was made"
    );
    assert!(!handing.pending());
}

#[test]
fn a_handover_is_refused_once_a_gone_record_has_been_waited_for_as_long_as_the_wait_allows() {
    let (rig, runner, handing, tried, asked) = with_an_unheard_end();
    handing.attempt(&runner);
    assert!(handing.pending(), "the request did not wait");

    rig.clock.advance(CARRY_WAIT.as_secs());
    handing.attempt(&runner);

    let answer: serde_json::Value = serde_json::from_str(&asked.join().unwrap()).unwrap();
    let error = answer["error"].as_str().unwrap();
    assert!(
        error.starts_with("handover refused: the turn of #7 cannot be carried: the call's record "),
        "{error}"
    );
    assert!(error.contains("cannot be read"), "{error}");
    assert_eq!(tried.execs.load(Ordering::SeqCst), 0, "it execed anyway");
    assert!(!handing.pending());
}

#[test]
fn a_handover_is_refused_at_once_when_a_calls_record_is_there_and_cannot_be_read() {
    let (rig, runner, handing, tried, asked) = with_an_unheard_end();
    rig.claude.spoil_records();

    handing.attempt(&runner);

    let answer: serde_json::Value = serde_json::from_str(&asked.join().unwrap()).unwrap();
    let error = answer["error"].as_str().unwrap();
    assert!(
        error.starts_with("handover refused: the turn of #7 cannot be carried: the call's record "),
        "{error}"
    );
    assert_eq!(tried.execs.load(Ordering::SeqCst), 0, "it execed anyway");
    assert!(!handing.pending());
}

// Asks for a handover while the record of the running call is gone, lets the
// wait for its end run out, and gives the refusal the request is answered with.
fn refused_once_the_wait_is_up(
    rig: &Rig,
    runner: &Arc<Mutex<Runner>>,
    handing: &Arc<Handing>,
    wake: Sender<()>,
) -> serde_json::Value {
    let asked = {
        let handing = Arc::clone(handing);
        std::thread::spawn(move || handing.ask(&wake))
    };
    eventually("the record seen gone", || {
        matches!(
            runner.lock().unwrap().carry_calls(),
            Err(NotCarried::Waiting(_))
        )
    });
    rig.clock.advance(CARRY_WAIT.as_secs());
    serde_json::from_str(&asked.join().unwrap()).unwrap()
}

// Whether an `add 7` asked while a pass holds the runner's lock is answered from the posted copy.
fn add_is_answered_while_the_lock_is_held(runner: &Arc<Mutex<Runner>>) -> bool {
    let postings = runner.lock().unwrap().postings();
    let pass = runner.lock().unwrap();
    let (replied, reply) = mpsc::channel();
    let asking = {
        let runner = Arc::clone(runner);
        std::thread::spawn(move || {
            let _ = replied.send(crate::runner::answer(&runner, &postings, "add", Some("7")));
        })
    };
    let answered = reply.recv_timeout(CALL_PATIENCE).is_ok();
    drop(pass);
    asking.join().unwrap();
    answered
}

#[test]
fn a_handover_waits_for_a_queued_verb_to_be_taken_up() {
    let rig = Rig::new("shep");
    let runner = Arc::new(rig.open().unwrap());
    rig.ask(&runner, "status", None);
    let (handing, tried) = refusing("stand-in", &Arc::default());
    assert!(add_is_answered_while_the_lock_is_held(&runner));

    let (wake, _) = mpsc::channel();
    let answer: serde_json::Value = serde_json::from_str(&handing.ask(&wake)).unwrap();
    assert_eq!(answer["handover"], "waiting for the runner's quiet point");
    handing.attempt(&runner);
    assert_eq!(
        tried.execs.load(Ordering::SeqCst),
        0,
        "the add is not taken yet"
    );
    assert!(handing.pending());

    crate::runner::step(&runner).unwrap();
    assert!(rig.ask(&runner, "status", None)["work_item"].is_object());
    handing.attempt(&runner);
    assert_eq!(
        tried.execs.load(Ordering::SeqCst),
        1,
        "it goes once the add is in"
    );
    assert!(!handing.pending());
}

#[test]
fn a_refused_handover_lets_the_queue_take_verbs_again() {
    let rig = Rig::new("shep");
    let runner = Arc::new(rig.open().unwrap());
    rig.ask(&runner, "status", None);
    let (handing, tried) = refusing("stand-in", &Arc::default());
    let (wake, _) = mpsc::channel();
    handing.ask(&wake);

    handing.attempt(&runner);

    assert_eq!(tried.execs.load(Ordering::SeqCst), 1);
    assert!(add_is_answered_while_the_lock_is_held(&runner));
}

#[test]
fn a_withdrawn_handover_is_not_made_when_the_hold_ends() {
    let rig = Rig::new("shep");
    let hold = Hold::default();
    let feeding = Arc::new(AtomicBool::new(true));
    let (handing, tried) = refusing("stand-in", &feeding);
    let (runner, worker, wake) = in_a_turn(&rig, &hold, Arc::clone(&handing));
    handing.ask(&wake);
    assert!(handing.pending());

    let answer: serde_json::Value = serde_json::from_str(&handing.withdraw()).unwrap();
    assert_eq!(answer["handover"], "withdrawn");
    assert!(!handing.pending());
    feeding.store(false, Ordering::SeqCst);
    handing.attempt(&runner);
    assert_eq!(tried.execs.load(Ordering::SeqCst), 0);

    let answer: serde_json::Value = serde_json::from_str(&handing.withdraw()).unwrap();
    assert_eq!(answer["handover"], "none waiting");
    hold.release();
    assert!(worker.stop(CALL_PATIENCE, || {}));
}

#[test]
fn a_handover_waits_out_a_notice_hook_that_is_running() {
    use crate::hooks::Role;
    use crate::settings::HookPoint;
    let rig = Rig::new("shep");
    let flag = rig.home.path().join("go");
    let script = format!(
        "read line; {}",
        crate::test::waits_for(&flag, CALL_PATIENCE)
    );
    rig.hook("slow.sh", &["done"], &script);
    let runner = Arc::new(rig.open().unwrap());
    runner
        .lock()
        .unwrap()
        .tell_hooks(HookPoint::Done, Role::Kelpie, serde_json::json!({ "n": 1 }));
    let (handing, tried) = refusing("stand-in", &Arc::default());
    let (wake, woken) = mpsc::channel();
    let (died, _) = mpsc::channel();
    let hooks = Hooks {
        on_wake: Box::new(|_| {}),
        finished: Box::new(|_| {}),
        handover: Arc::clone(&handing),
    };
    let worker = Worker::spawn(Arc::clone(&runner), hooks, wake.clone(), woken, died);

    let answer: serde_json::Value = serde_json::from_str(&handing.ask(&wake)).unwrap();

    assert_eq!(answer["handover"], "waiting for the runner's quiet point");
    assert_eq!(tried.execs.load(Ordering::SeqCst), 0, "it did not wait");
    assert!(handing.pending());
    std::fs::write(&flag, "").unwrap();
    eventually("the handover once the hook ended", || {
        tried.execs.load(Ordering::SeqCst) == 1
    });
    assert!(worker.stop(CALL_PATIENCE, || {}));
}

#[test]
fn a_second_request_while_one_waits_is_refused() {
    let rig = Rig::new("shep");
    let hold = Hold::default();
    let feeding = Arc::new(AtomicBool::new(true));
    let (handing, _) = refusing("stand-in", &feeding);
    let (_runner, worker, wake) = in_a_turn(&rig, &hold, Arc::clone(&handing));
    let first = {
        let (handing, wake) = (Arc::clone(&handing), wake.clone());
        std::thread::spawn(move || handing.ask(&wake))
    };
    eventually("the first request", || handing.pending());

    let second: serde_json::Value = serde_json::from_str(&handing.ask(&wake)).unwrap();

    assert_eq!(
        second["error"],
        "a handover is already waiting for the runner's quiet point"
    );
    feeding.store(false, Ordering::SeqCst);
    first.join().unwrap();
    hold.release();
    assert!(worker.stop(CALL_PATIENCE, || {}));
}

// A build to start for the check: a script that runs `body`.
fn build(rig: &Rig, body: &str) -> std::path::PathBuf {
    let path = rig.home.path().join("build.sh");
    crate::test::write_script(&path, &format!("#!/bin/sh\n{body}\n"));
    path
}

#[test]
fn a_build_that_starts_as_the_runner_passes_the_check() {
    let rig = Rig::new("shep");
    let build = build(&rig, "[ -n \"$KELPIE_HANDOVER_CHECK\" ] && exit 0; exit 9");

    assert_eq!(starts(&build, &[], Duration::from_secs(10)), Ok(()));
}

#[test]
fn a_build_that_stops_on_the_settings_is_refused_with_what_it_said() {
    let rig = Rig::new("shep");
    let build = build(
        &rig,
        "echo 'kelpie runner koji: a setting is not valid' >&2; exit 1",
    );

    let why = starts(&build, &[], Duration::from_secs(10)).unwrap_err();

    assert_eq!(
        why,
        "the installed build would not start as this runner: kelpie runner koji: a setting is not valid"
    );
}

#[test]
fn a_build_that_does_not_finish_its_check_is_stopped() {
    let rig = Rig::new("shep");
    let build = build(&rig, "exec sleep 30");

    let why = starts(&build, &[], Duration::from_millis(200)).unwrap_err();

    assert_eq!(
        why,
        "the installed build did not finish its start check in time"
    );
}

#[test]
fn a_feed_that_begins_during_the_start_check_refuses_the_exec() {
    let rig = Rig::new("shep");
    let build = build(&rig, "sleep 0.5");
    let feeding = Arc::new(AtomicBool::new(false));
    let begins = Arc::clone(&feeding);
    let later = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        begins.store(true, Ordering::SeqCst);
    });

    let verdict = fitness(&build, &[], true, &|| feeding.load(Ordering::SeqCst));

    later.join().unwrap();
    assert_eq!(
        verdict,
        Verdict::Refused("a child's stdin began to be written".into())
    );
}

#[test]
fn a_build_that_starts_with_no_feed_going_is_carryable() {
    let rig = Rig::new("shep");
    let build = build(&rig, "exit 0");

    assert_eq!(fitness(&build, &[], true, &|| false), Verdict::Carryable);
}

#[test]
fn the_probe_refuses_a_target_that_reads_only_format_3_when_a_goose_turn_is_carried() {
    let rig = Rig::new("shep");
    // A build from before goose turns crossed: it reads `Carry` as format 3.
    let before_goose = build(
        &rig,
        "[ \"$SHEP_PROCESS_HANDOVER_PROBE\" = 3 ] && exit 94; exit 95",
    );

    let carry = a_carry_holding_a_goose_turn();
    assert_eq!(carry.pids(), [4242]);

    let refused = Handover::new(&carry)
        .target(&before_goose)
        .exec(&|_: &Path| Verdict::Carryable)
        .unwrap_err();

    assert!(
        matches!(refused, HandOverError::Format { format: 6, .. }),
        "{refused}"
    );
}

// A `Carry` as the old image makes it for one running goose turn. Its
// private fields are written as the wire has them, with the typed parts
// serialized from the types the adapter fills.
fn a_carry_holding_a_goose_turn() -> Carry {
    let adopted = Adopted {
        pid: 4242,
        record: "calls/7.call.json".into(),
        resumes: None,
        flying: None,
        harness: Harness::Goose,
        kept: Some(Kept {
            forwarder: "calls/a.sock".into(),
            server: KeptServer::Gateway("paddock".into()),
            session: SessionId("goose-7".into()),
            lease: Some("lease-1".into()),
            model: Some("qwen3.8:27b".into()),
        }),
        session: Some(SessionId("goose-7".into())),
    };
    let flying = Flying {
        left: 600,
        began: Timestamp(1_000),
        again: false,
        first: true,
        compact: false,
    };
    let draft = serde_json::json!({
        "issue": "7", "pull_request": null, "role": "worker", "kind": "turn",
        "agent": "goose-worker", "harness": "goose", "model": null,
        "effort": null, "session": "goose-7", "started": 1_000,
    });
    serde_json::from_value(serde_json::json!({ "turns": [{
        "issue": 7,
        "adopted": adopted,
        "flying": flying,
        "started": 1_000,
        "open": { "draft": draft, "pacer": {}, "ran_from": null, "no_model": false },
    }] }))
    .expect("the goose turn is a `Carry` as the wire has it")
}

#[test]
fn the_new_image_says_it_took_over_from_a_handover_with_both_builds() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let carry: Carry = serde_json::from_value(serde_json::json!({
        "turns": [], "build": "0.3.1+1f2e3a4",
    }))
    .unwrap();

    let line = carry.took_over_line(&runner.lock().unwrap());

    // The commit this test binary was built from, when it was built in a checkout
    let this = match crate::home::COMMIT {
        Some(commit) => format!("{}+{commit}", env!("CARGO_PKG_VERSION")),
        None => env!("CARGO_PKG_VERSION").to_owned(),
    };
    assert_eq!(
        line,
        format!(
            "took over: shep, from kelpie 0.3.1+1f2e3a4 to kelpie {this}, 0 turns carried, \
             0 work items open, 0 rulings waiting"
        )
    );
}

#[test]
fn a_handover_from_an_image_that_named_no_build_says_an_earlier_one() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let carry: Carry = serde_json::from_value(serde_json::json!({ "turns": [] })).unwrap();

    let line = carry.took_over_line(&runner.lock().unwrap());

    assert!(
        line.starts_with("took over: shep, from an earlier build to kelpie "),
        "{line}"
    );
}

mod codex;
mod exec;
mod goose;
mod leases;
mod pi;
