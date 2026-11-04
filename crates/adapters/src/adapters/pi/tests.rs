use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;
use crate::adapters::harness::ERROR_TAIL;
use crate::ports::{Browser, CodeGraph, Role};
use crate::profile::WorkerProfile;
use crate::settings::{ContextSize, Effort, EndpointUrl, ModelHost};
use crate::test::{NO_PROGRAM, OpenSandbox};

// Recorded from pi 1.1.0 on qwen3.8:27b through the paddock, by the ignored test
// below, with the temporary folder renamed: a new session asked to say ok...
pub(super) const FRESH: &str = include_str!("../../../../../fixtures/pi-p-fresh.jsonl");
// ...the same session resumed and asked what it said...
const RESUMED: &str = include_str!("../../../../../fixtures/pi-p-resumed.jsonl");
// ...and a worker refused a write by `kelpie confine`, a command by the
// sandbox and another by `kelpie guard`, its streaming deltas left out.
const REFUSED: &str = include_str!("../../../../../fixtures/pi-p-refused.jsonl");

pub(super) const FRESH_ID: &str = "0b6f3c1e-7d2a-4f4e-9a51-3c8d2e6f1a07";
pub(super) const WORKER_ID: &str = "5a2e9d40-1c7b-4b38-8e6f-2d4a9c1b7e53";
pub(super) const URL: &str = "http://192.0.2.9:11434/v1";

pub(super) fn output(code: i32, stdout: &str, stderr: &str) -> Output {
    Output {
        status: crate::test::exited(code),
        stdout: stdout.into(),
        stderr: stderr.into(),
    }
}

pub(super) fn id(id: &str) -> SessionId {
    SessionId(id.into())
}

pub(super) fn strings(call: &AgentCall) -> Vec<String> {
    argv(call, &Files::of(call))
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect()
}

// A stand-in for pi that keeps its arguments and prints `stdout`.
pub(super) fn stand_in(world: &World, stdout: &str) -> PiCli {
    std::fs::write(world.path("stdout.jsonl"), stdout).unwrap();
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\ncat {}\n",
        world.path("argv").display(),
        world.path("stdout.jsonl").display()
    );
    crate::test::write_script(&world.path("pi"), &script);
    PiCli::on(&Base::default().sandboxed(Arc::new(OpenSandbox::default()), world.path("home")))
        .with_program(world.path("pi"))
}

// The wrapper prints crabpot's own refusal when a Windows session's grant meets
// the profile read grant's exclusions. The turn's error must carry every conflict.
#[test]
fn a_start_refused_for_profile_conflicts_names_each_conflict() {
    let conflicts = vec![
        crabpot::ProfileConflict {
            path: r"C:\Users\R\.cargo".into(),
            exclusion: r"\\?\C:\Users\R\.cargo\credentials".into(),
            write: false,
        },
        crabpot::ProfileConflict {
            path: r"C:\Users\R\.rustup".into(),
            exclusion: r"\\?\C:\Users\R\.rustup\NTUSER.DAT*".into(),
            write: false,
        },
    ];
    let stderr = format!(
        "crabpot: {}\n",
        crabpot::StartError::ProfileConflicts(conflicts.clone())
    );
    let err = parse_result(&output(1, "", &stderr), &id(FRESH_ID)).unwrap_err();
    let said = err.to_string();
    for conflict in &conflicts {
        assert!(said.contains(&conflict.to_string()), "{conflict}: {said}");
    }
}

#[test]
fn a_pi_workers_commands_ask_for_the_test_lease_at_the_dogs_door() {
    let w = World::new();
    let call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    let vars: Vec<String> = env_args(&call, &Files::of(&call))
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect();
    let door = format!("{}=/k/dog/lease.sock", crate::lease::door::SOCKET_VAR);
    assert!(vars.contains(&door), "{vars:?}");
}

// The worker's instructions send its scratch files to the folder `TMPDIR`
// names, and zsh ignores `TMPDIR` for a heredoc and writes under `TMPPREFIX`.
#[test]
fn a_pi_call_s_zsh_makes_its_heredoc_files_in_the_call_s_scratch_folder() {
    let w = World::new();
    let call = w.call(URL, Role::Worker, Session::New(id(WORKER_ID)));
    let files = Files::of(&call);
    let vars: Vec<String> = env_args(&call, &files)
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect();
    let temp = format!("TMPDIR={}", files.scratch.display());
    assert!(vars.contains(&temp), "{vars:?}");
    let prefix = format!("TMPPREFIX={}/zsh", files.scratch.display());
    assert!(vars.contains(&prefix), "{vars:?}");
}

