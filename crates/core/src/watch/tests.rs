//! The watch, run over a runner log and transcripts written to a temporary folder

use crate::test::ticket;
use std::io::Write;
use std::path::{Path, PathBuf};

use jiff::tz::TimeZone;

use super::{Format, Options, Scope, Watch};

struct Rig {
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    // The project's folder, where the runner keeps `steps.jsonl`
    fn folder(&self) -> PathBuf {
        self.dir.path().to_owned()
    }

    fn steps(&self) -> PathBuf {
        self.folder().join("steps.jsonl")
    }

    // The shepherd's log of what the runner prints, which `watch` does not read
    fn out_log(&self) -> PathBuf {
        self.dir.path().join("koji-0-out.log")
    }

    fn transcripts(&self) -> PathBuf {
        self.dir.path().join("transcripts")
    }

    fn append(&self, path: &Path, lines: &[&str]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    // A step as the runner writes it: its JSON object with the time first
    fn step(&self, at: &str, json: &str) {
        let line = json.replacen('{', &format!(r#"{{"at":"{at}","#), 1);
        self.append(&self.steps(), &[&line]);
    }

    fn transcript(&self, session: &str, lines: &[&str]) {
        self.append(&self.transcripts().join(format!("{session}.jsonl")), lines);
    }

    fn options(&self, scope: Scope, back: usize) -> Options {
        Options {
            scope,
            back,
            full: false,
            zone: TimeZone::UTC,
        }
    }

    // The step that names a Claude Code session, as the runner logs it
    fn started(&self, session: &str, who: &str) -> String {
        format!(
            r#"{{"step":"session-started",{who},"harness":"claude-code","session":"{session}","transcript":{{"folder":{:?}}}}}"#,
            self.transcripts()
        )
    }
}

// Columns are padded for the eye, so tests compare them squeezed.
fn shown(lines: &[super::Line]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.to_string()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

#[test]
fn a_project_manager_wake_shows_its_prompt_and_answer_as_pm() {
    let rig = Rig::new();
    let (mut watch, first) = Watch::start(rig.folder(), rig.options(Scope::Project, 0));
    assert!(first.is_empty());

    rig.step(
        "2026-10-10T12:00:00.100+00:00",
        &rig.started("pm-1", r#""role":"pm","agent":"manager""#),
    );
    rig.transcript(
        "pm-1",
        &[
            r#"{"type":"user","message":{"role":"user","content":"The board changed: #7 is ready"},"timestamp":"2026-10-10T12:00:01.000Z"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Picking #7."}]},"timestamp":"2026-10-10T12:00:02.000Z"}"#,
        ],
    );

    let lines = shown(&watch.poll());
    assert_eq!(lines.len(), 3, "{lines:#?}");
    assert!(
        lines[0].starts_with("12:00:00 runner step session-started agent=manager"),
        "{lines:#?}"
    );
    assert_eq!(
        lines[1],
        "12:00:01 pm prompt The board changed: #7 is ready"
    );
    assert_eq!(lines[2], "12:00:02 pm said Picking #7.");
}

fn user(at: &str, text: &str) -> String {
    format!(
        r#"{{"type":"user","message":{{"role":"user","content":"{text}"}},"timestamp":"{at}"}}"#
    )
}

fn said(at: &str, text: &str) -> String {
    format!(
        r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{text}"}}]}},"timestamp":"{at}"}}"#
    )
}

// Two work items' workers, a reviewer and the project manager, all written.
fn busy_project(rig: &Rig) {
    for (at, who, session) in [
        (
            "12:00:00",
            r#""issue":7,"role":"worker","agent":"opus-high""#,
            "w7",
        ),
        (
            "12:00:01",
            r#""issue":8,"role":"worker","agent":"sonnet""#,
            "w8",
        ),
        ("12:00:02", r#""role":"pm","agent":"manager""#, "pm"),
        (
            "12:00:03",
            r#""issue":7,"role":"reviewer","agent":"defect-hunter""#,
            "r7",
        ),
    ] {
        rig.step(
            &format!("2026-10-10T{at}.000+00:00"),
            &rig.started(session, who),
        );
    }
    rig.transcript("w7", &[&user("2026-10-10T12:00:10.000Z", "Build #7")]);
    rig.transcript("w8", &[&said("2026-10-10T12:00:11.000Z", "Building #8")]);
    rig.transcript("pm", &[&said("2026-10-10T12:00:12.000Z", "Waiting")]);
    rig.transcript("r7", &[&said("2026-10-10T12:00:13.000Z", "Looks fine")]);
    rig.step(
        "2026-10-10T12:00:14.000+00:00",
        r#"{"step":"ended","issue":8,"session":"w8"}"#,
    );
    rig.step(
        "2026-10-10T12:00:15.000+00:00",
        r#"{"step":"pm-answered","session":"pm","woke_for":["tell"],"acted":[]}"#,
    );
}

#[test]
fn watching_one_issue_shows_only_its_sessions_and_steps() {
    let rig = Rig::new();
    busy_project(&rig);
    let (_watch, lines) = Watch::start(rig.folder(), rig.options(Scope::Issue(ticket(7)), 100));
    let lines = shown(&lines);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("#7 worker (opus-high) prompt Build #7"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("#7 reviewer (defect-hunter) said Looks fine"))
    );
    assert!(
        lines
            .iter()
            .all(|l| !l.contains("#8") && !l.contains("manager")),
        "{lines:#?}"
    );
    assert_eq!(lines.len(), 4, "{lines:#?}");
}

#[test]
fn watching_the_project_manager_shows_its_sessions_and_steps() {
    let rig = Rig::new();
    busy_project(&rig);
    let (_watch, lines) = Watch::start(rig.folder(), rig.options(Scope::Pm, 100));
    let lines = shown(&lines);
    assert_eq!(lines.len(), 3, "{lines:#?}");
    assert!(lines[0].starts_with("12:00:02 runner step session-started"));
    assert_eq!(lines[1], "12:00:12 pm said Waiting");
    assert!(lines[2].starts_with("12:00:15 runner step pm-answered"));
}

#[test]
fn back_shows_the_last_lines_of_all_sources_sorted_by_time() {
    let rig = Rig::new();
    busy_project(&rig);
    let (_watch, lines) = Watch::start(rig.folder(), rig.options(Scope::Project, 4));
    let lines = shown(&lines);
    assert_eq!(lines.len(), 4, "{lines:#?}");
    assert_eq!(lines[0], "12:00:12 pm said Waiting");
    assert_eq!(
        lines[1],
        "12:00:13 #7 reviewer (defect-hunter) said Looks fine"
    );
    assert!(lines[2].starts_with("12:00:14 runner step ended #8"));
    assert!(lines[3].starts_with("12:00:15 runner step pm-answered"));
}

#[test]
fn it_starts_at_the_end_and_follows_a_session_already_running() {
    let rig = Rig::new();
    busy_project(&rig);
    let (mut watch, first) = Watch::start(rig.folder(), rig.options(Scope::Project, 0));
    assert!(first.is_empty());
    assert!(watch.poll().is_empty());

    rig.transcript("w7", &[&said("2026-10-10T12:01:00.000Z", "Tests pass")]);
    assert_eq!(
        shown(&watch.poll()),
        ["12:01:00 #7 worker (opus-high) said Tests pass"]
    );
}

#[test]
fn a_line_still_being_written_is_shown_once_it_ends() {
    let rig = Rig::new();
    busy_project(&rig);
    let (mut watch, _) = Watch::start(rig.folder(), rig.options(Scope::Project, 0));
    let path = rig.transcripts().join("pm.jsonl");
    let line = said("2026-10-10T12:01:00.000Z", "Half");
    let (head, tail) = line.split_at(line.len() / 2);
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    write!(file, "{head}").unwrap();
    assert!(watch.poll().is_empty());
    writeln!(file, "{tail}").unwrap();
    assert_eq!(shown(&watch.poll()), ["12:01:00 pm said Half"]);
}

#[test]
fn a_session_resumed_shows_only_its_new_call() {
    let rig = Rig::new();
    rig.step(
        "2026-10-10T12:00:00.000+00:00",
        &rig.started("w7", r#""issue":7,"role":"worker","agent":"opus-high""#),
    );
    rig.transcript(
        "w7",
        &[
            &user("2026-10-10T12:00:01.000Z", "First turn"),
            &said("2026-10-10T12:00:02.000Z", "First answer"),
        ],
    );
    let (mut watch, _) = Watch::start(rig.folder(), rig.options(Scope::Project, 0));

    // The next call resumes the session, a minute later.
    rig.step(
        "2026-10-10T12:01:00.000+00:00",
        &rig.started("w7", r#""issue":7,"role":"worker","agent":"opus-high""#),
    );
    rig.transcript("w7", &[&user("2026-10-10T12:01:01.000Z", "CI failed")]);
    let lines = shown(&watch.poll());
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(lines[0].starts_with("12:01:00 runner step session-started"));
    assert_eq!(lines[1], "12:01:01 #7 worker (opus-high) prompt CI failed");
}

#[test]
fn a_session_started_while_watching_is_read_from_its_first_line() {
    let rig = Rig::new();
    let (mut watch, _) = Watch::start(rig.folder(), rig.options(Scope::Project, 0));
    rig.transcript(
        "w7",
        &[
            &user("2026-10-10T12:00:01.000Z", "Build #7"),
            &said("2026-10-10T12:00:02.000Z", "On it"),
        ],
    );
    rig.step(
        "2026-10-10T12:00:00.100+00:00",
        &rig.started("w7", r#""issue":7,"role":"worker","agent":"opus-high""#),
    );
    let lines = shown(&watch.poll());
    assert_eq!(lines.len(), 3, "{lines:#?}");
    assert_eq!(lines[1], "12:00:01 #7 worker (opus-high) prompt Build #7");
}

#[test]
fn tools_and_their_results_show_as_one_clipped_line_each() {
    let rig = Rig::new();
    rig.step(
        "2026-10-10T12:00:00.000+00:00",
        &rig.started("w7", r#""issue":7,"role":"worker","agent":"opus-high""#),
    );
    let long = "x".repeat(300);
    rig.transcript(
        "w7",
        &[
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"git log\n  --oneline"}}]},"timestamp":"2026-10-10T12:00:01.000Z"}"#,
            &format!(
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"abc\nline two\n{long}"}}]}},"timestamp":"2026-10-10T12:00:02.000Z"}}"#
            ),
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","is_error":true,"content":[{"type":"text","text":"no such file"}]}]},"timestamp":"2026-10-10T12:00:03.000Z"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Maybe the path","signature":"s"}]},"timestamp":"2026-10-10T12:00:04.000Z"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"","signature":"s"}]},"timestamp":"2026-10-10T12:00:05.000Z"}"#,
        ],
    );
    let (_watch, lines) = Watch::start(rig.folder(), rig.options(Scope::Project, 100));
    let lines = shown(&lines);
    assert_eq!(lines.len(), 5, "{lines:#?}");
    assert_eq!(
        lines[1],
        "12:00:01 #7 worker (opus-high) tool Bash git log --oneline"
    );
    assert!(lines[2].starts_with("12:00:02 #7 worker (opus-high) result abc line two xxx"));
    assert!(lines[2].ends_with('…'), "{}", lines[2]);
    assert_eq!(
        lines[3],
        "12:00:03 #7 worker (opus-high) result error: no such file"
    );
    assert_eq!(
        lines[4],
        "12:00:04 #7 worker (opus-high) thinking Maybe the path"
    );
}

