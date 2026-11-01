use super::*;

fn tools(agent: &Agent) -> Option<&str> {
    model(agent).tools.as_ref().map(NonBlank::as_str)
}

const WORKER: &str = "---\nrole: implementer\nharness: claude-code\nmodel: claude-sonnet-5-5\n\
                      effort: high\ntools: Bash,Read,Edit,Write,Glob,Grep,Skill,WebFetch\n---\n";

#[test]
fn an_implementer_on_claude_code_names_its_own_tools() {
    let dir = folder(&[("fetcher.md", WORKER)]);
    let agents = Agents::load(dir.path()).unwrap();
    let fetcher = agents.get(&name("fetcher")).unwrap();
    assert_eq!(
        tools(fetcher),
        Some("Bash,Read,Edit,Write,Glob,Grep,Skill,WebFetch")
    );
}

#[test]
fn a_reviewer_on_claude_code_names_its_own_tools() {
    let reviewer = "---\nrole: reviewer\nharness: claude-code\nmodel: claude-sonnet-5-5\n\
                    effort: high\ntools: Read,Glob,Grep,Skill\n---\n\nReview it.\n";
    let dir = folder(&[("skilled.md", reviewer)]);
    let agents = Agents::load(dir.path()).unwrap();
    let skilled = agents.get(&name("skilled")).unwrap();
    assert_eq!(tools(skilled), Some("Read,Glob,Grep,Skill"));
}

#[test]
fn an_agent_that_names_no_tools_starts_with_its_roles() {
    let agents = Agents::embedded().bind("defect-hunter", super::OPUS_HIGH);
    for default in ["defect-hunter"] {
        let agent = agents.get(&name(default)).unwrap();
        assert_eq!(tools(agent), None, "{default}");
    }
}

#[test]
fn another_harness_takes_no_tools() {
    for harness in ["codex", "pi\nurl: http://box:11434/v1\ncontext: 65536"] {
        let text = format!(
            "---\nrole: implementer\nharness: {harness}\nmodel: m\neffort: low\ntools: Bash\n---\n"
        );
        let err = refused(&[("other.md", &text)]);
        assert!(
            err.contains("takes no `tools`") && err.contains("claude-code"),
            "{err}"
        );
    }
}

#[test]
fn a_blank_tools_list_is_refused() {
    let text = WORKER.replace("Bash,Read,Edit,Write,Glob,Grep,Skill,WebFetch", "\"\"");
    let err = refused(&[("blank.md", &text)]);
    assert!(err.contains("tools"), "{err}");
}

#[test]
fn the_project_manager_takes_no_tools() {
    let text =
        "---\nrole: pm\nharness: claude-code\nmodel: m\neffort: low\ntools: Bash\n---\n\nPrompt.\n";
    let err = refused(&[("kept.md", text)]);
    assert!(err.contains("tools"), "{err}");
}
