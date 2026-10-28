use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::adapters::{Base, LambLabels};
use crate::ports::{Agents, Ending, Tools};
use crate::settings::Effort;
use crate::test::OpenSandbox;

#[test]
fn a_runners_calls_on_every_harness_leave_kelpies_codex_login_unread() {
    let dir = tempfile::tempdir().unwrap();
    let login = dir.path().join("codex");
    let claude =
        ClaudeCli::on(&Base::default().in_runtime(dir.path().join("home"), &login, Vec::new()));
    let settings = dir.path().join("call.srt.json");
    claude
        .base
        .sandbox
        .wrap(
            &crate::ports::Policy::default(),
            &settings,
            &std::process::Command::new("claude"),
        )
        .unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    let unread = format!("{}/**", login.display());
    assert!(
        written["filesystem"]["denyRead"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == unread.as_str()),
        "{written}"
    );
}

#[test]
fn a_calls_activity_is_when_its_session_transcript_was_last_written() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let claude =
        ClaudeCli::on(&Base::default().sandboxed(Arc::new(OpenSandbox::default()), home.clone()));
    let mut call = call(Role::Worker, fresh());
    call.settings = dir.path().join("koji/worker/settings.json");
    call.cwd = dir.path().join("wt");
    std::fs::create_dir_all(&call.cwd).unwrap();
    assert_eq!(
        claude.last_active(&call),
        CallActivity::Nothing,
        "no transcript yet"
    );

    let folder = sandbox::transcripts(&sandbox::config_folder(&call), &call.cwd).unwrap();
    std::fs::create_dir_all(&folder).unwrap();
    let transcript = folder.join(format!("{}.jsonl", call.session.id().0));
    std::fs::write(&transcript, "{\"type\":\"assistant\"}\n").unwrap();
    assert_eq!(
        claude.last_active(&call),
        crate::ports::written_at(&transcript)
    );
    assert!(matches!(claude.last_active(&call), CallActivity::At(_)));
}

#[test]
fn a_calls_transcript_is_in_the_folder_claude_code_names_for_its_working_folder() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let claude =
        ClaudeCli::on(&Base::default().sandboxed(Arc::new(OpenSandbox::default()), home.clone()));
    let mut call = call(Role::Worker, fresh());
    call.settings = dir.path().join("koji/worker/settings.json");
    call.cwd = dir.path().join("wt");
    std::fs::create_dir_all(&call.cwd).unwrap();
    assert_eq!(
        claude.transcript(&call),
        Some(crate::ports::Transcript {
            folder: sandbox::transcripts(&sandbox::config_folder(&call), &call.cwd).unwrap(),
            threads: None,
        })
    );
}

// Recorded from Claude Code 2.1.283 on Haiku: a call asked to say ok.
const RESULT: &str = include_str!("../../../../../fixtures/claude-p-result.json");
// Recorded from Claude Code 2.1.283 on Haiku: a new session, then the
// same session resumed for a second call.
const FRESH: &str = include_str!("../../../../../fixtures/claude-p-fresh.json");
const RESUMED: &str = include_str!("../../../../../fixtures/claude-p-resumed.json");
// Recorded: `--resume` onto a session killed before it wrote a transcript.
const NO_SESSION_STDERR: &str = include_str!("../../../../../fixtures/claude-p-no-session.stderr");

fn output(code: i32, stdout: &str, stderr: &str) -> Output {
    Output {
        status: crate::test::exited(code),
        stdout: stdout.into(),
        stderr: stderr.into(),
    }
}

fn resume(id: &str) -> Session {
    Session::Resume(SessionId(id.into()))
}

fn fresh() -> Session {
    Session::New(SessionId("7e812e8a-3bf1-42a7-bddf-0ab283b7372a".into()))
}

fn call(role: Role, session: Session) -> AgentCall {
    AgentCall {
        issue: crate::ticket::TicketId::from_issue(6),
        settings: PathBuf::from("/k/worker/settings.json"),
        instructions: Some(PathBuf::from("/k/worker/instructions.md")),
        ..AgentCall::new(
            role,
            Tools::Work,
            crate::settings::AgentHarness::ClaudeCode,
            "claude-sonnet-5",
            Effort::Medium,
            session,
            PathBuf::from("/tmp"),
            "implement #6",
        )
    }
}

