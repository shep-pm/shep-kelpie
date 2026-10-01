use std::path::Path;

use super::*;
use crate::settings::{LoopReviewer, Runs};
use crate::test::{project_table, with_tables};
use crate::webhook::KelpieSettings;

const EXAMPLE: &str = include_str!("../../../settings.example.toml");

const AGENTS: &str = "[agents.opus-high]\nharness = \"claude-code\"\n\
                      model = \"claude-opus-5-5\"\neffort = \"high\"\n\
                      [agents.haiku]\nharness = \"claude-code\"\n\
                      model = \"claude-haiku-4-5-20251001\"\neffort = \"low\"\n";

// The example's project naming `names` in its `[agents]` table.
fn project(names: &str) -> Settings {
    let entry = with_tables(EXAMPLE, &format!("[app.dogs.kelpie.agents]\n{names}"));
    Settings::from_table(
        &project_table(&entry),
        "shep",
        Path::new("/h"),
        Path::new("/p"),
    )
    .unwrap()
}

fn kelpie(section: &str) -> KelpieSettings {
    KelpieSettings::from_section(section).unwrap()
}

fn pair(model: &RoleModel) -> (&str, Effort) {
    (model.model.as_str(), model.effort)
}

#[test]
fn a_project_that_names_no_agent_keeps_its_models_on_claude_code() {
    let settings = project("");
    let agents = settings.role_agents(&kelpie(AGENTS).agents).unwrap();
    assert_eq!(agents.worker, settings.models.worker);
    assert_eq!(pair(&agents.worker), ("claude-sonnet-5", Effort::Medium));
    assert_eq!(pair(&agents.reviewer), ("claude-sonnet-5", Effort::Medium));
    assert_eq!(pair(&agents.judge), ("claude-opus-5-5", Effort::Low));
}

#[test]
fn each_role_runs_on_the_agent_it_names() {
    let settings = project("worker = \"opus-high\"\njudge = \"haiku\"\n");
    let agents = settings.role_agents(&kelpie(AGENTS).agents).unwrap();
    assert_eq!(pair(&agents.worker), ("claude-opus-5-5", Effort::High));
    assert_eq!(pair(&agents.reviewer), ("claude-sonnet-5", Effort::Medium));
    assert_eq!(
        pair(&agents.judge),
        ("claude-haiku-4-5-20251001", Effort::Low)
    );
}

#[test]
fn a_role_naming_an_agent_kelpie_lacks_is_refused_naming_both() {
    let settings = project("reviewer = \"fable\"\n");
    let err = settings
        .role_agents(&kelpie(AGENTS).agents)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("`agents.reviewer` names fable, which is not defined"),
        "{err}"
    );
    assert!(err.contains("[agents.fable]"), "{err}");
}

#[test]
fn an_agent_takes_a_known_harness_and_nothing_else() {
    let codex = "[agents.x]\nharness = \"codex\"\nmodel = \"m\"\neffort = \"low\"\n";
    assert!(KelpieSettings::from_section(codex).is_err());
    let extra = format!("{AGENTS}sandbox = \"none\"\n");
    assert!(KelpieSettings::from_section(&extra).is_err());
    assert!(AgentName::try_from("Opus".to_owned()).is_err());
}

#[test]
fn the_projects_claude_round_runs_on_its_reviewer_agent() {
    let settings = project("reviewer = \"opus-high\"\n");
    let lineup = settings.lineup(&kelpie(AGENTS), Path::new("/h")).unwrap();
    let claude = lineup.iter().find(|r| r.name.as_str() == "claude").unwrap();
    let Runs::Claude(session) = &claude.runs else {
        panic!("{claude:?}");
    };
    assert_eq!(pair(&session.model()), ("claude-opus-5-5", Effort::High));
}

