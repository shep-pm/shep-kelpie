use serde_json::{Value, json};

use super::*;
use crate::test::{Rig, Scripted, git, write_script};

const BODY: &str = "Build the thing.\n\n## Acceptance criteria\n\n- [ ] It works\n";

const REQUEST: &str = "Let the maintainer pause one project from lookout";

/// A rig whose project lists two implementers, and the issue writer it runs
struct Setup {
    rig: Rig,
    settings: Settings,
    paths: ProjectPaths,
    listed: RoleAgents,
    writer: Writer,
}

impl Setup {
    fn new() -> Self {
        let rig = Rig::new("acme");
        rig.implementers(&["sonnet-high", "opus-high"]);
        let settings = rig.settings();
        let paths = rig.paths();
        let agents = Agents::load(&paths.agents).unwrap();
        let listed = settings.role_agents(&agents).unwrap();
        let writer = Writer::of(&agents).unwrap();
        Self {
            rig,
            settings,
            paths,
            listed,
            writer,
        }
    }

    fn project(&self) -> Project<'_> {
        Project {
            settings: &self.settings,
            remote: self
                .settings
                .git
                .remote
                .as_ref()
                .expect("the example sets it"),
            paths: &self.paths,
            agents: &self.listed,
            kelpie: Path::new(Rig::KELPIE),
        }
    }

    // Runs the issue writer on its own, its session answering `reply`.
    fn headless(&self, reply: &'static str) -> Result<Vec<String>, String> {
        self.rig.claude.script([Scripted::Text(reply)]);
        let forge = &self.rig.forge;
        headless(
            &self.project(),
            &self.writer,
            REQUEST,
            forge,
            &self.rig.claude,
        )
    }

    // Issue `number` on the forge, as the issue writer would have filed it.
    fn filed(&self, number: u64, title: &str, body: &str, labels: &[&str]) {
        self.rig.forge.open_issue(number, title, body);
        for label in labels {
            self.rig.forge.label(number, label);
        }
    }
}

// The guard command in a Claude Code settings file's hooks.
fn guard_hook(settings: &Value) -> String {
    let hooks = settings["hooks"]["PreToolUse"].as_array().unwrap();
    let guard = hooks
        .iter()
        .find(|h| h["matcher"] == "Bash|Agent|Task")
        .unwrap();
    guard["hooks"][0]["command"].as_str().unwrap().to_owned()
}

#[test]
fn a_request_runs_one_session_that_files_and_kelpie_prints_what_it_filed() {
    let s = Setup::new();
    s.rig.forge.set_repo_labels(&[HUMAN]);
    s.filed(41, "Pause a project", BODY, &[HUMAN, "agent:sonnet-high"]);
    s.filed(42, "Its lookout button", BODY, &[HUMAN, "agent:opus-high"]);
    s.rig.forge.link_sub_issue(41, 42);

    let lines = s.headless("Filed both.\n{\"filed\": [41, 42], \"note\": \"Split in two.\"}");

    assert_eq!(
        lines.unwrap(),
        [
            "the issue writer filed 2 on shep-pm/shep, `ready-for-human` for you to read, then \
             label `ready-for-agent`:",
            "#41 Pause a project (agent:sonnet-high)",
            "#42 Its lookout button (agent:opus-high, sub-issue of #41)",
            "note: Split in two.",
        ]
    );
    // The repo lacked the agents' labels, so they were made before the session.
    assert_eq!(
        s.rig.forge.repo_labels_now(),
        [HUMAN, "agent:sonnet-high", "agent:opus-high"]
    );
    let seen = s.rig.claude.all_seen();
    let [seen] = seen.as_slice() else {
        panic!("one session, not {}", seen.len())
    };
    let call = &seen.call;
    assert_eq!(call.role, ports::Role::IssueWriter);
    assert_eq!(
        (call.model.as_str(), call.effort.as_str()),
        ("claude-opus-5-5", "medium")
    );
    assert!(matches!(call.session, Session::New(_)), "a fresh session");
    assert!(call.prompt.contains(REQUEST), "{}", call.prompt);
    assert!(
        call.cwd.starts_with(s.paths.issues()),
        "{}",
        call.cwd.display()
    );
    assert!(
        !call.cwd.exists(),
        "its checkout is removed once its reply is read"
    );
    // Read-only on the repo: it writes nothing, and its commands are its guard's.
    assert_eq!(seen.sandbox["filesystem"]["allowWrite"], json!([]));
    assert_eq!(
        seen.sandbox["network"]["allowedDomains"],
        json!(["github.com", "api.github.com"])
    );
    let deny = seen.settings["permissions"]["deny"].to_string();
    for tool in ["Edit", "Write", "Agent", "WebFetch"] {
        assert!(deny.contains(&format!("\"{tool}\"")), "{tool}: {deny}");
    }
    // On its own no one answers a prompt, so the guard alone holds its commands.
    assert_eq!(seen.settings["permissions"]["allow"], json!(["Bash"]));
    let scratch = call.settings.with_extension("tmp");
    let ledger = scratch.join("ledger.json");
    assert_guarded(&seen.settings, HUMAN, &ledger);
    assert_file_tools_read_only(&seen.settings, &call.cwd, &s.rig);
    // Its sandbox reads none of the secrets, the history or kelpie's homes,
    // and inside those only its checkout and the guard's binary.
    let files = &seen.sandbox["filesystem"];
    let denied = files["denyRead"].to_string();
    for path in ["~/.ssh/**", "~/.zsh_history", "~/.bash_history"] {
        assert!(denied.contains(&format!("\"{path}\"")), "{path}: {denied}");
    }
    let kelpie_home = format!("\"{}/**\"", s.paths.kelpie_home.display());
    assert!(
        denied.contains(&kelpie_home)
            || denied.contains(&format!("\"{}/**\"", s.paths.shep_home.display()))
    );
    assert_eq!(
        files["allowRead"],
        json!([call.cwd, Rig::KELPIE]),
        "the checkout and the guard alone"
    );
    // The run is in the project's usage ledger.
    let lines = usage::read(&s.paths.folder.join(usage::FILE)).unwrap();
    let [usage::Line::Call(line)] = lines.as_slice() else {
        panic!("one call line, not {lines:?}")
    };
    let line = serde_json::to_value(line).unwrap();
    assert_eq!(
        (
            &line["role"],
            &line["kind"],
            &line["agent"],
            &line["harness"]
        ),
        (
            &json!("issue-writer"),
            &json!("issues"),
            &json!("issue-writer"),
            &json!("claude-code")
        )
    );
    assert_eq!(
        (
            &line["model"],
            &line["effort"],
            &line["ended"],
            &line["issue"]
        ),
        (
            &json!("claude-opus-5-5"),
            &json!("medium"),
            &json!("answered"),
            &json!(null)
        )
    );
    assert_eq!(line["session"], json!(call.session.id().0));
}

