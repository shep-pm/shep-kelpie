use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::ports::Checks;
use crate::runner::{Pass, Runner, StepReport, advance, step};
use crate::test::{Hold, Rig, Scripted, git, write_in};
use crate::work_item::{Attached, Holder};

// A held call's start or end that never comes fails the test waiting on it.
const PATIENCE: Duration = Duration::from_secs(30);

// A process that lives until dropped, standing in for `shep kelpie attach`
struct Live(Child);

impl Live {
    fn new() -> Self {
        let child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        Self(child)
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// The pid of a process that has already ended
fn ended() -> u32 {
    let mut child = Command::new("true").spawn().unwrap();
    child.wait().unwrap();
    child.id()
}

fn attach(rig: &Rig, runner: &Mutex<Runner>, pid: u32) -> Value {
    rig.ask(runner, "attach", Some(&format!("7 {pid}")))
}

fn detach(rig: &Rig, runner: &Mutex<Runner>, pid: u32) -> Value {
    rig.ask(runner, "detach", Some(&format!("7 {pid}")))
}

fn attached(rig: &Rig, runner: &Mutex<Runner>) -> Value {
    rig.ask(runner, "status", None)["work_item"]["attached"].clone()
}

// Commits `file` in issue 7's worktree and pushes it, as the maintainer
// would from the attached session, and returns the new head.
fn push_from_the_worktree(rig: &Rig, file: &str) -> String {
    let worktree = rig.worktree_7();
    write_in(&worktree, file, "steered by hand\n");
    git(&worktree, &["add", file]);
    git(&worktree, &["commit", "--quiet", "-m", file]);
    git(&worktree, &["push", "--quiet", "origin", "HEAD"]);
    git(&worktree, &["rev-parse", "HEAD"])
}

#[test]
fn an_attached_item_runs_nothing_and_its_push_is_the_worker_s_once_it_detaches() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    let me = Live::new();
    let answer = attach(&rig, &runner, me.pid());
    let session = rig.ask(&runner, "status", None)["work_item"]["session"].clone();
    let settings = rig.paths().worker.join("settings-7.json");
    assert_eq!(
        answer,
        json!({
            "attach": "ready",
            "issue": 7,
            "session": session,
            "worktree": rig.worktree_7(),
            "command": {
                "program": "claude",
                "args": ["--settings", settings, "--resume", session],
                "cwd": rig.worktree_7(),
                "env": {},
            },
        })
    );
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert!(
        written["hooks"]["PreToolUse"].is_array(),
        "the worker's guard holds the session: {written}"
    );
    let hold = attached(&rig, &runner);
    assert_eq!(hold["by"]["pid"], me.pid());
    assert!(
        hold["by"]["started"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "{hold}"
    );
    assert_eq!(hold["since"], Rig::EPOCH);
    assert_eq!(hold.get("session"), None);

    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(rig.verdict(&runner), None, "no merge ruling while attached");
    let calls = rig.claude.all_calls().len();
    let pushed = push_from_the_worktree(&rig, "steer.txt");
    rig.forge.set_checks(&pushed, Checks::Passed);
    assert_eq!(
        attach(&rig, &runner, me.pid())["attach"],
        "ready",
        "asking again keeps the hold"
    );

    let status = detach(&rig, &runner, me.pid());
    assert_eq!(status["work_item"].get("attached"), None);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"][0]["kind"],
        json!({ "kind": "merge", "head": pushed }),
        "the push is the worker's, not a change kelpie did not make"
    );
    assert_eq!(rig.claude.all_calls().len(), calls, "no call ran for it");
    assert_eq!(
        detach(&rig, &runner, me.pid())["work_item"]["issue"],
        7,
        "a second detach changes nothing"
    );
}

#[test]
fn attach_waits_for_the_call_in_flight_and_no_call_starts_after_it() {
    let rig = Rig::new("rotom");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    rig.claude
        .script([Scripted::Hold(hold.clone()), Scripted::Say("done")]);
    let (wake, woken) = mpsc::channel();
    runner.lock().unwrap().wake_with(wake);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(PATIENCE), "the first turn began");

    let me = Live::new();
    assert_eq!(
        attach(&rig, &runner, me.pid()),
        json!({ "attach": "waiting", "issue": 7 })
    );
    assert_eq!(attached(&rig, &runner)["by"]["pid"], me.pid());
    hold.release();
    woken.recv_timeout(PATIENCE).expect("the turn ended");
    advance(&runner).unwrap();
    assert_eq!(attach(&rig, &runner, me.pid())["attach"], "ready");
    assert!(matches!(advance(&runner).unwrap(), Pass::Idle));
    assert_eq!(rig.claude.calls().len(), 1, "the hold starts no turn");

