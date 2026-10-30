use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use super::*;
use crate::ports::ReviewerError;
use crate::runner::BOARD_POLL;
use crate::test::{CALL_PATIENCE, FakeShepherd, Hold, Rig, Scripted, ScriptedRound};

// A shepherd that holds a project `older`, running or not, whose table is the rig's, less what
// `edit` changes, and a look at it from the rig's own project.
fn beside_older(
    rig: &Rig,
    online: bool,
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> (Look, FakeShepherd) {
    let shepherd = FakeShepherd::new();
    let launch = crate::flock::Launch {
        kelpie: "/opt/kelpie".into(),
        shep_home: shepherd.home().to_owned(),
        kelpie_home: None,
    };
    let entry = std::fs::read_to_string(rig.settings_file()).unwrap();
    let mut table = crate::test::project_table(&entry);
    edit(&mut table);
    let older = ProjectName::try_from("older").unwrap();
    shepherd.holds(launch.runner(&older, table), online);
    let paths = rig.paths();
    let look = Look::new(
        shepherd.dial(),
        shepherd.home().to_owned(),
        "shep".into(),
        look::Sources {
            folder: paths.folder.clone(),
            agents: paths.agents.clone(),
        },
        rig.home.path().to_owned(),
    );
    (look, shepherd)
}

#[test]
fn a_runner_whose_checkout_another_project_runs_refuses_to_start_and_names_it() {
    let rig = Rig::new("shep");
    let (look, _shepherd) = beside_older(&rig, true, |table| {
        table["git"]["remote"] = "someone/else".into();
    });
    let Err(Exit::Refused(why)) = unshared(&look, &rig.settings()) else {
        panic!("the runner started beside `older`");
    };
    assert!(why.contains("project `older`"), "{why}");
    assert!(why.contains("shep kelpie set git.checkout"), "{why}");
}

#[test]
fn a_runner_whose_repo_another_project_runs_refuses_to_start_and_names_it() {
    let rig = Rig::new("shep");
    let (look, _shepherd) = beside_older(&rig, true, |table| {
        table["git"]["checkout"] = "/src/elsewhere".into();
    });
    let Err(Exit::Refused(why)) = unshared(&look, &rig.settings()) else {
        panic!("the runner started beside `older`");
    };
    assert!(why.contains("project `older`"), "{why}");
    assert!(why.contains(Rig::REMOTE), "{why}");
}

#[test]
fn a_runner_starts_beside_a_stopped_project_with_its_checkout_and_repo() {
    let rig = Rig::new("shep");
    let (look, _shepherd) = beside_older(&rig, false, |_| {});
    assert_eq!(unshared(&look, &rig.settings()), Ok(()));
}

// The code is what shep's `stop_exit_codes` matches to leave a runner stopped.
#[test]
fn a_refused_start_exits_with_the_code_the_flock_entry_stops_on() {
    let refused = Exit::Refused("old home".into());
    assert_eq!(refused.code(), REFUSED);
    assert_eq!(refused.message(), "old home");
    assert_eq!(Exit::from("no HOME").code(), 1);
}

/// A worker whose first turn on issue 7 is held open by `hold`
fn in_a_turn(rig: &Rig, hold: &Hold) -> (Arc<Mutex<Runner>>, Worker) {
    let runner = Arc::new(rig.open().unwrap());
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Hold(hold.clone())]);
    let worker = spawn(&runner);
    assert!(hold.entered(CALL_PATIENCE), "the worker's turn never began");
    (runner, worker)
}

fn spawn(runner: &Arc<Mutex<Runner>>) -> Worker {
    spawn_with(runner, mpsc::channel(), Box::new(|_| {}))
}

fn spawn_with(
    runner: &Arc<Mutex<Runner>>,
    (wake, woken): (Sender<()>, Receiver<()>),
    finished: OnFinished,
) -> Worker {
    let (died, _) = mpsc::channel();
    let hooks = Hooks {
        on_wake: Box::new(|_| {}),
        finished,
        handover: Handing::never(),
    };
    Worker::spawn(Arc::clone(runner), hooks, wake, woken, died)
}

// A hook that counts the stops asked of it, each of which fails
fn counting() -> (Arc<AtomicUsize>, OnFinished) {
    let stops = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&stops);
    let hook = Box::new(move |runner: &Arc<Mutex<Runner>>| {
        runner.lock().unwrap().stop_failed("the shepherd refused");
        counted.fetch_add(1, Ordering::SeqCst);
    });
    (stops, hook)
}