#[test]
fn a_session_reviewer_names_an_agent_from_the_same_list() {
    let guard = "loop_guard = 8\n";
    let entry = EXAMPLE.replace(
        guard,
        &format!("{guard}reviewers = [\"deep\", \"claude\"]\n"),
    );
    let table = project_table(&entry);
    let settings = Settings::from_table(&table, "shep", Path::new("/h"), Path::new("/p")).unwrap();
    let section = format!(
        "{AGENTS}[local_reviewers.deep]\nkind = \"session\"\nagent = \"opus-high\"\n\
         paths = [\"src/**\"]\n"
    );
    let lineup = settings.lineup(&kelpie(&section), Path::new("/h")).unwrap();
    let names: Vec<&str> = lineup.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["deep", "claude"]);
    let Runs::Claude(deep) = &lineup[0].runs else {
        panic!("{:?}", lineup[0]);
    };
    assert_eq!(pair(&deep.model()), ("claude-opus-5-5", Effort::High));
    assert_eq!(lineup[0].paths()[0].as_str(), "src/**");
    assert!(!LoopReviewer::is_local(&lineup[0]));

    let missing = "[local_reviewers.deep]\nkind = \"session\"\nagent = \"opus-high\"\n";
    let err = settings
        .lineup(&kelpie(missing), Path::new("/h"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("`local_reviewers.deep.agent` names opus-high, which is not defined"),
        "{err}"
    );
}

const LIMITED: &str = "[agents.codex]\nharness = \"stand-in\"\nmodel = \"gpt-5-codex\"\n\
                       effort = \"medium\"\nusage = \"codex\"\n\
                       [agents.qwen]\nharness = \"stand-in\"\nmodel = \"qwen3-coder\"\n\
                       effort = \"low\"\nusage = \"none\"\n\
                       [agents.box]\nharness = \"stand-in\"\nmodel = \"qwen3-coder\"\n\
                       effort = \"low\"\nusage = \"none\"\nlease = \"gpu-box\"\n";

#[test]
fn each_role_is_held_back_by_its_agents_account_or_lease() {
    let defined = kelpie(&format!("{AGENTS}{LIMITED}")).agents;
    let none = project("").role_agents(&defined).unwrap();
    assert_eq!(none.limits, RoleLimits::default());
    assert_eq!(none.limits.worker, Limit::Account(Account::Claude));

    let named = project("worker = \"codex\"\nreviewer = \"qwen\"\njudge = \"box\"\n");
    let limits = named.role_agents(&defined).unwrap().limits;
    assert_eq!(limits.worker, Limit::Account(Account::Codex));
    assert_eq!(limits.reviewer, Limit::Lease(LeaseName::gpu()));
    let Limit::Lease(lease) = limits.judge else {
        panic!("{:?}", limits.judge);
    };
    assert_eq!(lease.as_str(), "gpu-box");

    let claude = project("worker = \"opus-high\"\n");
    let limits = claude.role_agents(&defined).unwrap().limits;
    assert_eq!(limits.worker, Limit::Account(Account::Claude));
}

#[test]
fn a_lease_on_an_agent_whose_usage_is_read_is_refused() {
    let both = "[agents.x]\nharness = \"stand-in\"\nmodel = \"m\"\neffort = \"low\"\n\
                usage = \"codex\"\nlease = \"gpu\"\n";
    let err = project("worker = \"x\"\n")
        .role_agents(&kelpie(both).agents)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("`agents.x` sets `lease`, which only an agent with `usage = \"none\"` takes"),
        "{err}"
    );
    let unknown = "[agents.x]\nharness = \"claude-code\"\nmodel = \"m\"\neffort = \"low\"\n\
                   usage = \"gemini\"\n";
    assert!(KelpieSettings::from_section(unknown).is_err());
}

#[test]
fn claude_code_takes_no_usage_reader_but_its_own() {
    for usage in ["codex", "none"] {
        let section = format!(
            "[agents.x]\nharness = \"claude-code\"\nmodel = \"claude-opus-5-5\"\n\
             effort = \"low\"\nusage = \"{usage}\"\n"
        );
        let err = project("worker = \"x\"\n")
            .role_agents(&kelpie(&section).agents)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&format!(
                "`agents.x` runs on claude-code, whose usage is read with `claude`, \
                 so it cannot set `usage = \"{usage}\"`: leave `usage` out"
            )),
            "{err}"
        );
    }
    let own = "[agents.x]\nharness = \"claude-code\"\nmodel = \"m\"\neffort = \"low\"\n\
               usage = \"claude\"\n";
    let limits = project("worker = \"x\"\n").role_agents(&kelpie(own).agents);
    assert_eq!(
        limits.unwrap().limits.worker,
        Limit::Account(Account::Claude)
    );
}

