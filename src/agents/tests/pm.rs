//! The project manager's agent file: a Claude Code session with a prompt

use super::{folder, name, pair, refused};
use crate::agents::{Agents, Role};
use crate::settings::{AgentHarness, Effort};

#[test]
fn kelpies_own_project_manager_is_opus_at_medium_with_a_prompt() {
    let agents = Agents::embedded();
    let pm = agents.get(&name("pm")).unwrap();
    assert_eq!(pm.role, Role::Pm);
    assert_eq!(pair(pm), ("claude-opus-5-5", Effort::Medium));
    assert_eq!(
        pm.runs.session().unwrap().0.harness,
        AgentHarness::ClaudeCode
    );
    let prompt = pm.prompt.as_deref().unwrap();
    assert!(prompt.contains("`pm-notes.md`"), "{prompt}");
}

#[test]
fn a_project_manager_off_claude_code_or_without_a_prompt_is_refused() {
    let codex = "---\nrole: pm\nharness: codex\nmodel: gpt-5.5\neffort: high\n---\nDecide.\n";
    let err = refused(&[("mine.md", codex)]);
    assert!(
        err.contains("the project manager runs on claude-code, not codex"),
        "{err}"
    );
    let bare = "---\nrole: pm\nharness: claude-code\nmodel: claude-opus-5-5\neffort: low\n---\n";
    let err = refused(&[("mine.md", bare)]);
    assert!(
        err.contains("the project manager's prompt is the file's body"),
        "{err}"
    );
    let keyed =
        "---\nrole: pm\nharness: claude-code\nmodel: m\neffort: low\npaths: [\"x\"]\n---\nx\n";
    assert!(refused(&[("mine.md", keyed)]).contains("paths"));
}

#[test]
fn a_file_of_its_own_replaces_the_project_manager() {
    let mine = "---\nrole: pm\nharness: claude-code\nmodel: claude-sonnet-5-5\neffort: high\n---\n\
                Pick the oldest.\n";
    let dir = folder(&[("pm.md", mine)]);
    let agents = Agents::load(dir.path()).unwrap();
    let pm = agents.get(&name("pm")).unwrap();
    assert_eq!(pair(pm), ("claude-sonnet-5-5", Effort::High));
    assert_eq!(pm.prompt.as_deref(), Some("Pick the oldest."));
}