#[test]
fn the_guard_extension_names_the_base_branch_only_when_it_is_not_the_default() {
    let w = World::new();
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    let fence = call.reach.fence.as_mut().unwrap();
    assert!(!guard_extension(fence).contains("--base="));
    fence.guard.base = "v2".to_owned();
    assert!(guard_extension(fence).contains("--base=v2"));
}

#[test]
fn a_new_worker_session_carries_its_model_tools_and_guard() {
    let w = World::new();
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    call.instructions = Some(PathBuf::from("/k/worker/instructions.md"));
    call.prompt = "implement #7".into();
    let home = w.path("settings.pi");
    assert_eq!(
        strings(&call),
        [
            "--model",
            "kelpie/qwen3.8:27b",
            "--thinking",
            "low",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-themes",
            "--no-approve",
            "--offline",
            "--session-dir",
            home.join("sessions").to_str().unwrap(),
            "--tools",
            "read,bash,edit,write,grep,find,ls",
            "--no-mcp",
            "--append-system-prompt",
            "/k/worker/instructions.md",
            "--extension",
            w.path("settings.guard.ts").to_str().unwrap(),
            "-p",
            "--mode",
            "json",
            "--session-id",
            WORKER_ID,
            "--",
            "implement #7",
        ]
    );
}

#[test]
fn a_resumed_session_keeps_its_id_and_gets_its_instructions_again() {
    let w = World::new();
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::Resume(id(WORKER_ID)));
    call.instructions = Some(PathBuf::from("/k/worker/instructions.md"));
    let argv = strings(&call);
    let at = argv.iter().position(|a| a == "--session-id").unwrap();
    assert_eq!(argv[at + 1], WORKER_ID);
    assert!(argv.iter().any(|a| a == "--append-system-prompt"));
}

#[test]
fn each_kind_of_call_gets_its_own_tools() {
    let w = World::new();
    let mut call = w.call(URL, Role::Reviewer, Session::New(id(FRESH_ID)));
    call.tools = Tools::Review;
    let argv = strings(&call);
    let at = argv.iter().position(|a| a == "--tools").unwrap();
    assert_eq!(argv[at + 1], "read,grep,find,ls");
    assert!(!argv.iter().any(|a| a == "--extension"));

    let mut retro = w.call(URL, Role::Worker, Session::Resume(id(FRESH_ID)));
    retro.tools = Tools::Retro;
    let argv = strings(&retro);
    let at = argv.iter().position(|a| a == "--tools").unwrap();
    assert_eq!(argv[at + 1], "read,grep,find,ls");

    let mut review = w.call(URL, Role::Reviewer, Session::New(id(FRESH_ID)));
    assert!(strings(&review).iter().any(|a| a == "--no-tools"));
    review.reach.read = vec![w.path("extra")];
    let argv = strings(&review);
    let at = argv.iter().position(|a| a == "--tools").unwrap();
    assert_eq!(argv[at + 1], "read");

    // A call with servers names each server's tools after its own, and one with none to use them
    // loads no server.
    let mut served = w.call(URL, Role::Reviewer, Session::New(id(FRESH_ID)));
    served.tools = Tools::Review;
    served.reach.mcp_servers = [(
        "docs".to_owned(),
        json!({ "url": "https://d.example.com/mcp" }),
    )]
    .into();
    let argv = strings(&served);
    let at = argv.iter().position(|a| a == "--tools").unwrap();
    assert_eq!(argv[at + 1], "read,grep,find,ls,mcp__docs__*");
    served.tools = Tools::Pm;
    let argv = strings(&served);
    assert!(argv.iter().any(|a| a == "--no-tools"));
    assert!(argv.iter().any(|a| a == "--no-mcp"));
    assert!(!argv.iter().any(|a| a == "builtin:mcp"));
}

#[test]
fn a_call_still_trusts_no_project_with_servers_loaded() {
    let w = World::new();
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    call.reach.mcp_servers = [(
        "docs".to_owned(),
        json!({ "url": "https://d.example.com/mcp" }),
    )]
    .into();
    let argv = strings(&call);
    assert!(argv.iter().any(|a| a == "--no-approve"), "{argv:?}");
    assert!(!argv.iter().any(|a| a == "--approve"), "{argv:?}");
    assert!(argv.iter().any(|a| a == "--no-extensions"), "{argv:?}");
    assert_eq!(
        values(&argv, "--extension").len(),
        2,
        "the guard and the MCP client"
    );
    assert!(values(&argv, "--extension").contains(&"builtin:mcp"));
    assert!(!argv.iter().any(|a| a == "--no-mcp"), "{argv:?}");
}