fn strings(call: &AgentCall) -> Vec<String> {
    argv(call)
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect()
}

#[test]
fn a_new_worker_session_carries_the_whole_profile() {
    assert_eq!(
        strings(&call(Role::Worker, fresh())),
        [
            "-p",
            "implement #6",
            "--model",
            "claude-sonnet-5",
            "--effort",
            "medium",
            "--setting-sources",
            "project",
            "--settings",
            "/k/worker/settings.json",
            "--tools",
            "Bash,Read,Edit,Write,Glob,Grep,Skill",
            "--strict-mcp-config",
            "--output-format",
            "json",
            "--permission-mode",
            "bypassPermissions",
            "--session-id",
            "7e812e8a-3bf1-42a7-bddf-0ab283b7372a",
            "--append-system-prompt-file",
            "/k/worker/instructions.md",
        ]
    );
}

#[test]
fn a_resumed_session_names_it_and_keeps_its_instructions() {
    let argv = strings(&call(Role::Worker, resume("abc")));
    assert_eq!(argv[argv.len() - 2..], ["--resume", "abc"]);
    assert!(!argv.iter().any(|a| a == "--append-system-prompt-file"));
    assert!(!argv.iter().any(|a| a == "--session-id"));
}

#[test]
fn a_retro_resumes_the_workers_session_without_bypassing_permissions() {
    let mut retro = call(Role::Worker, resume("abc"));
    retro.tools = crate::ports::Tools::Retro;
    let argv = strings(&retro);
    assert!(!argv.iter().any(|a| a == "bypassPermissions"), "{argv:?}");
    assert_eq!(argv[argv.len() - 2..], ["--resume", "abc"]);
}

#[test]
fn a_retro_runs_on_an_allow_list_of_tools_and_no_mcp_server() {
    for role in [Role::Worker, Role::Pm] {
        let mut retro = call(role, fresh());
        retro.tools = crate::ports::Tools::Retro;
        let argv = strings(&retro);
        let at = argv.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(argv[at + 1], "Read,Glob,Grep,Skill", "{role:?}");
        assert!(argv.iter().any(|a| a == "--strict-mcp-config"));
    }
}

#[test]
fn a_retro_loads_the_plugin_folder_its_skill_is_in() {
    let mut retro = call(Role::Pm, fresh());
    retro.tools = crate::ports::Tools::Retro;
    retro.plugin_dirs = vec!["/k/skills/mattpocock".into()];
    let argv = strings(&retro);
    let at = argv.iter().position(|a| a == "--plugin-dir").unwrap();
    assert_eq!(argv[at + 1], "/k/skills/mattpocock");
}

#[test]
fn each_plugin_folder_is_its_own_plugin_dir() {
    let mut with = call(Role::Reviewer, fresh());
    with.plugin_dirs = vec!["/k/skills/mattpocock".into(), "/k/skills/review".into()];
    let argv = strings(&with);
    let dirs: Vec<&str> = argv
        .windows(2)
        .filter(|pair| pair[0] == "--plugin-dir")
        .map(|pair| pair[1].as_str())
        .collect();
    assert_eq!(dirs, ["/k/skills/mattpocock", "/k/skills/review"]);
    let argv = strings(&call(Role::Reviewer, fresh()));
    assert!(!argv.iter().any(|a| a == "--plugin-dir"));
}

#[test]
fn a_worker_gets_kelpies_own_plugins_by_folder_as_it_gets_any_other() {
    let dir = tempfile::tempdir().unwrap();
    let skills = crate::skills::Skills::load(
        &crate::settings::StepSkills::default(),
        &dir.path().join("skills"),
    );
    let mut worker = call(Role::Worker, fresh());
    worker.plugin_dirs = skills.plugin_dirs().to_vec();
    let argv = strings(&worker);
    let dirs: Vec<&str> = argv
        .windows(2)
        .filter(|pair| pair[0] == "--plugin-dir")
        .map(|pair| pair[1].as_str())
        .collect();
    assert_eq!(
        dirs,
        [
            dir.path().join("skills/mattpocock").to_str().unwrap(),
            dir.path().join("skills/kelpie-wording").to_str().unwrap(),
            dir.path().join("skills/kelpie-issue").to_str().unwrap(),
        ]
    );
}

