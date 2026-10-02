use std::path::Path;

use super::*;
use crate::settings::{LoopReviewer, NonBlank, ReviewerName, Runs};
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

// The example's project, its review loop running the local reviewer `first`, then `claude`.
fn reviewing(first: &str) -> Settings {
    let guard = "loop_guard = 8\n";
    let reviewers = format!("{guard}reviewers = [\"{first}\", \"claude\"]\n");
    let table = project_table(&EXAMPLE.replace(guard, &reviewers));
    Settings::from_table(&table, "shep", Path::new("/h"), Path::new("/p")).unwrap()
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
    assert_eq!(pair(&agents.worker), ("claude-sonnet-5-5", Effort::High));
    assert_eq!(pair(&agents.reviewer), ("claude-sonnet-5", Effort::Medium));
    assert_eq!(pair(&agents.judge), ("claude-opus-5-5", Effort::Low));
    assert_eq!(pair(&agents.auditor), ("claude-opus-5-5", Effort::High));
    assert_eq!(
        pair(&agents.deep_reviewer),
        ("claude-opus-5-5", Effort::High)
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
    let agents = settings.role_agents(&kelpie(AGENTS).agents).unwrap();
    assert_eq!(
        pair(&agents.deep_reviewer),
        ("claude-sonnet-5-5", Effort::Medium)
    );

    let settings = project("deep_reviewer = \"haiku\"\n");
    let agents = settings.role_agents(&kelpie(AGENTS).agents).unwrap();
    assert_eq!(
        pair(&agents.deep_reviewer),
        ("claude-haiku-4-5-20251001", Effort::Low)
    );
}

#[test]
fn the_whole_issue_check_runs_on_the_model_its_models_entry_names() {
    let mut settings = project("");
    settings.models.auditor = RoleModel {
        model: NonBlank::try_from("claude-opus-5-5".to_owned()).unwrap(),
        effort: Effort::Max,
        harness: AgentHarness::ClaudeCode,
    };
    let agents = settings.role_agents(&kelpie(AGENTS).agents).unwrap();
    assert_eq!(pair(&agents.auditor), ("claude-opus-5-5", Effort::Max));
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
    let gemini = "[agents.x]\nharness = \"gemini\"\nmodel = \"m\"\neffort = \"low\"\n";
    assert!(KelpieSettings::from_section(gemini).is_err());
    let extra = format!("{AGENTS}sandbox = \"none\"\n");
    assert!(KelpieSettings::from_section(&extra).is_err());
    assert!(AgentName::try_from("Opus".to_owned()).is_err());
}

#[test]
fn the_projects_claude_round_runs_on_its_reviewer_agent() {
    let mut settings = project("reviewer = \"opus-high\"\n");
    settings.review.reviewers = vec![ReviewerName::claude()];
    let lineup = settings.lineup(&kelpie(AGENTS), Path::new("/h")).unwrap();
    let claude = lineup.iter().find(|r| r.name.as_str() == "claude").unwrap();
    let Runs::Claude(session) = &claude.runs else {
        panic!("{claude:?}");
    };
    assert_eq!(pair(&session.model()), ("claude-opus-5-5", Effort::High));
}

#[test]
fn a_session_reviewer_names_an_agent_from_the_same_list() {
    let settings = reviewing("thorough");
    let section = format!(
        "{AGENTS}[local_reviewers.thorough]\nkind = \"session\"\nagent = \"opus-high\"\n\
         paths = [\"src/**\"]\n"
    );
    let lineup = settings.lineup(&kelpie(&section), Path::new("/h")).unwrap();
    let names: Vec<&str> = lineup.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["thorough", "claude"]);
    let Runs::Claude(thorough) = &lineup[0].runs else {
        panic!("{:?}", lineup[0]);
    };
    assert_eq!(pair(&thorough.model()), ("claude-opus-5-5", Effort::High));
    assert_eq!(lineup[0].paths()[0].as_str(), "src/**");
    assert!(!LoopReviewer::is_local(&lineup[0]));

    let missing = "[local_reviewers.thorough]\nkind = \"session\"\nagent = \"opus-high\"\n";
    let err = settings
        .lineup(&kelpie(missing), Path::new("/h"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("`local_reviewers.thorough.agent` names opus-high, which is not defined"),
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
    let settings = reviewing("local");
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
            QWEN.replace("http://box", "https://box"),
            "`agents.qwen` runs on pi, with a `url` kelpie cannot forward to: \
             kelpie's forwarder reaches a model server over plain `http://`",
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
fn a_pi_worker_beside_the_preview_or_guard_hooks_is_refused_at_load() {
    let mut settings = project("worker = \"qwen\"\n");
    let defined = kelpie(QWEN).agents;
    assert!(settings.role_agents(&defined).is_ok());
    settings.preview.enabled = true;
    let err = settings.role_agents(&defined).unwrap_err().to_string();
    assert!(err.contains("which cannot run `preview.enabled`"), "{err}");
    settings.preview.enabled = false;
    settings.worker.guard_hooks = vec![crate::settings::GuardHook {
        event: crate::settings::HookEvent::PreToolUse,
        matcher: None,
        command: "true".to_owned().try_into().unwrap(),
    }];
    let err = settings.role_agents(&defined).unwrap_err().to_string();
    assert!(
        err.contains("which cannot run `worker.guard_hooks`"),
        "{err}"
    );
    let reviewer = project("reviewer = \"qwen\"\n");
    assert!(
        reviewer.role_agents(&defined).is_ok(),
        "a pi reviewer runs no preview"
    );
}

#[test]
fn a_pi_deep_reviewer_may_not_be_allowed_the_model_host_or_the_loopback_either() {
    // Its sessions that run commands get the worker's allowed domains.
    for domain in ["box", "localhost", "127.0.0.1"] {
        let mut settings = project("deep_reviewer = \"qwen\"\n");
        settings.worker.allowed_domains = vec![domain.to_owned().try_into().unwrap()];
        let err = settings
            .role_agents(&kelpie(QWEN).agents)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("worker.allowed_domains")
                && err.contains(domain)
                && err.contains("a pi deep_reviewer"),
            "{err}"
        );
    }
    let mut settings = project("deep_reviewer = \"qwen\"\n");
    settings.worker.allowed_domains = vec!["github.com".to_owned().try_into().unwrap()];
    assert!(settings.role_agents(&kelpie(QWEN).agents).is_ok());
}

#[test]
fn a_deep_reviewer_on_pi_or_codex_beside_guard_hooks_is_refused_at_load() {
    let hooks = vec![crate::settings::GuardHook {
        event: crate::settings::HookEvent::PreToolUse,
        matcher: None,
        command: "true".to_owned().try_into().unwrap(),
    }];
    let defined = format!("{}{CODEX}", QWEN);
    let defined = kelpie(&defined).agents;
    for (agent, harness) in [("qwen", "pi"), ("gpt", "codex")] {
        let mut settings = project(&format!("deep_reviewer = \"{agent}\"\n"));
        assert!(
            settings.role_agents(&defined).is_ok(),
            "no hooks, no refusal"
        );
        settings.worker.guard_hooks = hooks.clone();
        let err = settings.role_agents(&defined).unwrap_err().to_string();
        assert!(
            err.contains(&format!(
                "`agents.deep_reviewer` names {agent}, on {harness}, which cannot run `worker.guard_hooks`"
            )),
            "{err}"
        );
    }
    // The hooks are the worker's own, and a worker on Claude Code runs them.
    let mut claude = project("deep_reviewer = \"opus-high\"\n");
    claude.worker.guard_hooks = hooks;
    assert!(claude.role_agents(&kelpie(AGENTS).agents).is_ok());
}

#[test]
fn a_session_reviewer_on_pi_runs_on_pi() {
    let settings = reviewing("local");
    let section = format!("{QWEN}[local_reviewers.local]\nkind = \"session\"\nagent = \"qwen\"\n");
    let lineup = settings.lineup(&kelpie(&section), Path::new("/h")).unwrap();
    let Runs::Claude(local) = &lineup[0].runs else {
        panic!("{:?}", lineup[0]);
    };
    assert_eq!(local.model().harness.harness(), Harness::Pi);
    assert_eq!(local.limit, Limit::Lease(LeaseName::gpu()));
}

const CODEX: &str =
    "[agents.gpt]\nharness = \"codex\"\nmodel = \"gpt-6-sol\"\neffort = \"medium\"\n";

#[test]
fn a_codex_agent_runs_every_role_but_the_relay_on_the_codex_account() {
    let names = "worker = \"gpt\"\nreviewer = \"gpt\"\njudge = \"gpt\"\n\
                 planner = \"gpt\"\nauditor = \"gpt\"\n";
    let agents = project(names).role_agents(&kelpie(CODEX).agents).unwrap();
    for role in [
        &agents.worker,
        &agents.reviewer,
        &agents.judge,
        &agents.planner,
        &agents.auditor,
    ] {
        assert_eq!(role.harness, AgentHarness::Codex);
        assert_eq!(pair(role), ("gpt-6-sol", Effort::Medium));
    }
    let limits = agents.limits;
    for limit in [
        limits.worker,
        limits.reviewer,
        limits.judge,
        limits.planner,
        limits.auditor,
    ] {
        assert_eq!(limit, Limit::Account(Account::Codex));
    }
}

#[test]
fn a_codex_agent_takes_no_server_and_no_reader_but_codexs_own() {
    let cases = [
        (
            format!("{CODEX}url = \"http://box:11434/v1\"\ncontext = 65536\n"),
            "`agents.gpt` runs on codex, which takes no `url` or `context`",
        ),
        (
            format!("{CODEX}usage = \"claude\"\n"),
            "`agents.gpt` runs on codex, whose usage is read with `codex`",
        ),
        (
            format!("{CODEX}lease = \"gpu\"\n"),
            "`agents.gpt` sets `lease`",
        ),
    ];
    for (section, why) in cases {
        let err = project("worker = \"gpt\"\n")
            .role_agents(&kelpie(&section).agents)
            .unwrap_err()
            .to_string();
        assert!(err.contains(why), "{err}");
    }
}

#[test]
fn a_codex_worker_beside_the_preview_is_refused_at_load() {
    let mut settings = project("worker = \"gpt\"\n");
    let defined = kelpie(CODEX).agents;
    settings.preview.enabled = true;
    let err = settings.role_agents(&defined).unwrap_err().to_string();
    assert!(
        err.contains("names gpt, on codex, which cannot run `preview.enabled`"),
        "{err}"
    );
    assert!(project("judge = \"gpt\"\n").role_agents(&defined).is_ok());
}

#[test]
fn a_pi_worker_may_not_be_allowed_the_model_host_or_the_loopback() {
    for domain in ["box", "BOX", "localhost", "127.0.0.1", "*.localhost"] {
        let mut settings = project("worker = \"qwen\"\n");
        settings.worker.allowed_domains = vec![domain.to_owned().try_into().unwrap()];
        let err = settings
            .role_agents(&kelpie(QWEN).agents)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("worker.allowed_domains") && err.contains(domain),
            "{err}"
        );
    }
    let mut settings = project("worker = \"qwen\"\n");
    settings.worker.allowed_domains = vec!["github.com".to_owned().try_into().unwrap()];
    assert!(settings.role_agents(&kelpie(QWEN).agents).is_ok());
}