// The guard on every command, and the recorder of its ledger after each.
fn assert_guarded(settings: &Value, status: &str, ledger: &Path) {
    let guard = guard_hook(settings);
    let flags = [
        "guard".to_owned(),
        format!("--issues={status}"),
        "--agent=sonnet-high".to_owned(),
        "--agent=opus-high".to_owned(),
        format!("--ledger={}", ledger.display()),
    ];
    for flag in &flags {
        assert!(guard.contains(flag.as_str()), "{flag}: {guard}");
    }
    let post = settings["hooks"]["PostToolUse"].as_array().unwrap();
    let record = post.iter().find(|h| h["matcher"] == "Bash").unwrap();
    let record = record["hooks"][0]["command"].as_str().unwrap();
    assert_eq!(record, format!("{guard} --record"));
}

// Its file tools read the checkout, and no secret, history or folder beside it.
fn assert_file_tools_read_only(settings: &Value, checkout: &Path, rig: &Rig) {
    let deny: Vec<&str> = (settings["permissions"]["deny"].as_array().unwrap().iter())
        .filter_map(Value::as_str)
        .collect();
    for rule in [
        "Read(~/.config/gh/**)",
        "Read(~/.claude.json)",
        "Read(~/.claude/**)",
        "Read(~/.zsh_history)",
        "Read(~/.bash_history)",
    ] {
        assert!(deny.contains(&rule), "{rule}");
    }
    let rig_home = rig.home.path().canonicalize().unwrap();
    let beside = format!("Read(/{}/origin.git/**)", rig_home.display());
    assert!(deny.contains(&beside.as_str()), "{beside}");
    let checkout = checkout
        .canonicalize()
        .unwrap_or_else(|_| checkout.to_owned());
    let held = deny
        .iter()
        .filter_map(|r| r.strip_prefix("Read(/"))
        .find(|r| {
            let path = Path::new(r.trim_end_matches(')').trim_end_matches("/**"));
            checkout.starts_with(path)
        });
    assert_eq!(held, None, "the checkout stays readable");
}

#[test]
fn an_issue_without_criteria_or_one_listed_agent_is_named_with_what_it_lacks() {
    let s = Setup::new();
    s.filed(41, "Pause a project", BODY, &[HUMAN, "agent:sonnet-high"]);
    s.filed(43, "Untidy", "Do it.\n", &[HUMAN]);
    s.filed(44, "Unlisted", BODY, &[HUMAN, "agent:haiku-low"]);
    // The board reads the prefix in any case, so this is a second agent.
    s.filed(
        46,
        "Twice",
        BODY,
        &[HUMAN, "agent:opus-high", "Agent:sonnet-high"],
    );
    s.rig.forge.remove_issue(45);

    let err = s.headless("{\"filed\": [41, 43, 44, 45, 46]}").unwrap_err();

    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines[1], "#41 Pause a project (agent:sonnet-high)");
    assert_eq!(
        lines[2],
        "#43 Untidy: no `agent:` label; no acceptance criteria"
    );
    assert!(
        lines[3].starts_with("#44 Unlisted: label `agent:haiku-low` names no agent the project"),
        "{}",
        lines[3]
    );
    assert_eq!(
        lines[4],
        "#45: cannot read it back: gh failed: no issue #45"
    );
    assert_eq!(
        lines[5],
        "#46 Twice: the issue has more than one `agent:` label"
    );
    assert_eq!(lines.len(), 6, "{err}");
}

