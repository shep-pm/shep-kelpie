//! The worker's profile: the settings file and instructions kelpie starts it with
//!
//! The worker runs in `bypassPermissions`, so the file is its whole fence.
//! Claude Code's sandbox confines Bash and its children, and fails closed.
//! The sandbox does not cover Claude's own file tools, so a hook that runs
//! `shep-kelpie confine` holds those to the same folders. Both refuse Claude
//! Code's own files in the worktree. Deny rules keep what only the project
//! manager does, and credential paths, out of reach. `shep-kelpie guard` judges
//! every Bash call before any hook the project adds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::fence;
use crate::settings::{BuildDir, EnvName, GuardHook, HookEvent, NonBlank};
use crate::worktree::BASE;

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
pub(crate) const CREDENTIALS: [&str; 12] = [
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
    // The webhook URL's old file. Not all of `~/.kelpie`: worktrees and build
    // folders live there. The shepherd's home, with `dogs.toml`, is the profile's own.
    "~/.kelpie/settings.toml",
    // The authenticator secret, which answers a ruling from ntfy
    "~/.kelpie/totp/**",
];

// What only the project manager does: merge, mark ready, and summon.
const PM_ONLY: [&str; 5] = [
    "Bash(gh pr merge)",
    "Bash(gh pr merge *)",
    "Bash(gh pr ready)",
    "Bash(gh pr ready *)",
    "Bash(gh *review please*)",
];

// `gh api` could merge or relabel around the rules above, and `gh auth token`
// prints the maintainer's token. No worker needs either.
// Tools no worker needs that reach past its fence: `Monitor` runs a command
// or WebSocket no hook judges; `RemoteTrigger` starts cloud agents on the
// maintainer's account with none of this file; `Workflow` agents escape the
// worker's pacing; and kelpie, not the worker, owns its worktree.
const TOOLS_DENY: [&str; 5] = [
    "Monitor",
    "RemoteTrigger",
    "Workflow",
    "EnterWorktree",
    "ExitWorktree",
];

// What `shep-kelpie guard` judges: every command, and a subagent's isolation.
const GUARDED_TOOLS: &str = "Bash|Agent|Task";

const GH_DENY: [&str; 4] = [
    "Bash(gh api)",
    "Bash(gh api *)",
    "Bash(gh auth)",
    "Bash(gh auth *)",
];

// Pushes that rewrite or delete branches on the remote whatever they name:
// `--mirror` and `--all` push every local branch, and a `+` refspec forces.
// A rule's `*` needs something to match, so a flag straight after `push`
// takes a rule of its own.
const PUSH_FLAGS: [&str; 9] = [
    "Bash(git push *--mirror*)",
    "Bash(git push *--all*)",
    "Bash(git push *--delete*)",
    "Bash(git push -d*)",
    "Bash(git push * -d*)",
    "Bash(git push *--force*)",
    "Bash(git push -f*)",
    "Bash(git push * -f*)",
    "Bash(git push * +*)",
];

// Every Playwright MCP tool, which `shep-kelpie browse-guard` holds to the preview
const PLAYWRIGHT_TOOLS: &str = "mcp__playwright__.*";

// The mach service a dev server's file watcher looks up on macOS
const DEV_SERVER_MACH: [&str; 1] = ["com.apple.FSEvents"];

