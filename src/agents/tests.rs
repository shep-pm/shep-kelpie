use super::*;
use crate::settings::{Account, AgentHarness, AgentName, LeaseName};

mod bots;

const QWEN: &str = "---\nrole: implementer\nharness: pi\nmodel: qwen3.8:27b\neffort: low\n\
                    url: http://box:11434/v1\ncontext: 65536\n---\n";

fn name(name: &str) -> AgentName {
    AgentName::try_from(name.to_owned()).unwrap()
}

fn model(agent: &Agent) -> &RoleModel {
    agent.runs.session().expect("it runs sessions").0
}

fn limit(agent: &Agent) -> &Limit {
    agent.runs.session().expect("it runs sessions").1
}

fn pair(agent: &Agent) -> (&str, Effort) {
    (model(agent).model.as_str(), model(agent).effort)
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
        assert_eq!(model(agent).harness, AgentHarness::ClaudeCode);
        assert_eq!(*limit(agent), Limit::Account(Account::Claude));
        assert_eq!(agent.prompt, None, "the comments are not a prompt");
    }
    assert_eq!(
        Agents::embedded().agents.len(),
        DEFAULTS.len(),
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
    let AgentHarness::Pi(server) = &model(qwen).harness else {
        panic!("{qwen:?}");
    };
    assert_eq!(server.url.as_str(), "http://box:11434/v1");
    assert_eq!(server.context.get(), 65536);
    assert_eq!(*limit(qwen), Limit::Lease(LeaseName::gpu()));
    assert!(agents.get(&name("opus-high")).is_some());
    assert!(agents.get(&name("notes")).is_none());
}

#[test]
fn a_codex_agent_spends_the_codex_account() {
    let gpt = "---\nrole: implementer\nharness: codex\nmodel: gpt-6-sol\neffort: medium\n---\n";
    let dir = folder(&[("gpt.md", gpt)]);
    let agents = Agents::load(dir.path()).unwrap();
    let gpt = agents.get(&name("gpt")).unwrap();
    assert_eq!(model(gpt).harness, AgentHarness::Codex);
    assert_eq!(*limit(gpt), Limit::Account(Account::Codex));
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
    let huge = QWEN.replace("effort: low", "effort: huge");
    let err = refused(&[("qwen.md", &huge)]);
    assert!(
        err.contains("qwen.md: `effort`: unknown variant `huge`"),
        "{err}"
    );
    let blank = QWEN.replace("model: qwen3.8:27b", "model: \" \"");
    let err = refused(&[("qwen.md", &blank)]);
    assert!(err.contains("qwen.md: `model`: must not be blank"), "{err}");
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
    assert_eq!(model(kept).harness, AgentHarness::ClaudeCode);
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
    let home = dir.path().join("home");
    assert_eq!(
        write_defaults(&agents, &home).unwrap(),
        [
            "sonnet-high",
            "opus-high",
            "defect-hunter",
            "coderabbit",
            "cubic",
            "codex"
        ]
    );
    assert_eq!(Agents::load(&agents).unwrap(), Agents::embedded());
    assert!(!agents.join("qwen.md").exists(), "no qwen-review script");
    let mine = QWEN.replace("model: qwen3.8:27b", "model: mine");
    fs::write(agents.join("opus-high.md"), &mine).unwrap();
    fs::remove_file(agents.join("sonnet-high.md")).unwrap();
    assert_eq!(write_defaults(&agents, &home).unwrap(), ["sonnet-high"]);
    assert_eq!(
        fs::read_to_string(agents.join("opus-high.md")).unwrap(),
        mine
    );
    assert_eq!(write_defaults(&agents, &home).unwrap(), Vec::<&str>::new());
    let script = home.join(".claude/scripts/qwen-review.sh");
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    fs::write(&script, "#!/bin/sh\n").unwrap();
    assert_eq!(write_defaults(&agents, &home).unwrap(), ["qwen"]);
}

const REVIEWER: &str = "---\nrole: reviewer\nharness: claude-code\nmodel: claude-opus-5-5\n\
                        effort: high\npaths: [\"src/**\"]\n---\nRead {{DIFF}} for defects.\n";

const COMMAND: &str = "---\nrole: reviewer\nharness: command\ncommand: ~/bin/review\n---\n";

