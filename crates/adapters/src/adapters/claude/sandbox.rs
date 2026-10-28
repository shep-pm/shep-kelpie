//! The sandbox a Claude Code call runs in, whole
//!
//! Claude Code, its tools, hooks and MCP servers all run inside kelpie's
//! sandbox. Besides what the call's fence allows, it writes its transcript
//! folder and a scratch folder, and reaches the model's endpoint. Its own
//! sandbox stays off: on macOS a sandbox cannot start inside another.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{Claude, LOGIN_VAR};
use crate::adapters::harness::{bind_in_scratch, call_policy, program_folder, prompt_file};
use crate::dep_sources::ProcessEnv;
use crate::fence;
use crate::ports::{AgentCall, AgentError, Policy, Role};

impl Claude {
    // `claude` with `argv`, in the call's folder and with its own temporary files.
    pub(super) fn program_command(&self, call: &AgentCall, argv: Vec<OsString>) -> Command {
        let scratch = scratch(call);
        // A sheep has no `TMPDIR`, so a heredoc falls back to `/tmp`, which the
        // sandbox refuses. zsh writes it under `TMPPREFIX` instead, so that gets
        // the same folder. Only `claude` gets them, through `env`: `srt` keeps
        // its proxy socket under its own `TMPDIR`, which the call must not write.
        let mut temp = OsString::from("TMPDIR=");
        temp.push(&scratch);
        let mut prefix = OsString::from("TMPPREFIX=");
        prefix.push(scratch.join("zsh"));
        let mut command = crate::spawn::command(crate::spawn::env());
        command
            .arg(temp)
            .arg(prefix)
            .arg(&self.program)
            .args(argv)
            .current_dir(&call.cwd)
            .env("CLAUDE_CODE_TMPDIR", &scratch)
            .env("CLAUDE_CONFIG_DIR", config_folder(call))
            .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1");
        if let Some(login) = &self.login {
            command.env(LOGIN_VAR, login.expose());
        }
        command
    }
}

/// The model's endpoint, the one host every call reaches
///
/// The worker's commands reach it too, since they share the sandbox, so
/// `kelpie guard` refuses an agent started from them. Not
/// `platform.claude.com`, where a login is refreshed: the sandbox cannot
/// write the keychain, so a refresh there would be lost.
const MODEL_HOST: &str = "api.anthropic.com";

// Claude Code names a working folder's transcript folder this way, and
// hashes past this length. Kelpie refuses a folder it cannot name.
const LONGEST_NAME: usize = 200;

/// The whole call's policy: its fence, or none, and what Claude Code itself needs
///
/// A call with no fence may not read `unfenced` either, its own folder apart.
///
/// # Errors
///
/// [`AgentError::Setup`] when the call's transcript folder cannot be named.
pub(crate) fn policy(
    call: &AgentCall,
    home: &Path,
    program: &Path,
    unfenced: &[String],
) -> Result<Policy, AgentError> {
    let mut policy = call_policy(call, unfenced);
    policy.no_read.extend(fence::credentials());
    policy.no_keychain = call.reach.block_keychain;
    let config = config_folder(call);
    let transcripts = transcripts(&config, &call.cwd)?;
    // Auto memory there loads into every session in the folder, a review's too.
    policy.no_write.push(transcripts.join("memory"));
    policy.write.extend([config.clone(), scratch(call)]);
    // A fence denies the home folder whole, so what Claude Code reads to run is
    // named beside the call's own files: its config folder, with its login state,
    // transcripts and shell snapshots, the settings and skills it is given, and the
    // program it is. The maintainer's `~/.claude` is none of them.
    let own = [config]
        .into_iter()
        .chain(program_folder(program, &ProcessEnv, home))
        .chain([call.settings.clone(), scratch(call)]);
    policy.read.extend(
        own.chain(
            call.instructions
                .iter()
                .chain(&call.plugin_dirs)
                .chain(&call.reach.read)
                .cloned(),
        ),
    );
    // The hook that hands the prompt back after a compaction reads it inside the sandbox.
    if call.compact_window.is_some() {
        policy.read.push(prompt_file(&call.settings));
    }
    // The servers run inside the call's sandbox, and the code graph keeps its index in its own folder.
    if crate::ports::mcp::loads(call) {
        policy.read.push(super::mcp::mcp_file(&call.settings));
    }
    crate::adapters::mcp::fence_the_graph(&mut policy, call);
    crate::adapters::mcp::fence_the_browser(&mut policy, call);
    bind_in_scratch(&mut policy, call, &scratch(call));
    policy.hosts.push(MODEL_HOST.to_owned());
    Ok(policy)
}

