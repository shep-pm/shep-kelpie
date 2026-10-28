use std::path::Path;

use super::*;
use crate::agents::baseline::Baseline;
use crate::settings::{AgentName, Effort, Gateways, GuardHook, HookEvent};
use crate::test::{project_table, with_tables};

const EXAMPLE: &str = include_str!("../../../../../settings.example.toml");

/// The example's own `[agents]` table, which a test's table replaces
const LISTED: &str = "[app.dogs.kelpie.agents]\nimplementers = [\"sonnet-high\"]\n";

const HAIKU: &str = "---\nrole: implementer\nharness: claude-code\nmodel: claude-haiku-4-5-20251001\n\
     effort: low\n---\n";

const QWEN: &str = "---\nrole: implementer\nharness: pi\nmodel: qwen3.8:27b\neffort: low\n\
                    url: http://box:11434/v1\ncontext: 65536\n---\n";

const GPT: &str = "---\nrole: implementer\nharness: codex\nmodel: gpt-6-sol\neffort: medium\n---\n";

const BOX: &str = "---\nrole: implementer\nharness: stand-in\nmodel: qwen3-coder\neffort: low\n\
                   usage: none\n---\n";

const OPUS: &str = "harness: claude-code\nmodel: claude-opus-5-5\n";

// Kelpie's own agents with every baseline bound, then haiku, a pi agent, a Codex one and a
// stand-in on no limit.
fn book() -> Agents {
    bound(Agents::embedded())
        .with("haiku", HAIKU)
        .with("qwen", QWEN)
        .with("gpt", GPT)
        .with("box", BOX)
}

// `agents` with each baseline bound to Opus 5.5, the project manager at medium effort and
// the representative at low.
fn bound(agents: Agents) -> Agents {
    agents
        .bind_all(&format!("{OPUS}effort: high\n"))
        .bind("pm", &format!("{OPUS}effort: medium\n"))
        .bind("representative", &format!("{OPUS}effort: low\n"))
}

// The example's project with `names` as its `[agents]` table.
fn project(names: &str) -> Settings {
    try_project(names).unwrap()
}

// [`project`], with the refusal the table gives rather than a panic.
fn try_project(names: &str) -> Result<Settings, SettingsError> {
    assert!(EXAMPLE.contains(LISTED), "the example's agents table moved");
    let entry = EXAMPLE.replace(LISTED, &format!("[app.dogs.kelpie.agents]\n{names}"));
    Settings::from_table(
        &project_table(&entry),
        "shep",
        Path::new("/h"),
        Path::new("/p"),
    )
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
    let agents = settings.role_agents(&bound(Agents::embedded())).unwrap();
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
fn agents_issue_writer_is_refused_naming_the_skill_and_the_migration() {
    let err = try_project("implementers = [\"sonnet-high\"]\nissue_writer = \"scribe\"\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("agents.issue_writer"), "{err}");
    assert!(err.contains("the issue writer is gone"), "{err}");
    assert!(err.contains("`issue` skill"), "{err}");
    assert!(err.contains("shep kelpie settings migrate"), "{err}");
}

#[test]
fn fallback_after_is_off_when_absent_and_read_in_minutes() {
    let settings = project("implementers = [\"sonnet-high\"]\n");
    assert_eq!(settings.agents.fallback_after, None);

    let timed = project("implementers = [\"sonnet-high\"]\nfallback_after = 15\n");
    assert_eq!(timed.agents.fallback_after.map(|m| m.get()), Some(15));
}

#[test]
fn implementer_choice_is_order_when_absent_and_refuses_the_issue_writer_by_name() {
    let absent = project("implementers = [\"sonnet-high\", \"opus-high\"]\n");
    assert_eq!(absent.agents.implementer_choice, ImplementerChoice::Order);

    let ordered = project(
        "implementers = [\"sonnet-high\", \"opus-high\"]\nimplementer_choice = \"order\"\n",
    );
    assert_eq!(ordered.agents.implementer_choice, ImplementerChoice::Order);
    assert_eq!(ImplementerChoice::Order.as_str(), "order");

    let err = try_project("implementers = [\"sonnet-high\"]\nimplementer_choice = \"first\"\n")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("agents.implementer_choice") && err.contains("unknown variant `first`"),
        "{err}"
    );

    let written = "implementers = [\"sonnet-high\"]\nimplementer_choice = \"issue-writer\"\n";
    let err = try_project(written).unwrap_err().to_string();
    assert!(err.contains("agents.implementer_choice"), "{err}");
    assert!(err.contains("`issue-writer` is gone"), "{err}");
    assert!(err.contains("shep kelpie settings migrate"), "{err}");
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
                 off, move the check to a `command` or `write` hook in `hooks`, which runs \
                 on every harness, or build on Claude Code"
            )
        );
    }
}