// The Playwright MCP server runs outside the sandbox. These tools read a local
// file (or run code that could), so none is the worker's.
const PLAYWRIGHT_DENY: [&str; 4] = [
    "mcp__playwright__browser_run_code_unsafe",
    "mcp__playwright__browser_file_upload",
    "mcp__playwright__browser_drop",
    "mcp__playwright__browser_set_storage_state",
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
    /// The project's own guard hooks, which run after kelpie's
    pub guard_hooks: &'a [GuardHook],
    /// The domains the project's settings add to GitHub's
    pub allowed_domains: &'a [NonBlank],
    /// Variables the project's settings point into the build folder
    pub build_env: &'a BTreeMap<EnvName, BuildDir>,
    /// The preview's domains, for a project with the preview on; `None` with it off
    pub preview: Option<&'a [NonBlank]>,
    /// The shepherd's home, whose `dogs.toml` holds the webhook's URL
    pub shep_home: &'a Path,
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
        // The clone's own base branch, which a push of every branch would carry.
        let deny_write: Vec<PathBuf> = GIT_DENY
            .iter()
            .map(|p| git(p))
            .chain([git("refs/heads").join(BASE)])
            .chain(fence::deny_write(self.worktree))
            .collect();
        // `//` roots a rule at `/`: a single `/` is taken from the settings file.
        let shep_home = format!("Read(/{}/**)", self.shep_home.display());
        let mut deny: Vec<String> = CREDENTIALS
            .iter()
            .map(|p| format!("Read({p})"))
            .chain([shep_home])
            .chain(PM_ONLY.iter().map(|&r| r.to_owned()))
            .chain(GH_DENY.iter().map(|&r| r.to_owned()))
            .chain(TOOLS_DENY.iter().map(|&r| r.to_owned()))
            .chain(push_to_base())
            .chain(PUSH_FLAGS.iter().map(|&r| r.to_owned()))
            .collect();
        let preview_domains = self.preview.unwrap_or_default();
        let domains: Vec<&str> = GITHUB
            .into_iter()
            .chain(self.allowed_domains.iter().map(NonBlank::as_str))
            .chain(preview_domains.iter().map(NonBlank::as_str))
            .collect();
        // Without `strictAllowlist`, `bypassPermissions` lets a host outside the list through.
        let mut network = json!({ "allowedDomains": domains, "strictAllowlist": true });
        if self.preview.is_some() {
            // A dev server binds a local port, and its file watcher needs FSEvents.
            network["allowLocalBinding"] = true.into();
            network["allowMachLookup"] = json!(DEV_SERVER_MACH);
            deny.extend(PLAYWRIGHT_DENY.iter().map(|&r| r.to_owned()));
        }
        json!({
            "sandbox": {
                "enabled": true,
                "failIfUnavailable": true,
                "allowUnsandboxedCommands": false,
                // Without it, `gh` fails TLS verification on macOS: x509 OSStatus -26276.
                "enableWeakerNetworkIsolation": true,
                "filesystem": { "allowWrite": allow_write, "denyWrite": deny_write },
                "network": network,
            },
            "permissions": { "deny": deny },
            // A project's own settings could otherwise switch every hook off,
            // `confine` and the guard with them. This file outranks them.
            "disableAllHooks": false,
            "hooks": self.hooks(),
            "env": self.env(),
        })
    }

    fn env(&self) -> Value {
        let mut env = json!({ "CARGO_TARGET_DIR": self.build });
        if self.preview.is_some() {
            // Node's fetch ignores the sandbox's proxy without it.
            env["NODE_USE_ENV_PROXY"] = "1".into();
        }
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
        if let Some(domains) = self.preview {
            let guard = [self.kelpie.to_string_lossy().as_ref(), "browse-guard"]
                .into_iter()
                .chain(domains.iter().map(NonBlank::as_str))
                .map(shell_quote)
                .collect::<Vec<_>>()
                .join(" ");
            pre.push(entry(Some(PLAYWRIGHT_TOOLS), &guard));
        }
        let guard = [
            self.kelpie,
            Path::new("guard"),
            self.git_common_dir,
            self.worktree,
        ]
        .map(|p| shell_quote(&p.to_string_lossy()));
        pre.push(entry(Some(GUARDED_TOOLS), &guard.join(" ")));
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

// Pushes that name the branch kelpie cuts from, as `origin main`,
// `HEAD:main` or `refs/heads/main`. A repo without branch protection
// would otherwise take them.
fn push_to_base() -> impl Iterator<Item = String> {
    [" {b}", " {b} *", ":{b}*", "/{b}*"]
        .into_iter()
        .map(|p| format!("Bash(git push *{})", p.replace("{b}", BASE)))
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
            preview: None,
            shep_home: Path::new("/srv/shep"),
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
                "/k/repos/shep/.git/refs/heads/main",
                "/k/wt/shep/7/.claude",
                "/k/wt/shep/7/**/.claude",
                "/k/wt/shep/7/.mcp.json",
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
            preview: None,
            shep_home: Path::new("/srv/shep"),
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
    fn a_host_outside_the_list_is_refused_not_asked_about() {
        assert_eq!(settings(&[])["sandbox"]["network"]["strictAllowlist"], true);
    }

    fn with_preview(domains: &[NonBlank]) -> Value {
        WorkerProfile {
            worktree: Path::new("/k/wt/lab/7"),
            build: Path::new("/k/targets/lab/7"),
            git_common_dir: Path::new("/k/repos/lab/.git"),
            git_dir: Path::new("/k/repos/lab/.git/worktrees/7"),
            branch: "kelpie/7",
            kelpie: Path::new("/opt/kelpie"),
            guard_hooks: &[],
            allowed_domains: &[],
            build_env: &BTreeMap::new(),
            preview: Some(domains),
            shep_home: Path::new("/srv/shep"),
        }
        .settings()
    }

    #[test]
    fn a_preview_lets_a_dev_server_bind_and_watch_and_nothing_wider() {
        let api = [NonBlank::try_from("pokemon-go-api.github.io".to_owned()).unwrap()];
        let s = with_preview(&api);
        assert_eq!(
            s["sandbox"]["network"],
            json!({
                "allowedDomains": ["github.com", "api.github.com", "pokemon-go-api.github.io"],
                "strictAllowlist": true,
                "allowLocalBinding": true,
                "allowMachLookup": ["com.apple.FSEvents"],
            })
        );
        assert_eq!(s["env"]["NODE_USE_ENV_PROXY"], "1");
        assert_eq!(
            s["sandbox"]["filesystem"],
            settings(&[])["sandbox"]["filesystem"]
                .to_string()
                .replace("/shep/", "/lab/")
                .parse::<Value>()
                .unwrap(),
            "the write fence does not move"
        );
    }

    #[test]
    fn a_preview_denies_the_playwright_tools_that_reach_past_the_page() {
        let deny = with_preview(&[])["permissions"]["deny"].clone();
        let deny = strings(&deny);
        for tool in [
            "browser_run_code_unsafe",
            "browser_file_upload",
            "browser_drop",
            "browser_set_storage_state",
        ] {
            let rule = format!("mcp__playwright__{tool}");
            assert!(deny.contains(&rule.as_str()), "{rule}");
        }
    }

    // The preview widens nothing the fence on Claude Code's own files holds.
    #[test]
    fn a_preview_keeps_the_fence_on_claude_files_beside_its_own() {
        let s = with_preview(&[]);
        let deny_write = strings(&s["sandbox"]["filesystem"]["denyWrite"]);
        for path in [
            "/k/wt/lab/7/.claude",
            "/k/wt/lab/7/**/.claude",
            "/k/wt/lab/7/.mcp.json",
        ] {
            assert!(deny_write.contains(&path), "{path}: {deny_write:?}");
        }
        assert!(deny_write.contains(&"/k/repos/lab/.git/config"));
        let deny = s["permissions"]["deny"].to_string();
        assert!(deny.contains("Read(~/.ssh/**)"), "{deny}");
        assert!(deny.contains("mcp__playwright__browser_drop"), "{deny}");
        let pre = s["hooks"]["PreToolUse"].to_string();
        assert!(pre.contains("'confine'"), "{pre}");
        assert!(pre.contains("'browse-guard'"), "{pre}");
    }

    #[test]
    fn a_preview_holds_every_playwright_tool_to_kelpies_browse_guard() {
        let domains = [NonBlank::try_from("*.leekduck.com".to_owned()).unwrap()];
        assert_eq!(
            with_preview(&domains)["hooks"]["PreToolUse"][1],
            json!({
                "matcher": "mcp__playwright__.*",
                "hooks": [{
                    "type": "command",
                    "command": "'/opt/kelpie' 'browse-guard' '*.leekduck.com'",
                }],
            })
        );
        let hooks = settings(&[])["hooks"].to_string();
        assert!(!hooks.contains("browse-guard"), "{hooks}");
    }

    #[test]
    fn without_a_preview_nothing_binds_a_port() {
        let s = settings(&[]);
        let network = s["sandbox"]["network"].as_object().unwrap();
        assert!(!network.contains_key("allowLocalBinding"), "{network:?}");
        assert!(!network.contains_key("allowMachLookup"), "{network:?}");
        assert_eq!(s["env"]["NODE_USE_ENV_PROXY"], Value::Null);
        let deny = s["permissions"]["deny"].to_string();
        assert!(!deny.contains("mcp__playwright"), "{deny}");
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
    fn a_projects_settings_cannot_switch_the_hooks_off() {
        assert_eq!(settings(&[])["disableAllHooks"], false);
        assert_eq!(with_preview(&[])["disableAllHooks"], false);
    }

    #[test]
    fn tools_that_reach_past_the_fence_are_denied() {
        let deny = settings(&[])["permissions"]["deny"].clone();
        for tool in [
            "Monitor",
            "RemoteTrigger",
            "Workflow",
            "EnterWorktree",
            "ExitWorktree",
        ] {
            assert!(strings(&deny).contains(&tool), "{tool}: {deny}");
        }
    }

    #[test]
    fn gh_api_and_gh_auth_are_denied() {
        let deny = settings(&[])["permissions"]["deny"].clone();
        let deny = strings(&deny);
        for rule in [
            "Bash(gh api)",
            "Bash(gh api *)",
            "Bash(gh auth)",
            "Bash(gh auth *)",
        ] {
            assert!(deny.contains(&rule), "{rule}");
        }
    }

    #[test]
    fn a_push_to_the_base_branch_or_with_a_forcing_flag_is_denied() {
        let deny = settings(&[])["permissions"]["deny"].clone();
        assert_eq!(
            strings(&deny)
                .into_iter()
                .filter(|r| r.starts_with("Bash(git push"))
                .collect::<Vec<_>>(),
            [
                "Bash(git push * main)",
                "Bash(git push * main *)",
                "Bash(git push *:main*)",
                "Bash(git push */main*)",
                "Bash(git push *--mirror*)",
                "Bash(git push *--all*)",
                "Bash(git push *--delete*)",
                "Bash(git push -d*)",
                "Bash(git push * -d*)",
                "Bash(git push *--force*)",
                "Bash(git push -f*)",
                "Bash(git push * -f*)",
                "Bash(git push * +*)",
            ]
        );
    }

    #[test]
    fn the_clones_base_branch_is_not_writable() {
        let s = settings(&[]);
        let deny = strings(&s["sandbox"]["filesystem"]["denyWrite"]);
        assert!(
            deny.contains(&"/k/repos/shep/.git/refs/heads/main"),
            "{deny:?}"
        );
        let allow = strings(&s["sandbox"]["filesystem"]["allowWrite"]);
        assert!(!allow.iter().any(|p| p.ends_with("/main")), "{allow:?}");
    }

    // Unset, the sandbox blocks every Unix socket, kelpie's shepherd socket
    // included, so a worker cannot `shep trigger` its own ruling's answer.
    #[test]
    fn a_worker_reaches_no_unix_socket() {
        let s = settings(&[]);
        let network = s["sandbox"]["network"].as_object().unwrap();
        assert!(!network.contains_key("allowUnixSockets"), "{network:?}");
        assert!(!network.contains_key("allowAllUnixSockets"), "{network:?}");
    }

    #[test]
    fn credential_paths_are_unreadable() {
        let deny = settings(&[])["permissions"]["deny"].clone();
        let deny = strings(&deny);
        for rule in [
            "Read(~/.ssh/**)",
            "Read(~/.kelpie/projects/**)",
            "Read(~/.kelpie/settings.toml)",
            "Read(~/.kelpie/totp/**)",
            "Read(//srv/shep/**)",
        ] {
            assert!(deny.contains(&rule), "{rule}");
        }
        assert!(
            !deny.iter().any(|r| r.contains(".config/gh")),
            "gh cannot start without its config"
        );
        assert!(
            !deny.contains(&"Read(~/.kelpie/**)"),
            "a worker's worktree and build folder are under ~/.kelpie"
        );
    }

    #[test]
    fn every_worker_has_kelpies_guard_on_bash_and_agents_with_no_project_hooks() {
        let s = settings(&[]);
        assert_eq!(
            s["hooks"]["PreToolUse"][1],
            json!({
                "matcher": "Bash|Agent|Task",
                "hooks": [{
                    "type": "command",
                    "command": r"'/opt/kelpie'\''s bin/kelpie' 'guard' '/k/repos/shep/.git' '/k/wt/shep/7'",
                }],
            })
        );
        assert_eq!(s["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(s["hooks"]["PostToolUse"], Value::Null);
        let preview = with_preview(&[])["hooks"]["PreToolUse"][2].to_string();
        assert!(preview.contains("'guard'"), "{preview}");
    }

    #[test]
    fn the_projects_guard_hooks_are_carried_after_kelpies_own() {
        let s = settings(&[
            guard(HookEvent::PreToolUse, Some("Bash"), "~/bin/before"),
            guard(HookEvent::PostToolUse, None, "~/bin/after"),
        ]);
        assert_eq!(
            s["hooks"]["PreToolUse"][2],
            json!({
                "matcher": "Bash",
                "hooks": [{ "type": "command", "command": "~/bin/before" }],
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
