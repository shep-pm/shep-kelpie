use crate::test::ticket;
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::ports::CLEAN;
use crate::runner::{CHECKS_SETTLE, step};
use crate::settings::AgentHarness;
use crate::test::{Rig, Scripted, write_script};

// A server that indexes by writing a file into its cache folder, and keeps each run's
// arguments and the folder its daemon would bind in.
const INDEXER: &str = "#!/bin/sh\n\
    echo \"$@\" >> \"$CBM_CACHE_DIR/ran\"\n\
    echo \"$@\" > \"$CBM_CACHE_DIR/indexed-with\"\n\
    echo \"$CBM_RUNTIME_DIR\" > \"$CBM_CACHE_DIR/runtime\"\n\
    echo \"$CBM_ALLOWED_ROOT\" > \"$CBM_CACHE_DIR/root\"\n";

// The example's code graph table, as `settings.example.toml` writes it
const GRAPH_OFF: &str = "[app.dogs.kelpie.code_graph]\nenabled = false\nworker = false";

// The graph on for the worker's turns and the reviewers alike
fn turn_on(rig: &Rig) {
    rig.edit_settings(|s| {
        assert!(s.contains(GRAPH_OFF), "the example's code graph moved");
        s.replace(
            GRAPH_OFF,
            "[app.dogs.kelpie.code_graph]\nenabled = true\nworker = true",
        )
    });
}

// The graph on with no `worker` key, which is what `enabled` alone gives today
fn turn_on_for_reviewers(rig: &Rig) {
    rig.edit_settings(|s| {
        assert!(s.contains(GRAPH_OFF), "the example's code graph moved");
        s.replace(GRAPH_OFF, "[app.dogs.kelpie.code_graph]\nenabled = true")
    });
}

fn install(rig: &Rig, script: &str) {
    let binary = rig.paths().tools.code_graph();
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    write_script(&binary, script);
}

// The rig's pi worker and reviewer, as their agent files and the project's lists
fn pi_agents(rig: &Rig) {
    let pi = |role: &str| {
        format!(
            "---\nrole: {role}\nharness: pi\nmodel: qwen3.8:27b\neffort: low\n\
             url: http://box:11434/v1\ncontext: 65536\n---\nReview the change against \
             {{{{BASE}}}}. Output one finding per line as SEVERITY|file:line|what|why, or CLEAN.\n"
        )
    };
    rig.write_agent("pi-box", &pi("implementer"));
    rig.write_agent("pi-reviewer", &pi("reviewer"));
    rig.reviewers(&["pi-reviewer"]);
    rig.agent_board.ranks(&["pi-box", "pi-reviewer"]);
}

// The rig's goose worker. A goose reviewer cannot be named in an agent file yet (#789).
fn goose_agent(rig: &Rig) {
    let goose = "---\nrole: implementer\nharness: goose\nmodel: qwen3.8:27b\neffort: low\n\
                 url: http://box:11434/v1\ncontext: 65536\n---\n";
    rig.write_agent("goose-box", goose);
    rig.agent_board.ranks(&["goose-box"]);
}

// The rig's Codex worker and reviewer, as their agent files and the project's lists
fn codex_agents(rig: &Rig) {
    let codex = |role: &str| {
        format!(
            "---\nrole: {role}\nharness: codex\nmodel: gpt-6-sol\neffort: low\n---\n\
             Review the change against {{{{BASE}}}}. Output one finding per line as \
             SEVERITY|file:line|what|why, or CLEAN.\n"
        )
    };
    rig.write_agent("codex-box", &codex("implementer"));
    rig.write_agent("codex-reviewer", &codex("reviewer"));
    rig.reviewers(&["codex-reviewer"]);
    rig.agent_board.ranks(&["codex-box", "codex-reviewer"]);
}

// Issue 7's worker turn and a Claude reviewer's round after it.
fn work_and_review(rig: &Rig) -> Mutex<Runner> {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, qwen: clean by default
    step(&runner).unwrap(); // round 2, claude: scripted clean above
    runner
}

fn reads_code(rig: &Rig) -> Vec<crate::test::Seen> {
    (rig.claude.all_seen().into_iter())
        .filter(|s| matches!(s.call.role, Role::Worker | Role::Reviewer))
        .collect()
}