/// The folder Claude Code keeps the call's transcripts in
///
/// # Errors
///
/// [`AgentError::Setup`] when the working folder's name is past Claude Code's limit.
pub(crate) fn transcripts(config: &Path, cwd: &Path) -> Result<PathBuf, AgentError> {
    let cwd = crate::canonical(cwd).unwrap_or_else(|_| cwd.to_owned());
    // One `-` per UTF-16 unit, as Claude Code's JavaScript replaces them.
    let name: String = cwd
        .to_string_lossy()
        .chars()
        .flat_map(|c| {
            let kept = c.is_ascii_alphanumeric().then_some(c);
            std::iter::repeat_n(kept.unwrap_or('-'), kept.map_or(c.len_utf16(), |_| 1))
        })
        .collect();
    if name.len() > LONGEST_NAME {
        return Err(AgentError::Setup(format!(
            "{} is too long a path for kelpie to find its transcripts",
            cwd.display()
        )));
    }
    Ok(config.join("projects").join(name))
}

/// Claude Code's config folder for the call, which holds its state and its transcripts
///
/// Under the project's kelpie home, beside the folder of the call's settings: one
/// for each work item and role, so two calls running at once never write one
/// `.claude.json`, and one for each role of a call that is no work item's. It
/// outlives the work item's build folder, which the transcripts must.
pub(crate) fn config_folder(call: &AgentCall) -> PathBuf {
    let Some(project) = call.settings.parent().and_then(Path::parent) else {
        return call.settings.with_extension("claude");
    };
    let folder = project.join("claude");
    match call.issue.filter(|_| call.role != Role::Pm) {
        Some(ticket) => folder.join(ticket.to_string()).join(call.role.as_str()),
        None => folder.join(call.role.as_str()),
    }
}

/// The call's scratch folder, beside its settings file, for Claude Code's temporary files
pub(crate) fn scratch(call: &AgentCall) -> PathBuf {
    call.settings.with_extension("tmp")
}

