use super::*;
use crate::settings::{Account, AgentHarness, AgentName, EndpointUrl, GatewayName, ModelHost};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn url(text: &str) -> EndpointUrl {
    EndpointUrl::try_from(text.to_owned()).unwrap()
}

mod axes;
mod bots;
mod debug;
mod extends;
mod fit;
mod goose;
mod init;
mod legacy;
mod needs;
mod pm;
mod representative;
mod tools;
mod triager;
mod unbound;

// A binding to put on a baseline, which ships with none.
const OPUS_HIGH: &str = "harness: claude-code\nmodel: claude-opus-5-5\neffort: high\n";

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

// What is wrong with the first file of `files` that is left out.
fn refused(files: &[(&str, &str)]) -> String {
    left_out(folder(files).path())
}

fn left_out(dir: &Path) -> String {
    let agents = Agents::load(dir).unwrap();
    agents.broken.values().next().unwrap().to_string()
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
        Agents::embedded().bind_all(OPUS_HIGH).agents.len(),
        DEFAULTS.len() + baseline::ALL.len(),
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
    assert_eq!(server.host, ModelHost::Url(url("http://box:11434/v1")));
    assert_eq!(server.context.get(), 65536);
    assert_eq!(*limit(qwen), Limit::Unqueued);
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
    let err = left_out(dir.path());
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
    let err = left_out(dir.path());
    let path = dir.path().join("qwen.md");
    assert_eq!(
        err,
        format!(
            "agent file {}: `harness`: unknown variant `gemini`, expected one of \
             claude-code, pi, codex, goose, stand-in at line 3, column 10",
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
            "runs on pi, which needs the model's server as `url` or a `gateway`, and its \
             context size as `context`",
        ),
        (
            QWEN.replace("harness: pi", "harness: claude-code"),
            "runs on claude-code, which takes no `url`, `gateway` or `context`",
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
fn the_embedded_security_reviewer_file_parses_as_a_reviewer() {
    let agents = Agents::embedded().bind("security-reviewer", OPUS_HIGH);
    let reviewer = agents.get(&name("security-reviewer")).unwrap();
    assert_eq!(reviewer.role, Role::Reviewer);
    assert_eq!(model(reviewer).harness, AgentHarness::ClaudeCode);
    assert!(!reviewer.second_look, "it reads once");
    let prompt = reviewer.prompt.as_deref().unwrap();
    assert!(
        prompt.contains("{{DIFF}}"),
        "the diff goes in where the file says"
    );
    assert!(prompt.contains("CLEAN"));
    assert!(prompt.contains("SEVERITY|file:line|"));
}

#[test]
fn the_embedded_default_reviewers_are_a_lite_hunter_and_a_local_endpoint_on_paddock() {
    let agents = Agents::embedded().bind("defect-hunter", OPUS_HIGH);
    let lite = agents.get(&name("defect-hunter-lite")).unwrap();
    assert_eq!(lite.role, Role::Reviewer);
    assert_eq!(model(lite).harness, AgentHarness::ClaudeCode);
    assert_eq!(pair(lite), ("claude-opus-5-5", Effort::Medium));
    assert!(!lite.second_look, "it reads once");
    let full = agents.get(&name("defect-hunter")).unwrap();
    assert_eq!(
        lite.prompt, full.prompt,
        "the same prompt on a smaller budget"
    );

    let local = agents.get(&name("iq3-xxs-review")).unwrap();
    assert_eq!(local.role, Role::Reviewer);
    let (gateway, model) = local.runs.gateway().unwrap();
    assert_eq!((gateway.as_str(), model), ("paddock", "iq3_xxs"));
    assert!(local.runs.local().is_some() && local.prompt.is_none());
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
            "defect-hunter-lite",
            "iq3-xxs-review",
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

#[test]
fn a_default_is_written_as_a_stub_that_extends_the_shipped_agent() {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join("agents");
    write_defaults(&agents, &dir.path().join("home")).unwrap();
    assert_eq!(
        fs::read_to_string(agents.join("opus-high.md")).unwrap(),
        "---\nextends: kelpie:opus-high\n---\n"
    );
    assert!(
        !agents.join("triager.md").exists(),
        "a baseline gets no stub: it binds nothing, so a stub would be unbound"
    );
}

#[test]
fn defaults_are_also_written_whole_and_read_only_under_dot_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join("agents");
    write_defaults(&agents, &dir.path().join("home")).unwrap();
    let shipped = Agents::embedded();
    let kept = agents.join(".defaults");
    for (agent, _) in shipped.iter() {
        let file = kept.join(format!("{agent}.md"));
        let text = fs::read_to_string(&file).unwrap_or_else(|e| panic!("{agent}: {e}"));
        let written = Agents::load(folder(&[(&format!("{agent}.md"), &text)]).path()).unwrap();
        assert_eq!(written.get(agent), shipped.get(agent), "{agent}");
        assert!(fs::metadata(&file).unwrap().permissions().readonly());
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o444
        );
    }
    let triager = fs::read_to_string(kept.join("triager.md")).unwrap();
    let comment: Vec<&str> = triager.lines().skip(1).take(2).collect();
    let comment = comment.join("\n");
    assert!(comment.starts_with("# "), "{comment}");
    for said in ["Kelpie writes this file", "edit", "agents/triager.md"] {
        assert!(comment.contains(said), "{comment}");
    }
    assert!(
        kept.join("qwen.md").is_file(),
        "every shipped agent, the script or not"
    );
    assert_eq!(
        Agents::load(&agents).unwrap(),
        Agents::embedded(),
        "the folder holds no agent in `.defaults`"
    );
}

#[test]
fn baselines_put_back_a_file_that_was_edited_or_removed() {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join("agents");
    write_baselines(&agents).unwrap();
    let kept = agents.join(".defaults");
    let before = fs::read_to_string(kept.join("pm.md")).unwrap();
    let file = kept.join("pm.md");
    fs::remove_file(&file).unwrap();
    fs::write(&file, "mine").unwrap();
    fs::remove_file(kept.join("spec.md")).unwrap();
    write_baselines(&agents).unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
    assert!(kept.join("spec.md").is_file());
    assert!(fs::metadata(&file).unwrap().permissions().readonly());
    assert!(
        fs::read_dir(&kept).unwrap().all(|f| {
            let name = f.unwrap().file_name();
            name.to_string_lossy().ends_with(".md")
        }),
        "no half-written file is left beside them"
    );
}

#[test]
fn baselines_remove_a_file_kelpie_no_longer_ships() {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join("agents");
    let kept = agents.join(".defaults");
    fs::create_dir_all(&kept).unwrap();
    let stale = kept.join("advisor.md");
    fs::write(&stale, "---\nrole: advisor\n---\nVote.\n").unwrap();
    let mut read_only = fs::metadata(&stale).unwrap().permissions();
    read_only.set_readonly(true);
    fs::set_permissions(&stale, read_only).unwrap();
    write_baselines(&agents).unwrap();
    assert!(!kept.join("advisor.md").exists());
    assert!(kept.join("pm.md").is_file());
}

#[test]
fn a_file_whose_role_is_gone_is_refused_saying_what_it_went_with() {
    let roles = [
        ("advisor", "went with the ruling panel"),
        ("researcher", "went with the ruling panel"),
        (
            "issue-writer",
            "went with the issue writer, and an issue is written with kelpie's `issue` skill",
        ),
    ];
    for (role, went) in roles {
        let text =
            format!("---\nrole: {role}\nharness: claude-code\nmodel: m\neffort: low\n---\nVote.\n");
        let err = refused(&[("mine.md", &text)]);
        assert!(err.contains(&format!("`{role}`")), "{role}: {err}");
        assert!(err.contains(went), "{role}: {err}");
    }
}

#[test]
fn a_file_extending_a_baseline_whose_role_went_is_refused_with_migrate_s_words() {
    for (role, went) in [
        ("advisor", "the ruling panel"),
        ("researcher", "the ruling panel"),
        ("issue-writer", "the issue writer"),
    ] {
        let text = format!("---\nextends: kelpie:{role}\nmodel: m\n---\nAlso this.\n");
        let err = refused(&[("mine.md", &text)]);
        let why = format!("the `{role}` role went with {went}");
        assert!(err.contains(&why), "{role}: {err}");
        assert!(!err.contains("names no shipped agent"), "{role}: {err}");
        let err = crate::agents::baseline::check("mine", &text).unwrap_err();
        assert!(err.contains(&why), "{role}: {err}");
    }
}

#[test]
fn a_read_only_temp_file_left_by_a_crash_does_not_stop_a_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join("agents");
    let kept = agents.join(".defaults");
    fs::create_dir_all(&kept).unwrap();
    let stale = kept.join("pm.md.tmp");
    fs::write(&stale, "half").unwrap();
    let mut permissions = fs::metadata(&stale).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&stale, permissions).unwrap();
    write_baselines(&agents).unwrap();
    assert!(kept.join("pm.md").is_file());
}