    detach(&rig, &runner, me.pid());
    step(&runner).unwrap();
    assert_eq!(
        rig.claude.calls().len(),
        2,
        "the next turn runs once let go"
    );
    assert_eq!(
        rig.claude.calls()[1].session,
        crate::ports::Session::Resume(rig.claude.calls()[0].session.id().clone()),
        "and carries on from the same session"
    );
}

#[test]
fn an_item_with_no_session_or_parked_on_a_ruling_says_what_to_do_and_stays_as_it_was() {
    let rig = Rig::new("golbat");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let me = Live::new();
    let refused = attach(&rig, &runner, me.pid());
    let why = refused["error"].as_str().unwrap();
    assert!(
        why.starts_with("the worker on #7 has no session yet"),
        "{why}"
    );
    assert!(why.contains("`shep kelpie start`"), "{why}");
    assert_eq!(attached(&rig, &runner), Value::Null);

    let (rig, runner, _) = Rig::parked("golbat");
    let refused = attach(&rig, &runner, me.pid());
    let why = refused["error"].as_str().unwrap();
    assert!(
        why.starts_with(
            "the work item for #7 is parked on ruling 1: answer it with `shep kelpie rule 1"
        ),
        "{why}"
    );
    assert_eq!(attached(&rig, &runner), Value::Null);

    assert_eq!(
        rig.ask(&runner, "attach", Some(&format!("8 {}", me.pid()))),
        json!({ "error": "no work item for #8 is open" })
    );
    let too_big = format!("7 {}", u64::from(i32::MAX.unsigned_abs()) + 1);
    let bad = [
        None,
        Some("7"),
        Some("7 me"),
        Some("#7 12"),
        Some("7 0"),
        Some(&*too_big),
    ];
    for bad in bad {
        let refused = rig.ask(&runner, "attach", bad);
        let why = refused["error"].as_str().unwrap();
        assert!(
            why.starts_with("`attach` takes an issue number and the attaching process's pid"),
            "{why}"
        );
    }
}

#[test]
fn a_worker_on_another_harness_is_refused() {
    let rig = Rig::new("reactmap");
    rig.write_agent(
        "gpt",
        "---\nrole: implementer\nharness: codex\nmodel: gpt-6-sol\neffort: medium\n---\n",
    );
    rig.implementers(&["sonnet-high", "gpt"]);
    rig.forge.label(7, "agent:gpt");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let me = Live::new();
    let refused = attach(&rig, &runner, me.pid());
    let why = refused["error"].as_str().unwrap();
    assert!(
        why.starts_with(
            "the worker on #7 runs on codex, and only a Claude Code session can be attached"
        ),
        "{why}"
    );
    assert_eq!(attached(&rig, &runner), Value::Null);
}

#[test]
fn a_hold_whose_process_ended_is_let_go_at_the_next_pass_with_its_push() {
    let (rig, runner, _) = Rig::with_pull_request("chelone");
    let gone = ended();
    assert_eq!(
        attach(&rig, &runner, gone),
        json!({ "error": format!("no process {gone} is running") })
    );
    let me = Live::new();
    assert_eq!(attach(&rig, &runner, me.pid())["attach"], "ready");
    let pushed = push_from_the_worktree(&rig, "steer.txt");
    rig.forge.set_checks(&pushed, Checks::Passed);
    drop(me);

    rig.verdict(&runner);
    assert_eq!(attached(&rig, &runner), Value::Null);
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"][0]["kind"],
        json!({ "kind": "merge", "head": pushed })
    );
}

#[test]
fn only_the_process_holding_an_item_lets_it_go_until_it_ends() {
    let (rig, runner, _) = Rig::with_pull_request("xilriws");
    let (first, second) = (Live::new(), Live::new());
    assert_eq!(attach(&rig, &runner, first.pid())["attach"], "ready");
    assert_eq!(
        attach(&rig, &runner, second.pid()),
        json!({
            "error": format!("the work item for #7 is already attached, by process {}", first.pid())
        })
    );
    assert_eq!(
        detach(&rig, &runner, second.pid()),
        json!({
            "error": format!(
                "the work item for #7 is attached by process {}, not this one",
                first.pid()
            )
        })
    );
    let first_pid = first.pid();
    drop(first);
    assert_eq!(attach(&rig, &runner, second.pid())["attach"], "ready");
    assert_eq!(attached(&rig, &runner)["by"]["pid"], second.pid());
    assert_ne!(second.pid(), first_pid);
}