#[test]
fn the_sandbox_reads_the_code_graph_binary_and_writes_only_its_index_and_daemon_folder() {
    let w = World::new();
    let upstream = PiCli::on(&Base::default())
        .upstream(&World::server(URL))
        .unwrap();
    let socket = w.path("forwarder.sock");
    let graph = CodeGraph {
        binary: PathBuf::from("/k/tools/codebase-memory-mcp"),
        index: PathBuf::from("/k/graphs/7"),
        runtime: PathBuf::from("/k/cbm/7"),
    };
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    let files = Files::of(&call);
    let without = policy(&call, &files, &upstream, &socket, &NO_PROGRAM, &[]);
    call.reach.code_graph = Some(graph.clone());
    let with = policy(&call, &files, &upstream, &socket, &NO_PROGRAM, &[]);

    assert!(with.read.contains(&graph.binary) && with.read.contains(&graph.index));
    assert!(with.write.contains(&graph.index));
    assert!(!with.write.contains(&graph.binary));
    assert!(!without.read.contains(&graph.binary) && !without.write.contains(&graph.index));
    let granted: Vec<_> = with
        .write
        .iter()
        .filter(|p| !without.write.contains(p))
        .collect();
    assert_eq!(granted, [&graph.index, &graph.runtime]);
    let sockets: Vec<_> = (with.sockets.iter())
        .filter(|s| !without.sockets.contains(s))
        .collect();
    assert_eq!(
        sockets,
        [&graph.runtime],
        "its daemon's socket, and no other"
    );
    // pi's model traffic goes through a forwarder, and the daemon's socket must still reach srt.
    let network = &crate::adapters::srt::srt_settings(&with)["network"];
    assert!(network["mitmProxy"].is_object(), "{network}");
    assert!(
        (network["allowUnixSockets"].as_array().unwrap().iter())
            .any(|s| s == graph.runtime.to_str().unwrap()),
        "{network}"
    );
}

#[test]
fn the_sandbox_connects_only_to_the_browsers_socket_when_the_call_has_one() {
    let w = World::new();
    let upstream = PiCli::on(&Base::default())
        .upstream(&World::server(URL))
        .unwrap();
    let forwarder = w.path("forwarder.sock");
    let browser = Browser {
        kelpie: PathBuf::from("/k/kelpie"),
        socket: w.path("0123456789abcdef0123456789abcdef.sock"),
    };
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    let files = Files::of(&call);
    let without = policy(&call, &files, &upstream, &forwarder, &NO_PROGRAM, &[]);
    call.reach.browser = Some(Box::new(browser.clone()));
    let with = policy(&call, &files, &upstream, &forwarder, &NO_PROGRAM, &[]);

    assert!(!without.sockets.contains(&browser.socket));
    assert_eq!(
        with.sockets
            .iter()
            .filter(|s| !without.sockets.contains(s))
            .collect::<Vec<_>>(),
        [&browser.socket]
    );
}

// Every value that follows `flag`, in order
pub(super) fn values<'a>(argv: &'a [String], flag: &str) -> Vec<&'a str> {
    let at = |(i, a): (usize, &String)| (a == flag).then(|| argv[i + 1].as_str());
    argv.iter().enumerate().filter_map(at).collect()
}

#[test]
fn prepare_writes_pis_own_home_with_the_one_model() {
    let w = World::new();
    let call = w.call(URL, Role::Reviewer, Session::New(id(FRESH_ID)));
    stand_in(&w, FRESH).prepare(&call).unwrap();
    let home = w.path("settings.pi");
    let models: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join("models.json")).unwrap()).unwrap();
    assert_eq!(
        models,
        json!({ "providers": { "kelpie": {
            "baseUrl": "http://model.kelpie.test/v1",
            "api": "openai-completions",
            "apiKey": "kelpie",
            "models": [{ "id": "qwen3.8:27b", "contextWindow": 65536, "reasoning": true }],
        } } })
    );
    assert!(home.join("sessions").is_dir());
    assert!(!w.path("settings.guard.ts").exists());
}

#[test]
fn a_model_server_the_forwarder_cannot_dial_fails_before_the_call() {
    let w = World::new();
    let call = w.call(
        "https://192.0.2.9/v1",
        Role::Reviewer,
        Session::New(id(FRESH_ID)),
    );
    let err = stand_in(&w, FRESH).prepare(&call).unwrap_err();
    assert!(
        matches!(&err, AgentError::Setup(why) if why.contains("plain `http://`")),
        "{err:?}"
    );
}

#[test]
fn what_pi_cannot_give_a_worker_fails_before_the_call() {
    let w = World::new();
    let pi = stand_in(&w, FRESH);
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    let fence = call.reach.fence.as_mut().unwrap();
    fence.hooks = vec![crate::settings::GuardHook {
        event: crate::settings::HookEvent::PreToolUse,
        matcher: None,
        command: "true".to_owned().try_into().unwrap(),
    }];
    let err = pi.prepare(&call).unwrap_err();
    assert!(
        matches!(&err, AgentError::Setup(why) if why.contains("guard_hooks")),
        "{err:?}"
    );
}