#[test]
fn kelpies_own_reviewers_are_defect_hunter_and_qwen() {
    let agents = Agents::embedded();
    let hunter = agents.get(&name("defect-hunter")).unwrap();
    assert_eq!(hunter.role, Role::Reviewer);
    assert_eq!(pair(hunter), ("claude-opus-5-5", Effort::High));
    assert_eq!(model(hunter).harness, AgentHarness::ClaudeCode);
    assert!(hunter.second_look);
    assert!(hunter.paths.is_empty());
    let prompt = hunter.prompt.as_deref().unwrap();
    assert!(prompt.starts_with("You are reviewing a pull request for defects"));
    assert!(prompt.ends_with("--- diff against {{BASE}} ---\n{{DIFF}}\n--- end ---"));

    let qwen = agents.get(&name("qwen")).unwrap();
    assert_eq!(qwen.role, Role::Reviewer);
    let Runs::Command(command) = &qwen.runs else {
        panic!("{qwen:?}");
    };
    assert_eq!(
        command.command,
        PathBuf::from("~/.claude/scripts/qwen-review.sh")
    );
    assert_eq!(command.lease, None, "the script takes the GPU lock itself");
    assert_eq!(qwen.prompt, None);
}

#[test]
fn a_reviewer_runs_a_session_a_command_or_an_endpoint() {
    let dir = folder(&[
        ("mine.md", REVIEWER),
        ("script.md", COMMAND),
        (
            "box.md",
            "---\nrole: reviewer\nharness: endpoint\nurl: http://box:11434/v1\n\
             model: coder\ncontext: 32768\nlease: gpu-box\n---\n",
        ),
    ]);
    let agents = Agents::load(dir.path()).unwrap();
    let mine = agents.get(&name("mine")).unwrap();
    assert_eq!(pair(mine), ("claude-opus-5-5", Effort::High));
    assert_eq!(mine.paths[0].as_str(), "src/**");
    assert!(!mine.second_look);
    assert_eq!(mine.prompt.as_deref(), Some("Read {{DIFF}} for defects."));
    let Runs::Endpoint(endpoint) = &agents.get(&name("box")).unwrap().runs else {
        panic!("not an endpoint");
    };
    assert_eq!(endpoint.url.as_str(), "http://box:11434/v1");
    assert_eq!(endpoint.context.get(), 32768);
    assert_eq!(endpoint.lease.as_ref().unwrap().as_str(), "gpu-box");
    let script = agents.get(&name("script")).unwrap();
    assert!(matches!(&script.runs, Runs::Command(c) if c.lease.is_none()));
}

#[test]
fn a_key_of_the_other_role_is_refused_by_name() {
    let paths = QWEN.replace("effort: low\n", "effort: low\npaths: [\"src/**\"]\n");
    let err = refused(&[("qwen.md", &paths)]);
    assert!(err.contains("qwen.md: unknown field `paths`"), "{err}");
    let look = QWEN.replace("effort: low\n", "effort: low\nsecond_look: true\n");
    let err = refused(&[("qwen.md", &look)]);
    assert!(
        err.contains("qwen.md: unknown field `second_look`"),
        "{err}"
    );
    let command = "---\nrole: implementer\nharness: command\ncommand: /bin/x\n---\n";
    let err = refused(&[("x.md", command)]);
    assert!(
        err.contains("x.md: `harness`: unknown variant `command`"),
        "{err}"
    );
}

#[test]
fn a_reviewer_whose_keys_its_harness_cannot_use_is_refused_naming_why() {
    let cases = [
        (
            REVIEWER.replace("Read {{DIFF}} for defects.\n", ""),
            "runs a session on claude-code, whose prompt is the file's body",
        ),
        (
            REVIEWER.replace("effort: high\n", ""),
            "runs a session on claude-code, which needs `model` and `effort`",
        ),
        (
            format!("{COMMAND}Review it.\n"),
            "runs on command, which writes its own prompt: leave the body empty",
        ),
        (
            COMMAND.replace(
                "command: ~/bin/review\n",
                "command: ~/bin/review\nsecond_look: true\n",
            ),
            "runs on command, which cannot be shown its own findings",
        ),
        (
            COMMAND.replace(
                "command: ~/bin/review\n",
                "command: ~/bin/review\nmodel: m\n",
            ),
            "runs a command, which takes no `model`",
        ),
        (
            COMMAND.replace("~/bin/review", "bin/review"),
            "`command` must start with `/` or `~/`",
        ),
        (
            COMMAND.replace(
                "command: ~/bin/review\n",
                "command: ~/bin/review\nollama: http://h:1\n",
            ),
            "`ollama` needs a lease",
        ),
        (
            COMMAND.replace(
                "command: ~/bin/review\n",
                "command: ~/bin/review\nollama_model: m\n",
            ),
            "`ollama_model` needs `ollama`",
        ),
        (
            "---\nrole: reviewer\nharness: endpoint\nurl: http://box/v1\nmodel: m\n---\n"
                .to_owned(),
            "runs on an endpoint, which needs the server as `url`, its `model` and the \
             model's context size as `context`",
        ),
    ];
    for (text, why) in cases {
        let err = refused(&[("x.md", &text)]);
        assert!(err.contains("x.md: "), "{err}");
        assert!(err.contains(why), "{err}\nwanted: {why}");
    }
}
