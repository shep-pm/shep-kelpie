use std::path::Path;

use super::*;
use crate::settings::{Effort, GuardHook, HookEvent, LoopReviewer, NonBlank, ReviewerName, Runs};
use crate::test::{project_table, with_tables};
use crate::webhook::KelpieSettings;

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

// The example's project, its review running the local reviewer `first`, then `claude`.
fn reviewing(first: &str) -> Settings {
    let review = "[app.dogs.kelpie.review]\n";
    let reviewers = format!("{review}reviewers = [\"{first}\", \"claude\"]\n");
    let table = project_table(&EXAMPLE.replace(review, &reviewers));
    Settings::from_table(&table, "shep", Path::new("/h"), Path::new("/p")).unwrap()
}

fn kelpie(section: &str) -> KelpieSettings {
    KelpieSettings::from_section(section).unwrap()
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
fn a_project_that_names_nothing_builds_on_sonnet_high_and_reviews_on_its_models() {
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
    assert_eq!(pair(&agents.reviewer), ("claude-sonnet-5", Effort::Medium));
    assert_eq!(
        pair(&agents.deep_reviewer),
        ("claude-opus-5-5", Effort::High)
    );
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
fn a_local_implementer_is_never_the_default() {
    let settings = project("implementers = [\"qwen\", \"sonnet-high\"]\n");
    let agents = settings.role_agents(&book()).unwrap();
    assert_eq!(names(&agents), ["qwen", "sonnet-high"]);
    assert_eq!(agents.default_implementer.name.as_str(), "sonnet-high");
    assert!(agents.implementers[0].is_local());

    let err = project("implementers = [\"qwen\"]\n")
        .role_agents(&book())
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "setting `agents.implementers`: lists no implementer that is not a local model, \
         and a local one runs only the issues labelled for it: list one more, such as \
         `sonnet-high`, to run the rest"
    );
    let none = project("implementers = []\n").role_agents(&book());
    assert!(none.is_err());
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
fn the_deep_round_runs_on_the_agent_or_model_its_role_names() {
    let mut settings = project("");
    settings.models.deep_reviewer = RoleModel {
        model: NonBlank::try_from("claude-sonnet-5-5".to_owned()).unwrap(),
        effort: Effort::Medium,
        harness: AgentHarness::ClaudeCode,
    };
    let agents = settings.role_agents(&book()).unwrap();
    assert_eq!(
        pair(&agents.deep_reviewer),
        ("claude-sonnet-5-5", Effort::Medium)
    );

    let settings = project("deep_reviewer = \"haiku\"\n");
    let agents = settings.role_agents(&book()).unwrap();
    assert_eq!(
        pair(&agents.deep_reviewer),
        ("claude-haiku-4-5-20251001", Effort::Low)
    );
}

#[test]
fn a_review_role_naming_an_agent_kelpie_lacks_is_refused_naming_both() {
    let err = project("reviewer = \"fable\"\n")
        .role_agents(&book())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("`agents.reviewer` names fable, which has no agent file"),
        "{err}"
    );
    assert!(err.contains("`agents/fable.md`"), "{err}");
}

#[test]
fn the_projects_claude_round_runs_on_its_reviewer_agent() {
    let mut settings = project("reviewer = \"opus-high\"\n");
    settings.review.reviewers = vec![ReviewerName::claude()];
    let lineup = settings
        .lineup(&KelpieSettings::default(), &book(), Path::new("/h"))
        .unwrap();
    let claude = lineup.iter().find(|r| r.name.as_str() == "claude").unwrap();
    let Runs::Claude(session) = &claude.runs else {
        panic!("{claude:?}");
    };
    assert_eq!(pair(&session.model()), ("claude-opus-5-5", Effort::High));
}

#[test]
fn a_session_reviewer_names_an_agent_file() {
    let settings = reviewing("thorough");
    let section = "[local_reviewers.thorough]\nkind = \"session\"\nagent = \"opus-high\"\n\
                   paths = [\"src/**\"]\n";
    let lineup = settings
        .lineup(&kelpie(section), &book(), Path::new("/h"))
        .unwrap();
    let names: Vec<&str> = lineup.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["thorough", "claude"]);
    let Runs::Claude(thorough) = &lineup[0].runs else {
        panic!("{:?}", lineup[0]);
    };
    assert_eq!(pair(&thorough.model()), ("claude-opus-5-5", Effort::High));
    assert_eq!(lineup[0].paths()[0].as_str(), "src/**");
    assert!(!LoopReviewer::is_local(&lineup[0]));

    let missing = "[local_reviewers.thorough]\nkind = \"session\"\nagent = \"fable\"\n";
    let err = settings
        .lineup(&kelpie(missing), &book(), Path::new("/h"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("`local_reviewers.thorough.agent` names fable, which has no agent file"),
        "{err}"
    );
}

#[test]
fn each_review_role_is_held_back_by_its_agents_account_or_lease() {
    let none = project("").role_agents(&book()).unwrap();
    assert_eq!(none.limits, RoleLimits::default());

    let named = project("reviewer = \"qwen\"\ndeep_reviewer = \"box\"\n");
    let limits = named.role_agents(&book()).unwrap().limits;
    assert_eq!(limits.reviewer, Limit::Lease(LeaseName::gpu()));
    let Limit::Lease(lease) = limits.deep_reviewer else {
        panic!("{:?}", limits.deep_reviewer);
    };
    assert_eq!(lease.as_str(), "gpu-box");
}

#[test]
fn a_session_reviewer_carries_its_agents_limit_and_harness() {
    let settings = reviewing("local");
    let section = "[local_reviewers.local]\nkind = \"session\"\nagent = \"qwen\"\n";
    let lineup = settings
        .lineup(&kelpie(section), &book(), Path::new("/h"))
        .unwrap();
    let Runs::Claude(local) = &lineup[0].runs else {
        panic!("{:?}", lineup[0]);
    };
    assert_eq!(local.limit, Limit::Lease(LeaseName::gpu()));
    assert_eq!(local.model().harness.harness(), Harness::Pi);
    let Runs::Claude(claude) = &lineup[1].runs else {
        panic!("{:?}", lineup[1]);
    };
    assert_eq!(claude.limit, Limit::Account(Account::Claude));
}

#[test]
fn a_codex_agent_runs_every_role_on_the_codex_account() {
    let names = "implementers = [\"gpt\"]\nreviewer = \"gpt\"\ndeep_reviewer = \"gpt\"\n";
    let agents = project(names).role_agents(&book()).unwrap();
    let gpt = &agents.default_implementer;
    for role in [&gpt.model, &agents.reviewer, &agents.deep_reviewer] {
        assert_eq!(role.harness, AgentHarness::Codex);
        assert_eq!(pair(role), ("gpt-6-sol", Effort::Medium));
    }
    let limits = agents.limits;
    for limit in [&gpt.limit, &limits.reviewer, &limits.deep_reviewer] {
        assert_eq!(*limit, Limit::Account(Account::Codex));
    }
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
        let reviewer = hooked(project(&format!("reviewer = \"{name}\"\n")));
        assert!(
            reviewer.role_agents(&book()).is_ok(),
            "a reviewer runs no guard hooks"
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
