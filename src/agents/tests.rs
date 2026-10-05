use super::*;
use crate::settings::AgentName;

const QWEN: &str = "---\nrole: implementer\nharness: pi\nmodel: qwen3.8:27b\neffort: low\n\
                    url: http://box:11434/v1\ncontext: 65536\n---\n";

fn name(name: &str) -> AgentName {
    AgentName::try_from(name.to_owned()).unwrap()
}

fn pair(agent: &Agent) -> (&str, Effort) {
    (agent.model.model.as_str(), agent.model.effort)
}

// A fresh `agents` folder holding `files`, as (name, text).
fn folder(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (file, text) in files {
        fs::write(dir.path().join(file), text).unwrap();
    }
    dir
}

fn refused(files: &[(&str, &str)]) -> String {
    let dir = folder(files);
    Agents::load(dir.path()).unwrap_err().to_string()
}

#[test]
fn kelpies_own_agents_are_sonnet_and_opus_at_high_on_claude_code() {
    let agents = Agents::embedded();
    let sonnet = agents.get(&name("sonnet-high")).unwrap();
    assert_eq!(pair(sonnet), ("claude-sonnet-5-5", Effort::High));
    let opus = agents.get(&name("opus-high")).unwrap();
    assert_eq!(pair(opus), ("claude-opus-5-5", Effort::High));
    for agent in [sonnet, opus] {
        assert_eq!(agent.role, Role::Implementer);
        assert_eq!(agent.model.harness, AgentHarness::ClaudeCode);
        assert_eq!(agent.limit, Limit::Account(Account::Claude));
        assert_eq!(agent.prompt, None, "the comments are not a prompt");
    }
    assert_eq!(
        DEFAULTS.len(),
        2,
        "a default that failed to parse would be dropped"
    );
}

#[test]
fn a_folder_that_is_not_there_holds_kelpies_own_agents() {
    let dir = tempfile::tempdir().unwrap();
    let agents = Agents::load(&dir.path().join("agents")).unwrap();
    assert_eq!(agents, Agents::embedded());
}

#[test]
fn a_file_adds_an_agent_and_one_named_for_a_default_replaces_it() {
    let sonnet6 = "---\nrole: implementer\nharness: claude-code\nmodel: claude-sonnet-6\n\
                   effort: medium\n---\n\nKeep each commit small.\n\n";
    let dir = folder(&[
        ("qwen.md", QWEN),
        ("sonnet-high.md", sonnet6),
        ("notes.txt", "not an agent"),
    ]);
    let agents = Agents::load(dir.path()).unwrap();
    let sonnet = agents.get(&name("sonnet-high")).unwrap();
    assert_eq!(pair(sonnet), ("claude-sonnet-6", Effort::Medium));
    assert_eq!(sonnet.prompt.as_deref(), Some("Keep each commit small."));
    let qwen = agents.get(&name("qwen")).unwrap();
    let AgentHarness::Pi(server) = &qwen.model.harness else {
        panic!("{qwen:?}");
    };
    assert_eq!(server.url.as_str(), "http://box:11434/v1");
    assert_eq!(server.context.get(), 65536);
    assert_eq!(qwen.limit, Limit::Lease(LeaseName::gpu()));
    assert!(agents.get(&name("opus-high")).is_some());
    assert!(agents.get(&name("notes")).is_none());
}

#[test]
fn a_codex_agent_spends_the_codex_account() {
    let gpt = "---\nrole: implementer\nharness: codex\nmodel: gpt-6-sol\neffort: medium\n---\n";
    let dir = folder(&[("gpt.md", gpt)]);
    let agents = Agents::load(dir.path()).unwrap();
    let gpt = agents.get(&name("gpt")).unwrap();
    assert_eq!(gpt.model.harness, AgentHarness::Codex);
    assert_eq!(gpt.limit, Limit::Account(Account::Codex));
}

#[test]
fn a_file_that_does_not_parse_is_refused_naming_it() {
    let dir = folder(&[("broken.md", "role: implementer\n")]);
    let err = Agents::load(dir.path()).unwrap_err().to_string();
    let path = dir.path().join("broken.md");
    assert_eq!(
        err,
        format!(
            "agent file {}: must start with a `---` line, then the YAML frontmatter, \
             then a `---` line of its own",
            path.display()
        )
    );
    let unclosed = refused(&[("open.md", "---\nrole: implementer\n")]);
    assert!(
        unclosed.contains("must start with a `---` line"),
        "{unclosed}"
    );
}

#[test]
fn an_unknown_harness_is_refused_naming_the_file_and_the_key() {
    let gemini = QWEN.replace("harness: pi", "harness: gemini");
    let dir = folder(&[("qwen.md", &gemini)]);
    let err = Agents::load(dir.path()).unwrap_err().to_string();
    let path = dir.path().join("qwen.md");
    assert_eq!(
        err,
        format!(
            "agent file {}: `harness`: unknown variant `gemini`, expected one of \
             claude-code, pi, codex, stand-in at line 3, column 10",
            path.display()
        ),
        "line 3 of the file is `harness: gemini`"
    );
}

