use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;

use super::*;
use crate::adapters::SandboxRuntime;
use crate::ports::{Reach, Role};
use crate::preview::Tools as KelpieTools;
use crate::profile::WorkerProfile;
use crate::settings::{ContextSize, Effort, EndpointUrl};
use crate::test::OpenSandbox;

// Recorded from pi 0.85.1 on qwen3.8:27b through Ollama, by the ignored test
// below, with the temporary folder renamed: a new session asked to say ok...
const FRESH: &str = include_str!("../../../fixtures/pi-p-fresh.jsonl");
// ...the same session resumed and asked what it said...
const RESUMED: &str = include_str!("../../../fixtures/pi-p-resumed.jsonl");
// ...and a worker refused a write by `kelpie confine`, a command by the
// sandbox and another by `kelpie guard`, its streaming deltas left out.
const REFUSED: &str = include_str!("../../../fixtures/pi-p-refused.jsonl");

const FRESH_ID: &str = "0b6f3c1e-7d2a-4f4e-9a51-3c8d2e6f1a07";
const WORKER_ID: &str = "5a2e9d40-1c7b-4b38-8e6f-2d4a9c1b7e53";
const URL: &str = "http://10.0.0.9:11434/v1";

fn output(code: i32, stdout: &str, stderr: &str) -> Output {
    Output {
        status: ExitStatus::from_raw(code << 8),
        stdout: stdout.into(),
        stderr: stderr.into(),
    }
}

fn id(id: &str) -> SessionId {
    SessionId(id.into())
}

fn strings(call: &AgentCall) -> Vec<String> {
    argv(call, &Files::of(call))
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect()
}

// A stand-in for pi that keeps its arguments and prints `stdout`.
fn stand_in(world: &World, stdout: &str) -> PiCli {
    std::fs::write(world.path("stdout.jsonl"), stdout).unwrap();
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\ncat {}\n",
        world.path("argv").display(),
        world.path("stdout.jsonl").display()
    );
    crate::test::write_script(&world.path("pi"), &script);
    ClaudeCli::default()
        .sandboxed(Arc::new(OpenSandbox::default()), world.path("home"))
        .pi()
        .with_program(world.path("pi"))
}