fn worker_turns(rig: &Rig) -> Vec<crate::test::Seen> {
    (rig.claude.all_seen().into_iter())
        .filter(|s| s.call.role == Role::Worker)
        .collect()
}

// Issue 7's worker's first turn on its own, with the graph's server installed
fn worker_turn(rig: &Rig) -> Mutex<Runner> {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    runner
}

fn mcp_file(seen: &crate::test::Seen) -> PathBuf {
    seen.call.settings.with_extension("mcp.json")
}

#[test]
fn a_project_with_the_graph_on_gives_its_worker_and_reviewer_the_server_and_an_index_under_kelpies_home()
 {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig, INDEXER);
    work_and_review(&rig);

    let index = rig.paths().folder.join("code-graphs/7");
    let runtime = rig.paths().folder.join("cbm/7");
    let seen = reads_code(&rig);
    let roles: Vec<Role> = seen.iter().map(|s| s.call.role).collect();
    assert!(roles.contains(&Role::Worker) && roles.contains(&Role::Reviewer));
    for seen in &seen {
        let graph = seen.call.reach.code_graph.as_ref().expect("the server");
        assert_eq!(graph.index, index);
        assert_eq!(graph.runtime, runtime);
        assert_eq!(graph.binary, rig.paths().tools.code_graph());
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(mcp_file(seen)).unwrap()).unwrap();
        let server = &config["mcpServers"]["codebase-memory-mcp"];
        assert_eq!(server["env"]["CBM_CACHE_DIR"], index.to_str().unwrap());
        assert_eq!(server["env"]["CBM_RUNTIME_DIR"], runtime.to_str().unwrap());
        assert_eq!(
            seen.settings["permissions"]["allow"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|a| *a == "mcp__codebase-memory-mcp")
                .count(),
            1,
            "{:?}",
            seen.call.role
        );
    }
    assert!(index.starts_with(&rig.paths().kelpie_home));
    let indexed = std::fs::read_to_string(index.join("indexed-with")).unwrap();
    assert!(
        indexed.starts_with("cli index_repository --repo-path "),
        "{indexed}"
    );
    let ran = std::fs::read_to_string(index.join("ran")).unwrap();
    let first: Vec<&str> = ran.lines().take(2).collect();
    assert_eq!(
        first[0], "config set ui_enabled false",
        "the web UI off first"
    );
    assert!(first[1].starts_with("cli index_repository"), "{ran}");
    let told = std::fs::read_to_string(index.join("runtime")).unwrap();
    assert_eq!(PathBuf::from(told.trim()), runtime);
    assert!(
        runtime.is_dir(),
        "made for the daemon, which does not make it"
    );
    let root = std::fs::read_to_string(index.join("root")).unwrap();
    assert_eq!(
        PathBuf::from(root.trim()),
        seen[0].call.cwd,
        "the server is held to the worktree"
    );
}

#[test]
fn a_worker_gets_no_graph_unless_the_project_names_it() {
    // `enabled = true` with no `worker` key: the worker's turn carries no server, writes no
    // MCP file of its own, allows no `mcp__` tool, and indexes nothing.
    let rig = Rig::new("shep");
    turn_on_for_reviewers(&rig);
    install(&rig, INDEXER);
    worker_turn(&rig);

    let seen = worker_turns(&rig);
    assert!(!seen.is_empty(), "the worker's turn ran");
    for seen in &seen {
        assert_eq!(seen.call.reach.code_graph, None);
        assert!(!mcp_file(seen).exists());
        assert!(
            !seen.settings.to_string().contains("mcp__"),
            "{:?}",
            seen.settings
        );
    }
    assert!(
        !rig.paths().folder.join("code-graphs").exists(),
        "nothing is indexed before a reviewer's round"
    );

    // With `worker = true`, the same turn carries the server and builds the index it does today.
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig, INDEXER);
    worker_turn(&rig);

    let index = rig.paths().folder.join("code-graphs/7");
    let seen = worker_turns(&rig);
    assert!(!seen.is_empty(), "the worker's turn ran");
    for seen in &seen {
        let graph = seen.call.reach.code_graph.as_ref().expect("the server");
        assert_eq!(graph.index, index);
        assert_eq!(graph.binary, rig.paths().tools.code_graph());
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(mcp_file(seen)).unwrap()).unwrap();
        assert_eq!(
            config["mcpServers"]["codebase-memory-mcp"]["env"]["CBM_CACHE_DIR"],
            index.to_str().unwrap()
        );
        assert!(
            seen.settings["permissions"]["allow"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| *a == "mcp__codebase-memory-mcp"),
            "{:?}",
            seen.settings
        );
    }
    let indexed = std::fs::read_to_string(index.join("indexed-with")).unwrap();
    assert!(
        indexed.starts_with("cli index_repository --repo-path "),
        "{indexed}"
    );
}