#[test]
fn a_reply_that_is_not_the_list_keeps_the_checkout_and_names_the_session() {
    let s = Setup::new();
    let main = s.rig.land_on_origin("landed.txt");

    let err = s.headless("I filed two issues for this.").unwrap_err();

    let seen = s.rig.claude.all_seen();
    let call = &seen[0].call;
    let id = &call.session.id().0;
    assert!(err.contains("ended without the list of issues"), "{err}");
    assert!(err.contains("I filed two issues for this."), "{err}");
    let resume = format!("`cd {} && claude --resume {id}`", call.cwd.display());
    assert!(err.contains(&resume), "{err}\nwanted: {resume}");
    // The checkout it read is `main` as just fetched, detached.
    assert_eq!(git(&call.cwd, &["rev-parse", "HEAD"]).trim(), main);
    let branch = std::process::Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .current_dir(&call.cwd)
        .status()
        .unwrap();
    assert!(!branch.success(), "on no branch");
}

#[test]
fn the_prompt_lists_the_implementers_default_first_and_how_each_mode_ends() {
    let s = Setup::new();
    let headless = instructions(&s.writer, &s.project(), Mode::Headless);
    assert!(
        headless.starts_with(s.writer.prompt.trim_end()),
        "its file's body first"
    );
    let listed = "- `sonnet-high`: claude-sonnet-5-5 at high effort, the default\n\
                  - `opus-high`: claude-opus-5-5 at high effort\n";
    assert!(headless.contains(listed), "{headless}");
    assert!(headless.contains("--label 'ready-for-human'"), "{headless}");
    assert!(headless.contains("{\"filed\": ["), "{headless}");
    assert!(
        !headless.contains("`git"),
        "the guard runs no git: {headless}"
    );
    let interactive = instructions(&s.writer, &s.project(), Mode::Interactive);
    assert!(
        interactive.contains("--label 'ready-for-agent'"),
        "{interactive}"
    );
    assert!(
        !interactive.contains("{\"filed\""),
        "it talks to the maintainer"
    );
    for prompt in [&headless, &interactive] {
        for word in ["cost", "price", "cheap", "expensive", "budget"] {
            assert!(!prompt.to_lowercase().contains(word), "{word}");
        }
    }
}

#[test]
fn interactive_starts_claude_in_the_checkout_with_the_prompt_appended_and_the_guard() {
    let s = Setup::new();
    let dir = tempfile::tempdir().unwrap();
    let (program, said) = (dir.path().join("claude"), dir.path().join("argv"));
    let record = format!(
        "#!/bin/sh\nprintf '%s\\0' \"$PWD\" \"$@\" > '{}'\n",
        said.display()
    );
    write_script(&program, &record);

    let mut claude = interactive(&s.project(), &s.writer, REQUEST, program.as_os_str()).unwrap();
    assert!(claude.command.status().unwrap().success());

    let said = std::fs::read_to_string(&said).unwrap();
    let words: Vec<&str> = said.trim_end_matches('\0').split('\0').collect();
    let repo = s.settings.git.checkout.canonicalize().unwrap();
    assert_eq!(Path::new(words[0]).canonicalize().unwrap(), repo);
    let [settings, ledger] = claude.files.as_slice() else {
        panic!("its settings and its ledger, not {:?}", claude.files)
    };
    assert!(
        settings.starts_with(s.paths.issues()),
        "{}",
        settings.display()
    );
    let prompt = instructions(&s.writer, &s.project(), Mode::Interactive);
    let settings_arg = settings.display().to_string();
    assert_eq!(
        &words[1..],
        [
            "--model",
            "claude-opus-5-5",
            "--effort",
            "medium",
            "--settings",
            settings_arg.as_str(),
            "--append-system-prompt",
            prompt.as_str(),
            REQUEST,
        ]
    );
    let written: Value = serde_json::from_str(&std::fs::read_to_string(settings).unwrap()).unwrap();
    assert!(
        written.get("sandbox").is_none(),
        "the maintainer's own stays"
    );
    // The maintainer is asked before each command the guard lets through.
    assert!(written["permissions"].get("allow").is_none(), "{written}");
    assert_guarded(&written, READY, ledger);
    assert_file_tools_read_only(&written, &s.settings.git.checkout, &s.rig);
    assert!(
        s.rig.claude.all_seen().is_empty(),
        "no session of kelpie's own"
    );
}

#[test]
fn a_reply_is_read_from_its_first_brace_to_its_last() {
    let fenced = "Done.\n```json\n{\"filed\": [7], \"note\": \"\"}\n```";
    assert_eq!(
        read_reply(fenced),
        Some(Filed {
            filed: vec![7],
            note: String::new()
        })
    );
    for not in ["Done.", "{\"issues\": [7]}", "{\"filed\": \"7\"}"] {
        assert_eq!(read_reply(not), None, "{not}");
    }
}