// The project's own `hooks` are kelpie's to run, so no harness is asked to run them.
#[test]
fn an_implementer_off_claude_code_beside_project_hooks_loads() {
    for name in ["qwen", "gpt"] {
        let names = format!("implementers = [\"sonnet-high\", \"{name}\"]\n");
        let mut settings = project(&names);
        settings.hooks = vec![crate::settings::ProjectHook {
            point: crate::settings::HookPoint::Commit,
            command: "true".to_owned().try_into().unwrap(),
        }];
        assert!(settings.role_agents(&book()).is_ok(), "{name}");
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
fn goose_loads_mcp() {
    let server = ModelServer {
        host: ModelHost::Url("http://box:11434/v1".to_owned().try_into().unwrap()),
        context: 65_536_i64.try_into().unwrap(),
    };
    for harness in [
        AgentHarness::ClaudeCode,
        AgentHarness::Codex,
        AgentHarness::Pi(server.clone()),
        AgentHarness::Goose(server),
    ] {
        assert!(harness.loads_mcp(), "{harness:?}");
    }
}

// An agent file cannot name goose as an implementer yet, so the model is built directly.
#[test]
fn a_goose_implementer_beside_guard_hooks_or_the_model_hosts_domain_is_refused() {
    let goose = RoleModel {
        model: "qwen3.8:27b".to_owned().try_into().unwrap(),
        effort: Effort::Low,
        harness: AgentHarness::Goose(ModelServer {
            host: ModelHost::Url("http://box:11434/v1".to_owned().try_into().unwrap()),
            context: 65_536_i64.try_into().unwrap(),
        }),
        tools: None,
    };
    let name = AgentName::kelpies("goosey");
    let settings = project("implementers = [\"sonnet-high\"]\n");
    assert!(settings.under_worker_fence(&name, &goose).is_ok());

    let err = hooked(project("implementers = [\"sonnet-high\"]\n"))
        .under_worker_fence(&name, &goose)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("implementer goosey runs on goose, which cannot run `worker.guard_hooks`"),
        "{err}"
    );

    for domain in ["box", "BOX", "localhost", "127.0.0.1", "*.localhost"] {
        let mut settings = project("implementers = [\"sonnet-high\"]\n");
        settings.worker.allowed_domains = vec![domain.to_owned().try_into().unwrap()];
        let err = settings
            .under_worker_fence(&name, &goose)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("worker.allowed_domains") && err.contains(domain) && err.contains("goose"),
            "{err}"
        );
    }
    let mut settings = project("implementers = [\"sonnet-high\"]\n");
    settings.worker.allowed_domains = vec!["github.com".to_owned().try_into().unwrap()];
    assert!(settings.under_worker_fence(&name, &goose).is_ok());
}

#[test]
fn a_goose_worker_beside_an_after_command_hook_is_refused_at_load_naming_the_reason() {
    let goose = RoleModel {
        model: "qwen3.8:27b".to_owned().try_into().unwrap(),
        effort: Effort::Low,
        harness: AgentHarness::Goose(ModelServer {
            host: ModelHost::Url("http://box:11434/v1".to_owned().try_into().unwrap()),
            context: 65_536_i64.try_into().unwrap(),
        }),
        tools: None,
    };
    let name = AgentName::kelpies("goosey");
    let hook = |point| crate::settings::ProjectHook {
        point,
        command: "true".to_owned().try_into().unwrap(),
    };
    let mut settings = project("implementers = [\"sonnet-high\"]\n");
    settings.hooks = vec![
        hook(crate::settings::HookPoint::Command),
        hook(crate::settings::HookPoint::Write),
    ];
    assert!(settings.under_worker_fence(&name, &goose).is_ok());

    settings
        .hooks
        .push(hook(crate::settings::HookPoint::AfterCommand));
    let err = settings
        .under_worker_fence(&name, &goose)
        .unwrap_err()
        .to_string();
    for said in [
        "implementer goosey runs on goose",
        "`after_command`",
        "result",
        "`command` or `write` hook",
        "another harness",
    ] {
        assert!(err.contains(said), "{said}: {err}");
    }
    // The other harnesses run it.
    let claude = RoleModel {
        harness: AgentHarness::ClaudeCode,
        ..goose
    };
    assert!(settings.under_worker_fence(&name, &claude).is_ok());
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

#[test]
fn agents_representative_defaults_to_representative() {
    let unset = project("implementers = [\"sonnet-high\"]\n");

    assert_eq!(unset.agents.representative.as_str(), "representative");
    let agent = unset.representative(&book()).unwrap();
    assert_eq!(agent.name.as_str(), "representative");
    assert_eq!(pair(&agent.model), ("claude-opus-5-5", Effort::Low));
    assert_eq!(agent.limit, Limit::Account(Account::Claude));
}

#[test]
fn a_representative_named_with_no_file_or_a_file_of_another_role_is_refused_naming_the_key() {
    let missing = project("implementers = [\"sonnet-high\"]\nrepresentative = \"herald\"\n");
    let err = missing.representative(&book()).unwrap_err().to_string();
    assert!(
        err.contains("`agents.representative` names herald"),
        "{err}"
    );

    let wrong = project("implementers = [\"sonnet-high\"]\nrepresentative = \"pm\"\n");
    let err = wrong.representative(&book()).unwrap_err().to_string();
    assert!(
        err.contains("`agents.representative` names pm, whose agent file's role is `pm`"),
        "{err}"
    );
}

#[test]
fn naming_an_unbound_agent_is_refused_with_the_command_that_binds_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("mine.md"), "---\nrole: implementer\n---\n").unwrap();
    let agents = Agents::load(dir.path()).unwrap();
    let err = project("implementers = [\"mine\"]\n")
        .role_agents(&agents)
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "setting `agents`: `agents.implementers` names mine, which is unbound: run \
         `shep kelpie agents new implementer --name mine`"
    );
}