#[test]
fn a_worker_without_the_graph_still_gets_the_repo_s_own_servers() {
    // The graph is its own path through the call's reach, so a worker without it keeps what
    // `repo_setup` gives it.
    let rig = Rig::new("shep");
    turn_on_for_reviewers(&rig);
    install(&rig, INDEXER);
    let own =
        json!({"mcpServers": {"docs": {"type": "http", "url": "https://docs.example.com/mcp"}}});
    rig.land(".mcp.json", &own.to_string());
    worker_turn(&rig);

    let seen = worker_turns(&rig);
    assert_eq!(seen[0].call.reach.code_graph, None);
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(mcp_file(&seen[0])).unwrap()).unwrap();
    let given: Vec<String> = (config["mcpServers"].as_object().unwrap())
        .keys()
        .cloned()
        .collect();
    assert_eq!(given, vec!["docs".to_owned()]);
    assert!(
        seen[0].settings["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| *a == "mcp__docs"),
        "{:?}",
        seen[0].settings
    );
}

#[test]
fn a_reviewer_still_gets_the_graph_when_the_worker_does_not() {
    let rig = Rig::new("shep");
    turn_on_for_reviewers(&rig);
    install(&rig, INDEXER);
    work_and_review(&rig);

    let index = rig.paths().folder.join("code-graphs/7");
    let seen = reads_code(&rig);
    let roles: Vec<Role> = seen.iter().map(|s| s.call.role).collect();
    assert!(
        roles.contains(&Role::Worker) && roles.contains(&Role::Reviewer),
        "both calls ran: {roles:?}"
    );
    for seen in &seen {
        if seen.call.role == Role::Worker {
            assert_eq!(
                seen.call.reach.code_graph, None,
                "the worker's turn is not given it"
            );
            continue;
        }
        let graph = seen.call.reach.code_graph.as_ref().expect("the server");
        assert_eq!(graph.index, index);
        assert_eq!(graph.binary, rig.paths().tools.code_graph());
    }
    let indexed = std::fs::read_to_string(index.join("indexed-with")).unwrap();
    assert!(
        indexed.starts_with("cli index_repository --repo-path "),
        "the reviewer's round built the index: {indexed}"
    );
}

#[test]
fn a_pi_worker_and_reviewer_get_the_server_and_an_index() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig, INDEXER);
    pi_agents(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, the pi reviewer

    let index = rig.paths().folder.join("code-graphs/7");
    let seen = reads_code(&rig);
    let roles: Vec<Role> = seen.iter().map(|s| s.call.role).collect();
    assert!(roles.contains(&Role::Worker) && roles.contains(&Role::Reviewer));
    for seen in &seen {
        assert!(matches!(seen.call.harness, AgentHarness::Pi(_)));
        let graph = seen.call.reach.code_graph.as_ref().expect("the server");
        assert_eq!(graph.index, index);
        assert_eq!(graph.binary, rig.paths().tools.code_graph());
    }
    let root = std::fs::read_to_string(index.join("root")).unwrap();
    assert_eq!(PathBuf::from(root.trim()), seen[0].call.cwd);
}

#[test]
fn a_pi_worker_gets_no_graph_when_the_project_does_not_name_it() {
    let rig = Rig::new("shep");
    turn_on_for_reviewers(&rig);
    install(&rig, INDEXER);
    pi_agents(&rig);
    worker_turn(&rig);

    assert!(
        worker_turns(&rig)
            .iter()
            .all(|s| s.call.reach.code_graph.is_none()),
        "the pi worker carries no server"
    );
}

