//! Building the Claude calls the qwen-review loop makes itself: a fresh
//! review round, and the judge's one-shot on a single finding
//!
//! Neither is the worker's: both draw a fresh session id and run under
//! their own throwaway settings, never the worker's own. The Claude round
//! keeps its read tools, to check its own work against the worktree; the
//! judge has no tools, so its answer is exactly the JSON it was asked for
//! and nothing it read on the side.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use crate::ports::{AgentCall, Finding, Reach, Role, Session, Severity, Tools, Verdict};
use crate::settings::RoleModel;
use crate::shots::ShotsRun;
use crate::skills::{Skills, Step};
use crate::work_item::new_session_id;

/// Where the Claude round's throwaway settings go
const REVIEW_SETTINGS_FILE: &str = "review-settings.json";

/// Where the judge's throwaway settings go
const JUDGE_SETTINGS_FILE: &str = "judge-settings.json";

/// Kelpie's shots for a Claude round, and the folder they sit under
#[derive(Debug, Clone, Copy)]
pub(super) struct Screens<'a> {
    pub(super) dir: &'a Path,
    pub(super) run: &'a ShotsRun,
}

/// What a Claude round reviews, and against what
#[derive(Debug, Clone, Copy)]
pub(super) struct Round<'a> {
    pub(super) issue: u64,
    pub(super) worktree: &'a Path,
    pub(super) base: &'a str,
    pub(super) worker_folder: &'a Path,
    /// What the issue asks for, which the round checks the diff against
    pub(super) criteria: &'a str,
}

pub(super) fn reviewer_call(
    round: Round<'_>,
    model: &RoleModel,
    shots: Option<Screens<'_>>,
    skills: &Skills,
) -> Result<AgentCall, String> {
    let Round {
        issue,
        worktree,
        base,
        worker_folder,
        criteria,
    } = round;
    let diff = diff_against(worktree, base)?;
    let mut prompt = reviewer_prompt(base, &diff);
    if !criteria.trim().is_empty() {
        prompt.push_str(&format!(
            "\n\nThe issue this pull request resolves asks for the following. \
             Report anything it asks for that the diff leaves undone or gets wrong, \
             in the same format.\n\n{criteria}"
        ));
    }
    if let Some(shots) = shots {
        prompt.push_str(&shots_prompt(shots.run));
    }
    let prompt = skills.invoke(Step::Review, &prompt);
    let mut call = build_call(Role::Reviewer, issue, worktree, model, prompt)?;
    call.settings = worker_folder.join(REVIEW_SETTINGS_FILE);
    // A review skill may spawn sub-agents or run commands; the round does neither.
    call.tools = Tools::Review;
    call.reach.read = shots.map(|s| s.dir.to_owned()).into_iter().collect();
    call.plugin_dirs = skills.plugin_dirs().to_vec();
    Ok(call)
}

// A finding that names a PNG under `shots` is about a screenshot, and its
// judge may open that folder with Read and nothing else.
pub(in crate::runner) fn judge_call(
    issue: u64,
    worktree: &Path,
    base: &str,
    worker_folder: &Path,
    model: &RoleModel,
    finding: &Finding,
    shots: Option<&Path>,
) -> Result<AgentCall, String> {
    let diff = diff_against(worktree, base)?;
    let shot = shots.filter(|dir| is_shot(&finding.file, dir));
    let mut prompt = judge_prompt(base, &diff, finding);
    if shot.is_some() {
        prompt.push_str(&format!(
            "\n\nThe finding is about the screenshot {}. Open it with Read before you decide.",
            finding.file
        ));
    }
    let mut call = build_call(Role::Judge, issue, worktree, model, prompt)?;
    call.settings = worker_folder.join(JUDGE_SETTINGS_FILE);
    call.reach.read = shot.map(Path::to_owned).into_iter().collect();
    Ok(call)
}

// Whether `file` is a PNG really inside `dir`, kelpie's shots folder. A
// reviewer writes `file`, so `..` and symlinks must not reach past `dir`.
fn is_shot(file: &str, dir: &Path) -> bool {
    let path = Path::new(file);
    if path.extension().is_none_or(|e| e != "png")
        || path.components().any(|c| c == Component::ParentDir)
    {
        return false;
    }
    match (path.canonicalize(), dir.canonicalize()) {
        (Ok(file), Ok(dir)) => file.starts_with(dir) && file.is_file(),
        _ => false,
    }
}