#[test]
fn a_call_on_a_covered_repo_runs_without_the_keychain_and_any_other_with_it() {
    let w = World::new();
    let socket = w.path("forwarder.sock");
    let upstream = PiCli::on(&Base::default())
        .upstream(server(&w.call(URL, Role::Reviewer, Session::New(id(FRESH_ID)))).unwrap())
        .unwrap();
    let mut calls = [
        w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID))),
        w.call(URL, Role::Reviewer, Session::New(id(FRESH_ID))),
    ];
    for call in &mut calls {
        let policy = |call: &AgentCall| {
            let files = Files::of(call);
            policy(call, &files, &upstream, &socket, &NO_PROGRAM, &[])
        };
        assert!(!policy(call).no_keychain);
        call.reach.block_keychain = true;
        assert!(policy(call).no_keychain);
    }
}

#[test]
fn a_calls_transcript_is_in_pis_sessions_folder() {
    let w = World::new();
    let call = w.call(URL, Role::Reviewer, Session::New(id(WORKER_ID)));
    assert_eq!(
        stand_in(&w, "").transcript(&call),
        Some(crate::ports::Transcript {
            folder: w.path("settings.pi/sessions"),
            threads: None,
        })
    );
}

#[test]
fn the_sandbox_lets_pi_write_only_its_sessions_and_reach_only_the_forwarder() {
    let w = World::new();
    let call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    let socket = w.path("forwarder.sock");
    let upstream = PiCli::on(&Base::default())
        .upstream(server(&call).unwrap())
        .unwrap();
    let files = Files::of(&call);
    let policy = policy(&call, &files, &upstream, &socket, &NO_PROGRAM, &[]);
    let home = w.path("settings.pi");
    assert!(policy.write.contains(&home.join("sessions")));
    assert!(policy.write.contains(&home.join("settings.json.lock")));
    assert!(!policy.write.contains(&home.join("settings.json")));
    assert!(!policy.write.contains(&w.path("settings.prompt.md")));
    assert!(!policy.write.contains(&home));
    assert!(policy.write.contains(&w.path("wt")));
    assert!(policy.read.contains(&home));
    assert!(policy.read.contains(&w.path("settings.guard.ts")));
    assert!(policy.read.contains(&w.path("settings.compact.ts")));
    assert!(policy.read.contains(&w.path("settings.stdout.jsonl")));
    assert!(policy.read.contains(&w.path("settings.stderr.log")));
    assert!(!policy.read.contains(&w.path("settings.prompt.md")));
    assert!(
        !policy.hosts.iter().any(|h| h.contains("192.0.2.9")),
        "{:?}",
        policy.hosts
    );
    assert_eq!(
        policy.forward,
        Some(Forward {
            host: "model.kelpie.test".into(),
            socket: socket.clone()
        })
    );
    assert_eq!(
        policy.sockets,
        [Path::new("/k/dog/lease.sock")],
        "the dog's door"
    );
    for credentials in ["~/.pi/**", "~/.codex/**", "~/.claude.json"] {
        assert!(
            policy.no_read.iter().any(|p| p == credentials),
            "{credentials}"
        );
    }
    let open = w.call(URL, Role::Reviewer, Session::New(id(FRESH_ID)));
    let open_files = Files::of(&open);
    let policy = super::policy(&open, &open_files, &upstream, &socket, &NO_PROGRAM, &[]);
    assert_eq!(
        policy.write,
        [
            home.join("sessions"),
            w.path("settings.tmp"),
            home.join("auth.json.lock"),
            home.join("models-store.json.lock"),
            home.join("settings.json.lock"),
        ]
    );
    assert!(policy.hosts.is_empty(), "{:?}", policy.hosts);
    assert_eq!(
        policy.denied_hosts,
        ["192.0.2.9", "localhost", "127.0.0.1", "[::1]"]
    );
    assert_eq!(policy.denied_addresses, ["192.0.2.9"]);
    assert_eq!(
        policy.forward.map(|f| f.host),
        Some("model.kelpie.test".into())
    );
}

