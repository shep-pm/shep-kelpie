//! Building a reviewer's session call
//!
//! It is not the worker's: it draws a fresh session id and runs under its
//! own throwaway settings, never the worker's own. It keeps its read tools,
//! to check its own work against the worktree, and runs no command.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::ports::{AgentCall, Reach, Role, Session, Severity, Tools};
use crate::settings::{Limit, RoleModel};
use crate::work_item::new_session_id;

/// A fresh session of a reviewer on `model` over issue `issue`'s
/// `worktree`, asked `prompt`, its settings in `worker_folder`
pub(super) fn reviewer_call(
    issue: u64,
    worktree: &Path,
    worker_folder: &Path,
    (model, limit): (&RoleModel, &Limit),
    prompt: String,
) -> Result<AgentCall, String> {
    let mut call = build_call(Role::Reviewer, issue, worktree, (model, limit), prompt)?;
    // Named for the work item, whose reviewer runs beside other items' calls.
    call.settings = worker_folder.join(format!("review-settings-{issue}.json"));
    call.tools = Tools::Review;
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
        plugin_dirs: Vec::new(),
        tools: Tools::Answer,
        reach: Reach::default(),
        lease: limit.lease().cloned(),
    })
}

pub(in crate::runner) fn diff_against(
    repo: &Path,
    worktree: &Path,
    base: &str,
) -> Result<String, String> {
    // Neither a program the global config names nor one the worktree's
    // attributes pick turns the diff a reviewer reads into something else.
    let output = crate::worktree::trusted_command(repo, worktree)
        .map_err(|e| e.to_string())?
        .args(["diff", "--no-ext-diff", "--no-textconv", base])
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
    fn a_reviewers_session_keeps_its_read_tools_to_check_its_own_work() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
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
}
