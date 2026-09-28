//! Building the Claude calls the qwen-review loop makes itself: a fresh
//! review round, and the judge's one-shot on a single finding
//!
//! Neither is the worker's: both draw a fresh session id and run under
//! their own throwaway settings, never the worker's own. The Claude round
//! keeps its read tools, to check its own work against the worktree; the
//! judge has every tool denied, so its answer is exactly the JSON it was
//! asked for and nothing it read on the side.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::ports::{ClaudeCall, Finding, Role, Session, Severity, Verdict};
use crate::runner::turn;
use crate::settings::RoleModel;
use crate::shots::ShotsRun;
use crate::work_item::new_session_id;

/// The Claude round's throwaway settings: no sandbox fencing, no bypassed
/// permissions, but Read, Grep and Glob still work by default so it can
/// check its own work against the worktree
const REVIEW_SETTINGS_FILE: &str = "review-settings.json";

/// The judge's throwaway settings: every tool denied, so its one-shot answer
/// is schema-constrained text and nothing it read on the side
const JUDGE_SETTINGS_FILE: &str = "judge-settings.json";

/// Every tool name a Claude Code call can reach, denied outright for the judge
const NO_TOOLS: [&str; 11] = [
    "Bash",
    "Read",
    "Edit",
    "Write",
    "MultiEdit",
    "NotebookEdit",
    "Glob",
    "Grep",
    "WebFetch",
    "WebSearch",
    "Task",
];

/// Kelpie's shots for a Claude round, and the folder they sit under
#[derive(Debug, Clone, Copy)]
pub(super) struct Screens<'a> {
    pub(super) dir: &'a Path,
    pub(super) run: &'a ShotsRun,
}

pub(super) fn reviewer_call(
    worktree: &Path,
    worker_folder: &Path,
    model: &RoleModel,
    shots: Option<Screens<'_>>,
) -> Result<ClaudeCall, String> {
    let diff = diff_against_base(worktree)?;
    let settings = review_settings(worker_folder, shots.map(|s| s.dir))?;
    let mut prompt = reviewer_prompt(&base_ref(), &diff);
    if let Some(shots) = shots {
        prompt.push_str(&shots_prompt(shots.run));
    }
    build_call(Role::Reviewer, worktree, model, settings, prompt)
}

// A finding that names a PNG under `shots` is about a screenshot, and its
// judge may open that folder with Read and nothing else.
pub(in crate::runner) fn judge_call(
    worktree: &Path,
    worker_folder: &Path,
    model: &RoleModel,
    finding: &Finding,
    shots: Option<&Path>,
) -> Result<ClaudeCall, String> {
    let diff = diff_against_base(worktree)?;
    let shot = shots.filter(|dir| Path::new(&finding.file).starts_with(dir));
    let settings = judge_settings(worker_folder, shot)?;
    let mut prompt = judge_prompt(&base_ref(), &diff, finding);
    if shot.is_some() {
        prompt.push_str(&format!(
            "\n\nThe finding is about the screenshot {}. Open it with Read before you decide.",
            finding.file
        ));
    }
    build_call(Role::Judge, worktree, model, settings, prompt)
}

// The shape every call the review loop makes itself shares: a fresh
// session, the worktree as its folder, no instructions file, and whatever
// role, settings and prompt its caller worked out.
fn build_call(
    role: Role,
    worktree: &Path,
    model: &RoleModel,
    settings: PathBuf,
    prompt: String,
) -> Result<ClaudeCall, String> {
    let session = new_session_id().map_err(|e| format!("cannot draw a session id: {e}"))?;
    Ok(ClaudeCall {
        role,
        model: model.model.as_str().to_owned(),
        effort: model.effort,
        session: Session::New(session),
        cwd: worktree.to_owned(),
        settings,
        instructions: None,
        prompt,
        timeout: None,
        mcp_config: None,
    })
}

fn review_settings(worker_folder: &Path, shots: Option<&Path>) -> Result<PathBuf, String> {
    let path = worker_folder.join(REVIEW_SETTINGS_FILE);
    let settings = match shots {
        // Read outside the worktree is refused under `-p` unless the folder is added.
        Some(dir) => serde_json::json!({ "permissions": { "additionalDirectories": [dir] } }),
        None => serde_json::json!({}),
    };
    let text = serde_json::to_string_pretty(&settings).expect("settings are JSON");
    turn::write(worker_folder, &path, &text)?;
    Ok(path)
}

fn judge_settings(worker_folder: &Path, shot: Option<&Path>) -> Result<PathBuf, String> {
    let path = worker_folder.join(JUDGE_SETTINGS_FILE);
    let settings = match shot {
        Some(dir) => {
            let deny: Vec<&str> = NO_TOOLS.into_iter().filter(|t| *t != "Read").collect();
            serde_json::json!({ "permissions": { "deny": deny, "additionalDirectories": [dir] } })
        }
        None => serde_json::json!({ "permissions": { "deny": NO_TOOLS } }),
    };
    let text = serde_json::to_string_pretty(&settings).expect("settings are JSON");
    turn::write(worker_folder, &path, &text)?;
    Ok(path)
}

fn shots_prompt(run: &ShotsRun) -> String {
    format!(
        "\n\nKelpie took screenshots of this branch's UI from its dev server, at a \
         phone and a desktop width, light and dark. Open the ones the diff touches \
         with Read, and check the change looks right and nothing broke. A finding \
         about a screenshot gives its location as the PNG's full path and `:0`.\n\n\
         --- shots ---\n{}--- end ---",
        run.text()
    )
}

/// `origin/main`: the ref every call in this loop diffs against
fn base_ref() -> String {
    format!("origin/{}", crate::worktree::BASE)
}

