use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use crate::runner::{CARRY_WAIT, NotCarried, Pass, Runner, advance};
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted};

const PI: &str = "---\nrole: implementer\nharness: pi\nmodel: qwen3.8:27b\neffort: low\n\
                  url: http://box:11434/v1\ncontext: 65536\n---\n";

const GOOSE: &str = "---\nrole: implementer\nharness: goose\nmodel: qwen3.8:27b\neffort: low\n\
                     url: http://box:11434/v1\ncontext: 65536\n---\n";

const CODEX: &str = "---\nrole: implementer\nharness: codex\nmodel: gpt-6-sol\neffort: low\n---\n";

// A process that lives until dropped, standing in for `shep kelpie attach`
struct Live(Child);

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_handover_waits_while_a_pi_session_is_attached() {
    let (rig, runner, _) = Rig::with_pull_request_set("rotom", |rig| {
        rig.write_agent("pi-box", PI);
        rig.agent_board.ranks_first(&["pi-box"]);
    });
    assert!(runner.lock().unwrap().carry_calls().is_ok());
    let child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let me = Live(child);
    let pid = me.0.id();
    let attached = rig.ask(&runner, "attach", Some(&format!("7 {pid}")));
    assert_eq!(attached["attach"], "ready", "{attached}");

    let why = runner
        .lock()
        .unwrap()
        .carry_calls()
        .unwrap_err()
        .to_string();
    assert!(why.starts_with("#7 is attached"), "{why}");

    rig.ask(&runner, "detach", Some(&format!("7 {pid}")));
    assert!(runner.lock().unwrap().carry_calls().unwrap().is_empty());
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"].get("attached"),
        None
    );
}

#[test]
fn a_handover_waits_while_a_goose_session_is_attached() {
    let (rig, runner, _) = Rig::with_pull_request_set("rotom", |rig| {
        rig.write_agent("goose-box", GOOSE);
        rig.agent_board.ranks_first(&["goose-box"]);
    });
    assert!(runner.lock().unwrap().carry_calls().is_ok());
    let child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let me = Live(child);
    let pid = me.0.id();
    let attached = rig.ask(&runner, "attach", Some(&format!("7 {pid}")));
    assert_eq!(attached["attach"], "ready", "{attached}");

    let why = runner
        .lock()
        .unwrap()
        .carry_calls()
        .unwrap_err()
        .to_string();
    assert!(why.starts_with("#7 is attached"), "{why}");
    assert!(why.contains("goose"), "{why}");

    rig.ask(&runner, "detach", Some(&format!("7 {pid}")));
    assert!(runner.lock().unwrap().carry_calls().unwrap().is_empty());
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"].get("attached"),
        None
    );
}

#[test]
fn a_handover_waits_while_a_codex_session_is_attached() {
    let (rig, runner, _) = Rig::with_pull_request_set("rotom", |rig| {
        rig.write_agent("codex-box", CODEX);
        rig.agent_board.ranks_first(&["codex-box"]);
    });
    assert!(runner.lock().unwrap().carry_calls().is_ok());
    let child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let me = Live(child);
    let pid = me.0.id();
    let attached = rig.ask(&runner, "attach", Some(&format!("7 {pid}")));
    assert_eq!(attached["attach"], "ready", "{attached}");

    let why = runner
        .lock()
        .unwrap()
        .carry_calls()
        .unwrap_err()
        .to_string();
    assert!(why.starts_with("#7 is attached"), "{why}");
    assert!(why.contains("codex"), "{why}");

    rig.ask(&runner, "detach", Some(&format!("7 {pid}")));
    assert!(runner.lock().unwrap().carry_calls().unwrap().is_empty());
}

#[test]
fn a_drain_waits_while_a_pi_session_is_attached() {
    let (rig, runner, _) = Rig::with_pull_request_set("rotom", |rig| {
        rig.write_agent("pi-box", PI);
        rig.agent_board.ranks_first(&["pi-box"]);
    });
    let child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let me = Live(child);
    let pid = me.0.id();
    rig.ask(&runner, "attach", Some(&format!("7 {pid}")));

    let draining = rig.ask(&runner, "drain", None)["draining"]["calls"].clone();
    assert_eq!(
        draining,
        serde_json::json!([{"issue": "7", "role": "pi_session"}])
    );

    rig.ask(&runner, "detach", Some(&format!("7 {pid}")));
    let draining = rig.ask(&runner, "drain", None)["draining"]["calls"].clone();
    assert_eq!(draining, serde_json::json!([]));
}

