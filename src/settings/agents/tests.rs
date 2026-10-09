use std::path::Path;

use super::*;
use crate::settings::{AgentName, Effort, GuardHook, HookEvent};
use crate::test::{project_table, with_tables};

const EXAMPLE: &str = include_str!("../../../settings.example.toml");

/// The example's own `[agents]` table, which a test's table replaces
const LISTED: &str = "[app.dogs.kelpie.agents]\nimplementers = [\"sonnet-high\"]\n";

const HAIKU: &str = "---\nrole: implementer\nharness: claude-code\nmodel: claude-haiku-4-5-20251001\n\
     effort: low\n---\n";

const QWEN: &str = "---\nrole: implementer\nharness: pi\nmodel: qwen3.8:27b\neffort: low\n\
                    url: http://box:11434/v1\ncontext: 65536\n---\n";

const GPT: &str = "---\nrole: implementer\nharness: codex\nmodel: gpt-6-sol\neffort: medium\n---\n";

const BOX: &str = "---\nrole: implementer\nharness: stand-in\nmodel: qwen3-coder\neffort: low\n\
                   usage: none\nlease: gpu-box\n---\n";

// Kelpie's own agents, with haiku, a pi agent, a Codex one and one on another GPU's lease.
fn book() -> Agents {
    Agents::embedded()
        .with("haiku", HAIKU)
        .with("qwen", QWEN)
        .with("gpt", GPT)
        .with("box", BOX)
}

// The example's project with `names` as its `[agents]` table.
fn project(names: &str) -> Settings {
    assert!(EXAMPLE.contains(LISTED), "the example's agents table moved");
    let entry = EXAMPLE.replace(LISTED, &format!("[app.dogs.kelpie.agents]\n{names}"));
    Settings::from_table(
        &project_table(&entry),
        "shep",
        Path::new("/h"),
        Path::new("/p"),
    )
    .unwrap()
}

fn pair(model: &RoleModel) -> (&str, Effort) {
    (model.model.as_str(), model.effort)
}

fn names(agents: &RoleAgents) -> Vec<&str> {
    agents
        .implementers
        .iter()
        .map(|i| i.name.as_str())
        .collect()
}

fn hooked(mut settings: Settings) -> Settings {
    settings.worker.guard_hooks = vec![GuardHook {
        event: HookEvent::PreToolUse,
        matcher: None,
        command: "true".to_owned().try_into().unwrap(),
    }];
    settings
}

#[test]
fn a_project_that_names_nothing_builds_on_sonnet_high() {
    let table = with_tables(&EXAMPLE.replace(LISTED, ""), "");
    let settings = Settings::from_table(
        &project_table(&table),
        "shep",
        Path::new("/h"),
        Path::new("/p"),
    )
    .unwrap();
    let agents = settings.role_agents(&Agents::embedded()).unwrap();
    assert_eq!(names(&agents), ["sonnet-high"]);
    let sonnet = &agents.default_implementer;
    assert_eq!(sonnet.name.as_str(), "sonnet-high");
    assert_eq!(pair(&sonnet.model), ("claude-sonnet-5-5", Effort::High));
    assert_eq!(sonnet.limit, Limit::Account(Account::Claude));
}

#[test]
fn the_implementers_keep_the_projects_order_once_each_and_the_first_is_the_default() {
    let settings = project("implementers = [\"opus-high\", \"haiku\", \"opus-high\"]\n");
    let agents = settings.role_agents(&book()).unwrap();
    assert_eq!(names(&agents), ["opus-high", "haiku"]);
    assert_eq!(agents.default_implementer.name.as_str(), "opus-high");
    let haiku = agents.implementer(&"haiku".to_owned().try_into().unwrap());
    assert_eq!(
        pair(&haiku.unwrap().model),
        ("claude-haiku-4-5-20251001", Effort::Low)
    );
    assert!(
        agents
            .implementer(&"sonnet-high".to_owned().try_into().unwrap())
            .is_none()
    );
}

#[test]
fn a_local_implementer_listed_first_is_the_default_and_may_be_the_only_one() {
    let settings = project("implementers = [\"qwen\", \"sonnet-high\"]\n");
    let agents = settings.role_agents(&book()).unwrap();
    assert_eq!(names(&agents), ["qwen", "sonnet-high"]);
    assert_eq!(agents.default_implementer.name.as_str(), "qwen");
    assert!(agents.default_implementer.is_local());

    let only = project("implementers = [\"qwen\", \"box\"]\n");
    let agents = only.role_agents(&book()).unwrap();
    assert_eq!(names(&agents), ["qwen", "box"]);

    let err = project("implementers = []\n")
        .role_agents(&book())
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "setting `agents.implementers`: lists no implementer: list one, such as `sonnet-high`"
    );
}

