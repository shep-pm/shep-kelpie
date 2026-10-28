use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

use super::*;
use crate::ports::{Checks, Cost, Timestamp, Usage};
use crate::runner::posted::Queued;
use crate::runner::{Answer, StepReport, step};
use crate::skills::Step;
use crate::state::{Finished, ProjectState, StateStore};
use crate::test::{Rig, Scripted, ticket};
use crate::work_item::{Seconds, TimingPhase};

mod board;
pub(in crate::runner) mod fixtures;
mod ticket;

// A running project with issue 7 in flight
fn with_issue_7(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    let runner = rig.open().unwrap();
    assert_eq!(
        rig.ask(&runner, "add", Some("7"))["work_item"]["issue"],
        "7"
    );
    (rig, runner)
}

#[test]
fn issue_lists_read_as_prose() {
    assert_eq!(issue_list(&[ticket(7)]), "#7");
    assert_eq!(issue_list(&[ticket(7), ticket(9)]), "#7 and #9");
    assert_eq!(
        issue_list(&[ticket(7), ticket(9), ticket(12)]),
        "#7, #9 and #12"
    );
}

#[test]
fn status_names_the_commit_this_build_was_made_from() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();

    let commit = rig.ask(&runner, "status", None)["commit"]
        .as_str()
        .expect("`status` names the commit this build was made from")
        .to_owned();

    let sha = commit.strip_suffix("-dirty").unwrap_or(&commit);
    assert_eq!(sha.len(), 7, "{commit}");
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()), "{commit}");
}

#[test]
fn status_leaves_out_lock_until_a_hold_is_recorded() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();

    let first = rig.ask(&runner, "status", None);
    assert!(first.get("lock").is_none(), "{first}");

    let held = rig.ask(&runner, "status", None)["lock"].clone();
    let here = format!("{}:", file!().replace("tests/mod.rs", "mod.rs"));
    let sites = held.as_object().expect("the holds by site");
    assert!(
        sites.keys().any(|site| site.starts_with(&here)),
        "no hold for the line that took the lock to answer: {held}"
    );
    assert!(sites.values().all(|seconds| seconds.is_f64()), "{held}");
}

#[test]
fn a_new_project_has_nothing_in_flight() {
    let rig = Rig::new("koji");
    rig.retro_on();
    let runner = rig.open().unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None),
        json!({
            "project": "koji",
            "merging": "ask",
            "board_poll_secs": 60,
            "work_item": null,
            "work_items": [],
            "active_items": 1,
            "working": [],
            "waiting_for_slot": [],
            "parked": [],
            "pending_rulings": 2,
            "adopted": [],
            "order": [],
            "skipped": [],
            "rulings": [],
            "leases": [],
            "history": [],
            "pacer": { "enabled": true, "claude": { "reading": null, "holding": null } },
            // kelpie seeds a vendored skill for the worker's four and for
            // the retro this rig turned on, so `status` names no command
            // for triage, spec and reset: they run kelpie's own prompt.
            "skills": Step::ALL.map(|step| json!({
                "step": step.as_str(),
                "skill": (Step::WORKER.contains(&step) || step == Step::Retro)
                    .then(|| format!("/mattpocock:{}", step.default_skill())),
                "fallback": null,
            })),
            "pull_request_template": "none",
            "board": rig.paths().board,
            "build": crate::home::stamp_of(&std::env::current_exe().unwrap()).unwrap(),
            "commit": crate::home::COMMIT,
        })
    );
}

#[test]
fn status_json_is_unchanged() {
    let (_rig, runner, head) = Rig::parked_set("koji", fixtures::rich);
    let runner = Arc::new(runner);
    let postings = runner.lock().unwrap().postings();
    // The fixture was recorded after a `drain` and a `finish`, so the status
    // carries the drain's calls and the run that is finishing
    answer(&runner, &postings, "drain", None);
    answer(&runner, &postings, "finish", None);

    let locked = fixtures::asked(&runner, &postings, "status", &head);
    assert_eq!(
        locked,
        fixtures::status(),
        "the status read with the runner's lock"
    );

    // `asked_during_a_pass` holds the lock itself, as a pass does
    let mut during = asked_during_a_pass(&runner, "status", None);
    fixtures::settled(&mut during, &head);
    assert_eq!(
        during,
        fixtures::status(),
        "the status read from the posted copy while a pass holds the lock"
    );
}

