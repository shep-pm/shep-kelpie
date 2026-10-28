//! The project manager through the runner's stand-ins: what wakes it, what
//! kelpie does with its answer, and what happens when it gives none

use std::sync::Mutex;
use std::time::Instant;

use serde_json::json;

use crate::ports::{AgentError, CallActivity, Role, Session, Timestamp};
use crate::runner::flight::advance;
use crate::runner::flight::tests::Driven;
use crate::runner::{Pass, Runner, StepReport, step};
use crate::settings::Harness;
use crate::test::ticket;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted, Seen};
use crate::ticket::TicketId;

use super::HOLD_FOR;

mod asked;
mod attach;
mod code;
mod conflicts;
mod follow_up;
mod free_slot;
mod hold;
mod inert;
mod lease;
mod main_red;
mod notes;
mod pick;
mod question;
mod quiet;
mod red;
mod requests;
mod rulings;
mod until;
mod wake_prompt;

const NOTHING: &str = "{\"pick\": null}";

const PICK_12: &str = "Neither touches open work, and #12 is smaller.\n\n```json\n\
    {\"pick\": 12, \"hold\": [], \"unstick\": null, \"reply\": null, \
    \"why\": \"#12 is small and touches nothing open.\"}\n```";

/// A running project whose settings name the project manager `pm`
fn with_pm(project: &str) -> (Rig, Mutex<Runner>) {
    pm_on(Rig::new(project))
}

/// ... on `rig`, whose runner is opened here
fn pm_on(rig: Rig) -> (Rig, Mutex<Runner>) {
    name_pm(&rig);
    let runner = rig.open().unwrap();
    (rig, runner)
}

/// Names the project manager in `rig`'s settings, before its runner is opened
fn name_pm(rig: &Rig) {
    rig.edit_settings(|s| {
        let listed = "\nimplementers = [\"sonnet-high\"]\n";
        assert!(s.contains(listed), "the example's implementers moved");
        s.replace(listed, "\nimplementers = [\"sonnet-high\"]\npm = \"pm\"\n")
    });
}

/// The project manager's calls, in order
fn pm_seen(rig: &Rig) -> Vec<Seen> {
    let all = rig.claude.all_seen().into_iter();
    all.filter(|s| s.call.role == Role::Pm).collect()
}

/// Moves the fake clock past [`HOLD_FOR`]
///
/// A held reason is queued as the pass that noticed it ends, and its window
/// ends on a later one, so a test calls this once the reason is due, and waits
/// for the wake after it.
fn past_the_window(rig: &Rig) {
    rig.clock.advance(HOLD_FOR);
}

fn answered(report: Option<StepReport>) -> (Vec<String>, Vec<String>, Vec<String>) {
    match report {
        Some(StepReport::PmAnswered {
            woke_for,
            acted,
            dropped,
            ..
        }) => (woke_for, acted, dropped),
        other => panic!("the project manager did not answer: {other:?}"),
    }
}

fn dispatched(report: Option<StepReport>) -> TicketId {
    match report {
        Some(StepReport::Dispatched { issue, .. }) => issue,
        other => panic!("nothing was dispatched: {other:?}"),
    }
}

/// Steps until a step reports what `wanted` matches, as a call ending on
/// its own thread brings it
fn step_until(runner: &Mutex<Runner>, wanted: impl Fn(&StepReport) -> bool) -> StepReport {
    let deadline = Instant::now() + CALL_PATIENCE;
    while Instant::now() < deadline {
        match step(runner).unwrap() {
            Some(report) if wanted(&report) => return report,
            _ => std::thread::yield_now(),
        }
    }
    panic!("no step reported what the test waited for");
}

