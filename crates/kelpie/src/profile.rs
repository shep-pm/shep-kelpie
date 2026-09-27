//! The worker's profile: the settings file and instructions kelpie starts it with
//!
//! The worker runs in `bypassPermissions`, so the file is its whole fence.
//! Claude Code's sandbox confines Bash and its children, and fails closed.
//! The sandbox does not cover Claude's own file tools, so a hook that runs
//! `kelpie confine` holds those to the same folders. Deny rules keep what
//! only the project manager does, and credential paths, out of reach.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::settings::{BuildDir, EnvName, GuardHook, HookEvent, NonBlank};

/// Kelpie's instructions to every worker, appended to its system prompt
pub const INSTRUCTIONS: &str = include_str!("worker-instructions.md");

/// The tools that write files without going through Bash
const FILE_TOOLS: &str = "Edit|Write|MultiEdit|NotebookEdit";

// Claude Code opens a worktree's whole common git dir to sandboxed writes.
// These are the parts a commit and a push do not need, and whose change
// would run code outside the sandbox. `refs/remotes` stays open for the
// push's tracking ref; kelpie fetches `origin/main` before cutting from it.
const GIT_DENY: [&str; 8] = [
    "config",
    "hooks",
    "info",
    "modules",
    "HEAD",
    "index",
    "packed-refs",
    "refs/tags",
];

// Read and written by no worker. Also denied to sandboxed Bash, which takes
// `Read` deny rules as its own. `~/.config/gh` stays readable: `gh` will not
// start without its config, and the worker opens its own pull request.
const CREDENTIALS: [&str; 10] = [
    "~/.ssh/**",
    "~/.aws/**",
    "~/.gnupg/**",
    "~/.docker/**",
    "~/.netrc",
    "~/.git-credentials",
    "~/.npmrc",
    "~/.cargo/credentials",
    "~/.cargo/credentials.toml",
    "~/.kelpie/projects/**",
];

// What only the project manager does: merge, mark ready, and summon.
const PM_ONLY: [&str; 5] = [
    "Bash(gh pr merge)",
    "Bash(gh pr merge *)",
    "Bash(gh pr ready)",
    "Bash(gh pr ready *)",
    "Bash(gh *review please*)",
];

// What `git push` and `gh` reach. A project adds its own, such as a registry.
const GITHUB: [&str; 2] = ["github.com", "api.github.com"];

/// Where one worker may write, and what it runs under
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerProfile<'a> {
    /// The work item's worktree
    pub worktree: &'a Path,
    /// The worker's build folder, its `CARGO_TARGET_DIR`
    pub build: &'a Path,
    /// The project repo's common git dir, which holds objects and refs
    pub git_common_dir: &'a Path,
    /// The worktree's own git dir, inside the common one
    pub git_dir: &'a Path,
    /// The work item's branch
    pub branch: &'a str,
    /// The kelpie binary, which the file-tool hook runs
    pub kelpie: &'a Path,
    /// The guard hooks the project's settings name
    pub guard_hooks: &'a [GuardHook],
    /// The domains the project's settings add to GitHub's
    pub allowed_domains: &'a [NonBlank],
    /// Variables the project's settings point into the build folder
    pub build_env: &'a BTreeMap<EnvName, BuildDir>,
}