#[test]
fn every_registered_action_is_answered_and_no_other() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    for action in ACTIONS.into_iter().filter(|a| {
        ![
            "rework", "adopt", "rule", "gate", "drop", "attach", "detach", "tell", "pm", "triage",
        ]
        .contains(a)
    }) {
        let params = (action == "add").then_some("7");
        assert_eq!(
            rig.ask(&runner, action, params)["project"],
            "koji",
            "{action}"
        );
    }
    assert_eq!(
        rig.ask(&runner, "rework", Some("71")),
        json!({ "error": "the work item for #7 is in flight" })
    );
    let no_pm = "the project has no project manager: name one in `agents.pm`, such as `pm`";
    let me = std::process::id();
    assert_eq!(
        rig.ask(&runner, "tell", Some("hold #4")),
        json!({ "error": no_pm })
    );
    assert_eq!(
        rig.ask(&runner, "pm", Some(&format!("attach {me}"))),
        json!({ "error": no_pm })
    );
    assert_eq!(
        rig.ask(&runner, "pm", Some(&format!("detach {me}"))),
        json!({ "error": no_pm })
    );
    assert_eq!(
        rig.ask(&runner, "pm", Some("attach 0")),
        json!({ "error": "`pm` takes `attach <pid>`, `attach <pid> <session pid>` or \
                          `detach <pid>`, not \"attach 0\"" })
    );
    assert_eq!(
        rig.ask(&runner, "adopt", Some("71")),
        json!({ "error": "cannot read pull request #71: gh failed: no pull request #71" })
    );
    assert_eq!(
        rig.ask(&runner, "rule", Some("71 yes")),
        json!({ "error": "ruling 71 is already answered, or was never raised" })
    );
    assert_eq!(
        rig.ask(&runner, "rule", None),
        json!({ "error": "`rule` takes `<id> yes`, `<id> no <note>`, `<id> rework <note>` or `<id> answer <text>`" })
    );
    assert_eq!(
        rig.ask(&runner, "merge", None),
        json!({ "error": "unknown action `merge`" })
    );
}

#[test]
fn timings_board_attach_and_refusal_json_are_unchanged() {
    // `timings`, over the three work items the fixture's history lists
    let rig = Rig::new("koji");
    fixtures::seed(&rig, 3);
    let runner = rig.open().unwrap();
    assert_eq!(rig.ask(&runner, "timings", None), fixtures::timings());

    // `board`, with two ready issues an open one blocks
    let rig = Rig::new("koji");
    fixtures::ready(&rig);
    let runner = rig.open().unwrap();
    step(&runner).unwrap();
    assert_eq!(rig.ask(&runner, "board", None), fixtures::board());

    // `attach` and `pm attach`, from the answer the runner builds for them: a
    // live `attach` asks `ps`, which a worker's sandbox cannot run
    let attaching = fixtures::attaching();
    assert_eq!(
        serde_json::to_value(&attaching).unwrap(),
        fixtures::attach()
    );
    assert_eq!(
        serde_json::to_value(Reply::Attaching(attaching.clone().into())).unwrap(),
        fixtures::attach(),
        "the protocol's answer serializes as the runner's"
    );
    let pm = fixtures::pm_attaching();
    assert_eq!(serde_json::to_value(&pm).unwrap(), fixtures::pm_attach());
    assert_eq!(
        serde_json::to_value(Reply::PmAttaching(pm.clone().into())).unwrap(),
        fixtures::pm_attach(),
        "the protocol's answer serializes as the runner's"
    );

    // A refusal, which is the one answer every trigger can give
    let (rig, runner, _) = Rig::with_pull_request("rotom");
    assert_eq!(rig.ask(&runner, "gate", None), fixtures::refusal());
}