#[test]
fn a_worker_queued_for_its_lease_is_woken_for_the_wait_and_never_as_stuck() {
    let (rig, runner) = with_pm("acme");
    rig.ask(&runner, "add", Some("7"));
    let queue = Hold::default();
    let work = Hold::default();
    rig.claude.script([
        Scripted::Queued(queue.clone(), Box::new(Scripted::Hold(work.clone()))),
        Scripted::Say("{\"pick\": null}"),
    ]);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(
        queue.entered(CALL_PATIENCE),
        "the turn never queued for its lease"
    );
    assert_eq!(driven.heard(), None, "the wait is no report");
    rig.clock.advance(11 * 60);
    // The board tells the wait on this pass, which opens its window.
    assert_eq!(step(&runner).unwrap(), None, "the wait is no report");
    past_the_window(&rig);

    let (woke_for, acted, dropped) = answered(step(&runner).unwrap());
    assert_eq!(
        woke_for,
        [
            "#7 has waited 11m for a lease on its model. Its worker is not stuck: it has had \
             no model to show anything through, and kelpie leaves its turn running. Reply to \
             the maintainer if the lease should be released."
        ]
    );
    assert_eq!((acted, dropped), (vec![], vec![]));
    let board = std::fs::read_to_string(rig.paths().board).unwrap();
    assert!(
        board.contains("worker turn running for 14m, waiting for its model\n"),
        "{board}"
    );
    assert!(
        board.contains("#7: worker waits 11m for its model's lease\n"),
        "{board}"
    );
    assert!(!board.contains("worker idle"), "{board}");
    assert_eq!(
        rig.ask(&runner, "status", None)["work_items"][0]["waiting"]["why"],
        "lease",
        "the turn still waits on its model"
    );

    // Half an hour in, its model is still taken, and it is still no stuck worker.
    rig.clock.advance(19 * 60);
    assert!(matches!(advance(&runner).unwrap(), Pass::Idle));
    assert_eq!(pm_seen(&rig).len(), 1, "the wait woke it a second time");
    let board = std::fs::read_to_string(rig.paths().board).unwrap();
    assert!(
        board.contains("worker turn running for 33m, waiting for its model\n"),
        "{board}"
    );
    assert!(!board.contains("worker idle"), "{board}");
    assert!(!work.returned(), "the queued turn was ended");

    // The lease comes, and the turn works on it.
    queue.release();
    assert_eq!(driven.heard(), None, "the grant is no report");
    assert!(work.entered(CALL_PATIENCE), "the turn never began");
    driven.pass();
    let board = std::fs::read_to_string(rig.paths().board).unwrap();
    assert!(
        board.contains("#7: worker has its model's lease, after waiting 33m\n"),
        "{board}"
    );
    work.release();
    assert!(work.answered(CALL_PATIENCE), "the turn never answered");
}

#[test]
fn a_worker_quiet_ten_minutes_past_its_grant_is_stuck_as_ever() {
    let (rig, runner) = with_pm("acme");
    rig.ask(&runner, "add", Some("7"));
    let queue = Hold::default();
    let work = Hold::default();
    rig.claude.script([
        Scripted::Queued(queue.clone(), Box::new(Scripted::Hold(work.clone()))),
        Scripted::Say("{\"pick\": null}"),
    ]);
    rig.claude.set_activity(CallActivity::Nothing);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(
        queue.entered(CALL_PATIENCE),
        "the turn never queued for its lease"
    );
    assert_eq!(driven.heard(), None, "the wait is no report");

    rig.clock.advance(2 * 60);
    queue.release();
    assert_eq!(driven.heard(), None, "the grant is no report");
    assert!(work.entered(CALL_PATIENCE), "the turn never began");
    rig.clock.advance(10 * 60);
    // The board tells the worker idle on this pass, which opens its window.
    assert_eq!(step(&runner).unwrap(), None, "the idle line is no report");
    past_the_window(&rig);

    let (woke_for, _, _) = answered(step(&runner).unwrap());
    assert_eq!(
        woke_for,
        ["#7 is stuck: its worker has shown no tool call or output for 10m"]
    );
    let board = std::fs::read_to_string(rig.paths().board).unwrap();
    let idle = "  - Session: worker turn running for 15m, idle: no tool call or output for 13m\n";
    assert!(board.contains(idle), "{board}");
    work.release();
    assert!(work.answered(CALL_PATIENCE), "the turn never answered");
}