#[test]
fn only_a_worker_bypasses_permissions() {
    let argv = strings(&call(Role::Reviewer, fresh()));
    assert!(!argv.iter().any(|a| a == "--permission-mode"));
}

#[test]
fn prepare_writes_the_settings_file_the_call_names() {
    let dir = tempfile::tempdir().unwrap();
    let mut review = call(Role::Reviewer, fresh());
    review.settings = dir.path().join("worker/review-settings.json");
    review.tools = Tools::Review;
    review.reach.read = vec![dir.path().join("extra")];
    ClaudeCli::default().prepare(&review).unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&review.settings).unwrap()).unwrap();
    assert_eq!(
        written["permissions"]["deny"],
        crate::trim::deny_with_trim(&["Agent", "Task", "Bash"])
    );
    assert_eq!(
        written["permissions"]["additionalDirectories"],
        serde_json::json!([dir.path().join("extra")])
    );
    assert_eq!(written["disableBundledSkills"], true);

    std::fs::create_dir_all(dir.path().join("taken/settings.json")).unwrap();
    review.settings = dir.path().join("taken/settings.json");
    let err = ClaudeCli::default().prepare(&review).unwrap_err();
    assert!(
        matches!(&err, AgentError::Setup(reason) if reason.starts_with("cannot write ")),
        "{err:?}"
    );
}

#[test]
fn a_success_result_is_read() {
    let reply = parse_result(&output(0, RESULT, ""), &fresh()).unwrap();
    assert_eq!(reply.session_id.0, "f83901c5-6d39-421b-b709-7828b56ba237");
    assert_eq!(reply.text, "ok");
    assert_eq!(
        reply.usage,
        Usage {
            input: 10,
            cache_write: 8003,
            cache_write_5m: 0,
            cache_read: 13673,
            output: 53,
        }
    );
    assert_eq!(reply.session_cost, Some(Cost(17_648_300)));
}