#[test]
fn a_goose_worker_gets_no_graph_when_the_project_does_not_name_it() {
    let rig = Rig::new("shep");
    turn_on_for_reviewers(&rig);
    install(&rig, INDEXER);
    goose_agent(&rig);
    worker_turn(&rig);

    assert!(
        worker_turns(&rig)
            .iter()
            .all(|s| s.call.reach.code_graph.is_none()),
        "the goose worker carries no server"
    );
}

#[test]
fn a_codex_worker_gets_no_graph_when_the_project_does_not_name_it() {
    let rig = Rig::new("shep");
    turn_on_for_reviewers(&rig);
    install(&rig, INDEXER);
    codex_agents(&rig);
    worker_turn(&rig);

    assert!(
        worker_turns(&rig)
            .iter()
            .all(|s| s.call.reach.code_graph.is_none()),
        "the Codex worker carries no server"
    );
}

#[test]
fn a_goose_worker_and_reviewer_get_the_server_and_an_index() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig, INDEXER);
    // A goose reviewer cannot be named in an agent file yet (#789), so the worker is run
    // and the reviewer's call is asked about directly.
    goose_agent(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn

    let index = rig.paths().folder.join("code-graphs/7");
    let seen: Vec<_> = (rig.claude.all_seen().into_iter())
        .filter(|s| s.call.role == Role::Worker)
        .collect();
    assert!(!seen.is_empty());
    for seen in &seen {
        assert!(matches!(seen.call.harness, AgentHarness::Goose(_)));
        let graph = seen.call.reach.code_graph.as_ref().expect("the server");
        assert_eq!(graph.index, index);
        assert_eq!(graph.binary, rig.paths().tools.code_graph());
    }
    let root = std::fs::read_to_string(index.join("root")).unwrap();
    assert_eq!(PathBuf::from(root.trim()), seen[0].call.cwd);

    let server = crate::settings::ModelServer {
        host: crate::settings::ModelHost::Url("http://box:11434/v1".to_owned().try_into().unwrap()),
        context: 65_536_i64.try_into().unwrap(),
    };
    let call = |role, tools| {
        AgentCall::new(
            role,
            tools,
            AgentHarness::Goose(server.clone()),
            "qwen3.8:27b",
            crate::settings::Effort::Low,
            crate::ports::Session::New(crate::work_item::new_session_id().unwrap()),
            "/wt".into(),
            "go",
        )
    };
    assert!(super::reads_code(
        &call(Role::Reviewer, Tools::Review),
        false
    ));
    assert!(super::reads_code(
        &call(Role::Triager, Tools::Review),
        false
    ));
    assert!(super::reads_code(&call(Role::Worker, Tools::Work), true));
    assert!(!super::reads_code(&call(Role::Worker, Tools::Work), false));
    assert!(!super::reads_code(&call(Role::Worker, Tools::Retro), true));
    assert!(!super::reads_code(
        &call(Role::Representative, Tools::Answer),
        true
    ));
}

#[test]
fn a_codex_worker_and_reviewer_get_the_server_and_an_index() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig, INDEXER);
    codex_agents(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, the Codex reviewer

    let index = rig.paths().folder.join("code-graphs/7");
    let seen = reads_code(&rig);
    let roles: Vec<Role> = seen.iter().map(|s| s.call.role).collect();
    assert!(roles.contains(&Role::Worker) && roles.contains(&Role::Reviewer));
    for seen in &seen {
        assert_eq!(seen.call.harness, AgentHarness::Codex);
        let graph = seen.call.reach.code_graph.as_ref().expect("the server");
        assert_eq!(graph.index, index);
        assert_eq!(graph.binary, rig.paths().tools.code_graph());
    }
    let root = std::fs::read_to_string(index.join("root")).unwrap();
    assert_eq!(PathBuf::from(root.trim()), seen[0].call.cwd);
}