#[test]
fn naming_an_agent_whose_file_cannot_be_used_is_refused_and_never_given_kelpies_own() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sonnet-high.md");
    std::fs::write(&file, "role: implementer\n").unwrap();
    let agents = Agents::load(dir.path()).unwrap();
    let err = project("implementers = [\"sonnet-high\"]\n")
        .role_agents(&agents)
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        format!(
            "setting `agents`: `agents.implementers` names sonnet-high, whose agent file {}: \
             must start with a `---` line, then the YAML frontmatter, then a `---` line of \
             its own",
            file.display()
        )
    );
}

// Baselines that bind nothing, as the shipped ones are once their keys move to bindings.
const BARE: [Baseline; 2] = [
    Baseline {
        name: "pm",
        text: "---\nrole: pm\n---\nYou manage.\n",
    },
    Baseline {
        name: "defect-hunter",
        text: "---\nrole: reviewer\n---\nYou hunt.\n",
    },
];

#[test]
fn naming_a_baseline_that_binds_nothing_and_has_no_file_is_refused_with_the_command() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("box.md"), BOX).unwrap();
    let bare = Agents::load_over(dir.path(), &BARE).unwrap();
    let err = project("implementers = [\"box\"]\npm = \"pm\"\n")
        .role_agents(&bare)
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "setting `agents`: `agents.pm` names pm, which is unbound: run \
         `shep kelpie agents new pm`"
    );
    let err = project("implementers = [\"defect-hunter\"]\n")
        .role_agents(&bare)
        .unwrap_err()
        .to_string();
    assert!(
        err.ends_with(
            "which is unbound: run `shep kelpie agents new reviewer --baseline defect-hunter`"
        ),
        "{err}"
    );
}

#[test]
fn a_baseline_a_book_binds_is_named_with_no_file() {
    let book = bound(Agents::load(tempfile::tempdir().unwrap().path()).unwrap());
    project("").role_agents(&book).unwrap();
}

// The example's project with `names` as its `[agents]` table and `contributions` as its
// `[contributions]` table, where it is given.
fn allowing(names: &str, contributions: &str) -> Settings {
    let entry = EXAMPLE.replace(LISTED, &format!("[app.dogs.kelpie.agents]\n{names}"));
    let entry = match contributions {
        "" => entry,
        lines => format!("{entry}\n[app.dogs.kelpie.contributions]\n{lines}"),
    };
    let table = project_table(&entry);
    Settings::from_table(&table, "shep", Path::new("/h"), Path::new("/p")).unwrap()
}

fn lineup(settings: &Settings) -> Result<Vec<String>, String> {
    let lineup = settings.lineup(&book(), &Gateways::default(), Path::new("/h"));
    let lineup = lineup.map_err(|e| e.to_string())?;
    Ok(lineup.iter().map(|r| r.name.to_string()).collect())
}

#[test]
fn an_absent_allow_lets_every_agent_through_as_before() {
    let settings = project("implementers = [\"opus-high\", \"qwen\"]\npm = \"pm\"\n");
    assert_eq!(settings.agents.allow, None);
    let agents = settings.role_agents(&book()).unwrap();
    assert_eq!(names(&agents), ["opus-high", "qwen"]);
    assert!(agents.pm.is_some());
    settings.representative(&book()).unwrap();
    settings.triager(&book()).unwrap();
    settings.security_reviewer(&book()).unwrap();
    assert_eq!(lineup(&settings).unwrap(), ["defect-hunter-lite"]);
}