#[test]
fn a_session_reviewer_carries_its_agents_limit() {
    let guard = "loop_guard = 8\n";
    let entry = EXAMPLE.replace(
        guard,
        &format!("{guard}reviewers = [\"local\", \"claude\"]\n"),
    );
    let table = project_table(&entry);
    let settings = Settings::from_table(&table, "shep", Path::new("/h"), Path::new("/p")).unwrap();
    let section =
        format!("{LIMITED}[local_reviewers.local]\nkind = \"session\"\nagent = \"qwen\"\n");
    let lineup = settings.lineup(&kelpie(&section), Path::new("/h")).unwrap();
    let Runs::Claude(local) = &lineup[0].runs else {
        panic!("{:?}", lineup[0]);
    };
    assert_eq!(local.limit, Limit::Lease(LeaseName::gpu()));
    let Runs::Claude(claude) = &lineup[1].runs else {
        panic!("{:?}", lineup[1]);
    };
    assert_eq!(claude.limit, Limit::Account(Account::Claude));
}

const QWEN: &str = "[agents.qwen]\nharness = \"pi\"\nmodel = \"qwen3.8:27b\"\n\
                    effort = \"low\"\nurl = \"http://box:11434/v1\"\ncontext = 65536\n";

#[test]
fn a_pi_agent_runs_on_its_server_and_holds_the_gpu() {
    let agents = project("worker = \"qwen\"\njudge = \"qwen\"\n")
        .role_agents(&kelpie(QWEN).agents)
        .unwrap();
    let AgentHarness::Pi(server) = &agents.worker.harness else {
        panic!("{:?}", agents.worker);
    };
    assert_eq!(server.url.as_str(), "http://box:11434/v1");
    assert_eq!(server.context.get(), 65536);
    assert_eq!(pair(&agents.worker), ("qwen3.8:27b", Effort::Low));
    assert_eq!(agents.limits.worker, Limit::Lease(LeaseName::gpu()));
    assert_eq!(agents.limits.judge, Limit::Lease(LeaseName::gpu()));
    assert_eq!(agents.reviewer.harness, AgentHarness::ClaudeCode);
    assert_eq!(agents.limits.reviewer, Limit::Account(Account::Claude));
}

#[test]
fn an_agent_must_say_where_its_model_runs_exactly_when_its_harness_needs_it() {
    let cases = [
        (
            QWEN.replace("url = \"http://box:11434/v1\"\n", ""),
            "`agents.qwen` runs on pi, which needs the model's server as `url` \
             and its context size as `context`",
        ),
        (
            QWEN.replace("harness = \"pi\"", "harness = \"claude-code\""),
            "`agents.qwen` runs on claude-code, which takes no `url` or `context`",
        ),
        (
            format!("{QWEN}usage = \"claude\"\n"),
            "`agents.qwen` runs on pi, whose usage is read with `none`",
        ),
    ];
    for (section, why) in cases {
        let err = project("worker = \"qwen\"\n")
            .role_agents(&kelpie(&section).agents)
            .unwrap_err()
            .to_string();
        assert!(err.contains(why), "{err}");
    }
}

#[test]
fn a_session_reviewer_on_pi_runs_on_pi() {
    let guard = "loop_guard = 8\n";
    let entry = EXAMPLE.replace(
        guard,
        &format!("{guard}reviewers = [\"local\", \"claude\"]\n"),
    );
    let table = project_table(&entry);
    let settings = Settings::from_table(&table, "shep", Path::new("/h"), Path::new("/p")).unwrap();
    let section = format!("{QWEN}[local_reviewers.local]\nkind = \"session\"\nagent = \"qwen\"\n");
    let lineup = settings.lineup(&kelpie(&section), Path::new("/h")).unwrap();
    let Runs::Claude(local) = &lineup[0].runs else {
        panic!("{:?}", lineup[0]);
    };
    assert_eq!(local.model().harness.harness(), Harness::Pi);
    assert_eq!(local.limit, Limit::Lease(LeaseName::gpu()));
}