#[test]
fn a_free_slot_with_two_ready_issues_wakes_it_and_its_pick_goes_first() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude.script([Scripted::Say(PICK_12)]);

    let (woke_for, acted, dropped) = answered(step(&runner).unwrap());
    assert_eq!(
        woke_for,
        ["a slot is free and 2 ready issues could fill it: pick one"]
    );
    assert_eq!((acted, dropped), (vec!["picked #12".to_owned()], vec![]));
    assert_eq!(
        dispatched(step(&runner).unwrap()),
        ticket(12),
        "the rule would take #9"
    );

    let [wake] = pm_seen(&rig).try_into().unwrap();
    let paths = rig.paths();
    assert_eq!(wake.call.cwd, paths.pm);
    assert!(matches!(wake.call.session, Session::New(_)));
    assert!(
        wake.call.prompt.starts_with(
            "You are the project manager for koji. Kelpie woke you because:\n\n\
             - a slot is free and 2 ready issues"
        ),
        "{}",
        wake.call.prompt
    );
    let board = std::fs::read_to_string(paths.pm.join("board.md")).unwrap();
    assert!(
        board.contains("#12"),
        "the wake read the board as it stood: {board}"
    );
    assert!(paths.pm.join("pm-notes.md").is_file());
}

#[test]
fn it_reads_its_folder_appends_to_its_notes_and_runs_nothing() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude.script([Scripted::Say(PICK_12)]);
    step(&runner).unwrap();

    let [wake] = pm_seen(&rig).try_into().unwrap();
    let notes = rig.paths().pm.join("pm-notes.md").display().to_string();
    let permissions = &wake.settings["permissions"];
    assert_eq!(permissions["allow"], json!([format!("Edit(/{notes})")]));
    let denied = permissions["deny"].to_string();
    for tool in ["\"Bash\"", "\"Agent\"", "\"WebFetch\""] {
        assert!(denied.contains(tool), "{tool} is not denied: {denied}");
    }
    assert!(
        !denied.contains("MultiEdit"),
        "Claude Code warns about a rule for a tool it lacks: {denied}"
    );
    let hook = wake.settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"].clone();
    let command = format!("'{}' 'confine' '--append' '{notes}'", Rig::KELPIE);
    assert_eq!(hook, json!(command));
    // The hook runs kelpie inside the fence, so the fence reads its binary.
    let allowed = wake.sandbox["filesystem"]["allowRead"].to_string();
    assert!(allowed.contains(Rig::KELPIE), "{allowed}");
    assert!(denied.contains("Read(~/.config/gh/**)"), "{denied}");
    let sandbox = wake.sandbox.to_string();
    assert!(
        sandbox.contains(".config/gh/**"),
        "gh's token is unread: {sandbox}"
    );
    assert!(
        !sandbox.contains("github.com"),
        "it reaches no forge: {sandbox}"
    );
}

#[test]
fn a_wake_logs_its_session_and_where_its_transcript_is_as_its_call_starts() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude.script([Scripted::Say(PICK_12)]);
    step(&runner).unwrap();

    let [wake] = pm_seen(&rig).try_into().unwrap();
    let started = runner.lock().unwrap().take_started();
    let session = wake.call.session.id().0.clone();
    let folder = wake.call.settings.with_extension("transcripts");
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(
        serde_json::to_value(&started[0]).unwrap(),
        json!({
            "step": "session-started",
            "role": "pm",
            "agent": "pm",
            "harness": "claude-code",
            "session": session,
            "transcript": {"folder": folder},
        })
    );
    assert!(
        runner.lock().unwrap().take_started().is_empty(),
        "each start is logged once"
    );
}