// Each open work item's issue and its turn's state, as `status` shows them
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

pub(super) fn calls_of_7(rig: &Rig, runner: &Mutex<Runner>) -> u64 {
    rig.ask(runner, "status", None)["work_items"][0]["calls"]
        .as_u64()
        .unwrap()
}

// Waits for a call on #7 to end after `before` were counted. A turn reads
// `ended` only until the worker moves #7 on, which a poll can miss.
pub(super) fn see_the_turn_end(rig: &Rig, runner: &Mutex<Runner>, before: u64) {
    eventually("the turn's end", || calls_of_7(rig, runner) > before);
}

fn eventually(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + CALL_PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "never saw {what}");
        std::thread::yield_now();
    }
}

// The copy a trigger reads while a pass holds the runner's lock
fn postings(runner: &Mutex<Runner>) -> Arc<Posted> {
    runner.lock().unwrap().postings()
}

// The loop runs a pass only when woken or when its wait runs out.
#[test]
fn status_and_drain_during_a_drain_wake_nothing_and_ask_the_forge_nothing_but_undrain_wakes() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let postings = postings(&runner);
    let (wake, woken) = mpsc::channel();
    let asked = rig.forge.asked();
    on_trigger(&runner, &postings, "drain", None, &wake);
    for _ in 0..20 {
        on_trigger(&runner, &postings, "status", None, &wake);
        on_trigger(&runner, &postings, "drain", None, &wake);
    }
    on_trigger(&runner, &postings, "timings", None, &wake);
    assert!(woken.try_recv().is_err(), "an ask woke the loop");
    assert_eq!(rig.forge.asked(), asked);

    on_trigger(&runner, &postings, "undrain", None, &wake);
    assert!(woken.try_recv().is_ok(), "`undrain` did not wake the loop");
    on_trigger(&runner, &postings, "add", Some("7"), &wake);
    assert!(woken.try_recv().is_ok(), "`add` did not wake the loop");
}

/// How long a trigger that only asks has to answer while a pass holds the
/// runner's lock, well inside shep's 60s `action_timeout`
const ANSWER_BOUND: Duration = Duration::from_secs(2);

/// How long to wait for a pass to reach the forge
const REACH_FORGE: Duration = Duration::from_secs(20);

// Ask `action` of a runner whose lock a pass holds, on a thread of its own,
// and say so when it does not answer.
fn asked_while_held(
    runner: &Arc<Mutex<Runner>>,
    postings: &Arc<Posted>,
    action: &str,
) -> serde_json::Value {
    let (answered, heard) = mpsc::channel();
    let asking = action.to_owned();
    let (runner, postings) = (Arc::clone(runner), Arc::clone(postings));
    std::thread::spawn(move || {
        let _ = answered.send(answer(&runner, &postings, &asking, None));
    });
    let reply = heard.recv_timeout(ANSWER_BOUND).unwrap_or_else(|_| {
        panic!("`{action}` did not answer while a pass held the runner's lock")
    });
    serde_json::from_str(&reply).expect("a JSON reply")
}

// A pass holds the runner's lock through its forge calls, each of which
// waits its turn behind the login's pace. `upgrade` and `pause` ask `status`
// and `drain` twice a second while they wait, and must not be held up by one.
#[test]
fn status_and_drain_answer_while_a_pass_waits_in_the_forge() {
    let rig = Rig::new("shep");
    let runner = Arc::new(rig.open().unwrap());
    let postings = postings(&runner);
    // A ready issue is what gives a pass a call to make with the lock held: dispatching it asks
    // the forge with the lock held. A quiet pass makes none, for its reads let the lock go.
    rig.forge.list_ready(8, false);
    let mut held = rig.forge.hold_next_call();
    let worker = spawn(&runner);
    // The reads at the start of a pass let the lock go, so hold calls one by one until a call
    // the pass makes with the lock held.
    let mut off_the_lock = 0;
    loop {
        assert!(
            held.waiting(REACH_FORGE),
            "the pass never reached a forge call it made with the lock held"
        );
        if runner.try_lock().is_err() {
            break;
        }
        off_the_lock += 1;
        assert!(off_the_lock < 20, "no call of the pass held the lock");
        // The next hold is set before this one lets go, so no call slips between them.
        let next = rig.forge.hold_next_call();
        drop(held);
        held = next;
    }

    let status = asked_while_held(&runner, &postings, "status");
    let drain = asked_while_held(&runner, &postings, "drain");
    assert_eq!(status["project"], "shep", "{status}");
    assert_eq!(
        drain["draining"]["calls"],
        serde_json::json!([]),
        "`drain` did not answer that it holds back every new call"
    );

    // The drain landed while the pass held the lock, so the runner keeps
    // holding back every new call once the pass is through the forge.
    drop(held);
    assert!(worker.stop(JOIN_BOUND, || {}));
    assert!(rig.ask(&runner, "status", None)["draining"].is_object());
}

