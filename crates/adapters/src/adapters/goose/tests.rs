use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;

use super::gateway_tests::{KEY, gateways, goose_on, on_gateway};
use super::*;
use crate::adapters::SandboxRuntime;
use crate::ports::{SessionId, Usage};
use crate::profile::WorkerProfile;
use crate::settings::{ContextSize, Effort, EndpointUrl, GatewayKind, GuardHook, HookEvent};
use crate::test::{OpenSandbox, StandInEndpoint};

// Written by hand from the event types in goose's source, because the builder
// had no model to record. #798 replaces them with recordings.
pub(super) const FRESH: &str = include_str!("../../../../../fixtures/goose-run-fresh.jsonl");
pub(super) const RESUMED: &str = include_str!("../../../../../fixtures/goose-run-resumed.jsonl");
const MAX_TURNS: &str = include_str!("../../../../../fixtures/goose-run-max-turns.jsonl");
const REFUSED: &str = include_str!("../../../../../fixtures/goose-run-refused.jsonl");
// The one fixture that is goose's own output: goose 1.54.0 with its server at a
// port nothing listens on, the temporary folder renamed.
pub(super) const UNREACHABLE: &str =
    include_str!("../../../../../fixtures/goose-run-unreachable.jsonl");

pub(super) const FRESH_ID: &str = "0b6f3c1e-7d2a-4f4e-9a51-3c8d2e6f1a07";
pub(super) const OTHER_ID: &str = "5a2e9d40-1c7b-4b38-8e6f-2d4a9c1b7e53";
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
    let argv = argv(call).unwrap();
    argv.into_iter().map(|a| a.into_string().unwrap()).collect()
}

// goose as a script that keeps its arguments, prints `stdout` and `stderr`, and
// exits with `code`.
pub(super) fn script(world: &World, stdout: &str, stderr: &str, code: u8) -> PathBuf {
    // Each script is a file of its own: `write_script` makes a file that is new.
    let n = world.scripts.replace(world.scripts.get() + 1);
    let (out, err, program) = (
        world.path(&format!("out-{n}")),
        world.path(&format!("err-{n}")),
        world.path(&format!("goose-{n}")),
    );
    std::fs::write(&out, stdout).unwrap();
    std::fs::write(&err, stderr).unwrap();
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\ncat {}\ncat {} >&2\nexit {code}\n",
        world.path("argv").display(),
        out.display(),
        err.display(),
    );
    crate::test::write_script(&program, &script);
    program
}

pub(super) fn stand_in(world: &World, stdout: &str) -> GooseCli {
    goose_with(world, script(world, stdout, "", 0))
}

pub(super) fn goose_with(world: &World, program: PathBuf) -> GooseCli {
    GooseCli::on(&Base::default().sandboxed(Arc::new(OpenSandbox::default()), world.path("home")))
        .with_program(program)
}

// Whether goose is offered no extension but `skills`
pub(super) fn only_skills(call: &AgentCall) -> bool {
    let argv = strings(call);
    let flags = argv.iter().filter(|a| a.starts_with("--with-")).count();
    flags == 1 && argv.windows(2).any(|a| a == ["--with-builtin", "skills"])
}

fn reviewer(world: &World, session: Session) -> AgentCall {
    world.call(URL, Role::Reviewer, session)
}

#[test]
fn a_new_reviewer_call_with_no_fence_names_its_session_and_loads_no_extension_but_skills() {
    let w = World::new();
    let mut call = reviewer(&w, Session::New(id(FRESH_ID)));
    call.prompt = "--help me, the model needs it".into();
    assert_eq!(
        strings(&call),
        [
            "run",
            "--no-profile",
            "--output-format",
            "stream-json",
            "--text=--help me, the model needs it",
            "--with-builtin",
            "skills",
            "--name",
            FRESH_ID,
        ]
    );
    assert!(only_skills(&call));
    for flag in ["--with-extension", "--recipe", "--resume"] {
        assert!(
            !strings(&call).iter().any(|a| a.starts_with(flag)),
            "{flag}"
        );
    }
}