#[test]
fn an_answer_naming_what_is_not_on_the_board_is_dropped_and_the_rule_picks() {
    let (rig, runner) = with_pm("rotom");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude.script([Scripted::Say(
        "{\"pick\": 77, \"hold\": [5], \"unstick\": {\"item\": 9, \"action\": \"retry\", \
         \"why\": \"x\"}, \"merge_order\": [1]}",
    )]);

    let (_, acted, dropped) = answered(step(&runner).unwrap());
    assert_eq!(acted, Vec::<String>::new());
    assert_eq!(
        dropped,
        [
            "`merge_order` is not a field kelpie acts on",
            "hold #5: not a ready issue on the board",
            "pick #77: not a ready issue on the board",
            "retry #9: not a stuck work item on the board",
        ]
    );
    assert_eq!(dispatched(step(&runner).unwrap()), ticket(9));
}

#[test]
fn with_the_project_manager_down_the_rule_picks_until_it_is_back() {
    let (rig, runner) = with_pm("eevee");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    let down = AgentError::Failed(Harness::ClaudeCode, "overloaded".into());
    rig.claude.script([Scripted::Fail(down)]);

    let reason = "claude failed: overloaded".to_owned();
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::PmFailed { reason })
    );
    assert_eq!(dispatched(step(&runner).unwrap()), ticket(9));
    assert_eq!(
        pm_seen(&rig).len(),
        1,
        "a project manager that is down is not woken"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["pm"]["down_until"], Rig::EPOCH + 600);
}

#[test]
fn with_no_project_manager_the_rule_picks_and_nothing_wakes() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.forge.list_ready(12, false);
    rig.forge.list_ready(9, false);
    assert_eq!(dispatched(step(&runner).unwrap()), ticket(9));
    assert!(pm_seen(&rig).is_empty());
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status.get("pm"), None);
    assert_eq!(
        rig.ask(&runner, "tell", Some("hold #12")),
        json!({ "error": "the project has no project manager: name one in `agents.pm`, such as `pm`" })
    );
}

#[test]
fn a_hold_over_every_ready_issue_with_nothing_open_goes() {
    let (rig, runner) = with_pm("ditto");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude
        .script([Scripted::Say("{\"pick\": 12, \"hold\": [9, 12]}")]);
    let (_, acted, dropped) = answered(step(&runner).unwrap());
    assert_eq!(acted, ["holds #9, #12"]);
    assert_eq!(dropped, ["pick #12: it holds #12 too"]);
    assert_eq!(
        dispatched(step(&runner).unwrap()),
        ticket(9),
        "the rule picks past the holds"
    );
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes
            .iter()
            .any(|n| n.ends_with("with no work item open to wait for, so its holds go")),
        "{notes:?}"
    );
}

#[test]
fn two_tells_wake_it_once_with_both_notes() {
    let (rig, runner) = with_pm("eevee");
    rig.ask(&runner, "tell", Some("first"));
    rig.ask(&runner, "tell", Some("second"));
    rig.claude.script([Scripted::Say("{\"pick\": null}")]);
    let (woke_for, ..) = answered(step(&runner).unwrap());
    assert_eq!(woke_for, ["the maintainer told you something"]);
    let [wake] = pm_seen(&rig).try_into().unwrap();
    assert!(wake.call.prompt.contains("  > first\n") && wake.call.prompt.contains("  > second\n"));
}

#[test]
fn a_failed_turn_wakes_it_and_its_retry_runs_the_turn_again() {
    let (rig, runner) = with_pm("golbat");
    rig.ask(&runner, "add", Some("7"));
    let failed = AgentError::Failed(Harness::ClaudeCode, "it crashed".into());
    rig.claude.script([
        Scripted::Fail(failed),
        Scripted::Say(
            "{\"pick\": null, \"unstick\": {\"item\": 7, \"action\": \"retry\", \
             \"why\": \"an outage\"}}",
        ),
        Scripted::Say("done"),
    ]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Failed { .. })
    ));
    past_the_window(&rig);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (woke_for, acted, _) = answered(Some(report));
    assert_eq!(
        woke_for,
        [
            "#7 is stuck: its worker's turn parked: the call failed once, the model host stayed unreachable for 30 minutes, or the turn stopped short twice, and ruling 7 asks \
          the maintainer"
        ]
    );
    assert_eq!(acted, ["retried #7"]);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ended { .. })
    ));
    assert_eq!(rig.claude.calls().len(), 2, "the worker's turn ran again");
}