impl WorkerProfile<'_> {
    /// The settings file's contents
    pub fn settings(&self) -> Value {
        let git = |p: &str| self.git_common_dir.join(p);
        let branch_ref = git("refs/heads").join(self.branch);
        let tracking_ref = git("refs/remotes/origin").join(self.branch);
        let allow_write = vec![
            self.worktree.to_owned(),
            self.build.to_owned(),
            git("objects"),
            self.git_dir.to_owned(),
            git("logs/refs/heads").join(self.branch),
            with_suffix(&branch_ref, ".lock"),
            branch_ref,
            git("logs/refs/remotes/origin").join(self.branch),
            with_suffix(&tracking_ref, ".lock"),
            tracking_ref,
        ];
        let deny_write: Vec<PathBuf> = GIT_DENY.iter().map(|p| git(p)).collect();
        let deny: Vec<String> = CREDENTIALS
            .iter()
            .map(|p| format!("Read({p})"))
            .chain(PM_ONLY.iter().map(|&r| r.to_owned()))
            .collect();
        let domains: Vec<&str> = GITHUB
            .into_iter()
            .chain(self.allowed_domains.iter().map(NonBlank::as_str))
            .collect();
        json!({
            "sandbox": {
                "enabled": true,
                "failIfUnavailable": true,
                "allowUnsandboxedCommands": false,
                // Without it, `gh` fails TLS verification on macOS: x509 OSStatus -26276.
                "enableWeakerNetworkIsolation": true,
                "filesystem": { "allowWrite": allow_write, "denyWrite": deny_write },
                "network": { "allowedDomains": domains },
            },
            "permissions": { "deny": deny },
            "hooks": self.hooks(),
            "env": self.env(),
        })
    }

    fn env(&self) -> Value {
        let mut env = json!({ "CARGO_TARGET_DIR": self.build });
        for (name, dir) in self.build_env {
            env[name.as_str()] = json!(self.build.join(dir.as_path()));
        }
        env
    }

    fn hooks(&self) -> Value {
        let confine = [self.kelpie, Path::new("confine"), self.worktree, self.build]
            .map(|p| shell_quote(&p.to_string_lossy()))
            .join(" ");
        let mut pre = vec![entry(Some(FILE_TOOLS), &confine)];
        let mut post = Vec::new();
        for hook in self.guard_hooks {
            let e = entry(
                hook.matcher.as_ref().map(|m| m.as_str()),
                hook.command.as_str(),
            );
            match hook.event {
                HookEvent::PreToolUse => pre.push(e),
                HookEvent::PostToolUse => post.push(e),
            }
        }
        let mut hooks = json!({ "PreToolUse": pre });
        if !post.is_empty() {
            hooks["PostToolUse"] = post.into();
        }
        hooks
    }
}

fn entry(matcher: Option<&str>, command: &str) -> Value {
    let mut entry = json!({ "hooks": [{ "type": "command", "command": command }] });
    if let Some(matcher) = matcher {
        entry["matcher"] = matcher.into();
    }
    entry
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    s.into()
}