// A minute on from its last board read, a runner whose one slot is held
// by a drained work item has nothing the board may do.
#[test]
fn an_idle_runner_whose_board_is_shut_sleeps_rather_than_spins() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    advance(&runner).unwrap();
    rig.ask(&runner, "drain", None);
    rig.ask(&runner, "add", Some("7"));
    rig.clock.advance(BOARD_POLL.as_secs() * 3);
    let mut passes = 0;
    let mut slept = Duration::ZERO;
    // A fake minute of the loop: a pass, then the wait it computes.
    while slept < BOARD_POLL {
        advance(&runner).unwrap();
        passes += 1;
        let wait = wait_after_pass(&runner.lock().unwrap());
        assert!(wait >= Duration::from_secs(1), "a wait of {wait:?}");
        rig.clock.advance(wait.as_secs());
        slept += wait;
    }
    assert!(passes <= 2, "{passes} passes in a minute");
}

// Each pass of an idle runner with a quiet board, then the wait it computes
#[test]
fn an_idle_runner_with_a_quiet_board_sleeps_longer_between_passes() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let mut waits = Vec::new();
    for _ in 0..5 {
        advance(&runner).unwrap();
        let wait = wait_after_pass(&runner.lock().unwrap());
        rig.clock.advance(wait.as_secs());
        waits.push(wait.as_secs());
    }
    assert_eq!(waits, [60, 120, 240, 300, 300]);
}

#[test]
fn a_wake_reads_the_settings_again_but_a_stop_does_not() {
    let rig = Rig::new("shep");
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&wakes);
    let on_wake: OnWake = Box::new(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    let (wake, woken) = mpsc::channel();
    let (died, _) = mpsc::channel();
    let runner = Arc::new(rig.open().unwrap());
    let hooks = Hooks {
        on_wake,
        finished: Box::new(|_| {}),
        handover: Handing::never(),
    };
    let worker = Worker::spawn(runner, hooks, wake.clone(), woken, died);
    wake.send(()).unwrap();
    eventually("the trigger's wake", || wakes.load(Ordering::SeqCst) == 1);

    assert!(worker.stop(CALL_PATIENCE, || {}));
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        1,
        "the stop read the settings"
    );
}

// A worker on two ready issues, each of whose passes starts a call, with the
// first pass held in the forge while the clock moves on `secs`, and the
// settings reads it makes counted. `stop_first` asks it to stop meanwhile.
fn busy_for(rig: &Rig, secs: u64, stop_first: bool) -> (Worker, Arc<AtomicUsize>, Hold, Hold) {
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    let runner = Arc::new(rig.open().unwrap());
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "add", Some("8"));
    let (seven, eight) = (Hold::default(), Hold::default());
    rig.claude
        .script([Scripted::Hold(seven.clone()), Scripted::Hold(eight.clone())]);
    let rereads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&rereads);
    let hooks = Hooks {
        on_wake: Box::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
        finished: Box::new(|_| {}),
        handover: Handing::never(),
    };
    let (wake, woken) = mpsc::channel();
    let (died, _) = mpsc::channel();
    let held = rig.forge.hold_next_call();
    let worker = Worker::spawn(runner, hooks, wake, woken, died);
    assert!(held.waiting(CALL_PATIENCE), "the first pass never began");
    rig.clock.advance(secs);
    if stop_first {
        worker.stopping.store(true, Ordering::SeqCst);
    }
    drop(held);
    (worker, rereads, seven, eight)
}

#[test]
fn a_runner_whose_passes_all_start_a_call_rereads_once_the_interval_has_passed() {
    let rig = Rig::new("shep");
    let (worker, rereads, seven, eight) = busy_for(&rig, pace::INTERVAL.as_secs(), false);

    assert!(seven.entered(CALL_PATIENCE), "#7's turn never began");
    assert!(eight.entered(CALL_PATIENCE), "#8's turn never began");
    // Two passes started a call and neither was idle; the second came inside
    // the interval of the read the first made.
    assert_eq!(rereads.load(Ordering::SeqCst), 1);
    assert!(worker.stop(JOIN_BOUND, || {}));
    assert_eq!(rereads.load(Ordering::SeqCst), 1);
}