#[test]
fn a_re_scope_or_ask_goes_to_the_maintainer_on_the_items_ruling() {
    let (rig, runner) = with_pm("chelone");
    rig.ask(&runner, "add", Some("7"));
    let failed = AgentError::Failed(Harness::ClaudeCode, "the parser fights the lexer".into());
    rig.claude.script([
        Scripted::Fail(failed),
        Scripted::Say(
            "{\"unstick\": {\"item\": 7, \"action\": \"re-scope\", \
             \"why\": \"split the lexer into its own issue\"}}",
        ),
    ]);
    step(&runner).unwrap();
    past_the_window(&rig);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, _) = answered(Some(report));
    assert_eq!(acted, ["put #7 to the maintainer on ruling 7"]);
    let ruling = &rig.ask(&runner, "status", None)["rulings"][0];
    let question = ruling["question"].as_str().unwrap();
    assert!(
        question.ends_with(
            "\n\nThe project manager proposes re-scoping it. PM says: split the lexer into its own \
             issue"
        ),
        "{question}"
    );
    assert_eq!(ruling["alerted"], false, "posted again with the proposal");
    assert_eq!(
        rig.claude.calls().len(),
        1,
        "the worker waits on the maintainer"
    );
}

#[test]
fn its_words_reach_a_ruling_on_one_line_and_cut_short() {
    let (rig, runner) = with_pm("chelone");
    rig.ask(&runner, "add", Some("7"));
    let failed = AgentError::Failed(Harness::ClaudeCode, "it crashed".into());
    let long = format!("line one\\n\\u001b[31m{}", "x".repeat(400));
    let answer =
        format!("{{\"unstick\": {{\"item\": 7, \"action\": \"ask\", \"why\": \"{long}\"}}}}");
    let answer: &'static str = Box::leak(answer.into_boxed_str());
    rig.claude
        .script([Scripted::Fail(failed), Scripted::Say(answer)]);
    step(&runner).unwrap();
    past_the_window(&rig);
    step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let ruling = &rig.ask(&runner, "status", None)["rulings"][0];
    let question = ruling["question"].as_str().unwrap();
    let (_, said) = question
        .split_once("\n\nThe project manager asks you to decide. PM says: ")
        .unwrap();
    assert!(said.starts_with("line one [31mxxx"), "{said}");
    assert!(!said.contains('\n'), "{said}");
    assert_eq!(said.chars().count(), 300 + " …".chars().count(), "{said}");
}

#[test]
fn a_tell_wakes_it_once_and_the_next_wake_resumes_its_session() {
    let (rig, runner) = with_pm("xilriws");
    rig.ask(&runner, "tell", Some("Hold #12 until the release"));
    assert_eq!(
        rig.ask(&runner, "status", None)["pm"]["told"],
        json!(["Hold #12 until the release"])
    );
    rig.claude.script([
        Scripted::Say("{\"pick\": null, \"reply\": \"Noted: #12 waits.\"}"),
        Scripted::Say("{\"pick\": null}"),
    ]);
    answered(step(&runner).unwrap());
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["pm"]["told"],
        json!(null),
        "carried by the wake it answered"
    );
    assert_eq!(status["pm"]["reply"], "Noted: #12 waits.");
    assert_eq!(step(&runner).unwrap(), None, "nothing new wakes it");

    rig.ask(&runner, "tell", Some("Release is out"));
    answered(step(&runner).unwrap());
    let [first, second] = pm_seen(&rig).try_into().unwrap();
    assert!(
        first
            .call
            .prompt
            .contains("  > Hold #12 until the release\n")
    );
    let Session::New(id) = &first.call.session else {
        panic!("the first wake starts the session");
    };
    assert_eq!(second.call.session, Session::Resume(id.clone()));
    assert!(
        second.call.prompt.starts_with(
            "Kelpie woke you again because:\n\n- the maintainer told you:\n\n  > Release is out\n"
        ),
        "{}",
        second.call.prompt
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["pm"]["session"],
        json!(id.0)
    );
}

