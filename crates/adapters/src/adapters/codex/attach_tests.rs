// A Codex worker's session in the maintainer's terminal: the worker's sandbox,
// overrides and hooks, with the maintainer at the prompt.

use std::ffi::OsStr;
use std::process::Command;
use std::sync::Arc;

use super::tests::{THREAD, WORKER_ID, World, id, stand_in, strings};
use super::*;
use crate::ports::Usage;
use crate::test::OpenSandbox;

/// The rollout of a session that ran three requests, compacted and ran one more
const ROLLOUT: &str = include_str!("../../../../../fixtures/codex-rollout-compacted.jsonl");

fn codex_in(world: &World, sandbox: &OpenSandbox) -> CodexCli {
    CodexCli::on(
        &Base::default().sandboxed(Arc::new(sandbox.clone()), world.path("home")),
        world.path("login"),
    )
}

// A worker's call whose Codex session kelpie has recorded, ready to resume.
fn resumable(world: &World, codex: &CodexCli) -> AgentCall {
    let mut call = world.fenced(Path::new("/k/kelpie"), Session::Resume(id(WORKER_ID)));
    call.instructions = Some(world.path("instructions.md"));
    std::fs::write(world.path("instructions.md"), "Be brief.").unwrap();
    codex.prepare(&call).unwrap();
    let thread = Thread {
        id: THREAD.into(),
        spent: Spent::default(),
        context: None,
        compact: false,
    };
    save_thread(&Files::of(&call), &id(WORKER_ID), &thread).unwrap();
    call
}

fn args_of(command: &Command) -> Vec<String> {
    command
        .get_args()
        .map(|a| a.to_str().unwrap().to_owned())
        .collect()
}

// The value of each `-c` in `argv`, in order.
fn overrides(argv: &[String]) -> Vec<&str> {
    argv.windows(2)
        .filter(|w| w[0] == "-c")
        .map(|w| w[1].as_str())
        .collect()
}

fn env_of<'a>(command: &'a Command, name: &str) -> Option<&'a OsStr> {
    command
        .get_envs()
        .find(|(n, _)| *n == OsStr::new(name))
        .and_then(|(_, v)| v)
}

#[test]
fn a_codex_session_for_the_terminal_resumes_in_the_workers_sandbox() {
    let w = World::new();
    let sandbox = OpenSandbox::default();
    let codex = codex_in(&w, &sandbox);
    let call = resumable(&w, &codex);

    let command = codex.foreground(&call).unwrap();

    let args = args_of(&command);
    assert_eq!(command.get_program(), "env");
    let at = args.iter().position(|a| a == "codex").unwrap();
    let args = args[at + 1..].to_vec();
    assert_eq!(args[0], "resume");
    assert_eq!(args[args.len() - 2..], ["--", THREAD]);
    let pair = |a: &str, b: &str| args.windows(2).any(|w| w == [a, b]);
    assert!(pair("--model", "gpt-6.1-sol"), "{args:?}");
    assert!(pair("--sandbox", "danger-full-access"), "{args:?}");
    assert!(pair("--ask-for-approval", "on-request"), "{args:?}");
    assert!(!args.iter().any(|a| a == "untrusted"), "{args:?}");
    assert!(args.iter().any(|a| a == "--dangerously-bypass-hook-trust"));
    let set = overrides(&args);
    for key in [
        "model_reasoning_effort=\"medium\"",
        "web_search=\"disabled\"",
        "developer_instructions=\"Be brief.\"",
    ] {
        assert!(set.contains(&key), "{key} in {set:?}");
    }
    for key in ["sqlite_home=", "log_dir=", "hooks.PreToolUse="] {
        assert!(set.iter().any(|c| c.starts_with(key)), "{key} in {set:?}");
    }
    for headless in [
        "exec",
        "--json",
        "--ignore-user-config",
        "--ignore-rules",
        "--dangerously-bypass-approvals-and-sandbox",
    ] {
        assert!(!args.iter().any(|a| a == headless), "{headless}: {args:?}");
    }
    let home = w.path("worker/settings.codex");
    assert_eq!(env_of(&command, "CODEX_HOME"), Some(home.as_os_str()));
    let scratch = w.path("worker/settings.tmp");
    assert_eq!(env_of(&command, "TMPDIR"), Some(scratch.as_os_str()));
    assert_eq!(command.get_current_dir(), Some(w.path("wt").as_path()));

    codex.sandboxed_command(&call).unwrap();
    let wrapped = sandbox.wrapped();
    assert_eq!(
        wrapped[0], wrapped[1],
        "the terminal is held to the headless call's policy"
    );
}