#[test]
fn a_resumed_call_resumes_by_name_and_gets_its_instructions_again() {
    let w = World::new();
    std::fs::write(w.path("instructions.md"), "You are a reviewer.\nBe brief.").unwrap();
    let mut call = reviewer(&w, Session::Resume(id(FRESH_ID)));
    call.instructions = Some(w.path("instructions.md"));
    call.prompt = "and now?".into();
    assert_eq!(
        strings(&call),
        [
            "run",
            "--no-profile",
            "--output-format",
            "stream-json",
            "--text=and now?",
            "--with-builtin",
            "skills",
            "--system=You are a reviewer.\nBe brief.",
            "--resume",
            "--name",
            FRESH_ID,
        ]
    );
    call.session = Session::New(id(FRESH_ID));
    assert!(strings(&call).iter().any(|a| a.starts_with("--system=")));
}

#[test]
fn instructions_goose_cannot_be_given_fail_before_the_call() {
    let w = World::new();
    let mut call = reviewer(&w, Session::New(id(FRESH_ID)));
    call.instructions = Some(w.path("missing.md"));
    let err = stand_in(&w, FRESH).prepare(&call).unwrap_err();
    assert!(
        matches!(&err, AgentError::Setup(why) if why.contains("missing.md")),
        "{err:?}"
    );
}

#[test]
fn the_call_s_environment_relocates_goose_and_closes_every_door() {
    let w = World::new();
    let goose = GooseCli::on(&Base::default())
        .with_gateways(gateways("http://192.0.2.9:11434", GatewayKind::Openai));
    let call = on_gateway(&w, Role::Reviewer);
    goose.prepare(&call).unwrap();
    let upstream = goose.upstream(server(&call).unwrap()).unwrap();
    let files = Files::of(&call.settings);
    let vars = env_args(&call, server(&call).unwrap(), &files, &upstream, None);
    let mut vars: Vec<String> = vars.into_iter().map(|v| v.into_string().unwrap()).collect();
    vars.sort();
    let (root, scratch) = (w.path("settings.goose"), w.path("settings.tmp"));
    let mut expected = vec![
        "GOOSE_CONTEXT_LIMIT=65536".to_owned(),
        "GOOSE_DISABLE_KEYRING=1".to_owned(),
        "GOOSE_MODE=chat".to_owned(),
        "GOOSE_MODEL=qwen3.8:27b".to_owned(),
        format!("GOOSE_PATH_ROOT={}", root.display()),
        "GOOSE_PROVIDER=openai".to_owned(),
        "GOOSE_TELEMETRY_OFF=1".to_owned(),
        "OPENAI_API_KEY=kelpie".to_owned(),
        format!("OPENAI_BASE_URL={}", upstream.worker_url()),
        format!("TMPDIR={}", scratch.display()),
    ];
    expected.sort();
    assert_eq!(vars, expected);
    assert!(
        upstream
            .worker_url()
            .starts_with("http://model.kelpie.test/")
    );

    let shown = format!("{vars:?}{:?}", strings(&call));
    assert!(!shown.contains(KEY), "{shown}");
    let mut folders = vec![w.path("")];
    while let Some(path) = folders.pop() {
        if path.is_dir() {
            folders.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
        } else if path.is_file() {
            let text = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&text);
            assert!(!text.contains(KEY), "{} holds the key", path.display());
        }
    }
    for folder in ["config", "data", "state"] {
        assert!(root.join(folder).is_dir(), "{folder}");
    }
}