#[test]
fn past_100k_of_context_its_session_is_compacted_before_the_next_wake() {
    let (rig, runner) = with_pm("reactmap");
    rig.ask(&runner, "tell", Some("first"));
    rig.claude.script([
        Scripted::SayAt("{\"pick\": null}", 100_001),
        Scripted::Say("compacted"),
        Scripted::Say("{\"pick\": null}"),
    ]);
    answered(step(&runner).unwrap());
    let compacted = step(&runner).unwrap();
    assert!(
        matches!(compacted, Some(StepReport::PmCompacted { .. })),
        "{compacted:?}"
    );
    rig.ask(&runner, "tell", Some("second"));
    answered(step(&runner).unwrap());

    let [_, compact, after] = pm_seen(&rig).try_into().unwrap();
    assert_eq!(compact.call.prompt, "/compact");
    assert!(matches!(compact.call.session, Session::Resume(_)));
    assert_eq!(after.call.session, compact.call.session);
    assert!(
        after.call.prompt.starts_with("You are the project manager"),
        "after a compaction the wake says the answer in full"
    );
}

#[test]
fn a_session_that_cannot_resume_starts_fresh_from_the_board() {
    let (rig, runner) = with_pm("koji");
    rig.ask(&runner, "tell", Some("first"));
    rig.claude.script([Scripted::Say("{\"pick\": null}")]);
    answered(step(&runner).unwrap());
    let old = rig.ask(&runner, "status", None)["pm"]["session"].clone();

    rig.ask(&runner, "tell", Some("second"));
    let gone = AgentError::NoSession(Harness::ClaudeCode, crate::ports::SessionId("x".into()));
    rig.claude
        .script([Scripted::Fail(gone), Scripted::Say("{\"pick\": null}")]);
    answered(step(&runner).unwrap());

    let [_, resumed, fresh] = pm_seen(&rig).try_into().unwrap();
    assert!(matches!(resumed.call.session, Session::Resume(_)));
    assert!(matches!(fresh.call.session, Session::New(_)));
    assert!(
        fresh.call.prompt.contains("  > second\n"),
        "the same wake, fresh"
    );
    assert!(fresh.call.prompt.starts_with("You are the project manager"));
    let new = rig.ask(&runner, "status", None)["pm"]["session"].clone();
    assert_ne!(new, old);
}