// The recorded results cache for an hour only, so the split is moved here.
#[test]
fn five_minute_cache_writes_are_kept_apart_from_the_hour_s() {
    let split = RESULT.replace(
        r#""cache_creation":{"ephemeral_1h_input_tokens":8003,"ephemeral_5m_input_tokens":0}"#,
        r#""cache_creation":{"ephemeral_1h_input_tokens":5003,"ephemeral_5m_input_tokens":3000}"#,
    );
    assert_ne!(split, RESULT, "the recorded split moved");
    let reply = parse_result(&output(0, &split, ""), &fresh()).unwrap();
    assert_eq!(
        (reply.usage.cache_write, reply.usage.cache_write_5m),
        (8003, 3000)
    );
    let none = RESULT.replace(r#""cache_creation":{"#, r#""cache_made":{"#);
    let reply = parse_result(&output(0, &none, ""), &fresh()).unwrap();
    assert_eq!(reply.usage.cache_write_5m, 0, "no split is all an hour's");
}

#[test]
fn the_project_manager_runs_on_an_allow_list_of_tools_and_no_mcp_server() {
    for pm in [
        argv(&call(Role::Pm, fresh())),
        terminal_argv(&call(Role::Pm, fresh())),
    ] {
        let at = pm.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(pm[at + 1], "Read,Glob,Grep,Edit,Write");
        assert!(pm.iter().any(|a| a == "--strict-mcp-config"));
        assert!(!pm.iter().any(|a| a == "bypassPermissions"));
    }
}

#[test]
fn the_representative_starts_with_no_tool_allow_list_as_it_did() {
    let mut answer = call(Role::Representative, fresh());
    answer.tools = Tools::Answer;
    let argv = strings(&answer);
    assert!(!argv.contains(&"--tools".into()), "{argv:?}");
    assert!(!argv.contains(&"--strict-mcp-config".into()), "{argv:?}");
}

#[test]
fn a_worker_runs_on_the_tools_its_skills_use_and_no_mcp_server() {
    for worker in [
        strings(&call(Role::Worker, fresh())),
        strings(&call(Role::Worker, resume("abc"))),
    ] {
        let at = worker.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(worker[at + 1], "Bash,Read,Edit,Write,Glob,Grep,Skill");
        assert!(worker.iter().any(|a| a == "--strict-mcp-config"));
    }
}

#[test]
fn a_reviewer_runs_on_the_tools_a_read_only_review_uses_and_no_mcp_server() {
    let mut review = call(Role::Reviewer, fresh());
    review.tools = Tools::Review;
    let argv = strings(&review);
    let at = argv.iter().position(|a| a == "--tools").unwrap();
    assert_eq!(argv[at + 1], "Read,Glob,Grep");
    assert!(argv.iter().any(|a| a == "--strict-mcp-config"));
}

#[test]
fn a_triager_runs_on_read_glob_grep_and_no_mcp_server() {
    let mut triage = call(Role::Triager, fresh());
    triage.tools = Tools::Review;
    let argv = strings(&triage);
    let at = argv.iter().position(|a| a == "--tools").unwrap();
    assert_eq!(argv[at + 1], "Read,Glob,Grep");
    assert!(argv.iter().any(|a| a == "--strict-mcp-config"));
    assert!(!argv.iter().any(|a| a == "bypassPermissions"));
}

#[test]
fn an_agent_files_tools_replace_a_workers_and_a_reviewers_but_no_others() {
    let tools_of = |call: &AgentCall| {
        let argv = strings(call);
        let at = argv.iter().position(|a| a == "--tools")?;
        Some(argv[at + 1].clone())
    };
    let mut worker = call(Role::Worker, fresh());
    worker.agent_tools = Some("Bash,Read,Edit,Write,Glob,Grep,Skill,WebFetch".into());
    assert_eq!(
        tools_of(&worker).as_deref(),
        Some("Bash,Read,Edit,Write,Glob,Grep,Skill,WebFetch")
    );
    let mut review = call(Role::Reviewer, fresh());
    review.tools = Tools::Review;
    review.agent_tools = Some("Read,Glob,Grep,Skill".into());
    assert_eq!(tools_of(&review).as_deref(), Some("Read,Glob,Grep,Skill"));
    // A retro stays read-only.
    worker.tools = Tools::Retro;
    assert_eq!(tools_of(&worker).as_deref(), Some("Read,Glob,Grep,Skill"));
}

#[test]
fn the_context_is_the_last_model_requests_input_not_the_calls_sum() {
    // Eight model requests, whose cache reads sum to 108,226 tokens.
    let many = include_str!("../../../../../fixtures/claude-p-question.json");
    let reply = parse_result(&output(0, many, ""), &fresh()).unwrap();
    assert_eq!(reply.usage.cache_read, 108_226);
    assert_eq!(reply.context, Some(2 + 1_573 + 30_304));
    let one = parse_result(&output(0, RESULT, ""), &fresh()).unwrap();
    assert_eq!(one.context, Some(10 + 8_003 + 13_673));
}

#[test]
fn a_resumed_call_reports_its_own_usage_and_the_session_cost_so_far() {
    let first = parse_result(&output(0, FRESH, ""), &fresh()).unwrap();
    let second = parse_result(&output(0, RESUMED, ""), &resume(&first.session_id.0)).unwrap();
    assert_eq!(first.session_id, second.session_id);
    assert_eq!(
        (first.usage, first.session_cost),
        (
            Usage {
                input: 10,
                cache_write: 8984,
                cache_write_5m: 0,
                cache_read: 13673,
                output: 148,
            },
            Some(Cost(20_085_300)),
        )
    );
    assert_eq!(
        (second.usage, second.session_cost),
        (
            Usage {
                input: 10,
                cache_write: 193,
                cache_write_5m: 0,
                cache_read: 22657,
                output: 34,
            },
            Some(Cost(22_917_000)),
        )
    );
}

#[test]
fn resuming_a_session_that_never_started_is_named() {
    let err = parse_result(&output(1, "", NO_SESSION_STDERR), &resume("0e2c")).unwrap_err();
    assert_eq!(err, AgentError::NoSession(CLAUDE, SessionId("0e2c".into())));
}

#[test]
fn the_same_stderr_for_a_new_session_is_a_plain_failure() {
    let err = parse_result(&output(1, "", NO_SESSION_STDERR), &fresh()).unwrap_err();
    assert!(matches!(err, AgentError::Failed(CLAUDE, _)), "{err:?}");
}

#[test]
fn an_error_result_is_a_failure_carrying_its_text() {
    let text = RESULT.replace("\"is_error\":false", "\"is_error\":true");
    let err = parse_result(&output(1, &text, ""), &fresh()).unwrap_err();
    assert_eq!(err, AgentError::Failed(CLAUDE, "ok".into()));
}

#[test]
fn a_result_without_its_cost_is_unreadable() {
    let text = RESULT.replace("\"total_cost_usd\":0.0176483,", "");
    let err = parse_result(&output(0, &text, ""), &fresh()).unwrap_err();
    assert!(matches!(err, AgentError::Unreadable(CLAUDE, _)), "{err:?}");
}

#[test]
fn no_json_and_a_failed_exit_reports_stderr() {
    let err = parse_result(&output(1, "", "not logged in"), &fresh()).unwrap_err();
    assert_eq!(err, AgentError::Failed(CLAUDE, "not logged in".into()));
}

// The wrapper prints crabpot's own refusal when a Windows session's grant meets
// the profile read grant's exclusions, which a list that drifted from the
// install's would cause. The turn's error must carry every conflict.
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
    let err = parse_result(&output(1, "", &stderr), &fresh()).unwrap_err();
    let said = err.to_string();
    for conflict in &conflicts {
        assert!(said.contains(&conflict.to_string()), "{conflict}: {said}");
    }
}