#[test]
fn a_pull_request_opened_while_attached_is_the_worker_s_and_goes_to_its_review() {
    let rig = Rig::new("webapp");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say("done")]);
    step(&runner).unwrap();
    let me = Live::new();
    assert_eq!(attach(&rig, &runner, me.pid())["attach"], "ready");

    push_from_the_worktree(&rig, "work.txt");
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    let item = detach(&rig, &runner, me.pid())["work_item"].clone();
    assert_eq!(item["pull_request"], 71);
    assert_eq!(item["phase"]["state"], "review", "{item}");
    assert_eq!(rig.claude.calls().len(), 1, "the worker was not sent back");
}

#[test]
fn the_session_s_process_holds_the_item_after_the_attach_command_dies() {
    let (rig, runner, head) = Rig::with_pull_request("golbat");
    let (cli, claude) = (Live::new(), Live::new());
    assert_eq!(attach(&rig, &runner, cli.pid())["attach"], "ready");
    let session = format!("7 {} {}", cli.pid(), claude.pid());
    assert_eq!(
        rig.ask(&runner, "attach", Some(&session)),
        json!({ "attach": "running", "issue": 7 })
    );
    assert_eq!(attached(&rig, &runner)["session"]["pid"], claude.pid());
    drop(cli);

    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(
        rig.verdict(&runner),
        None,
        "claude still runs, so nothing does"
    );
    assert_eq!(attached(&rig, &runner)["session"]["pid"], claude.pid());
    let other = Live::new();
    assert!(
        attach(&rig, &runner, other.pid())["error"]
            .as_str()
            .is_some_and(|e| e.contains("is already attached")),
        "a live session keeps out another attach"
    );

    drop(claude);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    assert_eq!(attached(&rig, &runner), Value::Null);
}

#[test]
fn a_session_pid_from_a_process_not_holding_the_item_is_refused() {
    let (rig, runner, _) = Rig::with_pull_request("rotom");
    let (holder, stranger, claude) = (Live::new(), Live::new(), Live::new());
    let session = format!("7 {} {}", stranger.pid(), claude.pid());
    let refused = rig.ask(&runner, "attach", Some(&session));
    assert_eq!(
        refused["error"],
        format!(
            "the work item for #7 is attached by process {}, not this one",
            stranger.pid()
        )
    );
    assert_eq!(attach(&rig, &runner, holder.pid())["attach"], "ready");
    let refused = rig.ask(&runner, "attach", Some(&session));
    assert_eq!(
        refused["error"],
        format!(
            "the work item for #7 is attached by process {}, not this one",
            holder.pid()
        )
    );
    assert_eq!(attached(&rig, &runner).get("session"), None);
}

#[test]
fn a_pid_now_naming_another_process_holds_nothing() {
    let me = Live::new();
    let super::Seen::Runs(now) = super::seen(super::PS, me.pid()) else {
        panic!("it runs")
    };
    let then = Holder {
        started: "Thu Jan  1 00:00:00 1970".into(),
        ..now.clone()
    };
    let hold = |by: Holder| Attached {
        by,
        session: None,
        since: crate::ports::Timestamp(0),
    };
    assert!(super::live(&hold(now)));
    assert!(!super::live(&hold(then)), "a reused pid ends the hold");
}

#[test]
fn drop_waits_for_the_attached_session_to_end() {
    let (rig, runner, _) = Rig::with_pull_request("acme");
    let me = Live::new();
    assert_eq!(attach(&rig, &runner, me.pid())["attach"], "ready");
    assert_eq!(
        rig.ask(&runner, "drop", None),
        json!({
            "error": format!(
                "the work item for #7 is attached, by process {}: quit that claude session \
                 first, which lets it go",
                me.pid()
            )
        })
    );
    assert!(rig.worktree_7().is_dir(), "its worktree stays");
    drop(me);
    assert_eq!(rig.ask(&runner, "drop", None)["work_items"], json!([]));
}

#[test]
fn a_ps_that_cannot_say_keeps_the_hold_and_one_that_finds_nothing_ends_it() {
    let dir = tempfile::tempdir().unwrap();
    let fake = |name: &str, exit: u8| {
        let path = dir.path().join(name);
        crate::test::write_script(&path, &format!("#!/bin/sh\nexit {exit}\n"));
        path.display().to_string()
    };
    let gone = ended();
    let hold = Attached {
        by: Holder {
            pid: gone,
            started: "Mon Oct  5 21:42:49 2026".into(),
        },
        session: None,
        since: crate::ports::Timestamp(0),
    };
    assert!(!super::live_by(&hold, super::PS), "a pid ps does not find");
    assert!(!super::live_by(&hold, &fake("ps-none", 1)));
    assert!(super::live_by(&hold, &fake("ps-broken", 2)), "a failing ps");
    let missing = dir.path().join("no-ps").display().to_string();
    assert!(super::live_by(&hold, &missing), "a ps that cannot run");
}