#[test]
fn full_shows_prompts_and_results_whole() {
    let rig = Rig::new();
    rig.step(
        "2026-10-10T12:00:00.000+00:00",
        &rig.started("pm", r#""role":"pm","agent":"manager""#),
    );
    let long = "y".repeat(300);
    rig.transcript(
        "pm",
        &[&user(
            "2026-10-10T12:00:01.000Z",
            &format!("Board:\\n- #7 ready\\n{long}"),
        )],
    );
    let mut options = rig.options(Scope::Project, 100);
    options.full = true;
    let (_watch, lines) = Watch::start(rig.folder(), options);
    let prompt = lines[1].to_string();
    let rows: Vec<&str> = prompt.lines().collect();
    assert_eq!(rows.len(), 3, "{prompt}");
    assert!(rows[0].ends_with("Board:"));
    assert!(rows[1].trim_start() == "- #7 ready");
    assert!(rows[2].ends_with(&long));
}

#[test]
fn a_codex_session_shows_from_its_rollout_once_kelpie_knows_its_thread() {
    let rig = Rig::new();
    let home = rig.dir.path().join("codex-home");
    let threads = rig.dir.path().join("threads");
    std::fs::create_dir_all(&threads).unwrap();
    std::fs::write(threads.join("w7.json"), r#"{"id":"0199-thread"}"#).unwrap();
    let rollout = home.join("sessions/2026/10/10/rollout-2026-10-10T12-00-00-0199-thread.jsonl");
    rig.step(
        "2026-10-10T12:00:00.000+00:00",
        &format!(
            r#"{{"step":"session-started","issue":7,"role":"worker","agent":"gpt","harness":"codex","session":"w7","transcript":{{"folder":{:?},"threads":{:?}}}}}"#,
            home.join("sessions"),
            threads
        ),
    );
    rig.append(
        &rollout,
        &[
            r#"{"timestamp":"2026-10-10T12:00:01.000Z","type":"session_meta","payload":{"id":"0199-thread"}}"#,
            r#"{"timestamp":"2026-10-10T12:00:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Build #7"}]}}"#,
            r#"{"timestamp":"2026-10-10T12:00:03.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"ls\"]}","call_id":"c1"}}"#,
            r#"{"timestamp":"2026-10-10T12:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"{\"output\":\"Cargo.toml\\n\",\"metadata\":{}}"}}"#,
            r#"{"timestamp":"2026-10-10T12:00:05.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done"}]}}"#,
            r#"{"timestamp":"2026-10-10T12:00:05.500Z","type":"event_msg","payload":{"type":"agent_message","message":"Done"}}"#,
        ],
    );
    let (_watch, lines) = Watch::start(rig.folder(), rig.options(Scope::Project, 100));
    let lines = shown(&lines);
    assert_eq!(lines.len(), 5, "{lines:#?}");
    assert_eq!(lines[1], "12:00:02 #7 worker (gpt) prompt Build #7");
    assert_eq!(lines[2], "12:00:03 #7 worker (gpt) tool shell bash -lc ls");
    assert_eq!(lines[3], "12:00:04 #7 worker (gpt) result Cargo.toml");
    assert_eq!(lines[4], "12:00:05 #7 worker (gpt) said Done");
}

#[test]
fn a_codex_first_turn_shows_from_the_rollout_no_session_claims() {
    let rig = Rig::new();
    let home = rig.dir.path().join("codex-home");
    let threads = rig.dir.path().join("threads");
    std::fs::create_dir_all(&threads).unwrap();
    // Another session's thread, and its rollout, written before this call.
    std::fs::write(threads.join("w9.json"), r#"{"id":"other"}"#).unwrap();
    let sessions = home.join("sessions/2026/10/10");
    rig.append(
        &sessions.join("rollout-2020-10-10T11-00-00-other.jsonl"),
        &[r#"{"timestamp":"2020-10-10T11:00:01.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Not mine"}]}}"#],
    );
    let (mut watch, _) = Watch::start(rig.folder(), rig.options(Scope::Project, 0));
    rig.step(
        "2020-10-10T12:00:00.000+00:00",
        &format!(
            r#"{{"step":"session-started","issue":7,"role":"worker","agent":"gpt","harness":"codex","session":"w7","transcript":{{"folder":{:?},"threads":{:?}}}}}"#,
            home.join("sessions"),
            threads
        ),
    );
    rig.append(
        &sessions.join("rollout-2020-10-10T12-00-00-fresh.jsonl"),
        &[r#"{"timestamp":"2020-10-10T12:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Mine"}]}}"#],
    );
    let lines = shown(&watch.poll());
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert_eq!(lines[1], "12:00:02 #7 worker (gpt) said Mine");
}

#[test]
fn a_pi_session_shows_from_its_session_file() {
    let rig = Rig::new();
    let sessions = rig.dir.path().join("pi-home/sessions");
    rig.step(
        "2026-10-10T12:00:00.000+00:00",
        &format!(
            r#"{{"step":"session-started","issue":7,"role":"reviewer","agent":"qwen","harness":"pi","session":"r7","transcript":{{"folder":{:?}}}}}"#,
            sessions
        ),
    );
    rig.append(
        &sessions.join("2026-10-10T12-00-00-000Z_r7.jsonl"),
        &[
            r#"{"type":"session","version":3,"id":"r7","timestamp":"2026-10-10T12:00:00.500Z","cwd":"/wt"}"#,
            r#"{"type":"message","id":"a1","timestamp":"2026-10-10T12:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"Review #7"}],"timestamp":1790856001000}}"#,
            r#"{"type":"message","id":"a2","timestamp":"2026-10-10T12:00:02.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Look at the diff"},{"type":"toolCall","id":"c1","name":"read","arguments":{"path":"src/lib.rs"}}]}}"#,
            r#"{"type":"message","id":"a3","timestamp":"2026-10-10T12:00:03.000Z","message":{"role":"toolResult","toolCallId":"c1","toolName":"read","content":[{"type":"text","text":"fn main() {}"}],"isError":false}}"#,
            r#"{"type":"message","id":"a4","timestamp":"2026-10-10T12:00:04.000Z","message":{"role":"assistant","content":[{"type":"text","text":"CLEAN"}]}}"#,
        ],
    );
    let (_watch, lines) = Watch::start(rig.folder(), rig.options(Scope::Project, 100));
    let lines = shown(&lines);
    assert_eq!(lines.len(), 6, "{lines:#?}");
    assert_eq!(lines[1], "12:00:01 #7 reviewer (qwen) prompt Review #7");
    assert_eq!(
        lines[2],
        "12:00:02 #7 reviewer (qwen) thinking Look at the diff"
    );
    assert_eq!(lines[3], "12:00:02 #7 reviewer (qwen) tool read src/lib.rs");
    assert_eq!(lines[4], "12:00:03 #7 reviewer (qwen) result fn main() {}");
    assert_eq!(lines[5], "12:00:04 #7 reviewer (qwen) said CLEAN");
}

// The step that names a goose call, as the runner logs it: the transcript
// folder is the call's goose root, whose sibling `<stem>.stdout.jsonl` is the
// stdout file.
fn goose_started(rig: &Rig) -> (String, PathBuf) {
    let root = rig.dir.path().join("worker/review-settings.goose");
    let step = format!(
        r#"{{"step":"session-started","issue":7,"role":"reviewer","agent":"qwen","harness":"goose","session":"g7","transcript":{{"folder":{root:?}}}}}"#
    );
    (
        step,
        rig.dir.path().join("worker/review-settings.stdout.jsonl"),
    )
}

#[test]
fn a_goose_session_shows_from_the_stdout_file_of_its_call() {
    let rig = Rig::new();
    let (step, stdout) = goose_started(&rig);
    rig.step("2026-10-10T12:00:00.000+00:00", &step);
    rig.append(
        &stdout,
        &[
            "    __( O)>  ● new session · openai qwen3.8:27b",
            "   \\____)    20261010_1 · /wt",
            "     L L     goose is ready",
            r#"{"type":"message","message":{"role":"assistant","created":1791633601,"content":[{"type":"thinking","thinking":"Look at the diff"},{"type":"toolRequest","id":"c1","toolCall":{"status":"success","value":{"name":"read","arguments":{"path":"src/lib.rs"}}}}]}}"#,
            r#"{"type":"message","message":{"role":"user","created":1791633602,"content":[{"type":"toolResponse","id":"c1","toolResult":{"status":"success","value":{"content":[{"type":"text","text":"fn main() {}"}],"isError":false}}}]}}"#,
            r#"{"type":"message","message":{"role":"assistant","created":1791633603,"content":[{"type":"text","text":"CLEAN"}]}}"#,
            r#"{"type":"error","error":"model host went away"}"#,
            r#"{"type":"complete","total_tokens":3}"#,
        ],
    );
    let (_watch, lines) = Watch::start(rig.folder(), rig.options(Scope::Project, 100));
    let lines = shown(&lines);
    assert_eq!(lines.len(), 6, "{lines:#?}");
    assert_eq!(
        lines[1],
        "12:00:01 #7 reviewer (qwen) thinking Look at the diff"
    );
    assert_eq!(lines[2], "12:00:01 #7 reviewer (qwen) tool read src/lib.rs");
    assert_eq!(lines[3], "12:00:02 #7 reviewer (qwen) result fn main() {}");
    assert_eq!(lines[4], "12:00:03 #7 reviewer (qwen) said CLEAN");
    // goose gives an error no time, so it takes the time of the line before.
    assert_eq!(
        lines[5],
        "12:00:03 #7 reviewer (qwen) result error: model host went away"
    );
}

#[test]
fn a_goose_error_with_nothing_before_it_takes_the_time_its_call_started() {
    let rig = Rig::new();
    let (step, stdout) = goose_started(&rig);
    rig.step("2026-10-10T12:00:00.000+00:00", &step);
    rig.append(&stdout, &[r#"{"type":"error","error":"no route to host"}"#]);
    let (_watch, lines) = Watch::start(rig.folder(), rig.options(Scope::Project, 100));
    assert_eq!(
        shown(&lines)[1],
        "12:00:00 #7 reviewer (qwen) result error: no route to host"
    );
}

// Every call at one settings path writes the same stdout file, so a call's
// lines belong to the newest session that names it.
fn goose_call(rig: &Rig, at: &str, session: &str) -> (String, PathBuf) {
    let (step, stdout) = goose_started(rig);
    let step = step.replace(r#""session":"g7""#, &format!(r#""session":"{session}""#));
    rig.step(at, &step);
    (step, stdout)
}

fn goose_said(created: u64, text: &str) -> String {
    format!(
        r#"{{"type":"message","message":{{"role":"assistant","created":{created},"content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
}

#[test]
fn two_goose_sessions_at_one_settings_path_show_the_file_once() {
    let rig = Rig::new();
    let (_, stdout) = goose_call(&rig, "2026-10-10T12:00:00.000+00:00", "g1");
    goose_call(&rig, "2026-10-10T12:05:00.000+00:00", "g2");
    // The file holds the second call only, since each call empties it.
    rig.append(&stdout, &[&goose_said(1791633901, "second")]);
    let (_watch, lines) = Watch::start(rig.folder(), rig.options(Scope::Project, 100));
    let said: Vec<_> = shown(&lines)
        .into_iter()
        .filter(|line| line.contains(" said "))
        .collect();
    assert_eq!(said, ["12:05:01 #7 reviewer (qwen) said second"]);
}

#[test]
fn a_goose_call_started_while_watching_replaces_the_one_before_it_at_that_path() {
    let rig = Rig::new();
    let (_, stdout) = goose_call(&rig, "2026-10-10T12:00:00.000+00:00", "g1");
    rig.append(&stdout, &[&goose_said(1791633601, "first")]);
    let (mut watch, _) = Watch::start(rig.folder(), rig.options(Scope::Project, 100));

    // The next call empties the file and writes it afresh, past the old offset.
    goose_call(&rig, "2026-10-10T12:05:00.000+00:00", "g2");
    std::fs::write(&stdout, "").unwrap();
    rig.append(
        &stdout,
        &[
            &goose_said(1791633901, "second"),
            &goose_said(1791633902, "and more"),
            &goose_said(1791633903, "and more still"),
        ],
    );
    let said: Vec<_> = shown(&watch.poll())
        .into_iter()
        .filter(|line| line.contains(" said "))
        .collect();
    assert_eq!(
        said,
        [
            "12:05:01 #7 reviewer (qwen) said second",
            "12:05:02 #7 reviewer (qwen) said and more",
            "12:05:03 #7 reviewer (qwen) said and more still",
        ]
    );
}

#[test]
fn a_goose_session_resumed_while_watching_reads_its_emptied_file_afresh() {
    let rig = Rig::new();
    let (_, stdout) = goose_call(&rig, "2026-10-10T12:00:00.000+00:00", "g1");
    rig.append(&stdout, &[&goose_said(1791633601, "first")]);
    let (mut watch, _) = Watch::start(rig.folder(), rig.options(Scope::Project, 100));

    goose_call(&rig, "2026-10-10T12:05:00.000+00:00", "g1");
    std::fs::write(&stdout, "").unwrap();
    rig.append(
        &stdout,
        &[
            &goose_said(1791633901, "second"),
            &goose_said(1791633902, "and more"),
        ],
    );
    let said: Vec<_> = shown(&watch.poll())
        .into_iter()
        .filter(|line| line.contains(" said "))
        .collect();
    assert_eq!(
        said,
        [
            "12:05:01 #7 reviewer (qwen) said second",
            "12:05:02 #7 reviewer (qwen) said and more",
        ]
    );
}

#[test]
fn a_goose_transcript_that_is_not_there_yet_is_waited_for() {
    let rig = Rig::new();
    let (mut watch, first) = Watch::start(rig.folder(), rig.options(Scope::Project, 0));
    assert!(first.is_empty());
    let (step, stdout) = goose_started(&rig);
    rig.step("2026-10-10T12:00:00.100+00:00", &step);
    // The runner's step is shown, and the stdout file is not there to read.
    assert_eq!(shown(&watch.poll()).len(), 1);
    assert!(watch.poll().is_empty());

    rig.append(
        &stdout,
        &[
            r#"{"type":"message","message":{"role":"assistant","created":1791633601,"content":[{"type":"text","text":"CLEAN"}]}}"#,
        ],
    );
    assert_eq!(
        shown(&watch.poll()),
        ["12:00:01 #7 reviewer (qwen) said CLEAN"]
    );
}

#[test]
fn lines_that_are_not_steps_or_cannot_be_read_are_passed_over() {
    let rig = Rig::new();
    rig.append(
        &rig.steps(),
        &[
            "not a step",
            r#"{"at":"2026-10-10T12:00:00.000+00:00","project":"koji"}"#,
            r#"{"step":"finished","issue":7}"#,
            r#"{"at":"2026-10-10T12:00:01.000+00:00","step":"finished","issue":7,"merged":true}"#,
        ],
    );
    let (_watch, lines) = Watch::start(rig.folder(), rig.options(Scope::Project, 100));
    assert_eq!(
        shown(&lines),
        ["12:00:01 runner step finished #7 merged=true"]
    );
}

#[test]
fn watch_follows_a_project_from_steps_jsonl_and_ignores_the_out_log() {
    let rig = Rig::new();
    // What a runner before `steps.jsonl` printed, and what one prints now.
    rig.append(
        &rig.out_log(),
        &[
            r#"2026-10-10T12:00:00.000+00:00 {"step":"dispatched","issue":6,"agent":"old","skipped":[]}"#,
            "2026-10-10T12:00:01.000+00:00 #7 dispatched to claude-sonnet-5 (medium)",
        ],
    );
    let (mut watch, first) = Watch::start(rig.folder(), rig.options(Scope::Project, 100));
    assert!(first.is_empty(), "{first:?}");

    rig.step(
        "2026-10-10T12:00:02.000+00:00",
        r#"{"step":"dispatched","issue":7,"agent":"sonnet","skipped":[]}"#,
    );
    rig.append(
        &rig.out_log(),
        &[r#"2026-10-10T12:00:03.000+00:00 {"step":"finished","issue":7,"merged":true}"#],
    );
    assert_eq!(
        shown(&watch.poll()),
        ["12:00:02 runner step dispatched #7 agent=sonnet"]
    );
}

#[test]
fn all_a_claude_session_said_is_read() {
    let rig = Rig::new();
    rig.transcript(
        "w7",
        &[
            &user("2026-10-10T12:00:01.000Z", "Build #7"),
            &said("2026-10-10T12:00:02.000Z", "Reading the issue."),
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Hm"},{"type":"tool_use","id":"c1","name":"Read","input":{"file_path":"src/lib.rs"}},{"type":"text","text":""}]},"timestamp":"2026-10-10T12:00:03.000Z"}"#,
            &said("2026-10-10T12:00:04.000Z", "Done, but the test hangs."),
        ],
    );

    let file = rig.transcripts().join("w7.jsonl");
    assert_eq!(
        super::said(Format::Claude, &file),
        ["Reading the issue.", "Done, but the test hangs."]
    );
}

#[test]
fn all_a_codex_rollout_said_is_read() {
    let rig = Rig::new();
    let rollout = rig.dir.path().join("rollout-2026-10-10T12-00-00-0199-thread.jsonl");
    rig.append(
        &rollout,
        &[
            r#"{"timestamp":"2026-10-10T12:00:01.000Z","type":"session_meta","payload":{"id":"0199-thread"}}"#,
            r#"{"timestamp":"2026-10-10T12:00:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Build #7"}]}}"#,
            r#"{"timestamp":"2026-10-10T12:00:03.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Looking."}]}}"#,
            r#"{"timestamp":"2026-10-10T12:00:03.500Z","type":"event_msg","payload":{"type":"agent_message","message":"Looking."}}"#,
            r#"{"timestamp":"2026-10-10T12:00:05.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done"}]}}"#,
        ],
    );

    assert_eq!(super::said(Format::Codex, &rollout), ["Looking.", "Done"]);
}

#[test]
fn all_a_pi_session_said_is_read() {
    let rig = Rig::new();
    let file = rig.dir.path().join("2026-10-10T12-00-00-000Z_r7.jsonl");
    rig.append(
        &file,
        &[
            r#"{"type":"session","version":3,"id":"r7","timestamp":"2026-10-10T12:00:00.500Z","cwd":"/wt"}"#,
            r#"{"type":"message","id":"a1","timestamp":"2026-10-10T12:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"Review #7"}],"timestamp":1790856001000}}"#,
            r#"{"type":"message","id":"a2","timestamp":"2026-10-10T12:00:02.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Look"},{"type":"text","text":"Reading the diff."},{"type":"toolCall","id":"c1","name":"read","arguments":{"path":"src/lib.rs"}}]}}"#,
            r#"{"type":"message","id":"a4","timestamp":"2026-10-10T12:00:04.000Z","message":{"role":"assistant","content":[{"type":"text","text":"CLEAN"}]}}"#,
        ],
    );

    assert_eq!(
        super::said(Format::Pi, &file),
        ["Reading the diff.", "CLEAN"]
    );
}

#[test]
fn a_goose_session_reads_as_nothing_said() {
    let rig = Rig::new();
    let (_, stdout) = goose_started(&rig);
    rig.append(
        &stdout,
        &[
            r#"{"type":"message","message":{"role":"assistant","created":1791633603,"content":[{"type":"text","text":"CLEAN"}]}}"#,
        ],
    );

    assert!(super::said(Format::Goose, &stdout).is_empty());
}

#[test]
fn a_half_written_last_line_is_passed_over() {
    let rig = Rig::new();
    let file = rig.transcripts().join("w7.jsonl");
    rig.transcript("w7", &[&said("2026-10-10T12:00:02.000Z", "Whole.")]);
    let mut open = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
    // A whole line with no end yet, and a cut one: neither is read.
    write!(open, "{}", said("2026-10-10T12:00:03.000Z", "Not ended")).unwrap();

    assert_eq!(super::said(Format::Claude, &file), ["Whole."]);

    writeln!(open).unwrap();
    write!(open, r#"{{"type":"assistant","message":{{"role":"assist"#).unwrap();

    assert_eq!(super::said(Format::Claude, &file), ["Whole.", "Not ended"]);
    assert!(super::said(Format::Claude, &rig.transcripts().join("none.jsonl")).is_empty());
}

#[test]
fn steps_of_keeps_every_line_naming_the_ticket() {
    let rig = Rig::new();
    rig.step(
        "2026-10-10T12:00:00.000+00:00",
        r#"{"step":"dispatched","issue":7,"agent":"sonnet","skipped":[]}"#,
    );
    rig.step(
        "2026-10-10T12:00:01.000+00:00",
        r#"{"step":"dispatched","issue":8,"agent":"sonnet","skipped":[]}"#,
    );
    rig.step(
        "2026-10-10T12:00:02.000+00:00",
        r#"{"step":"pm-woke","reason":"a board change"}"#,
    );
    // A step from before ticket ids held a bare number reads as a GitHub ticket.
    rig.step(
        "2026-10-10T12:00:03.000+00:00",
        r#"{"step":"turn-failed","issue":7,"why":"timed out"}"#,
    );
    rig.append(&rig.steps(), &["not a step"]);
    let steps = std::fs::read_to_string(rig.steps()).unwrap();

    assert_eq!(
        super::steps_of(&steps, ticket(7)),
        [
            "2026-10-10T12:00:00Z dispatched #7 agent=sonnet",
            "2026-10-10T12:00:03Z turn-failed #7 why=timed out",
        ]
    );
    assert_eq!(super::steps_of(&steps, ticket(9)), Vec::<String>::new());
}