#[test]
fn a_project_with_the_graph_off_gets_no_server_and_no_index() {
    let rig = Rig::new("shep");
    install(&rig, INDEXER);
    work_and_review(&rig);

    let seen = reads_code(&rig);
    assert!(seen.len() >= 2);
    for seen in &seen {
        assert_eq!(seen.call.reach.code_graph, None);
        assert!(!mcp_file(seen).exists());
        assert!(!seen.settings.to_string().contains("mcp__"));
    }
    assert!(!rig.paths().folder.join("code-graphs").exists());
}

#[test]
fn a_server_that_is_not_installed_is_a_logged_skip_and_the_calls_run() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    let runner = work_and_review(&rig);

    let seen = reads_code(&rig);
    assert!(seen.len() >= 2, "the turns ran");
    assert!(seen.iter().all(|s| s.call.reach.code_graph.is_none()));
    let notes = runner.lock().unwrap().take_notes();
    let skip = notes
        .iter()
        .find(|n| n.contains("runs without a code graph"))
        .expect("a note");
    assert!(skip.contains("tools install"), "{skip}");
}

#[test]
fn a_server_that_is_not_installed_is_said_once_across_calls_and_status_shows_it() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    let runner = work_and_review(&rig);

    assert!(reads_code(&rig).len() >= 2, "several calls met it");
    let notes = runner.lock().unwrap().take_notes();
    let said: Vec<_> = (notes.iter())
        .filter(|n| n.contains("runs without a code graph"))
        .collect();
    assert_eq!(said.len(), 1, "{notes:#?}");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["standing"], json!([said[0]]));
}

#[test]
fn an_indexer_that_fails_is_a_logged_skip_naming_why() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(
        &rig,
        "#!/bin/sh\necho 'no room for the index' >&2\nexit 3\n",
    );
    let runner = work_and_review(&rig);

    assert!(
        reads_code(&rig)
            .iter()
            .all(|s| s.call.reach.code_graph.is_none())
    );
    let notes = runner.lock().unwrap().take_notes();
    let skip = notes
        .iter()
        .find(|n| n.contains("runs without a code graph"))
        .expect("a note");
    assert!(skip.contains("no room for the index"), "{skip}");
}

#[test]
fn the_index_goes_when_the_work_item_finishes_and_its_worktree_goes() {
    let (rig, runner, _) = Rig::parked_set("shep", |rig| {
        turn_on(rig);
        install(rig, INDEXER);
    });
    let index = rig.paths().folder.join("code-graphs/7");
    let runtime = rig.paths().folder.join("cbm/7");
    assert!(
        index.join("indexed-with").is_file(),
        "the worker's turn indexed"
    );
    assert!(runtime.is_dir());

    rig.ask(&runner, "rule", Some("71 yes"));
    step(&runner).unwrap(); // marked ready
    rig.clock.advance(CHECKS_SETTLE);
    let finished = step(&runner).unwrap();
    assert!(matches!(
       finished,
       Some(crate::runner::StepReport::Finished { issue, .. }) if issue == ticket(7)
    ));
    assert!(!index.exists());
    assert!(!runtime.exists(), "the daemon's folder goes with it");
}

#[test]
fn a_call_ended_while_its_index_builds_kills_the_indexer_and_runs_without_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("codebase-memory-mcp");
    write_script(&binary, "#!/bin/sh\ncase \"$1\" in cli) sleep 600;; esac\n");
    let graph = CodeGraph {
        binary,
        index: dir.path().join("index"),
        runtime: dir.path().join("cbm"),
    };
    let ending = Ending::default();
    ending.end();
    let started = Instant::now();
    let err = index(&graph, dir.path(), &ending).unwrap_err();
    assert!(err.contains("the call was ended while"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "it waited the sleep out"
    );
}