#[test]
fn the_terminal_opens_on_a_folder_already_trusted_that_the_call_cannot_change() {
    let w = World::new();
    let sandbox = OpenSandbox::default();
    let codex = codex_in(&w, &sandbox);
    let call = resumable(&w, &codex);
    let config = Files::of(&call).home.join("config.toml");
    std::fs::write(&config, "stale").unwrap();

    codex.foreground(&call).unwrap();

    let trusted: toml::Table = std::fs::read_to_string(&config).unwrap().parse().unwrap();
    let cwd = call.cwd.to_str().unwrap();
    assert_eq!(
        trusted["projects"][cwd]["trust_level"].as_str(),
        Some("trusted")
    );
    let (policy, _) = &sandbox.wrapped()[0];
    assert!(
        !policy.write.iter().any(|p| config.starts_with(p)),
        "the call may write {}: {:?}",
        config.display(),
        policy.write
    );

    codex.foreground_ended(call.session.id());

    assert!(!config.exists(), "a later call finds the trust left behind");
}

#[test]
fn a_terminal_that_cannot_be_set_up_leaves_no_trust_behind() {
    #[derive(Debug)]
    struct Refusing;
    impl crate::ports::Sandbox for Refusing {
        fn wrap(
            &self,
            _: &crate::ports::Policy,
            _: &Path,
            _: &Command,
        ) -> Result<Command, crate::ports::SandboxError> {
            Err(crate::ports::SandboxError::Install(
                "there is no sandbox".into(),
            ))
        }
    }
    let w = World::new();
    let codex = codex_in(&w, &OpenSandbox::default());
    let call = resumable(&w, &codex);
    let refusing = CodexCli::on(
        &Base::default().sandboxed(Arc::new(Refusing), w.path("home")),
        w.path("login"),
    );

    assert!(refusing.foreground(&call).is_err());

    assert!(!Files::of(&call).home.join("config.toml").exists());
}

#[test]
fn a_session_kelpie_never_started_is_refused() {
    let w = World::new();
    let codex = codex_in(&w, &OpenSandbox::default());
    let call = w.fenced(Path::new("/k/kelpie"), Session::Resume(id(WORKER_ID)));
    codex.prepare(&call).unwrap();

    let err = codex.foreground(&call).unwrap_err();

    assert_eq!(err, AgentError::NoSession(Harness::Codex, id(WORKER_ID)));
}

#[test]
fn the_terminal_and_the_headless_call_share_their_overrides() {
    let w = World::new();
    let codex = codex_in(&w, &OpenSandbox::default());
    let mut call = resumable(&w, &codex);
    call.compact_window = std::num::NonZeroU32::new(100_000);
    let thread = thread(&Files::of(&call), &id(WORKER_ID)).unwrap().unwrap();

    let terminal = args_of(&codex.foreground(&call).unwrap());
    let headless = strings(&call, Some(&thread), Some("Be brief."));

    assert_eq!(overrides(&terminal), overrides(&headless));
    assert!(
        overrides(&terminal)
            .iter()
            .any(|c| c.starts_with("model_auto_compact_token_limit=")),
        "the shared list carries the compact window"
    );
    let features = |argv: &[String]| -> Vec<String> {
        argv.windows(2)
            .filter(|w| w[0] == "--disable")
            .map(|w| w[1].clone())
            .collect()
    };
    assert_eq!(features(&terminal), features(&headless));
}