/// `s` as one word to a POSIX shell
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::NonBlank;

    fn guard(event: HookEvent, matcher: Option<&str>, command: &str) -> GuardHook {
        GuardHook {
            event,
            matcher: matcher.map(|m| NonBlank::try_from(m.to_owned()).unwrap()),
            command: NonBlank::try_from(command.to_owned()).unwrap(),
        }
    }

    fn settings(hooks: &[GuardHook]) -> Value {
        with_domains(hooks, &[])
    }

    fn with_domains(hooks: &[GuardHook], domains: &[NonBlank]) -> Value {
        WorkerProfile {
            worktree: Path::new("/k/wt/shep/7"),
            build: Path::new("/k/targets/shep/7"),
            git_common_dir: Path::new("/k/repos/shep/.git"),
            git_dir: Path::new("/k/repos/shep/.git/worktrees/7"),
            branch: "kelpie/7",
            kelpie: Path::new("/opt/kelpie's bin/kelpie"),
            guard_hooks: hooks,
            allowed_domains: domains,
            build_env: &BTreeMap::new(),
        }
        .settings()
    }

    fn strings(v: &Value) -> Vec<&str> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect()
    }

    #[test]
    fn the_sandbox_is_on_and_fails_closed_with_no_way_out() {
        let s = settings(&[]);
        assert_eq!(s["sandbox"]["enabled"], true);
        assert_eq!(s["sandbox"]["failIfUnavailable"], true);
        assert_eq!(s["sandbox"]["allowUnsandboxedCommands"], false);
        assert_eq!(s["sandbox"]["excludedCommands"], Value::Null);
    }

    #[test]
    fn gh_can_verify_tls_inside_the_sandbox() {
        assert_eq!(
            settings(&[])["sandbox"]["enableWeakerNetworkIsolation"],
            true
        );
    }

    #[test]
    fn writes_go_only_to_the_worktree_the_build_folder_and_what_a_commit_and_push_need() {
        let s = settings(&[]);
        assert_eq!(
            strings(&s["sandbox"]["filesystem"]["allowWrite"]),
            [
                "/k/wt/shep/7",
                "/k/targets/shep/7",
                "/k/repos/shep/.git/objects",
                "/k/repos/shep/.git/worktrees/7",
                "/k/repos/shep/.git/logs/refs/heads/kelpie/7",
                "/k/repos/shep/.git/refs/heads/kelpie/7.lock",
                "/k/repos/shep/.git/refs/heads/kelpie/7",
                "/k/repos/shep/.git/logs/refs/remotes/origin/kelpie/7",
                "/k/repos/shep/.git/refs/remotes/origin/kelpie/7.lock",
                "/k/repos/shep/.git/refs/remotes/origin/kelpie/7",
            ]
        );
        assert_eq!(
            strings(&s["sandbox"]["filesystem"]["denyWrite"]),
            [
                "/k/repos/shep/.git/config",
                "/k/repos/shep/.git/hooks",
                "/k/repos/shep/.git/info",
                "/k/repos/shep/.git/modules",
                "/k/repos/shep/.git/HEAD",
                "/k/repos/shep/.git/index",
                "/k/repos/shep/.git/packed-refs",
                "/k/repos/shep/.git/refs/tags",
            ]
        );
        assert_eq!(s["env"]["CARGO_TARGET_DIR"], "/k/targets/shep/7");
    }

    #[test]
    fn build_env_points_into_the_build_folder_beside_cargo() {
        let build_env = BTreeMap::from([(
            EnvName::try_from("BUN_INSTALL_CACHE_DIR".to_owned()).unwrap(),
            BuildDir::try_from("bun".to_owned()).unwrap(),
        )]);
        let s = WorkerProfile {
            worktree: Path::new("/k/wt/lab/7"),
            build: Path::new("/k/targets/lab/7"),
            git_common_dir: Path::new("/k/repos/lab/.git"),
            git_dir: Path::new("/k/repos/lab/.git/worktrees/7"),
            branch: "kelpie/7",
            kelpie: Path::new("/opt/kelpie"),
            guard_hooks: &[],
            allowed_domains: &[],
            build_env: &build_env,
        }
        .settings();
        assert_eq!(
            s["env"],
            json!({
                "CARGO_TARGET_DIR": "/k/targets/lab/7",
                "BUN_INSTALL_CACHE_DIR": "/k/targets/lab/7/bun",
            })
        );
    }

    #[test]
    fn the_network_is_github_and_the_projects_own_domains_only() {
        assert_eq!(
            strings(&settings(&[])["sandbox"]["network"]["allowedDomains"]),
            ["github.com", "api.github.com"]
        );
        let npm = [NonBlank::try_from("registry.npmjs.org".to_owned()).unwrap()];
        assert_eq!(
            strings(&with_domains(&[], &npm)["sandbox"]["network"]["allowedDomains"]),
            ["github.com", "api.github.com", "registry.npmjs.org"]
        );
    }

    #[test]
    fn file_tools_are_held_to_the_same_folders_by_kelpie() {
        let s = settings(&[]);
        assert_eq!(
            s["hooks"]["PreToolUse"][0],
            json!({
                "matcher": "Edit|Write|MultiEdit|NotebookEdit",
                "hooks": [{
                    "type": "command",
                    "command": r"'/opt/kelpie'\''s bin/kelpie' 'confine' '/k/wt/shep/7' '/k/targets/shep/7'",
                }],
            })
        );
    }

    #[test]
    fn what_only_the_project_manager_does_is_denied() {
        let deny = settings(&[])["permissions"]["deny"].clone();
        let deny = strings(&deny);
        for rule in [
            "Bash(gh pr merge)",
            "Bash(gh pr merge *)",
            "Bash(gh pr ready)",
            "Bash(gh pr ready *)",
            "Bash(gh *review please*)",
        ] {
            assert!(deny.contains(&rule), "{rule}");
        }
    }

    #[test]
    fn credential_paths_are_unreadable() {
        let deny = settings(&[])["permissions"]["deny"].clone();
        let deny = strings(&deny);
        for rule in ["Read(~/.ssh/**)", "Read(~/.kelpie/projects/**)"] {
            assert!(deny.contains(&rule), "{rule}");
        }
        assert!(
            !deny.iter().any(|r| r.contains(".config/gh")),
            "gh cannot start without its config"
        );
    }

    #[test]
    fn the_projects_guard_hooks_are_carried_after_kelpies_own() {
        let s = settings(&[
            guard(
                HookEvent::PreToolUse,
                Some("Bash"),
                "node ~/.claude/hooks/git-gh-guard.js",
            ),
            guard(HookEvent::PostToolUse, None, "~/bin/after"),
        ]);
        assert_eq!(
            s["hooks"]["PreToolUse"][1],
            json!({
                "matcher": "Bash",
                "hooks": [{ "type": "command", "command": "node ~/.claude/hooks/git-gh-guard.js" }],
            })
        );
        assert_eq!(
            s["hooks"]["PostToolUse"],
            json!([{ "hooks": [{ "type": "command", "command": "~/bin/after" }] }])
        );
    }

    #[test]
    fn the_instructions_never_mention_money_or_limits() {
        let text = INSTRUCTIONS.to_lowercase();
        for word in ["budget", "cost", "spend", "token", "usage", "$", "limit"] {
            assert!(!text.contains(word), "the instructions mention {word:?}");
        }
    }
}
