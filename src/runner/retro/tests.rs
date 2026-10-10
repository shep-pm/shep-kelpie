//! The retro a finished work item's worker is asked for, through the
//! runner's stand-ins

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

use crate::ports::{AgentCall, AgentError, Checks, Role, Session, Tools};
use crate::runner::flight::advance;
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::settings::Harness;
use crate::test::{Hold, NO_RETRO, Rig, Scripted};
use crate::usage::{FILE, read};

// Real threads on real time, so a held call's wait has this ceiling.
const PATIENCE: Duration = Duration::from_secs(30);

const REPORT: &str = "## Navigation\n\nThe settings table was hard to find.\n";

// Merges the parked pull request and steps until the work item is gone,
// returning the reports on the way
fn merge(rig: &Rig, runner: &Mutex<Runner>) -> Vec<StepReport> {
    rig.ask(runner, "rule", Some("1 yes"));
    let mut reports = Vec::new();
    for _ in 0..6 {
        rig.clock.advance(CHECKS_SETTLE);
        reports.extend(step(runner).unwrap());
        if matches!(reports.last(), Some(StepReport::Finished { .. })) {
            return reports;
        }
    }
    panic!("the work item never finished: {reports:#?}");
}

// The reports kept for the rig's project, oldest name first
fn saved(rig: &Rig) -> Vec<PathBuf> {
    let folder = rig.paths().kelpie_home.join("retro-reports/unverified");
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries.map(|e| e.unwrap().path()).collect();
    files.sort();
    files
}

fn retro_calls(rig: &Rig) -> Vec<AgentCall> {
    let all = rig.claude.all_calls();
    all.into_iter()
        .filter(|c| c.role == Role::Worker && c.prompt.starts_with("/mattpocock:retro"))
        .collect()
}

fn ledger(rig: &Rig) -> Vec<Value> {
    let lines = read(&rig.paths().folder.join(FILE)).unwrap();
    lines
        .iter()
        .map(|line| serde_json::to_value(line).unwrap())
        .collect()
}

#[test]
fn a_merged_work_item_is_asked_for_a_retro_once_and_its_reply_is_saved_unverified() {
    let (rig, runner, _) = Rig::parked("koji");
    rig.retro_on();
    drop(runner);
    let runner = rig.open().unwrap();
    rig.claude.script([Scripted::Say(REPORT)]);

    merge(&rig, &runner);

    let calls = retro_calls(&rig);
    assert_eq!(calls.len(), 1, "{calls:#?}");
    let files = saved(&rig);
    let [file] = files.as_slice() else {
        panic!("one report: {files:?}")
    };
    let name = file.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("koji-7-") && name.ends_with(".md"),
        "{name}"
    );
    let text = fs::read_to_string(file).unwrap();
    let session = calls[0].session.id().0.clone();
    for expected in [
        "koji",
        "#7",
        "#71",
        "Merged: yes",
        "sonnet-high",
        session.as_str(),
        REPORT,
    ] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
    assert!(!rig.worktree_7().exists());

    let retros: Vec<Value> = ledger(&rig)
        .into_iter()
        .filter(|line| line["kind"] == "retro")
        .collect();
    let [line] = retros.as_slice() else {
        panic!("one retro line: {retros:#?}")
    };
    assert_eq!(
        (&line["issue"], &line["role"]),
        (&Value::from(7), &Value::from("worker"))
    );
}

// A pull request parked on its merge ruling, in a project that runs its retro
fn parked_with_retro() -> (Rig, Mutex<Runner>) {
    let (rig, runner, _) = Rig::parked_set("koji", Rig::retro_on);
    (rig, runner)
}

fn notes(runner: &Mutex<Runner>) -> String {
    runner.lock().unwrap().take_notes().join("\n")
}