#[test]
fn a_runner_whose_passes_all_start_a_call_rereads_nothing_inside_the_interval() {
    let rig = Rig::new("shep");
    let secs = pace::INTERVAL.as_secs() - 1;
    let (worker, rereads, seven, eight) = busy_for(&rig, secs, false);

    assert!(seven.entered(CALL_PATIENCE), "#7's turn never began");
    assert!(eight.entered(CALL_PATIENCE), "#8's turn never began");
    assert_eq!(rereads.load(Ordering::SeqCst), 0);
    assert!(worker.stop(JOIN_BOUND, || {}));
}

#[test]
fn a_stop_between_two_busy_passes_rereads_nothing() {
    let rig = Rig::new("shep");
    let (worker, rereads, _seven, _eight) = busy_for(&rig, pace::INTERVAL.as_secs(), true);

    assert!(worker.stop(JOIN_BOUND, || {}));
    assert_eq!(
        rereads.load(Ordering::SeqCst),
        0,
        "the stop read the settings"
    );
}

#[test]
fn a_stop_with_calls_running_lets_go_without_waiting_for_them() {
    let rig = Rig::new("acme");
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    let runner = Arc::new(rig.open().unwrap());
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "add", Some("8"));
    let (seven, eight) = (Hold::default(), Hold::default());
    rig.claude
        .script([Scripted::Hold(seven.clone()), Scripted::Hold(eight.clone())]);
    let worker = spawn(&runner);
    assert!(seven.entered(CALL_PATIENCE), "#7's turn never began");
    assert!(
        eight.entered(CALL_PATIENCE),
        "#8's turn never began beside #7's"
    );
    let running = vec![(7, "running".to_owned()), (8, "running".to_owned())];
    assert_eq!(turns(&rig, &runner), running);

    let at_stop = Arc::new(Mutex::new(None));
    let let_go = worker.stop(JOIN_BOUND, || {
        *at_stop.lock().unwrap() = Some((seven.returned(), eight.returned()));
        rig.claude.stop();
    });

    assert!(let_go, "the worker waited on its calls");
    assert_eq!(
        *at_stop.lock().unwrap(),
        Some((false, false)),
        "the stop waited for a call to end"
    );
    assert!(seven.answered(CALL_PATIENCE) && eight.answered(CALL_PATIENCE));
    assert_eq!(
        rig.claude.all_calls().len(),
        2,
        "a call began after the stop"
    );
    assert_eq!(turns(&rig, &runner), running, "a turn will not resume");
}

#[test]
fn a_calls_end_wakes_the_loop_to_record_it() {
    let rig = Rig::new("shep");
    let hold = Hold::default();
    let (runner, worker) = in_a_turn(&rig, &hold);
    assert_eq!(turns(&rig, &runner), [(7, "running".to_owned())]);
    // Draining, so nothing starts once the turn's end is recorded.
    assert!(rig.ask(&runner, "drain", None)["draining"].is_object());
    let before = calls_of_7(&rig, &runner);
    hold.release();
    // Well inside the minute until the next look at the board.
    see_the_turn_end(&rig, &runner, before);
    assert!(worker.stop(JOIN_BOUND, || {}));
}

#[test]
fn a_worker_waiting_for_its_next_look_lets_go_as_soon_as_it_is_asked() {
    let rig = Rig::new("shep");
    let hold = Hold::default();
    let (_runner, worker) = in_a_turn(&rig, &hold);
    // A failed review round is a step the worker waits after. It reads
    // the head the turn pushed.
    crate::test::git(
        &rig.worktree_7(),
        &["push", "--quiet", "origin", "HEAD:kelpie/7"],
    );
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    let down = ReviewerError::Failed("the GPU is gone".into());
    rig.reviewer.script([ScriptedRound::Fail(down)]);
    hold.release();
    eventually("the review round", || !rig.reviewer.seen().is_empty());

    assert!(
        worker.stop(JOIN_BOUND, || {}),
        "the worker slept through the stop"
    );
}