#[test]
fn the_sandbox_lets_goose_write_only_its_data_state_and_scratch_and_reach_only_the_forwarder() {
    let w = World::new();
    let call = reviewer(&w, Session::New(id(FRESH_ID)));
    let files = Files::of(&call.settings);
    let socket = w.path("forwarder.sock");
    let upstream = GooseCli::on(&Base::default())
        .upstream(server(&call).unwrap())
        .unwrap();
    let open = policy(
        &call,
        &files,
        &upstream,
        &socket,
        Path::new("/nowhere/goose"),
        &[],
    );
    let root = w.path("settings.goose");
    assert_eq!(
        open.write,
        [
            root.join("data"),
            root.join("state"),
            w.path("settings.tmp")
        ]
    );
    for read in [
        root.clone(),
        w.path("settings.tmp"),
        w.path("settings.stdout.jsonl"),
        w.path("settings.stderr.log"),
    ] {
        assert!(open.read.contains(&read), "{read:?}");
    }
    assert!(open.hosts.is_empty(), "{:?}", open.hosts);
    assert_eq!(
        open.denied_hosts,
        ["192.0.2.9", "localhost", "127.0.0.1", "[::1]"]
    );
    assert_eq!(open.denied_addresses, ["192.0.2.9"]);
    assert_eq!(
        open.forward,
        Some(Forward {
            host: "model.kelpie.test".into(),
            socket: socket.clone()
        })
    );
    assert!(!open.no_keychain);

    let mut covered = call.clone();
    covered.reach.block_keychain = true;
    assert!(
        policy(
            &covered,
            &files,
            &upstream,
            &socket,
            Path::new("/nowhere/goose"),
            &[]
        )
        .no_keychain
    );

    let mut reading = call.clone();
    reading.instructions = Some(w.path("instructions.md"));
    reading.plugin_dirs = vec![w.path("plugins/steps")];
    reading.reach.read = vec![w.path("extra")];
    let reads = policy(
        &reading,
        &files,
        &upstream,
        &socket,
        Path::new("/nowhere/goose"),
        &[],
    );
    for read in ["instructions.md", "plugins/steps", "extra"] {
        assert!(reads.read.contains(&w.path(read)), "{read}");
    }
}

#[test]
fn goose_reads_no_harnesss_login_its_own_included() {
    let w = World::new();
    let call = reviewer(&w, Session::New(id(FRESH_ID)));
    let upstream = GooseCli::on(&Base::default())
        .upstream(server(&call).unwrap())
        .unwrap();
    let files = Files::of(&call.settings);
    let policy = policy(
        &call,
        &files,
        &upstream,
        &w.path("forwarder.sock"),
        Path::new("/nowhere/goose"),
        &[],
    );
    for own in fence::HARNESSES {
        for credentials in own.credentials {
            assert!(
                policy.no_read.iter().any(|p| p == credentials),
                "{credentials}"
            );
        }
    }
    let goose_own = fence::HARNESSES
        .iter()
        .any(|own| own.credentials.iter().any(|c| c.contains("goose")));
    assert!(goose_own, "goose's own login is among them");
}

#[test]
fn a_pm_never_runs_on_goose() {
    let w = World::new();
    let goose = stand_in(&w, FRESH);
    let call = w.call(URL, Role::Pm, Session::New(id(FRESH_ID)));
    let err = goose.prepare(&call).unwrap_err();
    assert!(
        matches!(&err, AgentError::Setup(why) if why.contains("Claude Code alone")),
        "{err:?}"
    );
    assert!(!w.path("settings.goose").exists(), "nothing was made");
    goose
        .prepare(&reviewer(&w, Session::New(id(FRESH_ID))))
        .unwrap();
}

#[test]
fn what_goose_cannot_give_a_fenced_call_fails_before_the_call() {
    let w = World::new();
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(OTHER_ID)));
    call.role = Role::Reviewer;
    call.tools = Tools::Review;
    let fence = call.reach.fence.as_mut().unwrap();
    fence.hooks = vec![GuardHook {
        event: HookEvent::PreToolUse,
        matcher: None,
        command: "true".to_owned().try_into().unwrap(),
    }];
    let err = stand_in(&w, FRESH).prepare(&call).unwrap_err();
    assert!(
        matches!(&err, AgentError::Setup(why) if why.contains("worker.guard_hooks")),
        "{err:?}"
    );
}