#[test]
fn a_drain_waits_while_a_goose_session_is_attached() {
    let (rig, runner, _) = Rig::with_pull_request_set("rotom", |rig| {
        rig.write_agent("goose-box", GOOSE);
        rig.agent_board.ranks_first(&["goose-box"]);
    });
    let child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let me = Live(child);
    let pid = me.0.id();
    rig.ask(&runner, "attach", Some(&format!("7 {pid}")));

    let draining = rig.ask(&runner, "drain", None)["draining"]["calls"].clone();
    assert_eq!(
        draining,
        serde_json::json!([{"issue": "7", "role": "goose_session"}])
    );

    rig.ask(&runner, "detach", Some(&format!("7 {pid}")));
    let draining = rig.ask(&runner, "drain", None)["draining"]["calls"].clone();
    assert_eq!(draining, serde_json::json!([]));
}

#[test]
fn a_drain_waits_while_a_codex_session_is_attached() {
    let (rig, runner, _) = Rig::with_pull_request_set("rotom", |rig| {
        rig.write_agent("codex-box", CODEX);
        rig.agent_board.ranks_first(&["codex-box"]);
    });
    let child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let me = Live(child);
    let pid = me.0.id();
    rig.ask(&runner, "attach", Some(&format!("7 {pid}")));

    let draining = rig.ask(&runner, "drain", None)["draining"]["calls"].clone();
    assert_eq!(
        draining,
        serde_json::json!([{"issue": "7", "role": "codex_session"}])
    );

    rig.ask(&runner, "detach", Some(&format!("7 {pid}")));
    let draining = rig.ask(&runner, "drain", None)["draining"]["calls"].clone();
    assert_eq!(draining, serde_json::json!([]));
}

// The bridge's socket and the Chromium behind it live in this image, so the next could not take the turn.
#[test]
fn a_handover_waits_while_a_worker_turn_holds_a_browser() {
    let rig = Rig::new("rotom");
    let off = "[app.dogs.kelpie.browser]\nenabled = false";
    rig.edit_settings(|s| {
        assert!(s.contains(off), "the example's browser moved");
        s.replace(off, "[app.dogs.kelpie.browser]\nenabled = true")
    });
    let tools = rig.paths().tools;
    std::fs::create_dir_all(tools.playwright_mcp().parent().unwrap()).unwrap();
    std::fs::write(tools.playwright_mcp(), "").unwrap();
    std::fs::create_dir_all(tools.browsers().join("chromium-1200")).unwrap();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the call never began");

    let why = runner
        .lock()
        .unwrap()
        .carry_calls()
        .unwrap_err()
        .to_string();
    assert!(why.starts_with("#7's turn drives a browser"), "{why}");

    hold.release();
    assert!(hold.answered(CALL_PATIENCE), "the call never answered");
}