#[test]
fn refreshes_at_the_same_time_each_leave_every_baseline_whole() {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join("agents");
    std::thread::scope(|scope| {
        let refreshes: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| write_baselines(&agents)))
            .collect();
        for refresh in refreshes {
            refresh.join().unwrap().unwrap();
        }
    });
    let shipped = Agents::embedded();
    for (agent, _) in shipped.iter() {
        let text = fs::read_to_string(agents.join(".defaults").join(format!("{agent}.md")));
        assert!(text.unwrap().starts_with("---\n# Kelpie writes this file"));
    }
}

const REVIEWER: &str = "---\nrole: reviewer\nharness: claude-code\nmodel: claude-opus-5-5\n\
                        effort: high\npaths: [\"src/**\"]\n---\nRead {{DIFF}} for defects.\n";

const COMMAND: &str = "---\nrole: reviewer\nharness: command\ncommand: ~/bin/review\n---\n";

#[test]
fn kelpies_own_reviewers_are_defect_hunter_and_qwen() {
    let agents =
        Agents::embedded().bind("defect-hunter", &format!("{OPUS_HIGH}second_look: true\n"));
    let hunter = agents.get(&name("defect-hunter")).unwrap();
    assert_eq!(hunter.role, Role::Reviewer);
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
             model: coder\ncontext: 32768\n---\n",
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
    assert_eq!(endpoint.host, ModelHost::Url(url("http://box:11434/v1")));
    assert_eq!(endpoint.context.get(), 32768);
    let script = agents.get(&name("script")).unwrap();
    assert!(matches!(&script.runs, Runs::Command(_)));
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

const ENDPOINT: &str = "---\nrole: reviewer\nharness: endpoint\nurl: http://box:11434/v1\n\
                         model: coder\ncontext: 32768\n---\n";

#[test]
fn an_endpoint_reviewer_with_a_body_is_accepted() {
    let dir = folder(&[("box.md", &format!("{ENDPOINT}Report only panics.\n"))]);
    let agents = Agents::load(dir.path()).unwrap();
    let agent = agents.get(&name("box")).unwrap();
    assert_eq!(agent.prompt.as_deref(), Some("Report only panics."));
    let Runs::Endpoint(endpoint) = &agent.runs else {
        panic!("not an endpoint");
    };
    assert_eq!(endpoint.prompt.as_deref(), Some("Report only panics."));
    let bare = folder(&[("box.md", ENDPOINT)]);
    let agents = Agents::load(bare.path()).unwrap();
    let Runs::Endpoint(endpoint) = &agents.get(&name("box")).unwrap().runs else {
        panic!("not an endpoint");
    };
    assert_eq!(endpoint.prompt, None);
}

#[test]
fn a_body_naming_base_or_diff_is_refused_naming_the_file() {
    for placeholder in ["{{BASE}}", "{{DIFF}}"] {
        let text = format!("{ENDPOINT}Read the change against {placeholder}.\n");
        let err = refused(&[("box.md", &text)]);
        assert!(err.contains("box.md: "), "{err}");
        assert!(
            err.contains(&format!(
                "runs on endpoint, which sends the diff in chunks of its own, so its \
                 body cannot name {placeholder}"
            )),
            "{err}"
        );
    }
}

#[test]
fn a_command_and_a_bot_still_refuse_a_body() {
    let command = refused(&[("x.md", &format!("{COMMAND}Review it.\n"))]);
    assert!(command.contains("x.md: "), "{command}");
    assert!(
        command.contains("runs on command, which writes its own prompt: leave the body empty"),
        "{command}"
    );
    let bot = "---\nrole: reviewer\nharness: bot\nbot: cubic\nreviews: 5\nhours: 24\n---\n\
               Review it.\n";
    let err = refused(&[("y.md", bot)]);
    assert!(err.contains("y.md: "), "{err}");
    assert!(
        err.contains("runs on bot, which writes its own prompt: leave the body empty"),
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
            "---\nrole: reviewer\nharness: endpoint\nurl: http://box/v1\nmodel: m\n---\n"
                .to_owned(),
            "runs on an endpoint, which needs the server as `url` or a `gateway`, its \
             `model` and the model's context size as `context`",
        ),
    ];
    for (text, why) in cases {
        let err = refused(&[("x.md", &text)]);
        assert!(err.contains("x.md: "), "{err}");
        assert!(err.contains(why), "{err}\nwanted: {why}");
    }
}