#[test]
fn every_action_reads_as_its_typed_request() {
    let asked: [(&str, Option<&str>, Request); 35] = [
        ("status", None, Request::Status),
        ("add", Some("7"), Request::Add(ticket(7))),
        ("rework", Some("71"), Request::Rework(71)),
        ("adopt", Some("72"), Request::Adopt(72)),
        (
            "rule",
            Some("71 yes"),
            Request::Rule(ticket(71), RuleAnswer::Yes),
        ),
        (
            "rule",
            Some("71 no not yet"),
            Request::Rule(
                ticket(71),
                RuleAnswer::No {
                    note: "not yet".to_owned(),
                },
            ),
        ),
        (
            "rule",
            Some("71 rework split the parser"),
            Request::Rule(
                ticket(71),
                RuleAnswer::Rework {
                    note: "split the parser".to_owned(),
                },
            ),
        ),
        (
            "rule",
            Some("71 answer the third one"),
            Request::Rule(
                ticket(71),
                RuleAnswer::Text {
                    text: "the third one".to_owned(),
                },
            ),
        ),
        (
            "rule",
            Some("71 yes 6b0606e"),
            Request::Rule(
                ticket(71),
                RuleAnswer::Trust {
                    head: "6b0606e".to_owned(),
                },
            ),
        ),
        (
            "rule",
            Some("71 rerun"),
            Request::Rule(
                ticket(71),
                RuleAnswer::Do {
                    action: RuleAction::Rerun,
                },
            ),
        ),
        (
            "rule",
            Some("71 restore"),
            Request::Rule(
                ticket(71),
                RuleAnswer::Do {
                    action: RuleAction::Restore,
                },
            ),
        ),
        ("gate", None, Request::Gate(None)),
        ("gate", Some("7"), Request::Gate(Some(ticket(7)))),
        ("drop", None, Request::Drop(None)),
        ("drop", Some("7"), Request::Drop(Some(ticket(7)))),
        ("timings", None, Request::Timings(TIMINGS_DEFAULT)),
        ("timings", Some("3"), Request::Timings(3)),
        (
            "attach",
            Some("7 401"),
            Request::Attach(ticket(7), 401, None),
        ),
        (
            "attach",
            Some("7 401 402"),
            Request::Attach(ticket(7), 401, Some(402)),
        ),
        ("detach", Some("7 401"), Request::Detach(ticket(7), 401)),
        ("tell", Some("hold #4"), Request::Tell("hold #4".to_owned())),
        ("pm", Some("attach 401"), Request::PmAttach(401, None)),
        (
            "pm",
            Some("attach 401 402"),
            Request::PmAttach(401, Some(402)),
        ),
        ("pm", Some("detach 401"), Request::PmDetach(401)),
        ("drain", None, Request::Drain { at_tool: false }),
        ("drain", Some("pause"), Request::Drain { at_tool: true }),
        ("undrain", None, Request::Undrain { keep: false }),
        ("undrain", Some("keep"), Request::Undrain { keep: true }),
        ("finish", None, Request::Finish),
        ("start", None, Request::Start),
        ("pausing", None, Request::Pausing),
        ("board", None, Request::Board(None)),
        (
            "board",
            Some("move 282 2"),
            Request::Board(Some((ticket(282), 2))),
        ),
        (
            "board",
            Some("top 282"),
            Request::Board(Some((ticket(282), 1))),
        ),
        ("triage", Some("9"), Request::Triage(9)),
    ];
    let named: Vec<&str> = asked
        .iter()
        .map(|(_, _, request)| request.action())
        .collect();
    for action in ACTIONS {
        assert!(named.contains(&action), "{action} reads as nothing");
    }
    for (action, params, want) in asked {
        assert_eq!(read(action, params), Ok(want), "{action} {params:?}");
    }

    let refused: [(&str, Option<&str>, &str); 18] = [
        ("merge", None, "unknown action `merge`"),
        ("add", None, "`add` takes a ticket id"),
        ("add", Some("#7"), "\"#7\" is not a ticket id"),
        ("rework", None, "`rework` takes a pull request number"),
        ("rework", Some("x"), "\"x\" is not a pull request number"),
        ("adopt", None, "`adopt` takes a pull request number"),
        ("triage", None, "`triage` takes an issue number"),
        ("gate", Some("#7"), "\"#7\" is not a ticket id"),
        ("drop", Some("0"), "\"0\" is not a ticket id"),
        (
            "rule",
            None,
            "`rule` takes `<id> yes`, `<id> no <note>`, `<id> rework <note>` or `<id> answer <text>`",
        ),
        (
            "rule",
            Some("71 maybe"),
            "`rule` takes `<id> yes`, `<id> no <note>`, `<id> rework <note>` or `<id> answer <text>`, not \"71 maybe\"",
        ),
        (
            "timings",
            Some("zero"),
            "`timings` takes a count of finished work items, not \"zero\"",
        ),
        (
            "attach",
            Some("7"),
            "`attach` takes a ticket id and the attaching process's pid, and `attach` the pid of the session it started, not \"7\"",
        ),
        (
            "detach",
            None,
            "`detach` takes a ticket id and the attaching process's pid, and `attach` the pid of the session it started",
        ),
        ("tell", None, "`tell` takes a note for the project manager"),
        (
            "undrain",
            Some("now"),
            "`undrain` takes `keep` or nothing, not \"now\"",
        ),
        ("drain", Some("now"), "`drain` takes no params"),
        (
            "board",
            Some("move 282 x"),
            "`board` takes `move <issue> <position>` or `top <issue>`, or nothing to list, not \"move 282 x\"",
        ),
    ];
    for (action, params, why) in refused {
        assert_eq!(
            read(action, params),
            Err(why.to_owned()),
            "{action} {params:?}"
        );
    }
}