#[test]
fn an_idle_worker_wakes_it_and_a_retry_ends_the_call_and_resumes_it() {
    let (rig, runner) = with_pm("acme");
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    rig.claude.script([
        Scripted::Hold(hold.clone()),
        Scripted::Say("{\"unstick\": {\"item\": 7, \"action\": \"retry\", \"why\": \"hung\"}}"),
        Scripted::Say("done"),
    ]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the turn never began");
    rig.claude
        .set_activity(CallActivity::At(Timestamp(Rig::EPOCH)));
    rig.clock.advance(11 * 60);
    // The board tells the worker idle on this pass, which opens its window.
    assert_eq!(step(&runner).unwrap(), None, "the idle line is no report");
    past_the_window(&rig);

    let (woke_for, acted, _) = answered(step(&runner).unwrap());
    assert_eq!(
        woke_for,
        ["#7 is stuck: its worker has shown no tool call or output for 11m"]
    );
    assert_eq!(acted, ["ended #7's idle call, to retry"]);
    assert!(
        hold.answered(CALL_PATIENCE),
        "the idle call was never ended"
    );
    step_until(&runner, |r| matches!(r, StepReport::TimedOut { .. }));
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    step_until(&runner, |r| matches!(r, StepReport::Ended { .. }));
    let calls = rig.claude.calls();
    assert_eq!(
        calls[1].prompt,
        "Kelpie stopped your last turn: it ran past its ceiling. Commit any work that \
         builds now, then carry on from where you left off."
    );
}

// The wake's CI logs are read with the runner's lock let go: a trigger is answered while each
// is held, and the files are written under the lock from what the reads found.
#[test]
fn a_trigger_is_answered_while_the_failed_run_logs_are_read() {
    let (rig, runner, _) = red::raised("koji", |rig| {
        rig.forge.set_ci_log("--- run 9\nboom work.txt\n")
    });
    let hold = Hold::default();
    rig.claude
        .script([Scripted::HoldThenSay(hold.clone(), NOTHING)]);
    step(&runner).unwrap(); // queues the wake, which waits its window
    past_the_window(&rig);
    let read = rig.forge.hold_next_failed_logs_read();

    let (reply, pass) = rig.asked_while_held(&runner, read, "detach", Some("7 1"));

    assert!(
        reply.is_some(),
        "the trigger waited for the lock the read held"
    );
    assert!(matches!(pass, Ok(Pass::Started)), "{pass:?}");
    // The call runs on its own thread, so it is waited for before it is counted.
    assert!(hold.entered(CALL_PATIENCE), "the wake's call never began");
    assert_eq!(pm_seen(&rig).len(), 1, "the wake happened");
    hold.release();
    answered(Some(step_until(&runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));
}

#[test]
fn the_log_files_are_written_from_the_read_under_the_lock() {
    let log = "--- run 9\nFAIL boom work.txt\n";
    let (rig, runner, _) = red::raised("koji", |rig| rig.forge.set_ci_log(log));
    let pm = rig.paths().pm;
    std::fs::create_dir_all(pm.join("ci")).unwrap();
    std::fs::write(pm.join("ci/7.log"), "stale").unwrap();
    rig.claude.script([Scripted::Say(NOTHING)]);
    step(&runner).unwrap(); // queues the wake, which waits its window
    past_the_window(&rig);
    let read = rig.forge.hold_next_failed_logs_read();

    let (kept, pass) = rig.during_read(
        read,
        || advance(&runner),
        || std::fs::read_to_string(pm.join("ci/7.log")).unwrap(),
    );

    assert_eq!(
        kept.as_deref(),
        Some("stale"),
        "a file was written off the lock"
    );
    assert!(matches!(pass, Ok(Pass::Started)), "{pass:?}");
    assert_eq!(std::fs::read_to_string(pm.join("ci/7.log")).unwrap(), log);
    let board = std::fs::read_to_string(pm.join("board.md")).unwrap();
    assert!(board.contains(", CI log `ci/7.log`\n"), "{board}");
}

#[test]
fn a_trigger_is_answered_while_the_logs_of_a_workflow_red_on_main_are_read() {
    let (rig, runner) = main_red::red_again();
    rig.forge.set_run_log_tail(91, main_red::LOG);
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude.script([Scripted::Say(NOTHING)]);
    rig.clock.advance(crate::runner::BOARD_POLL.as_secs());
    step(&runner).unwrap(); // reads the board, which wakes it for the free slot
    let read = rig.forge.hold_next_run_log_tail_read();

    let (reply, pass) = rig.asked_while_held(&runner, read, "detach", Some("7 1"));

    reply.expect("the trigger waited for the lock the read held");
    pass.unwrap();
    let pm = rig.paths().pm;
    let log = std::fs::read_to_string(pm.join("ci/main-91.log")).unwrap();
    assert_eq!(log, main_red::LOG);
}
