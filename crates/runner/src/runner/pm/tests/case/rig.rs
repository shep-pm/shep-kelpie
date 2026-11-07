//! What the case tests share: a project parked on a red CI ruling, its wake
//! and the files its case reads

use std::sync::Mutex;

use serde_json::{Value, json};

use super::super::{answered, pm_seen, red as red_ci, step_until};
use crate::ports::HeadRun;
use crate::runner::{Runner, StepReport};
use crate::test::{Rig, Scripted, Seen};

pub(super) const NOTHING: &str = "{\"pick\": null}";

/// Raw issue text, so the hostile lines of one test are the ones its asserts look for
pub(super) const BODY: &str = "Make `--dry-run` the name.\n\n## Acceptance\n\n- it matches shep\n";

/// A project with #7's pull request 71 parked on a red CI ruling, its issue's
/// title and body as given
pub(super) fn red(project: &str, title: &str, body: &str) -> (Rig, Mutex<Runner>, String) {
    let (title, body) = (title.to_owned(), body.to_owned());
    red_ci::raised(project, move |rig| rig.forge.open_issue(7, &title, &body))
}

/// ... with CodeRabbit listed, which a runner reads as it opens
pub(super) fn red_with_coderabbit(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner, _) = red(project, "A title", BODY);
    drop(runner);
    rig.coderabbit_on();
    let runner = rig.open().unwrap();
    (rig, runner)
}

/// Tells the project manager something and waits for its answer, which is its wake
pub(super) fn wake(rig: &Rig, runner: &Mutex<Runner>) -> Seen {
    rig.ask(runner, "tell", Some("how is #7?"));
    rig.claude.script([Scripted::Say(NOTHING)]);
    answered(Some(step_until(runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));
    pm_seen(rig).pop().unwrap()
}

/// `cases/<issue>.md` in the project manager's folder, when there is one
pub(super) fn case(rig: &Rig, issue: u64) -> Option<String> {
    std::fs::read_to_string(rig.paths().pm.join(format!("cases/{issue}.md"))).ok()
}

pub(super) fn board_of(rig: &Rig) -> String {
    std::fs::read_to_string(rig.paths().pm.join("board.md")).unwrap()
}

/// Every file in the project manager's folder, as one text
pub(super) fn everything_it_holds(rig: &Rig) -> String {
    let mut all = String::new();
    let mut folders = vec![rig.paths().pm];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(folder).unwrap().flatten() {
            match entry.file_type().unwrap().is_dir() {
                true => folders.push(entry.path()),
                false => all.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default()),
            }
        }
    }
    all
}

pub(super) fn head_run(workflow: &str, run: u64, conclusion: Option<&str>) -> HeadRun {
    HeadRun {
        workflow: workflow.to_owned(),
        run,
        attempt: 1,
        conclusion: conclusion.map(str::to_owned),
    }
}

/// A line of `rulings.jsonl`: a ruling answered on `issue`
pub(super) fn past(issue: u64, at: u64, kind: &str, asked: &str, answer: &str) -> Value {
    json!({
        "id": 71, "pull_request": 71, "by_pm": false,
        "at": at, "issue": issue, "kind": kind, "asked": asked, "answer": answer,
    })
}

/// Writes `lines` as a JSON line each to `file`, after what it holds
pub(super) fn append_lines(file: std::path::PathBuf, lines: &[Value]) {
    let mut text = std::fs::read_to_string(&file).unwrap_or_default();
    lines
        .iter()
        .for_each(|line| text.push_str(&format!("{line}\n")));
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
}

/// The part of #7's case under `heading`, up to the next heading
pub(super) fn part(rig: &Rig, heading: &str) -> String {
    let case = case(rig, 7).unwrap();
    let part = case
        .split(heading)
        .nth(1)
        .unwrap_or_else(|| panic!("no {heading}: {case}"));
    part.split("\n## ").next().unwrap().to_owned()
}

/// Logs `steps` in the steps file, after what it holds
pub(super) fn steps_logged(rig: &Rig, steps: &[Value]) {
    append_lines(rig.paths().folder.join("steps.jsonl"), steps);
}

/// What #7's worker session said, in a Claude Code transcript its steps name
pub(super) fn worker_said(rig: &Rig, runner: &Mutex<Runner>, said: &[&str]) {
    let session = runner.lock().unwrap().state.work_items[0].session.0.clone();
    let folder = rig.paths().folder.join("transcripts");
    let blocks = |text: &str| json!([{ "type": "text", "text": text }]);
    let lines: Vec<Value> = said
        .iter()
        .map(|text| {
            let message = json!({ "role": "assistant", "content": blocks(text) });
            json!({ "type": "assistant", "timestamp": "2026-10-10T12:00:02Z", "message": message })
        })
        .collect();
    append_lines(folder.join(format!("{session}.jsonl")), &lines);
    steps_logged(
        rig,
        &[json!({
            "at": "2026-10-10T12:00:00.000+00:00", "step": "session-started", "issue": 7,
            "role": "worker", "agent": "sonnet", "harness": "claude-code", "session": session,
            "transcript": { "folder": folder },
        })],
    );
}