/// Where the sandbox's own settings for the call go, beside Claude Code's
pub(crate) fn sandbox_settings(call: &AgentCall) -> PathBuf {
    call.settings.with_extension("sandbox.json")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::*;
    use crate::adapters::{Base, ClaudeCli, LoginToken, SandboxRuntime};
    use crate::ports::Ending;
    use crate::ports::{Agents, Role, Session, SessionId, Tools};
    use crate::profile::CREDENTIALS;
    use crate::profile::WorkerProfile;
    use crate::settings::Effort;
    use crate::test::OpenSandbox;

    const RESULT: &str = include_str!("../../../../../fixtures/claude-p-result.json");

    struct World {
        dir: tempfile::TempDir,
    }

    impl World {
        fn new() -> Self {
            let w = Self {
                dir: crate::test::short_tempdir(),
            };
            for folder in ["home", "wt", "build", "worker"] {
                std::fs::create_dir_all(w.path(folder)).unwrap();
            }
            std::fs::write(w.path("result.json"), RESULT).unwrap();
            let script = format!(
                "#!/bin/sh\ntouch {}\ncat {}\n",
                w.path("ran").display(),
                w.path("result.json").display()
            );
            crate::test::write_script(&w.path("claude-program"), &script);
            w
        }

        fn path(&self, p: &str) -> PathBuf {
            self.dir.path().canonicalize().unwrap().join(p)
        }

        fn call(&self, role: Role) -> AgentCall {
            AgentCall {
                issue: crate::ticket::TicketId::from_issue(7),
                settings: self.path("worker/settings.json"),
                ..AgentCall::new(
                    role,
                    Tools::Answer,
                    crate::settings::AgentHarness::ClaudeCode,
                    "claude-sonnet-5",
                    Effort::Medium,
                    Session::New(SessionId("s".into())),
                    self.path("wt"),
                    "go",
                )
            }
        }

        fn worker(&self) -> AgentCall {
            let (wt, build) = (self.path("wt"), self.path("build"));
            let home = self.path("home");
            let profile = WorkerProfile {
                worktree: &wt,
                build: &build,
                home: &home,
                git_common_dir: Path::new("/k/repo/.git"),
                git_dir: Path::new("/k/repo/.git/worktrees/7"),
                branch: "kelpie/7",
                base: "main",
                kelpie_pushes: false,
                kelpie: Path::new("/k/kelpie"),
                skills: Path::new("/k/skills"),
                kelpie_home: Path::new("/k"),
                repo: Path::new("/k/repo"),
                guard_hooks: &[],
                allowed_domains: &[],
                registries: &[],
                build_env: &BTreeMap::new(),
                shep_home: Path::new("/k/shep"),
                door: Path::new("/k/dog/lease.sock"),
                token: None,
                login_token: None,
            };
            AgentCall {
                tools: Tools::Work,
                agent_tools: None,
                reach: profile.reach(),
                ..self.call(Role::Worker)
            }
        }

        // The shepherd's home, which the runner denies every call with no fence.
        fn shep_unread(&self) -> String {
            format!("{}/**", self.path("shep").display())
        }

        fn cli(&self, sandbox: Arc<dyn crate::ports::Sandbox>) -> ClaudeCli {
            let base = Base::default()
                .unfenced_unread(&self.path("home"), &self.path("shep"))
                .sandboxed(sandbox, self.path("home"));
            let token = LoginToken::new("sk-ant-oat01-for-the-test".to_owned());
            ClaudeCli::on(&base)
                .with_program(self.path("claude-program"))
                .with_login(token)
        }
    }

    #[test]
    fn every_role_runs_inside_the_sandbox_with_its_settings_beside_claudes() {
        let w = World::new();
        let sandbox = OpenSandbox::default();
        let cli = w.cli(Arc::new(sandbox.clone()));
        for call in [w.worker(), w.call(Role::Reviewer)] {
            cli.prepare(&call).unwrap();
            cli.run(&call, &Ending::default()).unwrap();
        }
        let wrapped = sandbox.wrapped();
        assert_eq!(wrapped.len(), 2);
        for (_, settings) in &wrapped {
            assert_eq!(settings, &w.path("worker/settings.sandbox.json"));
        }
    }

    // Without the deny in `World::cli`, no test of a call's sandbox would see
    // the shepherd's home read.
    #[test]
    fn the_test_cli_denies_an_unfenced_call_the_shepherds_home() {
        let w = World::new();
        let sandbox = OpenSandbox::default();
        let cli = w.cli(Arc::new(sandbox.clone()));
        let call = w.call(Role::Reviewer);
        cli.prepare(&call).unwrap();
        cli.run(&call, &Ending::default()).unwrap();
        let wrapped = sandbox.wrapped();
        let [(open, _)] = wrapped.as_slice() else {
            panic!("wrapped once, not {wrapped:?}")
        };
        assert!(
            open.no_read.contains(&w.shep_unread()),
            "{:?}",
            open.no_read
        );
    }

    // A linked worktree's `.git` file names a gitdir outside it, under the home
    // the deny covers, so a review that runs `git` needs it read back.
    #[test]
    fn an_unfenced_review_reads_the_git_dir_its_worktree_points_to() {
        let w = World::new();
        let (repo, worktree) = crate::test::linked_worktree(&w.path("home"));
        let sandbox = OpenSandbox::default();
        let cli = w.cli(Arc::new(sandbox.clone()));
        let call = AgentCall {
            cwd: worktree,
            ..w.call(Role::Reviewer)
        };
        cli.prepare(&call).unwrap();
        cli.run(&call, &Ending::default()).unwrap();
        let wrapped = sandbox.wrapped();
        let [(open, _)] = wrapped.as_slice() else {
            panic!("wrapped once, not {wrapped:?}")
        };
        assert!(open.read.contains(&repo.join(".git")), "{:?}", open.read);
    }

    #[test]
    fn a_missing_sandbox_runs_nothing() {
        let w = World::new();
        // A Windows machine with no `crabpot windows-install` has no sandbox
        // to run a call in.
        let none = SandboxRuntime::new()
            .on(crate::adapters::srt::Platform::Windows)
            .with_install(crate::adapters::host::WindowsInstall {
                provisioned: false,
                setup_installed: None,
                setup_expected: 1,
                loopback_open: false,
                seclogon_runs: true,
                desktop_session: true,
            });
        let cli = w.cli(Arc::new(none));
        for call in [w.worker(), w.call(Role::Reviewer)] {
            let err = cli.run(&call, &Ending::default()).unwrap_err();
            assert!(
                matches!(&err, AgentError::Setup(why) if why.contains("crabpot windows-install")),
                "{err:?}"
            );
        }
        assert!(!w.path("ran").exists(), "claude ran without its sandbox");
    }

    #[test]
    fn a_worker_writes_its_fence_its_transcripts_and_its_scratch_and_reaches_the_model() {
        let w = World::new();
        let policy = policy(&w.worker(), &w.path("home"), &w.path("claude-program"), &[]).unwrap();
        let config = config_folder(&w.worker());
        let transcripts = transcripts(&config, &w.path("wt")).unwrap();
        let scratch = w.path("worker/settings.tmp");
        assert_eq!(policy.write[..2], [w.path("wt"), w.path("build")]);
        assert_eq!(
            policy.write[policy.write.len() - 2..],
            [config.clone(), scratch.clone()]
        );
        assert!(transcripts.starts_with(&config));
        assert!(policy.no_write.contains(&transcripts.join("memory")));
        assert!(policy.no_write.contains(&w.path("wt/.claude")));
        assert!(policy.no_write.contains(&w.path("wt/**/.codex")));
        assert_eq!(policy.hosts[..2], ["github.com", "api.github.com"]);
        assert_eq!(policy.hosts.last().unwrap(), MODEL_HOST);
        assert!(policy.read.contains(&w.path("worker/settings.json")));
        assert!(policy.no_read.iter().any(|p| p == "~/.ssh/**"));
        assert!(policy.no_read.iter().any(|p| p == "~/.codex/**"));
        assert!(policy.verify_tls);
    }

    // Claude Code keeps its state in the call's own config folder, and the login comes
    // in its token, so nothing of the maintainer's `~/.claude` is read, or written.
    #[test]
    fn the_fence_reads_nothing_of_the_homes_claude_folder() {
        let w = World::new();
        let home = w.path("home");
        let claude = [home.join(".claude"), home.join(".claude.json")];
        for call in [w.worker(), w.call(Role::Reviewer)] {
            let policy = policy(&call, &home, &w.path("claude-program"), &[]).unwrap();
            let all = policy.read.iter().chain(&policy.write);
            for path in all {
                assert!(!claude.iter().any(|own| path.starts_with(own)), "{path:?}");
            }
            let no_read = &policy.no_read;
            assert!(no_read.iter().all(|p| p != "~/.claude/projects/**"));
            for login in ["~/.claude/.credentials.json", "~/.claude.json"] {
                assert!(no_read.iter().any(|p| p == login), "{login}: {no_read:?}");
            }
            let config = config_folder(&call);
            assert!(policy.read.contains(&config) && policy.write.contains(&config));
        }
    }

    #[test]
    fn a_workers_scratch_folder_is_where_it_may_bind_sockets_and_a_reviewers_is_not() {
        let w = World::new();
        let policy = |call: &AgentCall| {
            policy(call, &w.path("home"), &w.path("claude-program"), &[]).unwrap()
        };
        assert_eq!(
            policy(&w.worker()).socket_folders,
            [w.path("worker/settings.tmp")]
        );
        let reviewer = AgentCall {
            tools: Tools::Review,
            ..w.call(Role::Reviewer)
        };
        assert!(policy(&reviewer).socket_folders.is_empty());
        assert!(policy(&w.call(Role::Triager)).socket_folders.is_empty());
    }

    #[test]
    fn a_worker_reads_the_script_its_command_hook_runs_and_its_folder() {
        let w = World::new();
        let mut call = w.worker();
        let folder = w.path("home/x");
        call.reach.fence.as_mut().unwrap().guard.hooks = crate::test::hook_beside_another(&folder);
        let policy = policy(&call, &w.path("home"), &w.path("claude-program"), &[]).unwrap();
        crate::test::assert_reads_the_hook_and_its_folder(&policy, &folder);
    }

    #[test]
    fn a_retro_reads_the_plugin_folder_its_skill_is_in() {
        let w = World::new();
        let call = AgentCall {
            role: Role::Pm,
            tools: Tools::Retro,
            plugin_dirs: vec![w.path("skills/mattpocock")],
            ..w.worker()
        };
        let policy = policy(&call, &w.path("home"), &w.path("claude-program"), &[]).unwrap();
        assert!(policy.read.contains(&w.path("skills/mattpocock")));
    }

    // The hook that hands a compacted turn's prompt back runs `cat` on it
    // inside the sandbox, which denies the whole of kelpie's home but for
    // what the call reads.
    #[test]
    fn a_worker_with_a_compact_window_reads_back_the_prompt_its_hook_prints() {
        let w = World::new();
        let mut call = w.worker();
        let without = policy(&call, &w.path("home"), &w.path("claude-program"), &[]).unwrap();
        assert!(!without.read.contains(&w.path("worker/settings.prompt.md")));
        call.compact_window = std::num::NonZeroU32::new(200_000);
        let with = policy(&call, &w.path("home"), &w.path("claude-program"), &[]).unwrap();
        assert!(with.read.contains(&w.path("worker/settings.prompt.md")));
    }

    #[test]
    fn a_call_on_a_covered_repo_runs_without_the_keychain_and_any_other_with_it() {
        let w = World::new();
        for mut call in [w.worker(), w.call(Role::Reviewer)] {
            let home = w.path("home");
            assert!(
                !policy(&call, &home, &w.path("claude-program"), &[])
                    .unwrap()
                    .no_keychain
            );
            call.reach.block_keychain = true;
            assert!(
                policy(&call, &home, &w.path("claude-program"), &[])
                    .unwrap()
                    .no_keychain
            );
        }
    }

    #[test]
    fn a_review_writes_nothing_of_the_worktrees_and_reaches_only_the_model() {
        let w = World::new();
        let mut review = w.call(Role::Reviewer);
        review.settings = w.path("worker/review-settings.json");
        review.reach.read = vec![w.path("extra")];
        let policy = policy(&review, &w.path("home"), &w.path("claude-program"), &[]).unwrap();
        assert_eq!(
            policy.write,
            [config_folder(&review), w.path("worker/review-settings.tmp"),]
        );
        assert_eq!(policy.hosts, [MODEL_HOST]);
        let mut denied: Vec<String> = CREDENTIALS.map(str::to_owned).into();
        denied.extend([
            "~/.claude/.credentials.json".into(),
            "~/.claude.json".into(),
            "~/.codex/**".into(),
            "~/.pi/**".into(),
            "~/.config/goose/**".into(),
            "~/.local/share/goose/**".into(),
            "~/.local/state/goose/**".into(),
            "~/Library/Application Support/Block/goose/**".into(),
        ]);
        assert_eq!(policy.no_read, denied);
        assert!(policy.read.contains(&w.path("extra")));
        assert!(!policy.verify_tls);
    }

    // The shepherd's home holds the sheep entry's `env`, a gateway's key with
    // it, and `dogs.toml` the webhook's URL. The worktree under it stays readable.
    #[test]
    fn an_unfenced_call_reads_nothing_of_the_shepherds_home_but_its_own_folder() {
        let w = World::new();
        let mut review = w.call(Role::Reviewer);
        review.plugin_dirs = vec![w.path("skills/mattpocock")];
        let shep = vec![format!("{}/**", w.path("").display())];
        let policy = policy(&review, &w.path("home"), &w.path("claude-program"), &shep).unwrap();
        assert!(policy.no_read.contains(&shep[0]), "{:?}", policy.no_read);
        assert!(policy.read.contains(&w.path("wt")));
        assert!(policy.read.contains(&w.path("skills/mattpocock")));
        let deny = super::super::settings::unfenced_reads(&review, &shep);
        let deny = deny.join(" ");
        let worker = format!("Read(/{}/**)", w.path("worker").display());
        assert!(deny.contains(&worker), "{deny}");
        for own in ["wt", "skills/mattpocock"] {
            assert!(
                !deny.contains(&w.path(own).display().to_string()),
                "{own} is denied: {deny}"
            );
        }
        let without =
            super::policy(&review, &w.path("home"), &w.path("claude-program"), &[]).unwrap();
        assert!(
            !without.read.contains(&w.path("wt")),
            "nothing to read around"
        );
    }

    #[test]
    fn no_role_can_start_a_background_command_or_agent() {
        let w = World::new();
        let cli = w.cli(Arc::new(OpenSandbox::default()));
        for call in [w.worker(), w.call(Role::Reviewer)] {
            let command = cli.sandboxed_command(&call).unwrap();
            let set = command
                .get_envs()
                .find(|(name, _)| *name == "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS");
            assert_eq!(
                set.and_then(|(_, value)| value),
                Some(std::ffi::OsStr::new("1")),
                "{:?} can still run a task in the background",
                call.role
            );
        }
    }

    // Only the command inside `srt` gets the folder, as `env TMPDIR=<folder>
    // TMPPREFIX=<folder>/zsh claude ...`: `srt` keeps its proxy socket under
    // its own `TMPDIR`, past 104 bytes it cannot start, and a folder the call
    // writes lets it delete the socket.
    #[test]
    fn every_role_gets_a_temp_folder_its_sandbox_can_write_and_srt_keeps_its_own() {
        let w = World::new();
        let cli = w.cli(Arc::new(OpenSandbox::default()));
        let triager = w.call(Role::Triager);
        for call in [w.worker(), w.call(Role::Reviewer), triager] {
            let command = cli.sandboxed_command(&call).unwrap();
            let policy = policy(&call, &w.path("home"), &w.path("claude-program"), &[]).unwrap();
            let role = call.role;
            let env = |name: &str| command.get_envs().find(|(n, _)| *n == name);
            assert!(env("TMPDIR").is_none(), "{role:?}: srt would see TMPDIR");
            let claude_tmp = env("CLAUDE_CODE_TMPDIR").and_then(|(_, value)| value);
            let claude_tmp = PathBuf::from(claude_tmp.expect("CLAUDE_CODE_TMPDIR"));
            assert!(
                policy.write.contains(&claude_tmp),
                "{role:?}: {claude_tmp:?}"
            );
            assert_eq!(command.get_program(), crate::spawn::env().as_os_str());
            let mut words = command.get_args();
            let temp = words.next().unwrap().to_str().unwrap();
            let folder = PathBuf::from(temp.strip_prefix("TMPDIR=").expect(temp));
            assert!(
                policy.write.contains(&folder),
                "{role:?}: TMPDIR {folder:?} is not in {:?}",
                policy.write
            );
            // zsh ignores `TMPDIR` for a heredoc and writes under `TMPPREFIX`.
            let prefix = words.next().unwrap().to_str().unwrap();
            let prefix = PathBuf::from(prefix.strip_prefix("TMPPREFIX=").expect(prefix));
            assert_eq!(prefix, folder.join("zsh"), "{role:?}");
            assert_eq!(words.next().unwrap(), w.path("claude-program").as_os_str());
        }
    }

    type Cmd = fn(&ClaudeCli, &AgentCall) -> Result<Command, AgentError>;

    // A call's login is the token the maintainer made with `claude setup-token`.
    // It is a variable of the command inside the sandbox and never an argument,
    // which `ps` shows.
    #[test]
    fn every_role_s_claude_gets_the_login_token() {
        let w = World::new();
        let token = "sk-ant-oat01-for-the-test";
        let with = w
            .cli(Arc::new(OpenSandbox::default()))
            .with_login(LoginToken::new(token.to_owned()));
        assert!(
            !format!("{with:?}").contains(token),
            "Debug shows the token"
        );
        let triager = w.call(Role::Triager);
        for call in [w.worker(), w.call(Role::Reviewer), triager] {
            let role = call.role;
            let login = |cli: &ClaudeCli, command: Cmd| {
                let command = command(cli, &call).unwrap();
                let set = command.get_envs().find(|(n, _)| *n == LOGIN_VAR);
                let args: Vec<_> = command.get_args().collect();
                assert!(!args.iter().any(|a| a.to_string_lossy().contains(token)));
                set.map(|(_, value)| value.map(|v| v.to_string_lossy().into_owned()))
            };
            let session = ClaudeCli::sandboxed_command;
            assert_eq!(
                login(&with, session),
                Some(Some(token.to_owned())),
                "{role:?}"
            );
            let terminal = ClaudeCli::terminal_command;
            assert_eq!(
                login(&with, terminal),
                Some(Some(token.to_owned())),
                "{role:?}"
            );
        }
    }

    // `claude` keeps its login per config folder, and each call has one of its own,
    // so with no token it would start and fail with its own "not logged in", which a
    // ruling would carry. The call says what doctor says instead, and runs nothing.
    #[test]
    fn a_call_with_no_login_token_refuses_with_the_fix_whatever_the_keychain() {
        let w = World::new();
        let token = LoginToken::new("sk-ant-oat01-for-the-test".to_owned());
        let cli =
            |login: Option<LoginToken>| w.cli(Arc::new(OpenSandbox::default())).with_login(login);
        let blocked = |mut call: AgentCall| {
            call.reach.block_keychain = true;
            call
        };
        for call in [w.worker(), w.call(Role::Reviewer), blocked(w.worker())] {
            for run in [ClaudeCli::sandboxed_command, ClaudeCli::terminal_command] {
                let err = run(&cli(None), &call).unwrap_err();
                let AgentError::Setup(why) = &err else {
                    panic!("{err:?}")
                };
                assert!(why.contains("`claude setup-token`"), "{why}");
                assert!(
                    why.contains("`shep secret set CLAUDE_CODE_OAUTH_TOKEN`"),
                    "{why}"
                );
                run(&cli(token.clone()), &call).unwrap();
            }
            let err = cli(None).run(&call, &Ending::default()).unwrap_err();
            assert!(matches!(err, AgentError::Setup(_)), "{err:?}");
        }
        assert!(!w.path("ran").exists(), "claude ran with no login");
    }

    #[cfg(unix)]
    #[test]
    fn the_scratch_folder_is_emptied_and_a_link_there_is_not_followed() {
        let w = World::new();
        let cli = w.cli(Arc::new(OpenSandbox::default()));
        let call = w.worker();
        std::fs::create_dir_all(w.path("elsewhere")).unwrap();
        std::fs::write(w.path("elsewhere/keep"), "").unwrap();
        std::os::unix::fs::symlink(w.path("elsewhere"), scratch(&call)).unwrap();
        cli.run(&call, &Ending::default()).unwrap();
        assert!(w.path("elsewhere/keep").exists());
        assert!(scratch(&call).is_dir() && !scratch(&call).is_symlink());
        let config = config_folder(&call);
        assert!(transcripts(&config, &w.path("wt")).unwrap().is_dir());
    }

    // Go writes its module cache, and a test can leave a temp folder, with
    // read-only folders that a plain removal cannot empty.
    #[cfg(unix)]
    #[test]
    fn a_scratch_folder_holding_read_only_folders_is_emptied_and_the_call_runs() {
        use std::os::unix::fs::PermissionsExt;

        let w = World::new();
        let cli = w.cli(Arc::new(OpenSandbox::default()));
        let call = w.worker();
        let held = scratch(&call).join("mod/cache");
        std::fs::create_dir_all(&held).unwrap();
        std::fs::write(held.join("file"), "").unwrap();
        std::fs::set_permissions(held.join("file"), PermissionsExt::from_mode(0o444)).unwrap();
        std::fs::set_permissions(&held, PermissionsExt::from_mode(0o555)).unwrap();
        std::fs::set_permissions(scratch(&call).join("mod"), PermissionsExt::from_mode(0o555))
            .unwrap();
        cli.run(&call, &Ending::default()).unwrap();
        assert!(w.path("ran").exists(), "claude did not run");
        assert!(scratch(&call).is_dir());
        assert_eq!(std::fs::read_dir(scratch(&call)).unwrap().count(), 0);
        let mode = std::fs::metadata(scratch(&call))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o200, 0, "scratch is read-only: {mode:o}");
    }

    #[test]
    fn a_session_for_the_terminal_resumes_in_the_worker_s_sandbox_and_asks_as_usual() {
        let w = World::new();
        let sandbox = OpenSandbox::default();
        let cli = w.cli(Arc::new(sandbox.clone()));
        let call = AgentCall {
            session: Session::Resume(SessionId("s".into())),
            prompt: String::new(),
            ..w.worker()
        };
        cli.prepare(&call).unwrap();
        let command = cli.foreground(&call).unwrap();
        let args: Vec<&str> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        let settings = w.path("worker/settings.json");
        let temp = format!("TMPDIR={}", w.path("worker/settings.tmp").display());
        let prefix = format!("TMPPREFIX={}", w.path("worker/settings.tmp/zsh").display());
        let claude = w.path("claude-program");
        assert_eq!(command.get_program(), crate::spawn::env().as_os_str());
        assert_eq!(
            args,
            [
                temp.as_str(),
                prefix.as_str(),
                claude.to_str().unwrap(),
                "--model",
                "claude-sonnet-5",
                "--effort",
                "medium",
                "--setting-sources",
                "project",
                "--settings",
                settings.to_str().unwrap(),
                "--tools",
                "Bash,Read,Edit,Write,Glob,Grep,Skill",
                "--strict-mcp-config",
                "--resume",
                "s",
            ]
        );
        assert_eq!(command.get_current_dir(), Some(w.path("wt").as_path()));
        // Only those it sets: it also unsets whatever `spawn` hides.
        let set = command.get_envs().filter(|(_, value)| value.is_some());
        let env: Vec<_> = set.map(|(name, _)| name).collect();
        assert_eq!(
            env,
            [
                "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "CLAUDE_CODE_TMPDIR",
                "CLAUDE_CONFIG_DIR",
            ]
        );
        let wrapped = sandbox.wrapped();
        let [(policy_used, file)] = wrapped.as_slice() else {
            panic!("wrapped once, not {wrapped:?}")
        };
        assert_eq!(
            policy_used,
            &policy(&call, &w.path("home"), &w.path("claude-program"), &[]).unwrap()
        );
        assert_eq!(file, &w.path("worker/settings.sandbox.json"));
        assert!(
            !w.path("ran").exists(),
            "nothing runs until the terminal does"
        );
    }

    #[test]
    fn the_transcript_folder_is_named_as_claude_code_names_it() {
        let config = Path::new("/k/claude/37/worker");
        assert_eq!(
            transcripts(config, Path::new("/Users/me/.kelpie/wt/koji/37")).unwrap(),
            Path::new("/k/claude/37/worker/projects/-Users-me--kelpie-wt-koji-37")
        );
        // An emoji is two UTF-16 units, so two dashes.
        assert_eq!(
            transcripts(config, Path::new("/x/é🐑")).unwrap(),
            Path::new("/k/claude/37/worker/projects/-x----")
        );
        let long = format!("/{}", "a".repeat(200));
        assert!(matches!(
            transcripts(config, Path::new(&long)),
            Err(AgentError::Setup(_))
        ));
    }
}