#[test]
fn a_missing_or_unknown_key_is_refused_naming_it() {
    let missing = QWEN.replace("effort: low\n", "");
    let err = refused(&[("qwen.md", &missing)]);
    assert!(err.contains("qwen.md: missing field `effort`"), "{err}");
    let extra = QWEN.replace("role: implementer\n", "role: implementer\nsandbox: none\n");
    let err = refused(&[("qwen.md", &extra)]);
    assert!(
        err.contains("qwen.md: unknown field `sandbox`") && err.ends_with("at line 3, column 1"),
        "{err}"
    );
    let judge = QWEN.replace("role: implementer", "role: judge");
    let err = refused(&[("qwen.md", &judge)]);
    assert!(
        err.contains("qwen.md: `role`: unknown variant `judge`"),
        "{err}"
    );
    let small = QWEN.replace("context: 65536", "context: 12");
    let err = refused(&[("qwen.md", &small)]);
    assert!(
        err.contains("qwen.md: `context`: must be at least 4096 tokens"),
        "{err}"
    );
}

#[test]
fn a_file_whose_name_is_no_agents_name_is_skipped_and_named() {
    let dir = folder(&[
        ("README.md", "# What these are\n"),
        ("Notes.md", QWEN),
        ("qwen.md", QWEN),
    ]);
    let agents = Agents::load(dir.path()).unwrap();
    assert!(agents.get(&name("qwen")).is_some());
    let skipped: Vec<String> = agents.skipped().collect();
    let line = |file: &str| {
        format!(
            "agent file {} is skipped: an agent's name is lowercase letters, digits and `-`",
            dir.path().join(file).display()
        )
    };
    assert_eq!(skipped, [line("Notes.md"), line("README.md")]);
    assert_eq!(Agents::embedded().skipped().count(), 0);
}

#[test]
fn a_change_to_any_md_file_changes_the_snapshot() {
    let dir = folder(&[("qwen.md", QWEN)]);
    let before = snapshot(dir.path());
    assert_eq!(before, snapshot(dir.path()));
    fs::write(dir.path().join("qwen.md"), QWEN.replace("low", "high")).unwrap();
    let edited = snapshot(dir.path());
    assert_ne!(before, edited);
    fs::write(dir.path().join("notes.txt"), "not an agent").unwrap();
    assert_eq!(edited, snapshot(dir.path()));
    fs::write(dir.path().join("mine.md"), QWEN).unwrap();
    assert_ne!(edited, snapshot(dir.path()));
    assert_eq!(snapshot(&dir.path().join("none")), []);
}

#[test]
fn a_kept_agent_is_written_on_claude_code_once_and_only_when_usable() {
    let dir = tempfile::tempdir().unwrap();
    let opus = name("opus-medium");
    assert!(write_kept(dir.path(), &opus, "claude-opus-5-5", Effort::Medium).unwrap());
    let agents = Agents::load(dir.path()).unwrap();
    let kept = agents.get(&opus).unwrap();
    assert_eq!(pair(kept), ("claude-opus-5-5", Effort::Medium));
    assert_eq!(kept.model.harness, AgentHarness::ClaudeCode);
    assert_eq!(kept.prompt, None);
    let written = fs::read_to_string(dir.path().join("opus-medium.md")).unwrap();
    assert!(!write_kept(dir.path(), &opus, "claude-haiku", Effort::Low).unwrap());
    let kept = fs::read_to_string(dir.path().join("opus-medium.md")).unwrap();
    assert_eq!(kept, written, "never over a file");
    assert!(!write_kept(dir.path(), &name("odd"), "a\"b", Effort::Low).unwrap());
    assert!(!dir.path().join("odd.md").exists());
}

#[test]
fn a_harness_missing_a_key_it_needs_is_refused_naming_the_key() {
    let cases = [
        (
            QWEN.replace("url: http://box:11434/v1\n", ""),
            "runs on pi, which needs the model's server as `url` and its context size \
             as `context`",
        ),
        (
            QWEN.replace("harness: pi", "harness: claude-code"),
            "runs on claude-code, which takes no `url` or `context`",
        ),
        (
            QWEN.replace("http://box", "https://box"),
            "runs on pi, with a `url` kelpie cannot forward to: kelpie's forwarder \
             reaches a model server over plain `http://`",
        ),
        (
            QWEN.replace("role: implementer\n", "role: implementer\nusage: claude\n"),
            "runs on pi, whose usage is read with `none`, so it cannot set `usage: claude`",
        ),
        (
            "---\nrole: implementer\nharness: codex\nmodel: gpt\neffort: low\nlease: gpu\n---\n"
                .to_owned(),
            "sets `lease`, which only an agent with `usage: none` takes",
        ),
        (
            "---\nrole: implementer\nharness: claude-code\nmodel: m\neffort: low\n\
             usage: codex\n---\n"
                .to_owned(),
            "runs on claude-code, whose usage is read with `claude`, so it cannot set \
             `usage: codex`: leave `usage` out",
        ),
    ];
    for (text, why) in cases {
        let err = refused(&[("x.md", &text)]);
        assert!(err.contains("x.md: "), "{err}");
        assert!(err.contains(why), "{err}\nwanted: {why}");
    }
}

#[test]
fn defaults_are_written_where_missing_and_never_over_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join("agents");
    assert_eq!(
        write_defaults(&agents).unwrap(),
        ["sonnet-high", "opus-high"]
    );
    assert_eq!(Agents::load(&agents).unwrap(), Agents::embedded());
    let mine = QWEN.replace("model: qwen3.8:27b", "model: mine");
    fs::write(agents.join("opus-high.md"), &mine).unwrap();
    fs::remove_file(agents.join("sonnet-high.md")).unwrap();
    assert_eq!(write_defaults(&agents).unwrap(), ["sonnet-high"]);
    assert_eq!(
        fs::read_to_string(agents.join("opus-high.md")).unwrap(),
        mine
    );
    assert_eq!(write_defaults(&agents).unwrap(), Vec::<&str>::new());
}