#[cfg(unix)]
#[test]
fn an_index_that_fails_under_a_daemon_folder_too_long_for_its_socket_says_so() {
    let dir = crate::test::short_tempdir();
    let binary = dir.path().join("codebase-memory-mcp");
    let failing =
        "#!/bin/sh\ncase \"$1\" in cli) echo 'endpoint could not be created' >&2; exit 1;; esac\n";
    write_script(&binary, failing);
    let graph = |runtime: &str| CodeGraph {
        binary: binary.clone(),
        index: dir.path().join("index"),
        runtime: dir.path().join(runtime),
    };
    let long = "x".repeat(80);
    let err = index(&graph(&long), dir.path(), &Ending::default()).unwrap_err();
    assert!(err.contains("endpoint could not be created"), "{err}");
    assert!(err.contains(&long) && err.contains("socket"), "{err}");

    let err = index(&graph("cbm"), dir.path(), &Ending::default()).unwrap_err();
    assert!(
        !err.contains("socket"),
        "a path that fits is not blamed: {err}"
    );
}

// Refuses a server by its name, and drops the env key `TOKEN` of a goose server, whatever the
// entry says.
#[derive(Debug)]
struct Stood;

impl crate::ports::HarnessMcp for Stood {
    fn refusal(&self, harness: &AgentHarness, name: &str, _: &serde_json::Value) -> Option<String> {
        (name == "old" && !matches!(harness, AgentHarness::ClaudeCode))
            .then(|| "the stand-in cannot load it".to_owned())
    }

    fn skipped_env(
        &self,
        harness: &AgentHarness,
        name: &str,
        _: &serde_json::Value,
    ) -> Option<String> {
        matches!(harness, AgentHarness::Goose(_)).then(|| format!("the key `TOKEN` of {name}"))
    }
}

fn model_server() -> crate::settings::ModelServer {
    crate::settings::ModelServer {
        host: crate::settings::ModelHost::Url("http://box:11434/v1".to_owned().try_into().unwrap()),
        context: 65_536_i64.try_into().unwrap(),
    }
}

fn call_on(harness: AgentHarness) -> AgentCall {
    let mut call = AgentCall::new(
        Role::Reviewer,
        Tools::Review,
        harness,
        "qwen3.8:27b",
        crate::settings::Effort::Low,
        crate::ports::Session::New(crate::work_item::new_session_id().unwrap()),
        "/wt".into(),
        "go",
    );
    for name in ["docs", "old"] {
        call.reach
            .mcp_servers
            .insert(name.to_owned(), json!({ "command": "/bin/true" }));
    }
    call
}

#[test]
fn a_server_pi_cannot_load_is_left_out_with_a_note() {
    let rig = Rig::new("shep");
    rig.harness_mcp_is(std::sync::Arc::new(Stood));
    let runner = rig.open().unwrap();
    let mut call = call_on(AgentHarness::Pi(model_server()));

    runner
        .lock()
        .unwrap()
        .without_servers_the_harness_cannot_load(&mut call);

    let kept: Vec<_> = call.reach.mcp_servers.keys().collect();
    assert_eq!(kept, ["docs"]);
    let notes = runner.lock().unwrap().take_notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("runs without the MCP server `old`: the stand-in cannot load it"),
        "{notes:?}"
    );
}

#[test]
fn a_goose_server_that_loads_without_a_key_is_kept_with_a_note_saying_which() {
    let rig = Rig::new("shep");
    rig.harness_mcp_is(std::sync::Arc::new(Stood));
    let runner = rig.open().unwrap();
    let mut call = call_on(AgentHarness::Goose(model_server()));

    runner
        .lock()
        .unwrap()
        .without_servers_the_harness_cannot_load(&mut call);

    assert_eq!(call.reach.mcp_servers.len(), 1);
    let notes = runner.lock().unwrap().take_notes().join("\n");
    assert!(
        notes.contains("runs the MCP server `docs` without the key `TOKEN` of docs"),
        "{notes}"
    );
}

#[test]
fn a_harness_the_port_has_nothing_to_say_of_keeps_every_server_without_a_note() {
    let rig = Rig::new("shep");
    rig.harness_mcp_is(std::sync::Arc::new(Stood));
    let runner = rig.open().unwrap();
    let mut call = call_on(AgentHarness::ClaudeCode);

    runner
        .lock()
        .unwrap()
        .without_servers_the_harness_cannot_load(&mut call);

    assert_eq!(call.reach.mcp_servers.len(), 2);
    assert_eq!(runner.lock().unwrap().take_notes(), Vec::<String>::new());
}
