//! The sandbox a Claude Code call runs in, whole
//!
//! Claude Code, its tools, hooks and MCP servers all run inside kelpie's
//! sandbox. Besides what the call's fence allows, it writes its transcript
//! folder and a scratch folder, and reaches the model's endpoint. Its own
//! sandbox stays off: on macOS a sandbox cannot start inside another.

use std::path::{Path, PathBuf};

use crate::ports::{AgentCall, AgentError, Fence, Policy};
use crate::profile::CREDENTIALS;

/// The model's endpoint, the one host every call reaches
///
/// Not `platform.claude.com`, where a login is refreshed: the sandbox
/// cannot write the keychain, so a refresh there would be lost.
const MODEL_HOST: &str = "api.anthropic.com";

/// The macOS service a dev server's file watcher looks up
const DEV_SERVER_SERVICES: [&str; 1] = ["com.apple.FSEvents"];

// Claude Code names a working folder's transcript folder this way, and
// hashes past this length. Kelpie refuses a folder it cannot name.
const LONGEST_NAME: usize = 200;

/// What a fenced session may do, from its fence alone
pub(crate) fn fence_policy(fence: &Fence) -> Policy {
    Policy {
        write: fence.write.clone(),
        no_write: fence.no_write.clone(),
        no_read: fence.no_read.clone(),
        read: Vec::new(),
        hosts: fence.hosts.clone(),
        sockets: Vec::new(),
        listen: fence.preview.is_some(),
        services: match fence.preview {
            Some(_) => DEV_SERVER_SERVICES.map(str::to_owned).into(),
            None => Vec::new(),
        },
        // Without it, `gh` fails TLS verification on macOS: x509 OSStatus -26276.
        verify_tls: true,
    }
}

/// The whole call's policy: its fence, or none, and what Claude Code itself needs
///
/// # Errors
///
/// [`AgentError::Setup`] when the call's transcript folder cannot be named.
pub(crate) fn policy(call: &AgentCall, home: &Path) -> Result<Policy, AgentError> {
    let mut policy = match &call.reach.fence {
        Some(fence) => fence_policy(fence),
        None => Policy {
            no_read: CREDENTIALS.map(str::to_owned).into(),
            ..Policy::default()
        },
    };
    let transcripts = transcripts(home, &call.cwd)?;
    // Auto memory there loads into every session in the folder, a review's too.
    policy.no_write.push(transcripts.join("memory"));
    policy.write.extend([transcripts, scratch(call)]);
    // Claude Code reads its own files for the call wherever they are kept.
    policy.read.extend(
        [&call.settings, &scratch(call)]
            .into_iter()
            .chain(&call.instructions)
            .chain(&call.mcp_config)
            .chain(&call.plugin_dirs)
            .chain(&call.reach.read)
            .cloned(),
    );
    policy.hosts.push(MODEL_HOST.to_owned());
    Ok(policy)
}