#[test]
fn a_trigger_with_params_is_refused_and_changes_nothing() {
    let rig = Rig::new("rotom");
    let runner = rig.open().unwrap();
    assert_eq!(
        rig.ask(&runner, "drain", Some("now")),
        json!({ "error": "`drain` takes no params" })
    );
    assert_eq!(rig.ask(&runner, "status", None)["draining"], json!(null));
}

#[test]
fn a_second_add_while_one_is_in_flight_is_refused() {
    let (rig, runner) = with_issue_7("golbat");
    assert_eq!(
        rig.ask(&runner, "add", Some("8")),
        json!({ "error": "the work item for #7 is in flight" })
    );
}

#[test]
fn adding_an_issue_the_forge_cannot_show_changes_nothing() {
    let rig = Rig::new("reactmap");
    rig.forge.remove_issue(9);
    let runner = rig.open().unwrap();
    assert_eq!(
        rig.ask(&runner, "add", Some("9")),
        json!({ "error": "cannot read the issue: gh failed: no issue #9" })
    );
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
}

#[test]
fn gate_sends_a_work_item_saved_before_the_gate_to_its_merge_ruling() {
    let (rig, runner, head) = Rig::with_pull_request("webapp");
    drop(runner);
    let state = rig.paths().state;
    let text = std::fs::read_to_string(&state).unwrap();
    let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    let item = saved["work_items"][0].as_object_mut().unwrap();
    item.remove("phase");
    item.remove("red_head");
    std::fs::write(&state, saved.to_string()).unwrap();

    let runner = rig.open().unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(step(&runner).unwrap(), None, "left alone until asked");
    let status = rig.ask(&runner, "gate", None);
    assert_eq!(
        status["work_item"]["phase"],
        json!({ "state": "ci", "head": null, "since": Rig::EPOCH })
    );
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. })
     if id == ticket(71)));
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn gate_refuses_anything_but_an_ended_turn_with_a_known_pull_request() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    let refused = |runner: &Mutex<Runner>, error: &str| {
        assert_eq!(rig.ask(runner, "gate", None), json!({ "error": error }));
    };
    refused(&runner, "no work item is in flight");
    rig.ask(&runner, "add", Some("7"));
    refused(&runner, "the worker's turn on #7 is due, not ended");
    assert_eq!(
        rig.ask(&runner, "gate", Some("7")),
        json!({ "error": "the worker's turn on #7 is due, not ended" })
    );
    assert_eq!(
        rig.ask(&runner, "gate", Some("8")),
        json!({ "error": "no work item for #8 is open" })
    );
    assert_eq!(
        rig.ask(&runner, "gate", Some("#7")),
        json!({ "error": "\"#7\" is not a ticket id" })
    );

    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(&runner).unwrap();
    refused(&runner, "no pull request is known for #7");

    let (rig, runner, _) = Rig::with_pull_request("rotom");
    assert_eq!(
        rig.ask(&runner, "gate", None),
        json!({ "error": "the work item for #7 is already in the gate" })
    );
}

// Sends `action` the way a shep-channel handler thread does while a pass holds the
// runner's lock, and returns its reply. A reply that waits for the lock fails
// the test instead of hanging it.
fn asked_during_a_pass(runner: &Arc<Mutex<Runner>>, action: &str, params: Option<&str>) -> Value {
    let postings = runner.lock().unwrap().postings();
    let _pass = runner.lock().unwrap();
    let (sent, reply) = mpsc::channel();
    let (runner, action, params) = (
        Arc::clone(runner),
        action.to_owned(),
        params.map(str::to_owned),
    );
    std::thread::spawn(move || {
        let _ = sent.send(answer(&runner, &postings, &action, params.as_deref()));
    });
    let reply = reply
        .recv_timeout(Duration::from_secs(5))
        .expect("the trigger waited for the pass's lock");
    serde_json::from_str(&reply).expect("a JSON reply")
}