#[test]
fn no_json_and_a_clean_exit_is_unreadable() {
    let err = parse_result(&output(0, "hello", ""), &fresh()).unwrap_err();
    assert_eq!(err, AgentError::Unreadable(CLAUDE, "hello".into()));
}

#[test]
fn a_cost_is_kept_to_the_billionth_and_refuses_nonsense() {
    assert_eq!(Cost::from_usd(0.16096939999999998), Some(Cost(160_969_400)));
    assert_eq!(Cost::from_usd(0.0), Some(Cost(0)));
    assert_eq!(Cost::from_usd(-0.01), None);
    assert_eq!(Cost::from_usd(f64::NAN), None);
    assert_eq!(Cost(22_917_000).usd(), 0.022917);
}

#[derive(Debug, Default)]
struct Recorded(std::sync::Mutex<Vec<(u32, String)>>);

impl LambLabels for Recorded {
    fn label(&self, pid: u32, label: &str) {
        self.0.lock().unwrap().push((pid, label.to_owned()));
    }
}

#[test]
fn a_call_s_lamb_carries_its_issue_and_role() {
    let dir = tempfile::tempdir().unwrap();
    let (claude, pid_file) = (dir.path().join("claude-program"), dir.path().join("pid"));
    std::fs::write(dir.path().join("result.json"), RESULT).unwrap();
    let script = format!(
        "#!/bin/sh\necho $$ > {}\ncat {}/result.json\n",
        pid_file.display(),
        dir.path().display()
    );
    crate::test::write_script(&claude, &script);
    let lambs = Arc::new(Recorded::default());
    let base = Base::labelling(Arc::clone(&lambs) as Arc<dyn LambLabels>)
        .sandboxed(Arc::new(OpenSandbox::default()), dir.path().join("home"));
    let token = LoginToken::new("sk-ant-oat01-for-the-test".to_owned());
    let cli = ClaudeCli::on(&base).with_program(claude).with_login(token);
    for role in [Role::Worker, Role::Reviewer] {
        let mut call = call(role, fresh());
        call.cwd = dir.path().to_owned();
        call.settings = dir.path().join("worker/settings.json");
        cli.prepare(&call).unwrap();
        cli.run(&call, &Ending::default()).unwrap();
    }
    let labels = lambs.0.lock().unwrap().clone();
    let names: Vec<&str> = labels.iter().map(|(_, l)| l.as_str()).collect();
    assert_eq!(names, ["#6 worker", "#6 reviewer"]);
    let pid: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(labels[1].0, pid, "the label is not on the process that ran");
}