#[test]
fn a_prompt_is_the_first_message_of_the_resumed_session() {
    let w = World::new();
    let codex = codex_in(&w, &OpenSandbox::default());
    let mut call = resumable(&w, &codex);
    call.prompt = "pick up where you left off".into();

    let args = args_of(&codex.foreground(&call).unwrap());

    assert_eq!(
        args[args.len() - 3..],
        ["--", THREAD, "pick up where you left off"]
    );
}

// Puts the rollout of `THREAD` where Codex keeps it in the call's home.
fn rollout_of_terminal(call: &AgentCall, text: &str) {
    let folder = Files::of(call).home.join("sessions/2026/10/13");
    std::fs::create_dir_all(&folder).unwrap();
    let name = format!("rollout-2026-10-13T09-00-00-{THREAD}.jsonl");
    std::fs::write(folder.join(name), text).unwrap();
}

#[test]
fn the_end_of_a_terminal_session_leaves_the_next_turn_to_count_only_its_own() {
    let w = World::new();
    let codex = codex_in(&w, &OpenSandbox::default());
    let call = resumable(&w, &codex);
    let files = Files::of(&call);
    save_thread(
        &files,
        &id(WORKER_ID),
        &Thread {
            compact: true,
            context: Some(900),
            ..thread(&files, &id(WORKER_ID)).unwrap().unwrap()
        },
    )
    .unwrap();
    codex.foreground(&call).unwrap();
    rollout_of_terminal(&call, ROLLOUT);

    codex.foreground_ended(call.session.id());

    let kept = thread(&files, &id(WORKER_ID)).unwrap().unwrap();
    assert_eq!(kept.id, THREAD);
    assert_eq!(
        kept.spent,
        Spent {
            input_tokens: 116_705,
            cached_input_tokens: 79_104,
            cache_write_input_tokens: 0,
            output_tokens: 469,
        }
    );
    assert_eq!(kept.context, Some(19_478));
    assert!(!kept.compact, "the session compacted when it resumed");
    let next = stand_in(
        &w,
        concat!(
            r#"{"type":"thread.started","thread_id":""#,
            "01a0f7fd-4ed5-7773-80ea-165edc292d8d",
            "\"}\n",
            r#"{"type":"item.completed","item":{"id":"i","type":"agent_message","text":"ok"}}"#,
            "\n",
            r#"{"type":"turn.completed","usage":{"input_tokens":89318,"cached_input_tokens":50048,"output_tokens":349}}"#,
        ),
    );
    // Codex appends the turn's record to the rollout.
    let record = r#"{"type":"token_usage_record","payload":{"thread_token_usage":{"input_tokens":118705,"cached_input_tokens":79104,"output_tokens":569}}}"#;
    rollout_of_terminal(&call, &format!("{ROLLOUT}{record}\n"));
    let reply = next.run(&call, &Ending::default()).unwrap();
    assert_eq!(
        reply.usage,
        Usage {
            input: 2000,
            output: 100,
            ..Usage::default()
        }
    );
}

#[test]
fn a_terminal_session_that_left_no_rollout_leaves_the_record_as_it_was() {
    let w = World::new();
    let codex = codex_in(&w, &OpenSandbox::default());
    let call = resumable(&w, &codex);
    let files = Files::of(&call);
    let before = thread(&files, &id(WORKER_ID)).unwrap();
    codex.foreground(&call).unwrap();

    codex.foreground_ended(call.session.id());
    // A session that was never started in a terminal has nothing to end.
    codex.foreground_ended(&id("never-attached"));

    assert_eq!(thread(&files, &id(WORKER_ID)).unwrap(), before);
}