// The shape every call the review loop makes itself shares: a fresh
// session, the worktree as its folder, no instructions file, no tools, no
// fence and no plugins unless the caller adds them, and the role and prompt.
fn build_call(
    role: Role,
    issue: u64,
    worktree: &Path,
    model: &RoleModel,
    prompt: String,
) -> Result<AgentCall, String> {
    let session = new_session_id().map_err(|e| format!("cannot draw a session id: {e}"))?;
    Ok(AgentCall {
        role,
        issue,
        model: model.model.as_str().to_owned(),
        effort: model.effort,
        session: Session::New(session),
        cwd: worktree.to_owned(),
        settings: PathBuf::new(),
        instructions: None,
        prompt,
        timeout: None,
        mcp_config: None,
        plugin_dirs: Vec::new(),
        tools: Tools::Answer,
        reach: Reach::default(),
    })
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

fn diff_against(worktree: &Path, base: &str) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["diff", base])
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
    use crate::adapters::NO_TOOLS;
    use crate::runner::step;
    use crate::test::{Rig, Scripted, ScriptedRound};

    #[test]
    fn only_a_png_really_inside_the_shots_folder_is_a_shot() {
        let home = tempfile::tempdir().unwrap();
        let shots = home.path().join("shots/lab/7");
        std::fs::create_dir_all(shots.join("abc1234")).unwrap();
        let png = shots.join("abc1234/root-mobile-dark.png");
        std::fs::write(&png, "png").unwrap();
        let secret = home.path().join("settings.toml");
        std::fs::write(&secret, "url").unwrap();
        std::os::unix::fs::symlink(&secret, shots.join("abc1234/leak.png")).unwrap();
        std::fs::write(home.path().join("outside.png"), "png").unwrap();

        let cited = |file: &Path| is_shot(&file.display().to_string(), &shots);
        assert!(cited(&png));
        assert!(!cited(&shots.join("../../../settings.toml")));
        assert!(!cited(&shots.join("../../../outside.png")));
        assert!(!cited(&shots.join("abc1234/leak.png")), "a symlink out");
        assert!(!cited(&shots.join("abc1234/missing.png")));
        assert!(!cited(Path::new("src/app.tsx")));
    }

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
            json!({ "permissions": { "deny": ["Agent", "Task", "Bash"] } }),
            "Read, Grep and Glob stay, unlike the judge's"
        );
    }

    // The review skill answers under its own headings and spawns sub-agents
    // unless kelpie's headless rules and denies hold it to kelpie's format.
    #[test]
    #[ignore = "runs one real Claude round through the vendored code-review skill, about 1 min"]
    fn a_live_claude_round_through_the_code_review_skill_reads_as_findings() {
        use crate::adapters::ClaudeCli;
        use crate::ports::{Agents, read_review};
        use crate::settings::{Effort, StepSkills};
        use crate::test::git;

        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(
            repo.join("lib.rs"),
            "pub fn half(n: u32) -> u32 {\n    n / 2\n}\n",
        )
        .unwrap();
        git(&repo, &["add", "."]);
        git(
            &repo,
            &[
                "-c",
                "user.name=k",
                "-c",
                "user.email=k@k",
                "commit",
                "-qm",
                "half",
            ],
        );
        let base = git(&repo, &["rev-parse", "HEAD"]);
        let changed = "/// Returns the average of `a` and `b`\npub fn average(a: u32, b: u32) -> u32 {\n    \
                       (a + b) / 2\n}\n\npub fn half(n: u32) -> u32 {\n    n / 2\n}\n";
        std::fs::write(repo.join("lib.rs"), changed).unwrap();

        let skills = Skills::load(&StepSkills::default(), &home.path().join("skills"));
        let model = RoleModel {
            model: "claude-sonnet-5".to_owned().try_into().unwrap(),
            effort: Effort::Medium,
        };
        let worker = home.path().join("worker");
        let round = Round {
            issue: 71,
            worktree: &repo,
            base: &base,
            worker_folder: &worker,
            criteria: "",
        };
        let call = reviewer_call(round, &model, None, &skills).unwrap();
        let cli = ClaudeCli::default();
        cli.prepare(&call).expect("the settings were written");
        let reply = cli.run(&call).expect("the round ran");
        println!("--- reply ---\n{}\n--- end ---", reply.text);
        let findings = read_review(&reply.text).expect("the reply reads as a review");
        println!("{} finding(s): {findings:#?}", findings.len());
    }
}