#[test]
fn the_issue_writer_is_the_file_agents_issue_writer_names() {
    let settings = project("implementers = [\"sonnet-high\"]\n");
    assert_eq!(settings.agents.issue_writer.as_str(), "issue-writer");
    assert_eq!(settings.agents.fallback_after, None);

    let writer = "---\nrole: issue-writer\nharness: claude-code\nmodel: claude-sonnet-5-5\n\
                  effort: low\n---\nWrite issues.\n";
    let named = project("implementers = [\"sonnet-high\"]\nissue_writer = \"scribe\"\n");
    assert!(named.role_agents(&book().with("scribe", writer)).is_ok());
    let err = named.role_agents(&book()).unwrap_err().to_string();
    assert!(
        err.contains("`agents.issue_writer` names scribe, which has no agent file"),
        "{err}"
    );
    let wrong = project("implementers = [\"sonnet-high\"]\nissue_writer = \"opus-high\"\n");
    let err = wrong.role_agents(&book()).unwrap_err().to_string();
    assert!(
        err.contains("whose agent file's role is `implementer`"),
        "{err}"
    );

    let timed = project("implementers = [\"sonnet-high\"]\nfallback_after = 15\n");
    assert_eq!(timed.agents.fallback_after.map(|m| m.get()), Some(15));
}

#[test]
fn an_implementer_with_no_agent_file_is_refused_naming_it() {
    let err = project("implementers = [\"sonnet-high\", \"fable\"]\n")
        .role_agents(&book())
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "setting `agents`: `agents.implementers` names fable, which has no \
         agent file: write `agents/fable.md` in kelpie's home"
    );
}

#[test]
fn an_implementer_whose_file_is_a_reviewers_is_refused_naming_its_role() {
    let err = project("implementers = [\"defect-hunter\"]\n")
        .role_agents(&book())
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "setting `agents`: `agents.implementers` names defect-hunter, whose agent file's \
         role is `reviewer`: list it where that role goes, or name an agent whose role is \
         `implementer`"
    );
}

#[test]
fn the_issue_writer_listed_as_an_implementer_is_refused_naming_its_role() {
    let err = project("implementers = [\"issue-writer\"]\n")
        .role_agents(&book())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("whose agent file's role is `issue-writer`"),
        "{err}"
    );
}

#[test]
fn a_codex_implementer_builds_on_the_codex_account() {
    let agents = project("implementers = [\"gpt\"]\n")
        .role_agents(&book())
        .unwrap();
    let gpt = &agents.default_implementer;
    assert_eq!(gpt.model.harness, AgentHarness::Codex);
    assert_eq!(pair(&gpt.model), ("gpt-6-sol", Effort::Medium));
    assert_eq!(gpt.limit, Limit::Account(Account::Codex));
}

#[test]
fn an_implementer_off_claude_code_beside_guard_hooks_is_refused_at_load() {
    for (name, harness) in [("qwen", "pi"), ("gpt", "codex")] {
        let names = format!("implementers = [\"sonnet-high\", \"{name}\"]\n");
        assert!(project(&names).role_agents(&book()).is_ok());
        let err = hooked(project(&names))
            .role_agents(&book())
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            format!(
                "setting `agents.implementers`: implementer {name} runs on {harness}, which \
                 cannot run `worker.guard_hooks`, which are Claude Code hooks: turn those \
                 off or build on Claude Code"
            )
        );
    }
}

#[test]
fn a_pi_implementer_may_not_be_allowed_the_model_host_or_the_loopback() {
    let names = "implementers = [\"sonnet-high\", \"qwen\"]\n";
    for domain in ["box", "BOX", "localhost", "127.0.0.1", "*.localhost"] {
        let mut settings = project(names);
        settings.worker.allowed_domains = vec![domain.to_owned().try_into().unwrap()];
        let err = settings.role_agents(&book()).unwrap_err().to_string();
        assert!(
            err.contains("worker.allowed_domains") && err.contains(domain),
            "{err}"
        );
    }
    let mut settings = project(names);
    settings.worker.allowed_domains = vec!["github.com".to_owned().try_into().unwrap()];
    assert!(settings.role_agents(&book()).is_ok());
}

#[test]
fn an_agents_name_is_lowercase_letters_digits_and_dashes() {
    assert!(AgentName::try_from("Opus".to_owned()).is_err());
    assert!(AgentName::try_from("opus high".to_owned()).is_err());
    assert!(AgentName::try_from("opus-5-high".to_owned()).is_ok());
}

#[test]
fn a_project_manager_is_named_by_its_file_and_none_is_the_default() {
    let none = project("implementers = [\"sonnet-high\"]\n");
    assert_eq!(none.role_agents(&book()).unwrap().pm, None);

    let named = project("implementers = [\"sonnet-high\"]\npm = \"pm\"\n");
    let pm = named.role_agents(&book()).unwrap().pm.unwrap();
    assert_eq!(pm.name.as_str(), "pm");
    assert_eq!(pair(&pm.model), ("claude-opus-5-5", Effort::Medium));
    assert_eq!(pm.limit, Limit::Account(Account::Claude));

    let err = project("implementers = [\"sonnet-high\"]\npm = \"opus-high\"\n")
        .role_agents(&book())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("`agents.pm` names opus-high, whose agent file's role is `implementer`"),
        "{err}"
    );
}