#[test]
fn the_retro_resumes_the_workers_session_with_tools_that_change_nothing() {
    let (rig, runner) = parked_with_retro();
    rig.claude.script([Scripted::Say(REPORT)]);
    merge(&rig, &runner);

    let seen = rig.claude.all_seen();
    let worker = &seen[0].call;
    let retro = seen
        .iter()
        .find(|s| s.call.prompt.starts_with("/mattpocock:retro"))
        .expect("the retro ran");
    assert_eq!(
        retro.call.session,
        Session::Resume(worker.session.id().clone())
    );
    assert_eq!(retro.call.cwd, worker.cwd);
    assert_eq!(retro.call.tools, Tools::Retro);
    assert!(retro.call.prompt.contains("issue #7, has finished"));
    assert!(retro.call.prompt.contains("pull request is #71"));
    let denied: Vec<&str> = retro.settings["permissions"]["deny"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for tool in [
        "Bash",
        "Edit",
        "Write",
        "MultiEdit",
        "NotebookEdit",
        "Agent",
    ] {
        assert!(denied.contains(&tool), "{tool}: {denied:?}");
    }
}

#[test]
fn a_retro_that_fails_is_logged_and_skipped_without_holding_up_the_end() {
    let (rig, runner) = parked_with_retro();
    let broke = AgentError::Failed(Harness::ClaudeCode, "the model is down".into());
    rig.claude.script([Scripted::Fail(broke)]);
    merge(&rig, &runner);

    assert_eq!(retro_calls(&rig).len(), 1, "it is not retried");
    assert_eq!(saved(&rig), Vec::<PathBuf>::new());
    assert!(!rig.worktree_7().exists());
    let notes = notes(&runner);
    assert!(notes.contains("#7: the retro is skipped"), "{notes}");
    assert!(notes.contains("the model is down"), "{notes}");
}

#[test]
fn a_retro_whose_call_panics_is_skipped_and_does_not_end_the_runner() {
    let (rig, runner) = parked_with_retro();
    rig.claude.script([Scripted::Kill]);
    merge(&rig, &runner);

    assert_eq!(retro_calls(&rig).len(), 1, "it is not retried");
    assert_eq!(saved(&rig), Vec::<PathBuf>::new());
    assert!(!rig.worktree_7().exists());
    let notes = notes(&runner);
    assert!(notes.contains("#7: the retro is skipped"), "{notes}");
    assert!(notes.contains("panicked"), "{notes}");
}

#[test]
fn a_retro_that_comes_back_empty_saves_nothing() {
    let (rig, runner) = parked_with_retro();
    rig.claude.script([Scripted::Say("  \n")]);
    merge(&rig, &runner);

    assert_eq!(saved(&rig), Vec::<PathBuf>::new());
    assert!(notes(&runner).contains("came back empty"));
}

#[test]
fn a_project_that_sets_the_retro_to_none_asks_for_none() {
    let (rig, runner, _) = Rig::parked("koji");
    merge(&rig, &runner);

    assert_eq!(retro_calls(&rig), []);
    assert_eq!(
        rig.claude.all_calls().len(),
        2,
        "the turn and the Claude round"
    );
    assert_eq!(saved(&rig), Vec::<PathBuf>::new());
}

#[test]
fn a_work_item_closed_with_no_change_is_asked_for_a_retro_too() {
    let rig = Rig::new("koji");
    rig.retro_on();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.close_issue(7);
    rig.claude
        .script([Scripted::Say("closed"), Scripted::Say(REPORT)]);
    let mut reports = Vec::new();
    for _ in 0..4 {
        reports.extend(step(&runner).unwrap());
    }

    assert!(
        reports
            .iter()
            .any(|r| matches!(r, StepReport::Finished { closed: true, .. })),
        "{reports:#?}"
    );
    let files = saved(&rig);
    let [file] = files.as_slice() else {
        panic!("one report: {files:?}")
    };
    let text = fs::read_to_string(file).unwrap();
    assert!(text.contains("- Pull request: none"), "{text}");
    assert!(text.contains("- Merged: no"), "{text}");
}

#[test]
fn a_retro_cut_short_by_a_stop_is_not_asked_for_again_after_a_restart() {
    let (rig, runner) = parked_with_retro();
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    rig.ask(&runner, "rule", Some("1 yes"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);
    std::thread::scope(|scope| {
        let retro = scope.spawn(|| step(&runner));
        let began = hold.entered(PATIENCE);
        rig.claude.stop();
        assert!(began, "the retro never began");
        retro.join().unwrap().unwrap();
    });
    drop(runner);

    let runner = rig.open().unwrap();
    let finished = step(&runner).unwrap();
    assert!(
        matches!(finished, Some(StepReport::Finished { merged: true, .. })),
        "{finished:?}"
    );
    assert_eq!(retro_calls(&rig).len(), 1);
    assert_eq!(saved(&rig), Vec::<PathBuf>::new());
}

#[test]
fn a_retro_skill_that_cannot_load_runs_kelpies_own_prompt() {
    let (rig, runner, _) = Rig::parked_set("koji", |rig| {
        rig.edit_settings(|s| {
            let s = s.replace(NO_RETRO, "");
            format!("{s}\n[app.dogs.kelpie.skills]\nretro = {{ kind = \"path\", path = \"/no/such/skill\" }}\n")
        });
    });
    rig.claude.script([Scripted::Say(REPORT)]);
    merge(&rig, &runner);

    let retro = rig.claude.calls().pop().unwrap();
    assert!(
        retro
            .prompt
            .starts_with("Your work item, issue #7, has finished."),
        "{}",
        retro.prompt
    );
    assert_eq!(retro.tools, Tools::Retro);
    assert_eq!(saved(&rig).len(), 1);
}

#[test]
fn a_retro_in_flight_holds_no_slot_so_the_next_issue_opens() {
    // Under `auto` the item never parks, so it keeps its slot to the end.
    let (rig, runner, head) = Rig::with_pull_request_set("koji", |rig| {
        rig.retro_on();
        rig.merge_auto();
    });
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    rig.clock.advance(CHECKS_SETTLE);
    std::thread::scope(|scope| {
        let retro = scope.spawn(|| step(&runner));
        let began = hold.entered(PATIENCE);
        rig.forge.list_ready(8, false);
        // The merge notice goes first, then the board.
        let passes: Vec<String> = (0..3)
            .filter(|_| began)
            .map(|_| format!("{:?}", advance(&runner).unwrap()))
            .collect();
        hold.release();
        retro.join().unwrap().unwrap();
        assert!(began, "the retro never began");
        assert!(
            passes.iter().any(|p| p.contains("Dispatched { issue: 8")),
            "{passes:#?}"
        );
    });
}

#[test]
fn a_runner_that_drains_skips_the_retro_and_still_ends_the_merged_item() {
    let (rig, runner) = parked_with_retro();
    rig.ask(&runner, "drain", None);
    merge(&rig, &runner);

    assert_eq!(retro_calls(&rig), []);
    assert_eq!(saved(&rig), Vec::<PathBuf>::new());
    assert!(!rig.worktree_7().exists());
    let notes = notes(&runner);
    assert!(notes.contains("#7: no retro"), "{notes}");
    assert!(notes.contains("draining"), "{notes}");
}
