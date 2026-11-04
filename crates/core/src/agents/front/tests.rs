use super::*;

const PI: &str = "role: implementer\nharness: pi\nmodel: qwen3.8:27b\neffort: low\n\
                  url: http://box:11434/v1\ncontext: 65536\n";
const ENDPOINT: &str = "role: reviewer\nharness: endpoint\nurl: http://box:11434/v1\n\
                        model: coder\ncontext: 32768\n";
const COMMAND: &str = "role: reviewer\nharness: command\ncommand: ~/bin/review\n";

fn file(keys: &str) -> String {
    format!("---\n{keys}---\n")
}

#[test]
fn an_agent_file_that_names_lease_gpu_is_refused_with_the_gateway_to_use() {
    let pi_reviewer = PI.replace("implementer", "reviewer");
    for (what, keys) in [
        ("an implementer", PI),
        ("a pi reviewer", pi_reviewer.as_str()),
        ("an endpoint reviewer", ENDPOINT),
        ("a command reviewer", COMMAND),
    ] {
        let text = if what == "a pi reviewer" {
            format!("---\n{keys}lease: gpu\n---\nLook.\n")
        } else {
            file(&format!("{keys}lease: gpu\n"))
        };
        let refused = parse(&text).unwrap_err();
        assert!(refused.contains("`lease`"), "{what}: {refused}");
        assert!(refused.contains("`gateway`"), "{what}: {refused}");
    }
}

#[test]
fn any_other_lease_name_is_refused_the_same_way() {
    let refused = parse(&file(&format!("{PI}lease: gpu-box\n"))).unwrap_err();
    assert!(refused.contains("`gateway`"), "{refused}");
}

#[test]
fn a_model_server_reached_by_url_is_held_back_by_nothing_of_kelpies() {
    let agent = parse(&file(PI)).unwrap();
    assert_eq!(agent.runs.session().unwrap().1, &Limit::Unqueued);
}

const CLAUDE: &str =
    "role: implementer\nharness: claude-code\nmodel: claude-sonnet-5\neffort: medium\n";
const CODEX: &str = "role: implementer\nharness: codex\nmodel: gpt-5\neffort: low\n";
const GOOSE: &str = "role: implementer\nharness: goose\nmodel: qwen3.8:27b\neffort: low\n\
                     url: http://box:11434/v1\ncontext: 65536\n";
const BOT: &str = "role: reviewer\nharness: bot\nbot: coderabbit\nreviews: 3\nhours: 1\n";

#[test]
fn login_api_on_a_claude_code_implementer_or_reviewer_reads_as_billed_to_the_api() {
    let reviewer = CLAUDE.replace("implementer", "reviewer");
    for (what, keys) in [
        ("an implementer", CLAUDE),
        ("a reviewer", reviewer.as_str()),
    ] {
        let read = |login: &str| {
            let agent = parse(&format!("---\n{keys}{login}---\nLook.\n")).unwrap();
            let (model, limit) = agent.runs.session().unwrap();
            (model.harness.clone(), limit.clone())
        };
        let (harness, limit) = read("login: api\n");
        assert_eq!(harness, AgentHarness::ClaudeApi, "{what}");
        assert_eq!(limit, Limit::Api, "{what}");
        for plan in ["", "login: plan\n"] {
            let (harness, limit) = read(plan);
            assert_eq!(harness, AgentHarness::ClaudeCode, "{what}: {plan:?}");
            assert_eq!(limit, Limit::Account(Account::Claude), "{what}: {plan:?}");
        }
    }
}

#[test]
fn an_api_agents_limit_is_api_and_a_usage_key_beside_it_is_refused_with_its_line() {
    let reviewer = CLAUDE.replace("implementer", "reviewer");
    for (what, keys) in [
        ("an implementer", CLAUDE),
        ("a reviewer", reviewer.as_str()),
    ] {
        let agent = parse(&format!("---\n{keys}login: api\n---\nLook.\n")).unwrap();
        assert_eq!(agent.runs.session().unwrap().1, &Limit::Api, "{what}");
        for usage in ["claude", "codex", "none"] {
            let refused = parse(&format!(
                "---\n{keys}login: api\nusage: {usage}\n---\nLook.\n"
            ))
            .unwrap_err();
            assert!(
                refused.starts_with("`usage`: "),
                "{what} {usage}: {refused}"
            );
            assert!(
                refused.contains("reads no account"),
                "{what} {usage}: {refused}"
            );
            assert!(refused.ends_with("at line 7"), "{what} {usage}: {refused}");
        }
    }
}

#[test]
fn login_is_refused_with_its_line_on_a_pm_a_command_reviewer_and_a_harness_with_no_api_login() {
    let role = |name: &str| CLAUDE.replace("implementer", name);
    // A role that takes no `login` refuses it as it refuses any key.
    for name in ["pm", "representative", "triager"] {
        let refused = parse(&format!("---\n{}login: api\n---\nLook.\n", role(name))).unwrap_err();
        assert!(
            refused.contains("unknown field `login`"),
            "{name}: {refused}"
        );
    }
    // A reviewer that runs on its own spends no login.
    for (what, keys, line) in [
        ("command", COMMAND, 5),
        ("endpoint", ENDPOINT, 7),
        ("bot", BOT, 7),
    ] {
        for login in ["api", "plan"] {
            let refused = parse(&file(&format!("{keys}login: {login}\n"))).unwrap_err();
            assert!(refused.contains("`login`"), "{what}: {refused}");
            assert!(refused.contains(what), "{what}: {refused}");
            assert!(
                refused.ends_with(&format!("at line {line}")),
                "{what}: {refused}"
            );
        }
    }
    // A harness with no API login yet names itself.
    for (name, keys, line) in [("pi", PI, 8), ("codex", CODEX, 6), ("goose", GOOSE, 8)] {
        let refused = parse(&file(&format!("{keys}login: api\n"))).unwrap_err();
        assert!(refused.contains("`login`"), "{name}: {refused}");
        assert!(
            refused.contains(&format!("runs on {name}")),
            "{name}: {refused}"
        );
        assert!(
            refused.ends_with(&format!("at line {line}")),
            "{name}: {refused}"
        );
        // `plan` is what they read already.
        parse(&file(&format!("{keys}login: plan\n"))).unwrap();
    }
}

#[test]
fn a_login_other_than_plan_or_api_is_refused_with_its_line() {
    let refused = parse(&file(&format!("{CLAUDE}login: key\n"))).unwrap_err();
    assert!(refused.starts_with("`login`: "), "{refused}");
    assert!(refused.contains("`key`"), "{refused}");
    assert!(refused.contains("plan, api"), "{refused}");
    assert!(refused.contains("at line 6"), "{refused}");
}