#[test]
fn a_fence_adds_variables_but_replaces_none_of_the_calls() {
    let w = World::new();
    let goose = GooseCli::on(&Base::default())
        .with_gateways(gateways("http://192.0.2.9:11434", GatewayKind::Openai));
    let mut call = on_gateway(&w, Role::Representative);
    call.tools = Tools::Answer;
    let mut fence = w
        .fenced(URL, Path::new("/k/kelpie"), Session::New(id(OTHER_ID)))
        .reach
        .fence
        .unwrap();
    fence.env.clear();
    for (name, value) in [
        ("GOOSE_MODE", "auto"),
        ("OPENAI_API_KEY", "real"),
        ("FROM_THE_FENCE", "1"),
    ] {
        fence.env.insert(name.to_owned(), value.to_owned());
    }
    call.reach.fence = Some(fence);
    goose.prepare(&call).unwrap();
    let upstream = goose.upstream(server(&call).unwrap()).unwrap();
    let files = Files::of(&call.settings);
    let vars = env_args(&call, server(&call).unwrap(), &files, &upstream, None);
    let vars: Vec<String> = vars.into_iter().map(|v| v.into_string().unwrap()).collect();
    assert!(vars.contains(&"FROM_THE_FENCE=1".to_owned()), "{vars:?}");
    assert!(vars.contains(&"GOOSE_MODE=chat".to_owned()), "{vars:?}");
    assert!(
        vars.contains(&"OPENAI_API_KEY=kelpie".to_owned()),
        "{vars:?}"
    );
    assert_eq!(
        vars.iter().filter(|v| v.starts_with("GOOSE_MODE=")).count(),
        1
    );
    assert_eq!(
        vars.iter()
            .filter(|v| v.starts_with("OPENAI_API_KEY="))
            .count(),
        1
    );
}

#[test]
fn a_call_in_a_folder_with_project_plugins_or_goose_settings_is_refused_naming_it() {
    let w = World::new();
    let goose = stand_in(&w, FRESH);
    let call = reviewer(&w, Session::New(id(FRESH_ID)));
    goose.prepare(&call).unwrap();
    for folder in [".agents/plugins", ".config/goose"] {
        let path = w.path("wt").join(folder);
        std::fs::create_dir_all(&path).unwrap();
        let err = goose.prepare(&call).unwrap_err();
        assert!(
            matches!(&err, AgentError::Setup(why) if why.contains(path.to_str().unwrap())),
            "{folder}: {err:?}"
        );
        std::fs::remove_dir_all(&path).unwrap();
        goose.prepare(&call).unwrap();
    }
}

#[test]
fn a_resume_with_no_session_is_no_session_and_takes_no_lease() {
    let server = StandInEndpoint::start([]).like_paddock(KEY, &[]);
    let w = World::new();
    let goose = goose_on(&w, &server, FRESH, 0);
    let mut call = on_gateway(&w, Role::Reviewer);
    call.session = Session::Resume(id(FRESH_ID));
    goose.prepare(&call).unwrap();
    let err = goose.run(&call, &Ending::default()).unwrap_err();
    assert_eq!(err, AgentError::NoSession(Harness::Goose, id(FRESH_ID)));
    assert_eq!(err.to_string(), format!("goose has no session {FRESH_ID}"));
    assert_eq!(server.seen(), Vec::<String>::new(), "no lease was taken");
    assert!(!w.path("argv").exists(), "goose never ran");
}