#[test]
fn finish_with_no_work_item_open_stops_the_sheep_at_once() {
    let rig = Rig::new("shep");
    let runner = Arc::new(rig.open().unwrap());
    let (wake, woken) = mpsc::channel();
    let (stops, hook) = counting();
    let worker = spawn_with(&runner, (wake.clone(), woken), hook);
    assert_eq!(rig.ask(&runner, "finish", None)["run"], "finishing");
    wake.send(()).unwrap();
    eventually("the stop", || stops.load(Ordering::SeqCst) == 1);
    assert_eq!(rig.ask(&runner, "status", None)["run"], "finished");

    // A stop that did not land is asked again by another `finish`.
    rig.ask(&runner, "finish", None);
    wake.send(()).unwrap();
    eventually("the second stop", || stops.load(Ordering::SeqCst) == 2);
    assert!(worker.stop(JOIN_BOUND, || {}));
    assert_eq!(stops.load(Ordering::SeqCst), 2);
}

#[test]
fn finish_stops_the_sheep_only_once_the_last_work_item_ends() {
    let rig = Rig::new("shep");
    let runner = Arc::new(rig.open().unwrap());
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    let (wake, woken) = mpsc::channel();
    let (stops, hook) = counting();
    let worker = spawn_with(&runner, (wake.clone(), woken), hook);
    assert!(hold.entered(CALL_PATIENCE), "the worker's turn never began");
    let status = rig.ask(&runner, "finish", None);
    assert_eq!(
        (&status["run"], &status["working"]),
        (&"finishing".into(), &serde_json::json!(["7"]))
    );
    // Draining, so nothing starts once the turn's end is recorded.
    rig.ask(&runner, "drain", None);
    let before = calls_of_7(&rig, &runner);
    hold.release();
    see_the_turn_end(&rig, &runner, before);
    assert_eq!(stops.load(Ordering::SeqCst), 0, "#7 is still open");

    rig.ask(&runner, "drop", Some("7"));
    wake.send(()).unwrap();
    eventually("the stop", || stops.load(Ordering::SeqCst) == 1);
    assert!(worker.stop(JOIN_BOUND, || {}));
}

#[tokio::test]
async fn a_finished_runner_stops_its_own_sheep_as_pause_does() {
    use shep_client::shep_core::protocol::Request;

    let mut shepherd = crate::test::FakeShepherd::new();
    let launch = crate::flock::Launch {
        kelpie: "/opt/kelpie".into(),
        shep_home: shepherd.home().to_owned(),
        kelpie_home: None,
    };
    let project = ProjectName::try_from("koji").unwrap();
    let mut table = crate::test::project_table(include_str!("../../settings.example.toml"));
    table["git"]["checkout"] = "/src/koji".into();
    shepherd.holds(launch.runner(&project, table), true);
    let drained = r#"{"work_items":[],"draining":{"calls":[],"ceiling":0}}"#;
    shepherd.says("koji", &[drained]);
    let rig = Rig::new("koji");
    let runner = Arc::new(rig.open().unwrap());

    stop_own_sheep(&shepherd.dial(), shepherd.home(), &project, &runner);
    let deadline = Instant::now() + CALL_PATIENCE;
    while shepherd.sheep("koji").unwrap().1 {
        assert!(Instant::now() < deadline, "the sheep never stopped");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let asked: Vec<String> = (shepherd.writes().into_iter())
        .filter_map(|w| match w {
            Request::Trigger { action, .. } => Some(action),
            Request::Stop { .. } => Some("stop".to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(asked, ["status", "drain", "status", "status", "stop"]);
}

#[test]
fn start_is_refused_while_the_stop_is_under_way_and_taken_once_it_fails() {
    let rig = Rig::new("koji");
    let runner = Arc::new(rig.open().unwrap());
    rig.ask(&runner, "finish", None);
    crate::runner::step(&runner).unwrap();
    assert!(runner.lock().unwrap().take_stop());
    let stopping = "the runner finished and is stopping its sheep: `shep kelpie start` runs \
                    it again once it has stopped";
    assert_eq!(rig.ask(&runner, "start", None)["error"], stopping);
    rig.ask(&runner, "finish", None);
    assert!(
        !runner.lock().unwrap().take_stop(),
        "a second stop while one is under way"
    );

    // No shepherd answers here, so the stop fails and says so.
    let nowhere = tempfile::tempdir().unwrap();
    let project = ProjectName::try_from("koji").unwrap();
    runner.lock().unwrap().take_notes();
    let elsewhere = crate::test::FakeShepherd::new();
    stop_own_sheep(&elsewhere.dial(), nowhere.path(), &project, &runner);
    eventually("the failed stop", || {
        rig.ask(&runner, "start", None).get("error").is_none()
    });
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes[0].starts_with("the runner finished and cannot stop its sheep: cannot reach"),
        "{notes:?}"
    );
    assert_eq!(rig.ask(&runner, "status", None).get("run"), None);
}