#[test]
fn an_empty_allow_is_refused_naming_the_key() {
    let err = try_project("allow = []\n").unwrap_err().to_string();
    assert!(err.contains("agents.allow"), "{err}");
    assert!(err.contains("leave it out to allow every agent"), "{err}");
}

#[test]
fn an_implementer_allow_leaves_out_is_refused_naming_both_settings() {
    let settings =
        project("allow = [\"sonnet-high\"]\nimplementers = [\"sonnet-high\", \"opus-high\"]\n");
    let err = settings.role_agents(&book()).unwrap_err().to_string();
    assert_eq!(
        err,
        "setting `agents`: `agents.implementers` names opus-high, which `agents.allow` leaves \
         out: add it to `agents.allow`, or name another"
    );

    let held =
        project("allow = [\"sonnet-high\", \"opus-high\"]\nimplementers = [\"opus-high\"]\n");
    assert_eq!(names(&held.role_agents(&book()).unwrap()), ["opus-high"]);
}

#[test]
fn a_listed_or_default_reviewer_allow_leaves_out_is_refused() {
    let listed = project("allow = [\"sonnet-high\"]\nreviewers = [\"defect-hunter\"]\n");
    assert_eq!(
        lineup(&listed).unwrap_err(),
        "setting `agents`: `agents.reviewers` names defect-hunter, which `agents.allow` leaves \
         out: add it to `agents.allow`, or name another"
    );

    let default = project("allow = [\"sonnet-high\"]\n");
    assert_eq!(
        lineup(&default).unwrap_err(),
        "setting `agents`: `agents.reviewers` names defect-hunter-lite, which `agents.allow` \
         leaves out: add it to `agents.allow`, or list other reviewers in `agents.reviewers`"
    );

    let own = allowing(
        "allow = [\"sonnet-high\", \"defect-hunter-lite\"]\n",
        "reviewers = [\"defect-hunter\"]\n",
    );
    assert_eq!(
        lineup(&own).unwrap(),
        ["defect-hunter-lite"],
        "the default is allowed"
    );
    let err = own
        .contribution_lineup(&book(), &Gateways::default(), Path::new("/h"))
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "setting `agents`: `contributions.reviewers` names defect-hunter, which `agents.allow` \
         leaves out: add it to `agents.allow`, or name another"
    );
}

#[test]
fn the_project_manager_representative_triager_and_security_reviewer_are_held_to_allow() {
    let holds = |allow: &str| {
        project(&format!(
            "allow = [{allow}]\nimplementers = [\"sonnet-high\"]\npm = \"pm\"\n"
        ))
    };
    let only = holds("\"sonnet-high\"");
    let left_out = |at: &str, name: &str, err: String| {
        let said = format!("`{at}` names {name}, which `agents.allow` leaves out");
        assert!(err.contains(&said), "{err}");
    };
    left_out(
        "agents.pm",
        "pm",
        only.role_agents(&book()).unwrap_err().to_string(),
    );
    left_out(
        "agents.representative",
        "representative",
        only.representative(&book()).unwrap_err().to_string(),
    );
    left_out(
        "triage.agent",
        "triager",
        only.triager(&book()).unwrap_err().to_string(),
    );
    left_out(
        "contributions.security_reviewer",
        "security-reviewer",
        only.security_reviewer(&book()).unwrap_err().to_string(),
    );

    let all =
        holds("\"sonnet-high\", \"pm\", \"representative\", \"triager\", \"security-reviewer\"");
    assert!(all.role_agents(&book()).unwrap().pm.is_some());
    all.representative(&book()).unwrap();
    all.triager(&book()).unwrap();
    all.security_reviewer(&book()).unwrap();
}

#[test]
fn a_name_in_allow_with_no_file_loads_and_is_refused_where_used() {
    let settings =
        project("allow = [\"sonnet-high\", \"fable\"]\nimplementers = [\"sonnet-high\"]\n");
    assert_eq!(
        names(&settings.role_agents(&book()).unwrap()),
        ["sonnet-high"]
    );

    let used = project("allow = [\"sonnet-high\", \"fable\"]\nimplementers = [\"fable\"]\n");
    let err = used.role_agents(&book()).unwrap_err().to_string();
    assert_eq!(
        err,
        "setting `agents`: `agents.implementers` names fable, which has no agent file: write \
         `agents/fable.md` in kelpie's home"
    );
}