fn logged_in(home: PathBuf) -> ClaudeCli {
    let base = Base::default().sandboxed(Arc::new(OpenSandbox::default()), home);
    ClaudeCli::on(&base).with_login(LoginToken::new("sk-ant-oat01-for-the-test".to_owned()))
}

// The project's folder holds its `worker` folder, where every call's settings go.
fn in_project(project: &std::path::Path, role: Role, tools: Tools, issue: u64) -> AgentCall {
    AgentCall {
        issue: crate::ticket::TicketId::from_issue(issue),
        settings: project.join("worker/settings.json"),
        tools,
        cwd: project.join("worktrees/6"),
        ..call(role, fresh())
    }
}

fn config_dir(command: &Command) -> Option<PathBuf> {
    let set = command.get_envs().find(|(n, _)| *n == "CLAUDE_CONFIG_DIR");
    set.and_then(|(_, value)| value).map(PathBuf::from)
}

// Moved out of the home, the config folder is the call's own: its transcripts
// outlive the work item's build folder, and a resume finds them where the last turn wrote.
#[test]
fn a_call_keeps_its_config_folder_under_the_projects_kelpie_home() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("koji");
    std::fs::create_dir_all(project.join("worktrees/6")).unwrap();
    let cli = logged_in(dir.path().join("home"));
    let calls = [
        (
            in_project(&project, Role::Worker, Tools::Work, 6),
            "claude/6/worker",
        ),
        (
            in_project(&project, Role::Reviewer, Tools::Review, 6),
            "claude/6/reviewer",
        ),
        (in_project(&project, Role::Pm, Tools::Pm, 0), "claude/pm"),
        (in_project(&project, Role::Pm, Tools::Retro, 0), "claude/pm"),
    ];
    for (call, folder) in calls {
        let folder = project.join(folder);
        for run in [ClaudeCli::sandboxed_command, ClaudeCli::terminal_command] {
            let command = run(&cli, &call).unwrap();
            assert_eq!(
                config_dir(&command),
                Some(folder.clone()),
                "{:?}",
                call.role
            );
        }
        let transcript = cli.transcript(&call).unwrap();
        assert!(
            transcript.folder.starts_with(folder.join("projects")),
            "{transcript:?}"
        );
    }
}

// A work item open across the upgrade has its transcript where Claude Code kept it
// before, and the config folder it now resumes in has none.
#[test]
fn a_session_from_before_the_move_resumes_from_its_copied_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let project = dir.path().join("koji");
    let cli = logged_in(home.clone());
    let mut call = in_project(&project, Role::Worker, Tools::Work, 6);
    call.session = resume("0e2c0f3a-52a9-4a14-8d0b-1c1d9a7f3a11");
    std::fs::create_dir_all(&call.cwd).unwrap();
    let file = format!("{}.jsonl", call.session.id().0);
    let old = sandbox::transcripts(&home.join(".claude"), &call.cwd).unwrap();
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join(&file), "{\"before\":true}\n").unwrap();
    std::fs::write(old.join("other.jsonl"), "{}\n").unwrap();
    let moved = cli.transcript(&call).unwrap().folder;

    cli.prepare(&call).unwrap();
    cli.foreground(&call).unwrap();

    assert_eq!(
        std::fs::read_to_string(moved.join(&file)).unwrap(),
        "{\"before\":true}\n"
    );
    assert!(
        !moved.join("other.jsonl").exists(),
        "another session's file came too"
    );
    assert!(
        old.join(&file).exists(),
        "the old file is the maintainer's to keep"
    );

    // The session goes on in the new folder, and a later call leaves what it wrote.
    std::fs::write(moved.join(&file), "{\"before\":true}\n{\"after\":true}\n").unwrap();
    cli.foreground(&call).unwrap();
    let kept = std::fs::read_to_string(moved.join(&file)).unwrap();
    assert!(kept.contains("after"), "{kept}");

    // A session that starts new has nothing to copy.
    let fresh = AgentCall {
        session: fresh(),
        ..call
    };
    cli.foreground(&fresh).unwrap();
    assert!(
        !moved
            .join(format!("{}.jsonl", fresh.session.id().0))
            .exists()
    );
}