#[test]
fn a_model_behind_a_gateway_is_queued_by_it_and_a_url_model_is_not() {
    let on = QWEN.replace("url: http://box:11434/v1", "gateway: paddock");
    let endpoint = "---\nrole: reviewer\nharness: endpoint\ngateway: paddock\nmodel: coder\n\
                    context: 32768\n---\n";
    let dir = folder(&[("local.md", &on), ("box.md", endpoint)]);
    let agents = Agents::load(dir.path()).unwrap();
    let local = agents.get(&name("local")).unwrap();
    assert_eq!(*limit(local), Limit::Gateway);
    let paddock = GatewayName::try_from("paddock".to_owned()).unwrap();
    assert_eq!(local.runs.gateway(), Some((&paddock, "qwen3.8:27b")));

    for (text, why) in [
        (
            on.replace("gateway: paddock", "gateway: paddock\nurl: http://box/v1"),
            "names both",
        ),
        (
            REVIEWER.replace("effort: high", "effort: high\ngateway: paddock"),
            "no `url`, `gateway`",
        ),
    ] {
        let err = refused(&[("x.md", &text)]);
        assert!(err.contains(why), "{err}");
    }
}

#[test]
fn a_refresh_replaces_baselines_the_last_one_left_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join("agents");
    write_baselines(&agents).unwrap();
    write_baselines(&agents).unwrap();
    let file = agents.join(".defaults").join("pm.md");
    assert!(fs::metadata(&file).unwrap().permissions().readonly());
}