#[test]
fn a_new_worker_session_carries_its_model_tools_and_guard() {
    let w = World::new();
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    call.instructions = Some(PathBuf::from("/k/worker/instructions.md"));
    call.prompt = "implement #7".into();
    let home = w.path("worker/settings.pi");
    assert_eq!(
        strings(&call),
        [
            "-p",
            "--mode",
            "json",
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
            "--session-id",
            WORKER_ID,
            "--tools",
            "read,bash,edit,write,grep,find,ls",
            "--append-system-prompt",
            "/k/worker/instructions.md",
            "--extension",
            w.path("worker/settings.guard.ts").to_str().unwrap(),
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

    let mut judge = w.call(URL, Role::Judge, Session::New(id(FRESH_ID)));
    assert!(strings(&judge).iter().any(|a| a == "--no-tools"));
    judge.reach.read = vec![w.path("shots")];
    let argv = strings(&judge);
    let at = argv.iter().position(|a| a == "--tools").unwrap();
    assert_eq!(argv[at + 1], "read");
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

#[test]
fn prepare_writes_pis_own_home_with_the_one_model() {
    let w = World::new();
    let call = w.call(URL, Role::Judge, Session::New(id(FRESH_ID)));
    stand_in(&w, FRESH).prepare(&call).unwrap();
    let home = w.path("worker/settings.pi");
    let models: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join("models.json")).unwrap()).unwrap();
    assert_eq!(
        models,
        json!({ "providers": { "kelpie": {
            "baseUrl": URL,
            "api": "openai-completions",
            "apiKey": "kelpie",
            "models": [{ "id": "qwen3.8:27b", "contextWindow": 65536, "reasoning": true }],
        } } })
    );
    assert!(home.join("sessions").is_dir());
    assert!(!w.path("worker/settings.guard.ts").exists());
}

#[test]
fn a_fenced_call_gets_a_guard_that_runs_kelpies_checks() {
    let w = World::new();
    let call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    stand_in(&w, FRESH).prepare(&call).unwrap();
    let guard = std::fs::read_to_string(w.path("worker/settings.guard.ts")).unwrap();
    assert!(!guard.contains("__CHECKS__"), "{guard}");
    let (wt, build) = (w.path("wt"), w.path("build"));
    let line = guard
        .lines()
        .find(|l| l.starts_with("const CHECKS"))
        .unwrap();
    let checks: serde_json::Value =
        serde_json::from_str(line.split_once(" = ").unwrap().1.trim_end_matches(';')).unwrap();
    assert_eq!(checks["kelpie"], "/k/kelpie");
    assert_eq!(checks["confine"], json!(["confine", wt, build]));
    assert_eq!(checks["guard"][0], "guard");
    assert_eq!(checks["guard"][2], json!(wt));
}

#[test]
fn what_pi_cannot_give_a_worker_fails_before_the_call() {
    let w = World::new();
    let pi = stand_in(&w, FRESH);
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    call.mcp_config = Some(w.path("worker/mcp.json"));
    let err = pi.prepare(&call).unwrap_err();
    assert!(
        matches!(&err, AgentError::Setup(why) if why.contains("no MCP servers")),
        "{err:?}"
    );
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
fn the_sandbox_lets_pi_write_only_its_sessions_and_reach_only_its_server() {
    let w = World::new();
    let call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    let files = Files::of(&call);
    let AgentHarness::Pi(server) = &call.harness else {
        unreachable!()
    };
    let policy = policy(&call, server, &files);
    let home = w.path("worker/settings.pi");
    assert!(policy.write.contains(&home.join("sessions")));
    assert!(!policy.write.contains(&home));
    assert!(policy.write.contains(&w.path("wt")));
    assert!(policy.read.contains(&home));
    assert!(policy.read.contains(&w.path("worker/settings.guard.ts")));
    assert!(policy.hosts.contains(&"10.0.0.9".to_owned()));
    for login in ["~/.pi/**", "~/.codex/**", "~/.claude.json"] {
        assert!(policy.no_read.iter().any(|p| p == login), "{login}");
    }
    let open = w.call(URL, Role::Judge, Session::New(id(FRESH_ID)));
    let policy = super::policy(&open, server, &Files::of(&open));
    assert_eq!(
        policy.write,
        [
            home.join("sessions"),
            w.path("worker/settings.tmp"),
            home.join("auth.json.lock"),
            home.join("models-store.json.lock"),
        ]
    );
    assert_eq!(policy.hosts, ["10.0.0.9"]);
}

#[test]
fn a_servers_host_is_its_url_without_scheme_port_or_path() {
    assert_eq!(host("http://10.0.0.9:11434/v1"), "10.0.0.9");
    assert_eq!(host("https://models.example/v1"), "models.example");
    assert_eq!(host("http://[::1]:8080/v1"), "::1");
    assert_eq!(host("http://user@box.local:1234/v1"), "box.local");
}

#[test]
fn a_turn_answers_with_its_text_and_its_tokens_and_no_price() {
    let w = World::new();
    let pi = stand_in(&w, FRESH);
    let call = w.call(URL, Role::Judge, Session::New(id(FRESH_ID)));
    pi.prepare(&call).unwrap();
    let reply = pi.run(&call).unwrap();
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
    let call = w.call(URL, Role::Judge, Session::Resume(id(FRESH_ID)));
    pi.prepare(&call).unwrap();
    let err = pi.run(&call).unwrap_err();
    assert_eq!(err, AgentError::NoSession(Harness::Pi, id(FRESH_ID)));
    assert_eq!(err.to_string(), format!("pi has no session {FRESH_ID}"));

    let sessions = w.path("worker/settings.pi/sessions");
    std::fs::write(
        sessions.join(format!("2026-10-01T03-00-00-000Z_{FRESH_ID}.jsonl")),
        "",
    )
    .unwrap();
    let second = pi.run(&call).unwrap();
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
fn a_failed_call_says_why() {
    let err = parse_result(&output(1, "", "Error: model not found"), &id(FRESH_ID)).unwrap_err();
    assert_eq!(
        err,
        AgentError::Failed(Harness::Pi, "Error: model not found".into())
    );
    assert_eq!(err.to_string(), "pi failed: Error: model not found");

    let errored = FRESH.replace(
        r#""stopReason":"stop""#,
        r#""stopReason":"error","errorMessage":"connection refused""#,
    );
    let err = parse_result(&output(0, &errored, ""), &id(FRESH_ID)).unwrap_err();
    assert_eq!(
        err,
        AgentError::Failed(Harness::Pi, "connection refused".into())
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
    let mut call = w.call(URL, Role::Judge, Session::New(id(FRESH_ID)));
    call.harness = AgentHarness::ClaudeCode;
    let err = stand_in(&w, FRESH).prepare(&call).unwrap_err();
    assert!(matches!(err, AgentError::Setup(_)), "{err:?}");
}

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    // Under `/tmp`, and canonical, so a recorded path reads the same each time.
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let root = dir.path().canonicalize().unwrap();
        for folder in ["wt", "build", "worker", "outside"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        Self { _dir: dir, root }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    fn server(url: &str) -> ModelServer {
        ModelServer {
            url: EndpointUrl::try_from(url.to_owned()).unwrap(),
            context: ContextSize::try_from(65_536).unwrap(),
        }
    }

    fn call(&self, url: &str, role: Role, session: Session) -> AgentCall {
        AgentCall {
            harness: AgentHarness::Pi(Self::server(url)),
            role,
            issue: 7,
            model: "qwen3.8:27b".into(),
            effort: Effort::Low,
            session,
            cwd: self.path("wt"),
            settings: self.path("worker/settings.json"),
            instructions: None,
            prompt: String::new(),
            timeout: None,
            mcp_config: None,
            plugin_dirs: Vec::new(),
            tools: Tools::Answer,
            reach: Reach::default(),
            lease: None,
        }
    }

    fn fenced(&self, url: &str, kelpie: &Path, session: Session) -> AgentCall {
        let (wt, build, git) = (self.path("wt"), self.path("build"), self.path("wt/.git"));
        let profile = WorkerProfile {
            worktree: &wt,
            build: &build,
            git_common_dir: &git,
            git_dir: &git,
            branch: "kelpie/7",
            kelpie,
            kelpie_home: &self.path("kelpie"),
            repo: &self.path("repo"),
            private_names: &[],
            guard_hooks: &[],
            allowed_domains: &[],
            build_env: &BTreeMap::new(),
            preview: None,
            shep_home: &self.path("shep"),
        };
        AgentCall {
            role: Role::Worker,
            tools: Tools::Work,
            reach: profile.reach(),
            ..self.call(url, Role::Worker, session)
        }
    }
}

const NEEDS: &str = "needs KELPIE_TOOLS=<dir> from `shep kelpie tools install`, \
                     KELPIE_PI_URL=<server>/v1 running qwen3.8:27b, KELPIE_BIN=<kelpie>, \
                     and KELPIE_PI_RECORD=<dir> for what it prints. Run it under \
                     `kelpie lease run gpu`";

fn env(name: &str) -> String {
    std::env::var(name).expect(NEEDS)
}

// The real pi on the real model, under the real sandbox, recording what it
// prints: a turn, the same session resumed, and a worker whose file write
// `kelpie confine` refuses, one command the sandbox refuses and another
// `kelpie guard` refuses. The fixtures
// are these recordings with the temporary folder renamed.
#[test]
#[ignore = "runs pi on a local model"]
fn record_a_turn_a_resumed_turn_and_two_refusals() {
    let (url, kelpie) = (env("KELPIE_PI_URL"), PathBuf::from(env("KELPIE_BIN")));
    let record = PathBuf::from(env("KELPIE_PI_RECORD"));
    let tools = KelpieTools::at(env("KELPIE_TOOLS").into());
    let pi = ClaudeCli::default()
        .sandboxed(
            Arc::new(SandboxRuntime::new(tools)),
            std::env::var_os("HOME").unwrap().into(),
        )
        .pi();
    let world = World::new();
    let id = SessionId("0b6f3c1e-7d2a-4f4e-9a51-3c8d2e6f1a07".into());
    let save = |name: &str, out: &Output| {
        let text = String::from_utf8_lossy(&out.stdout)
            .replace(world.root.to_str().unwrap(), "/tmp/kelpie-pi");
        std::fs::write(record.join(name), text).unwrap();
    };

    let mut fresh = world.call(&url, Role::Judge, Session::New(id.clone()));
    fresh.prompt = "Reply with the single word ok.".into();
    pi.prepare(&fresh).unwrap();
    let out = run_raw(&pi, &fresh);
    save("pi-p-fresh.jsonl", &out);
    let first = parse_result(&out, &id).unwrap();

    let mut again = world.call(&url, Role::Judge, Session::Resume(id.clone()));
    again.prompt = "Which word did you reply with? Answer in one word.".into();
    let out = run_raw(&pi, &again);
    save("pi-p-resumed.jsonl", &out);
    let second = parse_result(&out, &id).unwrap();
    assert!(second.text.to_lowercase().contains("ok"), "{second:?}");
    assert!(second.usage.input + second.usage.cache_read > first.usage.input);

    let worker_id = SessionId("5a2e9d40-1c7b-4b38-8e6f-2d4a9c1b7e53".into());
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
    let mut command = pi.sandboxed_command(call).unwrap();
    command.stdin(std::process::Stdio::null()).output().unwrap()
}