/// The folder Claude Code keeps the call's transcripts in
///
/// # Errors
///
/// [`AgentError::Setup`] when the working folder's name is past Claude Code's limit.
pub(crate) fn transcripts(home: &Path, cwd: &Path) -> Result<PathBuf, AgentError> {
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_owned());
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
    Ok(home.join(".claude").join("projects").join(name))
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
    use crate::adapters::{ClaudeCli, SandboxRuntime};
    use crate::ports::{Agents, Reach, Role, Session, SessionId, Tools};
    use crate::preview::Tools as KelpieTools;
    use crate::profile::WorkerProfile;
    use crate::settings::Effort;
    use crate::test::OpenSandbox;

    const RESULT: &str = include_str!("../../../fixtures/claude-p-result.json");

    struct World {
        dir: tempfile::TempDir,
    }

    impl World {
        fn new() -> Self {
            let w = Self {
                dir: tempfile::tempdir().unwrap(),
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
            crate::test::write_script(&w.path("claude"), &script);
            w
        }

        fn path(&self, p: &str) -> PathBuf {
            self.dir.path().canonicalize().unwrap().join(p)
        }

        fn call(&self, role: Role) -> AgentCall {
            AgentCall {
                role,
                issue: 7,
                model: "claude-sonnet-5".into(),
                effort: Effort::Medium,
                session: Session::New(SessionId("s".into())),
                cwd: self.path("wt"),
                settings: self.path("worker/settings.json"),
                instructions: None,
                prompt: "go".into(),
                timeout: None,
                mcp_config: None,
                plugin_dirs: Vec::new(),
                tools: Tools::Answer,
                reach: Reach::default(),
            }
        }

        fn worker(&self) -> AgentCall {
            let (wt, build) = (self.path("wt"), self.path("build"));
            let profile = WorkerProfile {
                worktree: &wt,
                build: &build,
                git_common_dir: Path::new("/k/repo/.git"),
                git_dir: Path::new("/k/repo/.git/worktrees/7"),
                branch: "kelpie/7",
                kelpie: Path::new("/k/kelpie"),
                guard_hooks: &[],
                allowed_domains: &[],
                build_env: &BTreeMap::new(),
                preview: None,
                shep_home: Path::new("/k/shep"),
            };
            AgentCall {
                tools: Tools::Work,
                reach: profile.reach(),
                ..self.call(Role::Worker)
            }
        }

        fn cli(&self, sandbox: Arc<dyn crate::ports::Sandbox>) -> ClaudeCli {
            ClaudeCli::with_program(self.path("claude")).sandboxed(sandbox, self.path("home"))
        }
    }

    #[test]
    fn every_role_runs_inside_the_sandbox_with_its_settings_beside_claudes() {
        let w = World::new();
        let sandbox = OpenSandbox::default();
        let cli = w.cli(Arc::new(sandbox.clone()));
        for call in [w.worker(), w.call(Role::Reviewer), w.call(Role::Judge)] {
            cli.prepare(&call).unwrap();
            cli.run(&call).unwrap();
        }
        let wrapped = sandbox.wrapped();
        assert_eq!(wrapped.len(), 3);
        for (_, settings) in &wrapped {
            assert_eq!(settings, &w.path("worker/settings.sandbox.json"));
        }
    }

    #[test]
    fn a_missing_sandbox_runs_nothing() {
        let w = World::new();
        let none = SandboxRuntime::new(KelpieTools::at(w.path("no-tools")));
        let cli = w.cli(Arc::new(none));
        for call in [w.worker(), w.call(Role::Judge)] {
            let err = cli.run(&call).unwrap_err();
            assert!(
                matches!(&err, AgentError::Setup(why) if why.contains("tools install")),
                "{err:?}"
            );
        }
        assert!(!w.path("ran").exists(), "claude ran without its sandbox");
    }

    #[test]
    fn a_worker_writes_its_fence_its_transcripts_and_its_scratch_and_reaches_the_model() {
        let w = World::new();
        let policy = policy(&w.worker(), &w.path("home")).unwrap();
        let transcripts = transcripts(&w.path("home"), &w.path("wt")).unwrap();
        let scratch = w.path("worker/settings.tmp");
        assert_eq!(policy.write[..2], [w.path("wt"), w.path("build")]);
        assert_eq!(
            policy.write[policy.write.len() - 2..],
            [transcripts.clone(), scratch.clone()]
        );
        assert!(policy.no_write.contains(&transcripts.join("memory")));
        assert!(policy.no_write.contains(&w.path("wt/.claude")));
        assert!(policy.no_write.contains(&w.path("wt/**/.codex")));
        assert_eq!(policy.hosts, ["github.com", "api.github.com", MODEL_HOST]);
        assert!(policy.read.contains(&w.path("worker/settings.json")));
        assert!(policy.no_read.iter().any(|p| p == "~/.ssh/**"));
        assert!(policy.verify_tls);
        assert!(!policy.listen);
    }

    #[test]
    fn a_review_writes_nothing_of_the_worktrees_and_reaches_only_the_model() {
        let w = World::new();
        let mut review = w.call(Role::Reviewer);
        review.settings = w.path("worker/review-settings.json");
        review.reach.read = vec![w.path("shots")];
        let policy = policy(&review, &w.path("home")).unwrap();
        assert_eq!(
            policy.write,
            [
                transcripts(&w.path("home"), &w.path("wt")).unwrap(),
                w.path("worker/review-settings.tmp"),
            ]
        );
        assert_eq!(policy.hosts, [MODEL_HOST]);
        assert_eq!(policy.no_read, CREDENTIALS.map(str::to_owned));
        assert!(policy.read.contains(&w.path("shots")));
        assert!(!policy.verify_tls);
    }

    #[test]
    fn the_scratch_folder_is_emptied_and_a_link_there_is_not_followed() {
        let w = World::new();
        let cli = w.cli(Arc::new(OpenSandbox::default()));
        let call = w.worker();
        std::fs::create_dir_all(w.path("elsewhere")).unwrap();
        std::fs::write(w.path("elsewhere/keep"), "").unwrap();
        std::os::unix::fs::symlink(w.path("elsewhere"), scratch(&call)).unwrap();
        cli.run(&call).unwrap();
        assert!(w.path("elsewhere/keep").exists());
        assert!(scratch(&call).is_dir() && !scratch(&call).is_symlink());
        assert!(
            transcripts(&w.path("home"), &w.path("wt"))
                .unwrap()
                .is_dir()
        );
    }

    #[test]
    fn the_transcript_folder_is_named_as_claude_code_names_it() {
        let home = Path::new("/Users/me");
        assert_eq!(
            transcripts(home, Path::new("/Users/me/.kelpie/wt/hazels-lab/37")).unwrap(),
            Path::new("/Users/me/.claude/projects/-Users-me--kelpie-wt-hazels-lab-37")
        );
        // An emoji is two UTF-16 units, so two dashes.
        assert_eq!(
            transcripts(home, Path::new("/x/é🐑")).unwrap(),
            Path::new("/Users/me/.claude/projects/-x----")
        );
        let long = format!("/{}", "a".repeat(200));
        assert!(matches!(
            transcripts(home, Path::new(&long)),
            Err(AgentError::Setup(_))
        ));
    }
}
