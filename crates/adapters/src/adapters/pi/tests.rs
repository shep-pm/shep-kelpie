use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;
use crate::adapters::SandboxRuntime;
use crate::adapters::harness::ERROR_TAIL;
use crate::adapters::skills;
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

#[test]
fn a_steps_skill_runs_as_pis_own_slash_command() {
    let w = World::new();
    let plugin = w.path("skills/mattpocock");
    std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(plugin.join("skills/implement")).unwrap();
    std::fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        r#"{"name":"mattpocock"}"#,
    )
    .unwrap();
    std::fs::write(plugin.join("skills/implement/SKILL.md"), "---\n").unwrap();
    let mut call = w.call(URL, Role::Worker, Session::New(id(FRESH_ID)));
    call.plugin_dirs = vec![plugin.clone()];
    call.prompt = "/mattpocock:implement Implement issue #7".into();
    let argv = strings(&call);
    assert_eq!(argv.last().unwrap(), "/skill:implement Implement issue #7");
    let at = argv.iter().position(|a| a == "--skill").unwrap();
    assert_eq!(
        argv[at + 1],
        plugin.join("skills/implement").to_str().unwrap()
    );

    call.prompt = "/mattpocock:tdd Implement issue #7".into();
    let argv = strings(&call);
    assert_eq!(argv.last().unwrap(), "/mattpocock:tdd Implement issue #7");
    assert!(!argv.iter().any(|a| a == "--skill"));
}

// A plugin `name` holding a skill folder for each of `skills`
fn plugin(w: &World, name: &str, skills: &[&str]) -> PathBuf {
    let dir = w.path(&format!("plugins/{name}"));
    std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        dir.join(".claude-plugin/plugin.json"),
        format!(r#"{{"name":"{name}"}}"#),
    )
    .unwrap();
    for skill in skills {
        std::fs::create_dir_all(dir.join("skills").join(skill)).unwrap();
        std::fs::write(dir.join("skills").join(skill).join("SKILL.md"), "---\n").unwrap();
    }
    dir
}

// Every value that follows `flag`, in order
pub(super) fn values<'a>(argv: &'a [String], flag: &str) -> Vec<&'a str> {
    let at = |(i, a): (usize, &String)| (a == flag).then(|| argv[i + 1].as_str());
    argv.iter().enumerate().filter_map(at).collect()
}

#[test]
fn every_plugins_skills_load_after_the_steps_own_skill_folder() {
    let w = World::new();
    let steps = plugin(&w, "mattpocock", &["implement", "tdd"]);
    let style = plugin(&w, "style", &["house"]);
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    call.plugin_dirs = vec![steps.clone(), style.clone()];
    call.prompt = "/mattpocock:implement Implement issue #7".into();
    let argv = strings(&call);
    assert_eq!(
        values(&argv, "--skill"),
        [
            steps.join("skills/implement").to_str().unwrap(),
            steps.join("skills").to_str().unwrap(),
            style.join("skills").to_str().unwrap(),
        ]
    );
    let no_skills = argv.iter().position(|a| a == "--no-skills").unwrap();
    let first = argv.iter().position(|a| a == "--skill").unwrap();
    assert!(no_skills < first, "{argv:?}");
}

#[test]
fn the_wording_skill_loads_with_the_steps_skills_on_a_worker_call() {
    let w = World::new();
    let skills =
        crate::skills::Skills::load(&crate::settings::StepSkills::default(), &w.path("skills"));
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    call.plugin_dirs = skills.plugin_dirs().to_vec();
    call.prompt = "/mattpocock:implement Implement issue #7".into();
    let wording = w.path("skills/kelpie-wording/skills");
    assert!(wording.join("wording/SKILL.md").is_file());
    assert!(
        values(&strings(&call), "--skill").contains(&wording.to_str().unwrap()),
        "{:?}",
        strings(&call)
    );
}

#[test]
fn a_call_with_no_tools_loads_its_steps_skill_and_no_other() {
    let w = World::new();
    let steps = plugin(&w, "mattpocock", &["implement"]);
    let style = plugin(&w, "style", &["house"]);
    let mut call = w.call(URL, Role::Reviewer, Session::New(id(FRESH_ID)));
    call.plugin_dirs = vec![steps.clone(), style];
    call.prompt = "/mattpocock:implement Implement issue #7".into();
    let argv = strings(&call);
    assert!(argv.iter().any(|a| a == "--no-tools"), "{argv:?}");
    assert_eq!(
        values(&argv, "--skill"),
        [steps.join("skills/implement").to_str().unwrap()]
    );
}