// The shepherd's home holds the sheep entries' `env`, a gateway's key with
// it, and `dogs.toml`. A session with no fence reads none of it, but for
// its own folder.
#[test]
fn an_unfenced_pi_session_is_denied_the_shepherds_home_but_its_own_folder() {
    let w = World::new();
    stand_in(&w, FRESH);
    let sandbox = OpenSandbox::default();
    let pi = PiCli::on(
        &Base::default()
            .unfenced_unread(&w.path("home"), &w.path("shep"))
            .sandboxed(Arc::new(sandbox.clone()), w.path("home")),
    )
    .with_program(w.path("pi"));
    let call = w.call(URL, Role::Reviewer, Session::New(id(FRESH_ID)));
    pi.prepare(&call).unwrap();
    pi.run(&call, &Ending::default()).unwrap();
    let shep = format!("{}/**", w.path("shep").display());
    let wrapped = sandbox.wrapped();
    let [(open, _)] = wrapped.as_slice() else {
        panic!("wrapped once, not {wrapped:?}")
    };
    assert!(open.no_read.contains(&shep), "{:?}", open.no_read);
    assert!(open.read.contains(&w.path("wt")), "{:?}", open.read);
}

#[test]
fn a_turn_answers_with_its_text_and_its_tokens_and_no_price() {
    let w = World::new();
    let pi = stand_in(&w, FRESH);
    let call = w.call(URL, Role::Reviewer, Session::New(id(FRESH_ID)));
    pi.prepare(&call).unwrap();
    let reply = pi.run(&call, &Ending::default()).unwrap();
    assert_eq!(reply.session_id, id(FRESH_ID));
    assert_eq!(reply.text.trim(), "ok");
    assert_eq!(reply.session_cost, None);
    assert!(reply.usage.input > 0 && reply.usage.output > 0, "{reply:?}");
    let argv = std::fs::read_to_string(w.path("argv")).unwrap();
    assert!(argv.contains("--session-id\n0b6f3c1e"), "{argv}");
}