// The names a refresh in this process would take first, a crash's leftovers.
fn leftovers(kept: &Path, count: u32) -> Vec<PathBuf> {
    let mut left = Vec::new();
    for (agent, _, _) in DEFAULTS {
        for n in 0..count {
            let path = kept.join(format!("{agent}.md.{}-{n}.tmp", std::process::id()));
            fs::write(&path, "half").unwrap();
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&path, permissions).unwrap();
            left.push(path);
        }
    }
    left
}

#[test]
fn a_leftover_temp_file_of_a_crashed_process_with_the_same_pid_does_not_stop_a_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join("agents");
    let kept = agents.join(".defaults");
    fs::create_dir_all(&kept).unwrap();
    let left = leftovers(&kept, 8);
    write_baselines(&agents).unwrap();
    assert!(kept.join("pm.md").is_file());
    assert!(
        left.iter().any(|path| path.exists()),
        "a fresh temp file may be a live writer's, so it is left"
    );
}

#[test]
fn old_leftover_temp_files_are_removed_and_a_fresh_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join("agents");
    let kept = agents.join(".defaults");
    fs::create_dir_all(&kept).unwrap();
    let old = kept.join("pm.md.1-1.tmp");
    let fresh = kept.join("pm.md.2-1.tmp");
    for path in [&old, &fresh] {
        fs::write(path, "half").unwrap();
    }
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();
    for path in [&old, &fresh] {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).unwrap();
    }
    write_baselines(&agents).unwrap();
    assert!(!old.exists());
    assert!(fresh.exists());
}