#[test]
fn a_session_goose_has_lost_is_no_session_when_goose_says_so() {
    let w = World::new();
    let first = reviewer(&w, Session::New(id(FRESH_ID)));
    let goose = stand_in(&w, FRESH);
    goose.prepare(&first).unwrap();
    goose.run(&first, &Ending::default()).unwrap();

    let said = format!("Error: No session found with name '{FRESH_ID}'\n");
    let lost = goose_with(&w, script(&w, "", &said, 1));
    let call = reviewer(&w, Session::Resume(id(FRESH_ID)));
    let err = lost.run(&call, &Ending::default()).unwrap_err();
    assert_eq!(err, AgentError::NoSession(Harness::Goose, id(FRESH_ID)));
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
fn a_call_that_names_no_model_server_is_refused() {
    let w = World::new();
    let mut call = reviewer(&w, Session::New(id(FRESH_ID)));
    call.harness = AgentHarness::ClaudeCode;
    let err = stand_in(&w, FRESH).prepare(&call).unwrap_err();
    assert!(matches!(err, AgentError::Setup(_)), "{err:?}");
}

fn read_new(code: i32, stdout: &str, stderr: &str) -> Result<AgentReply, AgentError> {
    let session = Session::New(id(FRESH_ID));
    output::read(&output(code, stdout, stderr), &session, Spent::default()).reply
}

#[test]
fn a_turn_answers_with_its_last_message_and_the_tokens_its_session_spent_in_it() {
    assert_eq!(
        read_new(0, FRESH, ""),
        Ok(AgentReply {
            session_id: id(FRESH_ID),
            text: "ok".into(),
            usage: Usage {
                input: 20,
                cache_write: 0,
                cache_write_5m: 0,
                cache_read: 0,
                output: 3,
            },
            session_cost: None,
            context: None,
            compactions: 0,
        })
    );
}

#[test]
fn a_resumed_turn_reports_only_its_own_tokens() {
    // The fixture's totals are the session's, the first turn's among them.
    let session = Session::Resume(id(FRESH_ID));
    let before = Spent {
        input_tokens: 20,
        output_tokens: 3,
        ..Spent::default()
    };
    let read = output::read(&output(0, RESUMED, ""), &session, before);
    let reply = read.reply.unwrap();
    assert_eq!(reply.text, "I replied with the word ok.");
    assert_eq!((reply.usage.input, reply.usage.output), (49, 12));
    let totals = read.spent.unwrap();
    assert_eq!((totals.input_tokens, totals.output_tokens), (69, 15));
}

#[test]
fn a_session_s_totals_are_kept_between_its_calls() {
    let w = World::new();
    let call = reviewer(&w, Session::New(id(FRESH_ID)));
    let first = stand_in(&w, FRESH);
    first.prepare(&call).unwrap();
    first.run(&call, &Ending::default()).unwrap();

    let call = reviewer(&w, Session::Resume(id(FRESH_ID)));
    let second = stand_in(&w, RESUMED)
        .run(&call, &Ending::default())
        .unwrap();
    assert_eq!((second.usage.input, second.usage.output), (49, 12));
    let third = stand_in(&w, RESUMED)
        .run(&call, &Ending::default())
        .unwrap();
    assert_eq!(
        (third.usage.input, third.usage.output),
        (0, 0),
        "the totals the second turn left are the third's start"
    );
}

#[test]
fn a_session_s_totals_live_in_a_file_under_the_goose_root() {
    let w = World::new();
    let files = Files::of(&w.path("settings.json"));
    files.prepare().unwrap();
    let session = id(FRESH_ID);
    assert_eq!(files.spent(&session).unwrap(), None);
    let spent = Spent {
        input_tokens: 5,
        ..Spent::default()
    };
    files.keep(&session, spent).unwrap();
    assert_eq!(files.spent(&session).unwrap(), Some(spent));
    assert_eq!(files.spent(&id(OTHER_ID)).unwrap(), None);
    let err = files.keep(&id("../escape"), spent).unwrap_err();
    assert!(matches!(err, AgentError::Setup(_)), "{err:?}");
}

#[test]
fn a_new_session_counts_from_nothing_even_where_an_earlier_one_left_totals() {
    let w = World::new();
    let call = reviewer(&w, Session::New(id(FRESH_ID)));
    let goose = stand_in(&w, RESUMED);
    goose.prepare(&call).unwrap();
    goose.run(&call, &Ending::default()).unwrap();
    let again = stand_in(&w, FRESH).run(&call, &Ending::default()).unwrap();
    assert_eq!((again.usage.input, again.usage.output), (20, 3));
}

#[test]
fn a_turn_whose_tool_calls_the_fence_refused_answers_with_the_text_after_them() {
    let reply = read_new(0, REFUSED, "").unwrap();
    assert_eq!(reply.text, "Both calls were refused, so I changed nothing.");
    assert_eq!(reply.usage.output, 51);
}

#[test]
fn a_turn_that_hit_goose_s_turn_limit_fails_saying_so() {
    let err = read_new(0, MAX_TURNS, "").unwrap_err();
    let AgentError::Failed(Harness::Goose, why) = &err else {
        panic!("{err:?}");
    };
    assert!(why.contains("turn limit"), "{why}");
    assert!(
        why.contains("I've reached the maximum number of actions"),
        "{why}"
    );
}

#[test]
fn output_that_cannot_be_read_is_unreadable_and_an_error_carries_only_its_end() {
    let err = read_new(0, "hello", "").unwrap_err();
    assert_eq!(err, AgentError::Unreadable(Harness::Goose, "hello".into()));
    let err = read_new(0, "", "").unwrap_err();
    assert!(matches!(err, AgentError::Unreadable(..)), "{err:?}");

    // A message that cannot be read is not mistaken for the one before it.
    let broken = r#"{"type":"message","message":{"role":"assistant","content":7}}"#;
    let err = read_new(0, &format!("{FRESH}{broken}\n"), "").unwrap_err();
    assert!(matches!(err, AgentError::Unreadable(..)), "{err:?}");

    let long = format!("{}é tail", "x".repeat(10_000));
    let err = read_new(0, &long, "").unwrap_err();
    let AgentError::Unreadable(_, text) = &err else {
        panic!("{err:?}");
    };
    assert!(
        text.len() <= 2048 && text.ends_with("é tail"),
        "{}",
        text.len()
    );
    let err = read_new(1, "", &long).unwrap_err();
    assert!(
        err.to_string().len() < 2048 + 40,
        "{}",
        err.to_string().len()
    );
    let error = format!(r#"{{"type":"error","error":"{long}"}}"#);
    let err = read_new(1, &error, "").unwrap_err();
    assert!(
        err.to_string().len() < 2048 + 40,
        "{}",
        err.to_string().len()
    );
    assert!(err.to_string().ends_with("é tail"), "{err}");
}

#[test]
fn an_error_goose_exits_with_is_the_calls_failure_and_the_next_turn_still_counts_right() {
    let w = World::new();
    let call = reviewer(&w, Session::New(id(FRESH_ID)));
    let goose = stand_in(&w, FRESH);
    goose.prepare(&call).unwrap();
    goose.run(&call, &Ending::default()).unwrap();

    let error = r#"{"type":"error","error":"400 context length exceeded"}"#;
    let call = reviewer(&w, Session::Resume(id(FRESH_ID)));
    let err = goose_with(&w, script(&w, error, "", 1))
        .run(&call, &Ending::default())
        .unwrap_err();
    assert_eq!(
        err,
        AgentError::Failed(Harness::Goose, "400 context length exceeded".into())
    );
    let reply = stand_in(&w, RESUMED)
        .run(&call, &Ending::default())
        .unwrap();
    assert_eq!((reply.usage.input, reply.usage.output), (49, 12));
}

pub(super) struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    scripts: std::cell::Cell<u32>,
}

impl World {
    // Short, and canonical, so a recorded path reads the same each time.
    pub(super) fn new() -> Self {
        let dir = crate::test::short_tempdir();
        let root = dir.path().canonicalize().unwrap();
        for folder in ["wt", "build", "outside"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        Self {
            _dir: dir,
            root,
            scripts: std::cell::Cell::new(0),
        }
    }

    pub(super) fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    pub(super) fn call(&self, url: &str, role: Role, session: Session) -> AgentCall {
        let server = ModelServer {
            host: ModelHost::Url(EndpointUrl::try_from(url.to_owned()).unwrap()),
            context: ContextSize::try_from(65_536).unwrap(),
        };
        AgentCall {
            issue: crate::ticket::TicketId::from_issue(7),
            settings: self.path("settings.json"),
            ..AgentCall::new(
                role,
                Tools::Review,
                AgentHarness::Goose(server),
                "qwen3.8:27b",
                Effort::Low,
                session,
                self.path("wt"),
                "go",
            )
        }
    }

    pub(super) fn fenced(&self, url: &str, kelpie: &Path, session: Session) -> AgentCall {
        let (wt, build, git) = (self.path("wt"), self.path("build"), self.path("wt/.git"));
        let home = self.path("home");
        let profile = WorkerProfile {
            worktree: &wt,
            build: &build,
            home: &home,
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

const NEEDS: &str = "needs KELPIE_GOOSE_URL=<server>/v1 running qwen3.8:27b, \
                     KELPIE_GOOSE_RECORD=<dir> for what it prints, and KELPIE_BIN=<path> \
                     (a fenced worker's recording only). Run it under \
                     `kelpie lease run gpu --model <model>`";

fn env(name: &str) -> String {
    std::env::var(name).expect(NEEDS)
}

// The real goose on the real model, under the real sandbox, recording what it
// prints: a turn, the same session resumed, and a turn against a server
// nothing listens on. #798 replaces the fixtures with these recordings, the
// temporary folder renamed.
#[test]
#[ignore = "runs goose on a local model"]
fn record_a_turn_a_resumed_turn_and_an_unreachable_server() {
    let url = env("KELPIE_GOOSE_URL");
    let record = PathBuf::from(env("KELPIE_GOOSE_RECORD"));
    let goose = GooseCli::on(&Base::default().sandboxed(
        Arc::new(SandboxRuntime::under_test()),
        std::env::var_os("HOME").unwrap().into(),
    ));
    let world = World::new();
    let save = |name: &str, out: &Output| {
        let text = String::from_utf8_lossy(&out.stdout)
            .replace(world.root.to_str().unwrap(), "/tmp/kelpie-goose");
        std::fs::write(record.join(name), text).unwrap();
    };

    let mut fresh = world.call(&url, Role::Reviewer, Session::New(id(FRESH_ID)));
    fresh.prompt = "Reply with the single word ok.".into();
    goose.prepare(&fresh).unwrap();
    let out = run_raw(&goose, &fresh);
    save("goose-run-fresh.jsonl", &out);
    let first = output::read(&out, &fresh.session, Spent::default());
    assert!(first.reply.unwrap().text.to_lowercase().contains("ok"));

    let mut again = world.call(&url, Role::Reviewer, Session::Resume(id(FRESH_ID)));
    again.prompt = "Which word did you reply with? Answer in one word.".into();
    let out = run_raw(&goose, &again);
    save("goose-run-resumed.jsonl", &out);
    let second = output::read(&out, &again.session, first.spent.unwrap());
    assert!(second.reply.unwrap().text.to_lowercase().contains("ok"));

    let mut down = world.call(
        "http://127.0.0.1:9/v1",
        Role::Reviewer,
        Session::New(id(OTHER_ID)),
    );
    down.prompt = "Reply with the single word ok.".into();
    goose.prepare(&down).unwrap();
    let out = run_raw(&goose, &down);
    save("goose-run-unreachable.jsonl", &out);
    let read = output::read(&out, &down.session, Spent::default());
    assert!(
        matches!(read.reply, Err(AgentError::Unreachable(..))),
        "{:?}",
        read.reply
    );
}

// A fenced worker asked to write into `.claude/` and to merge a pull request,
// recording what goose prints when the hook refuses both. It needs a build of
// kelpie to run as the hook: `KELPIE_BIN` names it.
#[test]
#[ignore = "runs a fenced goose worker on a local model"]
fn record_a_worker_whose_write_and_shell_the_fence_refuses() {
    let url = env("KELPIE_GOOSE_URL");
    let record = PathBuf::from(env("KELPIE_GOOSE_RECORD"));
    let goose = GooseCli::on(&Base::default().sandboxed(
        Arc::new(SandboxRuntime::under_test()),
        std::env::var_os("HOME").unwrap().into(),
    ));
    let world = World::new();

    let kelpie = PathBuf::from(env("KELPIE_BIN"));
    let mut call = world.fenced(&url, &kelpie, Session::New(id(FRESH_ID)));
    call.prompt = "Write {} to .claude/settings.json, then run `gh pr merge 1`. \
                   If either is refused, say so in one sentence."
        .into();
    goose.prepare(&call).unwrap();
    let out = run_raw(&goose, &call);
    let text = String::from_utf8_lossy(&out.stdout)
        .replace(world.root.to_str().unwrap(), "/tmp/kelpie-goose");
    std::fs::write(record.join("goose-run-refused.jsonl"), text).unwrap();
    let read = output::read(&out, &call.session, Spent::default());
    assert!(read.reply.is_ok(), "{:?}", read.reply);
}

fn run_raw(goose: &GooseCli, call: &AgentCall) -> Output {
    let (mut command, _forwarder) = goose.sandboxed_command(call, None).unwrap();
    command.stdin(std::process::Stdio::null()).output().unwrap()
}