// A turn the next image finishes is counted there, from where its transcript stood as it began.
#[test]
fn a_carried_turn_s_requests_are_counted_when_the_next_image_adopts_it() {
    let rig = Rig::new("rotom");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    let lines = include_str!("../../../../../../fixtures/claude-transcript-request-sizes.jsonl");
    rig.claude.script([Scripted::Transcribed(
        lines.to_owned(),
        Box::new(Scripted::Hold(hold.clone())),
    )]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the call never began");

    let turns = runner.lock().unwrap().carry_calls().unwrap();
    assert_eq!(turns.len(), 1);
    runner.lock().unwrap().adopt_calls(turns);

    let total = || {
        let labels = [
            ("harness", "claude-code"),
            ("le", "+Inf"),
            ("role", "worker"),
        ];
        rig.samples.value("kelpie_request_context_tokens", &labels)
    };
    let began = std::time::Instant::now();
    while total() != Some(6.0) && began.elapsed() < CALL_PATIENCE {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(total(), Some(6.0), "the six requests of the transcript");
    let labels = [
        ("after", "start"),
        ("harness", "claude-code"),
        ("role", "worker"),
    ];
    let start = rig
        .samples
        .value("kelpie_cache_write_tokens_total", &labels);
    assert!(start.is_some_and(|written| written > 0.0), "{start:?}");

    hold.release();
}

// A turn whose call is held now, and whose record is gone once the call is released.
fn in_a_held_turn(rig: &Rig) -> (Mutex<Runner>, Hold) {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the call never began");
    (runner, hold)
}

// Lets the held call end and waits until its record is gone, with its end not yet heard by a pass.
fn ended_unheard(runner: &Mutex<Runner>, hold: &Hold) {
    hold.release();
    let began = std::time::Instant::now();
    while !matches!(
        runner.lock().unwrap().carry_calls(),
        Err(NotCarried::Waiting(_))
    ) {
        assert!(began.elapsed() < CALL_PATIENCE, "the record never went");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn a_turn_whose_record_is_gone_is_waited_for_until_its_end_is_heard() {
    let rig = Rig::new("rotom");
    let (runner, hold) = in_a_held_turn(&rig);
    ended_unheard(&runner, &hold);

    let waits = runner.lock().unwrap().carry_calls().unwrap_err();
    assert!(
        matches!(&waits, NotCarried::Waiting(why) if why.starts_with("the turn of #7 cannot be carried: the call's record ")),
        "{waits:?}"
    );

    assert!(matches!(
        advance(&runner).unwrap(),
        Pass::Report(_) | Pass::Idle | Pass::Started
    ));
    let turns = runner.lock().unwrap().carry_calls();
    assert!(
        turns.as_ref().is_ok_and(Vec::is_empty),
        "the end was heard, so nothing is left to carry: {turns:?}"
    );
}

#[test]
fn a_turn_whose_record_stays_gone_is_refused_once_the_wait_is_up() {
    let rig = Rig::new("rotom");
    let (runner, hold) = in_a_held_turn(&rig);
    ended_unheard(&runner, &hold);

    rig.clock.advance(CARRY_WAIT.as_secs() - 1);
    let waits = runner.lock().unwrap().carry_calls().unwrap_err();
    assert!(matches!(waits, NotCarried::Waiting(_)), "{waits:?}");

    rig.clock.advance(1);
    let refused = runner.lock().unwrap().carry_calls().unwrap_err();
    let NotCarried::Refused(why) = refused else {
        panic!("still waiting: {refused:?}");
    };
    assert!(
        why.starts_with("the turn of #7 cannot be carried: the call's record "),
        "{why}"
    );
    assert!(why.contains("cannot be read"), "{why}");
}

#[test]
fn a_turn_whose_record_is_there_and_unreadable_is_refused_at_once() {
    let rig = Rig::new("rotom");
    let (runner, hold) = in_a_held_turn(&rig);
    rig.claude.spoil_records();

    let refused = runner.lock().unwrap().carry_calls().unwrap_err();

    let NotCarried::Refused(why) = refused else {
        panic!("waited: {refused:?}");
    };
    assert!(
        why.starts_with("the turn of #7 cannot be carried: the call's record "),
        "{why}"
    );
    hold.release();
}

// The wait is a flight's own: a turn that follows one seen with its record gone starts its wait afresh.
#[test]
fn a_gone_record_is_waited_for_afresh_by_the_turn_after_one_that_was_seen_gone() {
    let rig = Rig::new("rotom");
    let (runner, first) = in_a_held_turn(&rig);
    let second = Hold::default();
    rig.claude.script([Scripted::Hold(second.clone())]);
    ended_unheard(&runner, &first);
    rig.clock.advance(CARRY_WAIT.as_secs() * 2);

    // One pass hears the first end, and the next starts the next turn of #7.
    assert!(matches!(advance(&runner).unwrap(), Pass::Report(_)));
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(second.entered(CALL_PATIENCE), "the next turn never began");

    ended_unheard(&runner, &second);
    let waits = runner.lock().unwrap().carry_calls().unwrap_err();
    assert!(matches!(waits, NotCarried::Waiting(_)), "{waits:?}");
}

// What the old image's call held, as a gateway's lease on `coder`, or with no lease at all.
fn kept_on_coder(lease: Option<&str>) -> crate::ports::Kept {
    crate::ports::Kept {
        forwarder: "calls/a.sock".into(),
        server: crate::ports::KeptServer::Gateway("paddock".into()),
        session: crate::ports::SessionId("pi-7".into()),
        lease: lease.map(str::to_owned),
        model: Some("coder".into()),
    }
}

// The turn the new image adopts, with `kept` as the old image's adapter noted for its call.
fn adopted_with(rig: &Rig, kept: crate::ports::Kept) -> Hold {
    let (runner, hold) = in_a_held_turn(rig);
    let mut turns = runner.lock().unwrap().carry_calls().unwrap();
    turns[0].adopted.kept = Some(kept);
    runner.lock().unwrap().adopt_calls(turns);
    hold
}

// The gauge is the model's, so a handover does not drop it to nothing until the carried call ends.
#[test]
fn a_carried_call_still_holding_its_gateway_lease_counts_as_held_on_its_model() {
    let rig = Rig::new("rotom");
    let hold = adopted_with(&rig, kept_on_coder(Some("L1")));
    let coder = [("model", "coder")];
    assert_eq!(
        rig.samples.value("kelpie_model_lease_held", &coder),
        Some(1.0)
    );
    assert_eq!(
        rig.samples.value("kelpie_model_lease_waiting", &coder),
        Some(0.0)
    );
    hold.release();
}

#[test]
fn a_carried_call_with_no_lease_counts_on_no_model() {
    let rig = Rig::new("rotom");
    let hold = adopted_with(&rig, kept_on_coder(None));
    assert_eq!(
        rig.samples
            .value("kelpie_model_lease_held", &[("model", "coder")]),
        None
    );
    hold.release();
}