#[test]
fn a_resumed_turn_reports_only_its_own_tokens() {
    let w = World::new();
    let pi = stand_in(&w, RESUMED);
    let call = w.call(URL, Role::Reviewer, Session::Resume(id(FRESH_ID)));
    pi.prepare(&call).unwrap();
    let err = pi.run(&call, &Ending::default()).unwrap_err();
    assert_eq!(err, AgentError::NoSession(Harness::Pi, id(FRESH_ID)));
    assert_eq!(err.to_string(), format!("pi has no session {FRESH_ID}"));

    let sessions = w.path("settings.pi/sessions");
    std::fs::write(
        sessions.join(format!("2026-10-01T03-00-00-000Z_{FRESH_ID}.jsonl")),
        "",
    )
    .unwrap();
    let second = pi.run(&call, &Ending::default()).unwrap();
    let first = parse_result(&output(0, FRESH, ""), &id(FRESH_ID)).unwrap();
    assert_eq!(second.session_id, first.session_id);
    assert!(second.text.to_lowercase().contains("ok"), "{second:?}");
    // pi prints only the resumed call's own messages, so its usage is its
    // own, though its prompt carries the first call's turn too.
    let answers = RESUMED.matches(r#""type":"message_end","message":{"role":"assistant""#);
    assert_eq!(answers.count(), 1);
    assert!(second.usage.input + second.usage.cache_read > first.usage.input);
}

#[test]
fn a_refused_tool_call_reaches_the_model_and_the_call_still_answers() {
    let reply = parse_result(&output(0, REFUSED, ""), &id(WORKER_ID)).unwrap();
    assert_eq!(reply.session_id, id(WORKER_ID));
    assert!(!reply.text.is_empty());
    assert!(reply.usage.output > 0, "{reply:?}");
    let refusals: Vec<&str> = REFUSED
        .lines()
        .filter(|l| l.contains(r#""type":"tool_execution_end""#) && l.contains(r#""isError":true"#))
        .collect();
    let said = |what: &str| refusals.iter().any(|l| l.contains(what));
    assert!(said("is Claude Code's own configuration"), "kelpie confine");
    assert!(said("Operation not permitted"), "the sandbox");
    assert!(said("only the project manager merges"), "kelpie guard");
}

#[test]
fn only_an_answer_the_model_ended_itself_is_a_success() {
    let ended = |reason: &str| {
        let text = FRESH.replace(
            r#""stopReason":"stop""#,
            &format!(r#""stopReason":"{reason}""#),
        );
        parse_result(&output(0, &text, ""), &id(FRESH_ID))
    };
    assert!(ended("stop").is_ok());
    assert_eq!(
        ended("length"),
        Err(AgentError::Failed(
            Harness::Pi,
            "the model ran out of output tokens".into()
        ))
    );
    assert_eq!(
        ended("toolUse"),
        Err(AgentError::Failed(
            Harness::Pi,
            "the model stopped for the reason `toolUse`".into()
        ))
    );
}

#[test]
fn a_turn_the_pause_aborted_is_read_for_its_usage_and_any_other_abort_is_a_failure() {
    let aborted = FRESH.replace(r#""stopReason":"stop""#, r#""stopReason":"aborted""#);
    let whole = parse_result(&output(0, FRESH, ""), &id(FRESH_ID)).unwrap();
    // pi may leave with a failing status after an abort; the marker the
    // extension leaves is what tells a pause from any other.
    for status in [0, 1] {
        let ran = output(status, &aborted, "");
        let reply = parse_turn(&ran, &id(FRESH_ID), true).unwrap();
        assert_eq!(reply.usage, whole.usage, "status {status}");
        assert!(
            matches!(
                parse_turn(&ran, &id(FRESH_ID), false),
                Err(AgentError::Failed(Harness::Pi, _))
            ),
            "status {status}: an abort with no pause failed"
        );
    }
    // A pause does not excuse a turn that failed some other way.
    let errored = FRESH.replace(r#""stopReason":"stop""#, r#""stopReason":"error""#);
    assert!(parse_turn(&output(0, &errored, ""), &id(FRESH_ID), true).is_err());
}

#[test]
fn an_answer_missing_a_token_count_still_counts_the_rest() {
    let text = FRESH.replace(r#","cacheWrite":0"#, "");
    assert_ne!(text, FRESH);
    let reply = parse_result(&output(0, &text, ""), &id(FRESH_ID)).unwrap();
    let whole = parse_result(&output(0, FRESH, ""), &id(FRESH_ID)).unwrap();
    assert_eq!(reply.usage, whole.usage);
}

#[test]
fn a_last_answer_that_cannot_be_read_is_not_mistaken_for_an_earlier_one() {
    let broken = r#"{"type":"message_end","message":{"role":"assistant","content":7}}"#;
    let text = format!("{RESUMED}{broken}\n");
    let err = parse_result(&output(0, &text, ""), &id(FRESH_ID)).unwrap_err();
    assert!(
        matches!(err, AgentError::Unreadable(Harness::Pi, _)),
        "{err:?}"
    );
}

#[test]
fn an_error_carries_only_the_end_of_a_long_output() {
    let long = format!("{}é tail", "x".repeat(10_000));
    let err = parse_result(&output(0, &long, ""), &id(FRESH_ID)).unwrap_err();
    let AgentError::Unreadable(_, text) = &err else {
        panic!("{err:?}");
    };
    assert!(
        text.len() <= ERROR_TAIL && text.ends_with("é tail"),
        "{}",
        text.len()
    );
    let err = parse_result(&output(1, "", &long), &id(FRESH_ID)).unwrap_err();
    assert!(
        err.to_string().len() < ERROR_TAIL + 20,
        "{}",
        err.to_string().len()
    );
}

#[test]
fn a_failed_call_says_why() {
    let err = parse_result(&output(1, "", "Error: model not found"), &id(FRESH_ID)).unwrap_err();
    assert_eq!(
        err,
        AgentError::Failed(Harness::Pi, "Error: model not found".into())
    );
    assert_eq!(err.to_string(), "pi failed: Error: model not found");

    let errored = FRESH.replace(
        r#""stopReason":"stop""#,
        r#""stopReason":"error","errorMessage":"model not found""#,
    );
    let err = parse_result(&output(0, &errored, ""), &id(FRESH_ID)).unwrap_err();
    assert_eq!(
        err,
        AgentError::Failed(Harness::Pi, "model not found".into())
    );

    let err = parse_result(&output(0, "hello", ""), &id(FRESH_ID)).unwrap_err();
    assert_eq!(err, AgentError::Unreadable(Harness::Pi, "hello".into()));

    let err = parse_result(&output(0, FRESH, ""), &id(WORKER_ID)).unwrap_err();
    assert!(
        matches!(err, AgentError::Unreadable(Harness::Pi, _)),
        "{err:?}"
    );
}

#[test]
fn a_call_on_no_model_server_is_refused() {
    let w = World::new();
    let mut call = w.call(URL, Role::Reviewer, Session::New(id(FRESH_ID)));
    call.harness = AgentHarness::ClaudeCode;
    let err = stand_in(&w, FRESH).prepare(&call).unwrap_err();
    assert!(matches!(err, AgentError::Setup(_)), "{err:?}");
}

pub(super) struct World {
    _dir: tempfile::TempDir,
    pub(super) root: PathBuf,
}

impl World {
    // Short, and canonical, so a recorded path reads the same each time.
    pub(super) fn new() -> Self {
        let dir = crate::test::short_tempdir();
        let root = dir.path().canonicalize().unwrap();
        for folder in ["wt", "build", "outside"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        Self { _dir: dir, root }
    }

    pub(super) fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    fn server(url: &str) -> ModelServer {
        ModelServer {
            host: ModelHost::Url(EndpointUrl::try_from(url.to_owned()).unwrap()),
            context: ContextSize::try_from(65_536).unwrap(),
        }
    }

    pub(super) fn call(&self, url: &str, role: Role, session: Session) -> AgentCall {
        AgentCall {
            issue: crate::ticket::TicketId::from_issue(7),
            settings: self.path("settings.json"),
            ..AgentCall::new(
                role,
                Tools::Answer,
                AgentHarness::Pi(Self::server(url)),
                "qwen3.8:27b",
                Effort::Low,
                session,
                self.path("wt"),
                String::new(),
            )
        }
    }

    pub(super) fn fenced(&self, url: &str, kelpie: &Path, session: Session) -> AgentCall {
        let (wt, build, git) = (self.path("wt"), self.path("build"), self.path("wt/.git"));
        let profile = WorkerProfile {
            worktree: &wt,
            build: &build,
            home: &self.path("home"),
            git_common_dir: &git,
            git_dir: &git,
            branch: "kelpie/7",
            base: "main",
            kelpie_pushes: false,
            kelpie,
            skills: &self.path("kelpie/skills"),
            kelpie_home: &self.path("kelpie"),
            repo: &self.path("repo"),
            guard_hooks: &[],
            allowed_domains: &[],
            registries: &[],
            build_env: &BTreeMap::new(),
            shep_home: &self.path("shep"),
            door: Path::new("/k/dog/lease.sock"),
            token: None,
            login_token: None,
        };
        AgentCall {
            role: Role::Worker,
            tools: Tools::Work,
            agent_tools: None,
            reach: profile.reach(),
            ..self.call(url, Role::Worker, session)
        }
    }
}

const API_KEY: &str = "sk-ant-api03-for-the-test";

// `call` as an agent file with `login: api` makes it: pi on Anthropic's own model
fn on_the_api(call: AgentCall) -> AgentCall {
    AgentCall {
        harness: AgentHarness::PiApi(ContextSize::try_from(200_000).unwrap()),
        model: "claude-sonnet-4-5".into(),
        ..call
    }
}

// What a command sets `var` to, or none when it unsets it or leaves it alone.
fn credential(command: &Command, var: &str) -> Option<String> {
    let set = command.get_envs().find(|(n, _)| *n == var);
    set.and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
}

#[test]
fn a_pi_api_call_gets_the_api_key_in_its_environment_and_in_no_argument() {
    let w = World::new();
    let pi = stand_in(&w, FRESH).with_api_key(ApiKey::new(API_KEY.to_owned()));
    for role in [Role::Worker, Role::Reviewer] {
        let call = on_the_api(w.call(URL, role, Session::New(id(WORKER_ID))));
        for turn in [Turn::Headless, Turn::Terminal] {
            let launched = pi.launch(&call, &(), turn, None).unwrap();
            let command = launched.command;
            assert_eq!(credential(&command, API_VAR).as_deref(), Some(API_KEY));
            let args = command.get_args().map(|a| a.to_string_lossy().into_owned());
            assert!(!args.collect::<String>().contains(API_KEY), "{role:?}");
        }
    }
    assert!(!format!("{pi:?}").contains(API_KEY), "Debug shows the key");
}

#[test]
fn a_pi_call_on_a_server_never_sees_the_api_key_even_when_the_runner_holds_it() {
    let w = World::new();
    let pi = stand_in(&w, FRESH).with_api_key(ApiKey::new(API_KEY.to_owned()));
    for role in [Role::Worker, Role::Reviewer] {
        let call = w.call(URL, role, Session::New(id(WORKER_ID)));
        for turn in [Turn::Headless, Turn::Terminal] {
            let launched = pi.launch(&call, &(), turn, None).unwrap();
            assert!(
                launched.forwarder.is_some(),
                "{role:?}: a server call has one"
            );
            assert_eq!(credential(&launched.command, API_VAR), None, "{role:?}");
        }
    }
}

#[test]
fn a_pi_api_call_opens_no_forwarder_writes_no_kelpie_provider_and_dials_only_the_anthropic_host() {
    let w = World::new();
    let pi = stand_in(&w, FRESH).with_api_key(ApiKey::new(API_KEY.to_owned()));
    let call = on_the_api(w.call(URL, Role::Reviewer, Session::New(id(FRESH_ID))));
    pi.prepare(&call).unwrap();
    let home = w.path("settings.pi");
    let models: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join("models.json")).unwrap()).unwrap();
    assert_eq!(models.pointer("/providers/kelpie"), None, "{models}");
    assert_eq!(
        std::fs::read_to_string(home.join("auth.json")).unwrap(),
        "{}"
    );
    assert!(home.join("sessions").is_dir());

    let launched = pi.launch(&call, &(), Turn::Headless, None).unwrap();
    assert!(launched.forwarder.is_none());
    let sockets = std::fs::read_dir(w.path(".")).unwrap();
    let sockets = sockets.filter_map(Result::ok).map(|e| e.path());
    let sockets: Vec<_> = sockets
        .filter(|p| p.extension().is_some_and(|e| e == "sock"))
        .collect();
    assert!(sockets.is_empty(), "{sockets:?}");

    let files = Files::of(&call);
    let open = fenced(&call, &files, &NO_PROGRAM, &[]);
    assert_eq!(open.hosts, ["api.anthropic.com"]);
    assert_eq!(open.forward, None);
    assert!(open.denied_hosts.is_empty(), "{:?}", open.denied_hosts);
    let worker = on_the_api(w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID))));
    let fenced = fenced(&worker, &Files::of(&worker), &NO_PROGRAM, &[]);
    assert!(
        fenced.hosts.contains(&"api.anthropic.com".to_owned()),
        "{:?}",
        fenced.hosts
    );
    assert_eq!(fenced.forward, None);
}

#[test]
fn a_pi_api_call_runs_the_anthropic_model_and_reads_its_compaction_context_from_the_file() {
    let w = World::new();
    let pi = stand_in(&w, FRESH).with_api_key(ApiKey::new(API_KEY.to_owned()));
    let call = on_the_api(w.call(URL, Role::Worker, Session::New(id(WORKER_ID))));
    assert_eq!(
        values(&strings(&call), "--model"),
        ["anthropic/claude-sonnet-4-5"]
    );
    let served = w.call(URL, Role::Worker, Session::New(id(WORKER_ID)));
    assert_eq!(values(&strings(&served), "--model"), ["kelpie/qwen3.8:27b"]);

    // The file says 200000 tokens, so a window of 150000 leaves a reserve of 50000.
    let mut call = call;
    call.compact_window = NonZeroU32::new(150_000);
    pi.prepare(&call).unwrap();
    let text = std::fs::read_to_string(w.path("settings.pi/settings.json")).unwrap();
    let settings: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        settings,
        json!({ "compaction": { "reserveTokens": 50_000 } })
    );
}

#[test]
fn a_pi_api_call_with_no_key_refuses_with_the_fix_and_runs_nothing() {
    let w = World::new();
    let pi = stand_in(&w, FRESH);
    for role in [Role::Worker, Role::Reviewer] {
        let call = on_the_api(w.call(URL, role, Session::New(id(WORKER_ID))));
        for turn in [Turn::Headless, Turn::Terminal] {
            let err = pi.launch(&call, &(), turn, None).unwrap_err();
            let AgentError::Setup(why) = &err else {
                panic!("{err:?}")
            };
            assert!(why.contains("`shep secret set ANTHROPIC_API_KEY`"), "{why}");
        }
        let err = pi.run(&call, &Ending::default()).unwrap_err();
        assert!(matches!(err, AgentError::Setup(_)), "{err:?}");
    }
    assert!(!w.path("argv").exists(), "pi ran with no key");
}

// The call as the old image left it: pi running as a child, its output in the call's files.
#[cfg(unix)]
#[test]
fn a_pi_api_call_carried_across_a_handover_is_finished_with_no_forwarder() {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let w = World::new();
    let pi = stand_in(&w, FRESH);
    let call = on_the_api(w.call(URL, Role::Worker, Session::New(id(FRESH_ID))));
    let files = CallFiles::of(&call);
    std::fs::write(&files.stdout, FRESH).unwrap();
    std::fs::write(&files.stderr, "").unwrap();
    // The adopting side reaps it, as the new image reaps a child it never started.
    #[expect(clippy::zombie_processes, reason = "`adopt` waits for the pid")]
    let child = Command::new(crate::spawn::sh())
        .args(["-c", "sleep 0.3"])
        .process_group(0)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let record = serde_json::json!({
        "pid": child.id(), "pgid": child.id(), "role": "worker", "issue": 7,
        "stdout": files.stdout, "stderr": files.stderr, "ending": false, "flying": null,
    });
    std::fs::write(&files.record, record.to_string()).unwrap();

    let adopted = pi.adoption(&call).unwrap();
    assert_eq!(adopted.kept, None, "an API call keeps no forwarder");
    assert_eq!(adopted.harness, Harness::Pi);
    let reply = pi.adopt(&adopted, &Ending::default()).unwrap();

    assert_eq!(reply.session_id.0, FRESH_ID);
    assert_eq!(reply.text.trim(), "ok");
}