#[test]
fn every_kind_of_call_that_loads_a_plugins_skills_keeps_read_and_trusts_no_project() {
    let w = World::new();
    let steps = plugin(&w, "mattpocock", &["implement"]);
    let style = plugin(&w, "style", &["house"]);
    let kinds = [
        Tools::Work,
        Tools::Review,
        Tools::Answer,
        Tools::Pm,
        Tools::Retro,
    ];
    for tools in kinds {
        for reads in [false, true] {
            let mut call = w.call(URL, Role::Reviewer, Session::New(id(FRESH_ID)));
            call.tools = tools;
            call.plugin_dirs = vec![steps.clone(), style.clone()];
            call.prompt = "/mattpocock:implement Implement issue #7".into();
            if reads {
                call.reach.read = vec![w.path("extra")];
            }
            let argv = strings(&call);
            let which = format!("{tools:?}, reading {reads}: {argv:?}");
            assert!(!argv.iter().any(|a| a == "--approve"), "{which}");
            assert!(argv.iter().any(|a| a == "--no-approve"), "{which}");
            if values(&argv, "--skill").len() > 1 {
                let [names] = values(&argv, "--tools").try_into().expect(&which);
                assert!(names.split(',').any(|t| t == "read"), "{which}");
            }
        }
    }
}

#[test]
fn the_sandbox_reads_every_plugin_folder_a_call_loads_skills_from() {
    let w = World::new();
    let steps = plugin(&w, "mattpocock", &["implement"]);
    let style = plugin(&w, "style", &["house"]);
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    call.plugin_dirs = vec![steps.clone(), style.clone()];
    let upstream = PiCli::on(&Base::default())
        .upstream(server(&call).unwrap())
        .unwrap();
    let files = Files::of(&call);
    let socket = w.path("forwarder.sock");
    let policy = policy(&call, &files, &upstream, &socket, &NO_PROGRAM, &[]);
    for dir in skills::skill_dirs(&call) {
        let covered = policy.read.iter().any(|read| dir.starts_with(read));
        assert!(covered, "{}", dir.display());
    }
    assert!(policy.read.contains(&steps) && policy.read.contains(&style));
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
    root: PathBuf,
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

const NEEDS: &str = "needs KELPIE_PI_URL=<server>/v1 running qwen3.8:27b, KELPIE_BIN=<kelpie>, \
                     and KELPIE_PI_RECORD=<dir> for what it prints. Run it under \
                     `kelpie lease run gpu --model <model>`";

fn env(name: &str) -> String {
    std::env::var(name).expect(NEEDS)
}

// The real pi on the real model under the real sandbox: a turn, the session
// resumed, and a worker refused by `kelpie confine`, the sandbox and `kelpie
// guard`. The fixtures are these recordings with the temporary folder renamed.
#[test]
#[ignore = "runs pi on a local model"]
fn record_a_turn_a_resumed_turn_and_two_refusals() {
    let (url, kelpie) = (env("KELPIE_PI_URL"), PathBuf::from(env("KELPIE_BIN")));
    let record = PathBuf::from(env("KELPIE_PI_RECORD"));
    let pi = PiCli::on(&Base::default().sandboxed(
        Arc::new(SandboxRuntime::under_test()),
        std::env::var_os("HOME").unwrap().into(),
    ));
    let world = World::new();
    let fresh_id = id(FRESH_ID);
    let save = |name: &str, out: &Output| {
        let text = String::from_utf8_lossy(&out.stdout)
            .replace(world.root.to_str().unwrap(), "/tmp/kelpie-pi");
        std::fs::write(record.join(name), text).unwrap();
    };

    let mut fresh = world.call(&url, Role::Reviewer, Session::New(fresh_id.clone()));
    fresh.prompt = "Reply with the single word ok.".into();
    pi.prepare(&fresh).unwrap();
    let out = run_raw(&pi, &fresh);
    save("pi-p-fresh.jsonl", &out);
    let first = parse_result(&out, &fresh_id).unwrap();

    let mut again = world.call(&url, Role::Reviewer, Session::Resume(fresh_id.clone()));
    again.prompt = "Which word did you reply with? Answer in one word.".into();
    let out = run_raw(&pi, &again);
    save("pi-p-resumed.jsonl", &out);
    let second = parse_result(&out, &fresh_id).unwrap();
    assert!(second.text.to_lowercase().contains("ok"), "{second:?}");
    assert!(second.usage.input + second.usage.cache_read > first.usage.input);

    let worker_id = id(WORKER_ID);
    let mut worker = world.fenced(&url, &kelpie, Session::New(worker_id.clone()));
    worker.prompt = format!(
        "Do these three steps, then stop. First, use the write tool to create \
         .claude/settings.json holding {{}}. Second, use the bash tool to run \
         `echo hi > {}/x`. Third, use the bash tool to run `gh pr merge 1`. \
         Then say in one line per step what happened.",
        world.path("outside").display()
    );
    pi.prepare(&worker).unwrap();
    let out = run_raw(&pi, &worker);
    save("pi-p-refused.jsonl", &out);
    parse_result(&out, &worker_id).unwrap();
    assert!(!world.path("wt/.claude/settings.json").exists());
    assert!(!world.path("outside/x").exists());
}

fn run_raw(pi: &PiCli, call: &AgentCall) -> Output {
    let (mut command, _forwarder) = pi.sandboxed_command(call, None).unwrap();
    command.stdin(std::process::Stdio::null()).output().unwrap()
}
