//! Building the Claude calls the review makes itself: a fresh review round
//!
//! It is not the worker's: it draws a fresh session id and runs under its
//! own throwaway settings, never the worker's own. It keeps its read tools,
//! to check its own work against the worktree.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::ports::{AgentCall, Reach, Role, Session, Severity, Tools};
use crate::settings::{Limit, RoleModel};
use crate::skills::{Skills, Step};
use crate::work_item::new_session_id;

/// Where the Claude round's throwaway settings go
const REVIEW_SETTINGS_FILE: &str = "review-settings.json";

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
    (model, limit): (&RoleModel, &Limit),
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
    let prompt = skills.invoke(Step::Review, &prompt);
    let mut call = build_call(Role::Reviewer, issue, worktree, (model, limit), prompt)?;
    call.settings = worker_folder.join(REVIEW_SETTINGS_FILE);
    // A review skill may spawn sub-agents or run commands; the round does neither.
    call.tools = Tools::Review;
    call.plugin_dirs = skills.plugin_dirs().to_vec();
    Ok(call)
}

// The shape every call the review makes itself shares: a fresh
// session, the worktree as its folder, no instructions file, no tools, no
// fence and no plugins unless the caller adds them, and the role and prompt.
pub(in crate::runner) fn build_call(
    role: Role,
    issue: u64,
    worktree: &Path,
    (model, limit): (&RoleModel, &Limit),
    prompt: String,
) -> Result<AgentCall, String> {
    let session = new_session_id().map_err(|e| format!("cannot draw a session id: {e}"))?;
    Ok(AgentCall {
        role,
        harness: model.harness.clone(),
        issue,
        model: model.model.as_str().to_owned(),
        effort: model.effort,
        session: Session::New(session),
        cwd: worktree.to_owned(),
        settings: PathBuf::new(),
        instructions: None,
        prompt,
        timeout: None,
        plugin_dirs: Vec::new(),
        tools: Tools::Answer,
        reach: Reach::default(),
        lease: limit.lease().cloned(),
    })
}

pub(in crate::runner) fn diff_against(worktree: &Path, base: &str) -> Result<String, String> {
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

/// The severity as qwen's own wire format spells it
pub(super) fn severity_tag(severity: Severity) -> &'static str {
    match severity {
        Severity::Low => "LOW",
        Severity::Medium => "MEDIUM",
        Severity::High => "HIGH",
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::runner::step;
    use crate::test::{Rig, Scripted};
    use crate::trim::deny_with_trim;

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
            reviewer.settings["permissions"]["deny"],
            deny_with_trim(&["Agent", "Task", "Bash"]),
            "Read, Grep and Glob stay"
        );
        assert!(!reviewer.settings.to_string().contains("\"Read\""));
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
            harness: crate::settings::AgentHarness::ClaudeCode,
        };
        let worker = home.path().join("worker");
        let round = Round {
            issue: 71,
            worktree: &repo,
            base: &base,
            worker_folder: &worker,
            criteria: "",
        };
        let call = reviewer_call(round, (&model, &Limit::default()), &skills).unwrap();
        let cli = ClaudeCli::default();
        cli.prepare(&call).expect("the settings were written");
        let reply = cli.run(&call).expect("the round ran");
        println!("--- reply ---\n{}\n--- end ---", reply.text);
        let findings = read_review(&reply.text).expect("the reply reads as a review");
        println!("{} finding(s): {findings:#?}", findings.len());
    }
}