fn diff_against_base(worktree: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["diff", &base_ref()])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git diff: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn reviewer_prompt(base: &str, diff: &str) -> String {
    format!(
        "You are a founding engineer reviewing a junior developer's pull request. Be \
         extremely critical and check every line of the diff below, against \
         `{base}`. You may open any file in this worktree with Read, Grep or Glob \
         to check your work; do not run any command and do not edit anything.\n\n\
         Look for: code smells; duplicated code, types or logic; non-performant code; \
         non-idiomatic code for this language; hard-to-follow logic; poorly named \
         variables, functions and types; missing or inadequate doc comments; comments \
         that no longer match the code; missing error handling; unsafe assumptions \
         about input; and thin test coverage for new logic.\n\n\
         Report only real problems, never formatting or anything a linter already \
         enforces. Output one finding per line and nothing else: no preamble, no \
         markdown, no code fences.\n\
         SEVERITY|file:line|what is wrong|why it matters\n\n\
         SEVERITY must be HIGH, MEDIUM or LOW. If the diff is genuinely fine, output \
         exactly CLEAN and nothing else.\n\n\
         --- diff against {base} ---\n{diff}\n--- end ---"
    )
}

fn judge_prompt(base: &str, diff: &str, finding: &Finding) -> String {
    format!(
        "You are judging one code-review finding on a pull request. You did not write \
         the finding and will not fix it; you only decide whether it holds against the \
         diff below.\n\n\
         Finding:\n\
         severity: {}\n\
         location: {}:{}\n\
         what: {}\n\
         why: {}\n\n\
         Decide whether it holds. You may regrade its severity in either direction, \
         whether or not it holds. Output exactly one line of JSON and nothing else:\n\
         {{\"holds\": true|false, \"severity\": \"low\"|\"medium\"|\"high\", \"reason\": \"<one sentence>\"}}\n\n\
         --- diff against {base} ---\n{diff}\n--- end ---",
        severity_tag(finding.severity),
        finding.file,
        finding.line,
        finding.what,
        finding.why,
    )
}

/// The severity as qwen's own wire format spells it
pub(super) fn severity_tag(severity: Severity) -> &'static str {
    match severity {
        Severity::Low => "LOW",
        Severity::Medium => "MEDIUM",
        Severity::High => "HIGH",
    }
}

/// The judge's verdict, from a one-line JSON reply that may be wrapped in
/// prose or a code fence; `None` if no well-formed JSON object with the
/// expected shape can be found in it
pub(in crate::runner) fn parse_verdict(text: &str) -> Option<Verdict> {
    #[derive(serde::Deserialize)]
    struct Raw {
        holds: bool,
        severity: String,
        reason: String,
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    let raw: Raw = serde_json::from_str(&text[start..=end]).ok()?;
    let severity = match raw.severity.to_lowercase().as_str() {
        "low" => Severity::Low,
        "medium" => Severity::Medium,
        "high" => Severity::High,
        _ => return None,
    };
    Some(Verdict {
        holds: raw.holds,
        severity,
        reason: raw.reason,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::runner::step;
    use crate::test::{Rig, Scripted, ScriptedRound};

    #[test]
    fn a_judge_reply_wrapped_in_prose_or_fences_still_parses() {
        let plain = r#"{"holds": true, "severity": "medium", "reason": "it does hold"}"#;
        assert_eq!(
            parse_verdict(plain),
            Some(Verdict {
                holds: true,
                severity: Severity::Medium,
                reason: "it does hold".into(),
            })
        );
        let fenced = format!("```json\n{plain}\n```");
        assert_eq!(parse_verdict(&fenced), parse_verdict(plain));
        let cased = r#"{"holds": false, "severity": "HIGH", "reason": "no"}"#;
        assert_eq!(parse_verdict(cased).unwrap().severity, Severity::High);
    }

    #[test]
    fn an_unparseable_judge_reply_is_none() {
        assert_eq!(parse_verdict("I think it holds."), None);
        assert_eq!(parse_verdict(r#"{"holds": true}"#), None);
        assert_eq!(
            parse_verdict(r#"{"holds": true, "severity": "urgent", "reason": "x"}"#),
            None
        );
    }

    #[test]
    fn the_judge_gets_every_tool_denied() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);
        step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)

        rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
            severity: Severity::Low,
            file: "src/lib.rs".into(),
            line: 3,
            what: "unused variable".into(),
            why: "dead code".into(),
        }])]);
        step(&runner).unwrap(); // round 1's qwen call

        rig.claude.script([Scripted::Text(
            r#"{"holds": true, "severity": "low", "reason": "real, but minor"}"#,
        )]);
        step(&runner).unwrap(); // the judge's one-shot

        let all = rig.claude.all_seen();
        let judge = all
            .iter()
            .find(|s| s.call.role == Role::Judge)
            .expect("the judge ran");
        let denied: Vec<&str> = judge.settings["permissions"]["deny"]
            .as_array()
            .expect("a deny list")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(denied, NO_TOOLS, "every tool denied, nothing more or less");
    }

    #[test]
    fn the_claude_round_keeps_its_read_tools_to_check_its_own_work() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([
            Scripted::Push("work.txt", "work\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the worker's first turn
        step(&runner).unwrap(); // round 1, qwen: clean by default
        step(&runner).unwrap(); // round 2, claude: scripted clean above

        let all = rig.claude.all_seen();
        let reviewer = all
            .iter()
            .find(|s| s.call.role == Role::Reviewer)
            .expect("a reviewer round ran");
        assert_eq!(
            reviewer.settings,
            json!({}),
            "no tool is denied, unlike the judge's"
        );
    }
}