#[test]
fn status_sent_after_a_trigger_that_waits_for_the_pass_is_answered_at_once() {
    let rig = Rig::new("koji");
    let (runner, postings) = posted(&rig, rig.open().unwrap());
    let pass = runner.lock().unwrap();
    let ask = |action: &'static str| {
        let (runner, postings) = (Arc::clone(&runner), Arc::clone(&postings));
        let (sent, reply) = mpsc::channel();
        let handler = std::thread::spawn(move || {
            let _ = sent.send(answer(&runner, &postings, action, None));
        });
        (handler, reply)
    };

    let (finish, finished) = ask("finish");
    assert!(
        finished.recv_timeout(Duration::from_millis(100)).is_err(),
        "`finish` waits for the pass's lock"
    );
    let (status, statused) = ask("status");
    let status_reply = statused
        .recv_timeout(Duration::from_secs(5))
        .expect("`status` waited behind `finish`");
    assert_eq!(
        serde_json::from_str::<Value>(&status_reply).unwrap()["project"],
        "koji"
    );

    drop(pass);
    finished
        .recv_timeout(Duration::from_secs(5))
        .expect("`finish` is answered once the pass lets go");
    finish.join().unwrap();
    status.join().unwrap();
}

// A runner whose loop has posted its status, as it has by its first pass
fn posted(rig: &Rig, runner: Mutex<Runner>) -> (Arc<Mutex<Runner>>, Arc<Posted>) {
    let runner = Arc::new(runner);
    rig.ask(&runner, "status", None);
    let postings = runner.lock().unwrap().postings();
    (runner, postings)
}

#[test]
fn add_is_answered_while_the_pass_holds_the_lock() {
    let rig = Rig::new("koji");
    let (runner, postings) = posted(&rig, rig.open().unwrap());

    let reply = asked_during_a_pass(&runner, "add", Some("7"));

    assert_eq!(reply["project"], "koji", "{reply}");
    assert_eq!(
        reply["work_items"],
        json!([]),
        "the pass has not taken it up"
    );
    assert!(postings.needs_wake(), "the loop is asked for a pass");
    assert_eq!(postings.take_queued(), [Queued::Add(ticket(7))]);
}

#[test]
fn rework_adopt_and_triage_are_answered_while_the_pass_holds_the_lock() {
    let rig = Rig::new("koji");
    let (runner, postings) = posted(&rig, rig.open().unwrap());

    for (action, params) in [("rework", "71"), ("adopt", "72"), ("triage", "9")] {
        let reply = asked_during_a_pass(&runner, action, Some(params));
        assert_eq!(reply["project"], "koji", "{action}: {reply}");
    }

    assert_eq!(
        postings.take_queued(),
        [Queued::Rework(71), Queued::Adopt(72), Queued::Triage(9)]
    );
}

#[test]
fn rule_is_answered_while_the_pass_holds_the_lock_and_an_unknown_id_is_refused_at_once() {
    let (rig, runner, _) = Rig::parked("rotom");
    let (runner, postings) = posted(&rig, runner);

    let reply = asked_during_a_pass(&runner, "rule", Some("71 no not yet"));
    assert_eq!(reply["rulings"][0]["id"], "71", "{reply}");
    let refused = asked_during_a_pass(&runner, "rule", Some("9 yes"));
    assert_eq!(
        refused,
        json!({ "error": "ruling 9 is already answered, or was never raised" })
    );

    assert_eq!(
        postings.take_queued(),
        [Queued::Rule(ticket(71), Answer::No("not yet".into()))],
        "the refused id queued nothing"
    );
}

#[test]
fn timings_and_board_answer_from_the_posted_copy() {
    let rig = Rig::new("koji");
    let seconds = Seconds::of(&[(TimingPhase::Worker, 60), (TimingPhase::Ci, 40)]);
    let mut state = ProjectState::new();
    state.history = vec![Finished {
        issue: ticket(5),
        title: "Issue 5".to_owned(),
        pull_request: Some(65),
        merged: true,
        closed: false,
        at: Timestamp(Rig::EPOCH + 5),
        wall: seconds.total(),
        seconds,
        spend: None,
    }];
    let file = rig.paths().state;
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    StateStore::new(file).save(&state).unwrap();
    let runner = rig.open().unwrap();
    for issue in [282, 284] {
        rig.forge.list_ready(issue, false);
        rig.forge.block(issue, 32);
    }
    assert_eq!(step(&runner).unwrap(), None);
    let (runner, _) = posted(&rig, runner);
    let timings = rig.ask(&runner, "timings", None);
    let board = rig.ask(&runner, "board", None);
    assert_eq!(timings["items"], 1);
    assert_eq!(board["board"].as_array().map(Vec::len), Some(2));

    assert_eq!(asked_during_a_pass(&runner, "timings", None), timings);
    assert_eq!(asked_during_a_pass(&runner, "board", None), board);
}
