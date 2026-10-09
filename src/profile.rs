//! The worker's profile: the sandbox and instructions kelpie starts it with
//!
//! The worker runs with no one to ask, so its sandbox is its whole fence.
//! Writes go to its worktree, its build folder and what a commit and a push
//! need, and never to agents' own files in the worktree. Credential
//! paths are unreadable, hosts are GitHub's and the project's, and what only
//! the project manager does is a command it may not run. Each harness's
//! adapter enforces the fence its own way.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::fence;
use crate::lease::door;
use crate::ports::{Fence, Guard, Reach};
use crate::settings::{BuildDir, EnvName, GuardHook, NonBlank};
use crate::worktree::BASE;

/// Kelpie's instructions to every worker, appended to its system prompt
pub const INSTRUCTIONS: &str = include_str!("worker-instructions.md");

/// Kelpie's own worker instructions, naming `kelpie`, the binary a worker
/// runs its tests under the machine's test lease with
pub fn instructions(kelpie: &Path) -> String {
    INSTRUCTIONS.replace("{kelpie}", &kelpie.display().to_string())
}

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

// Read and written by no worker. `~/.config/gh` stays readable: `gh` will
// not start without its config, and the worker opens its own pull request.
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
    // Kelpie's old home. The maintainer's disk still holds a project's state
    // and settings file, the webhook URL's old file and the authenticator
    // secret there, so these rules still protect real files. Kelpie's home
    // and the shepherd's are the profile's own.
    "~/.kelpie/projects/**",
    "~/.kelpie/settings.toml",
    "~/.kelpie/totp/**",
];

// What only the project manager does: merge, mark ready, and summon. `kelpie
// guard` refuses these in any shape; these match a command's prefix only.
const PM_ONLY: [&str; 5] = [
    "gh pr merge",
    "gh pr merge *",
    "gh pr ready",
    "gh pr ready *",
    "gh *review please*",
];

// `gh api` could merge or relabel around the rules above, and `gh auth token`
// prints the maintainer's token. No worker needs either.
const GH_DENY: [&str; 4] = ["gh api", "gh api *", "gh auth", "gh auth *"];

// Pushes that rewrite or delete branches on the remote whatever they name:
// `--mirror` and `--all` push every local branch, and a `+` refspec forces.
// A rule's `*` needs something to match, so a flag straight after `push`
// takes a rule of its own.
const PUSH_FLAGS: [&str; 9] = [
    "git push *--mirror*",
    "git push *--all*",
    "git push *--delete*",
    "git push -d*",
    "git push * -d*",
    "git push *--force*",
    "git push -f*",
    "git push * -f*",
    "git push * +*",
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
    /// The kelpie binary, which runs kelpie's own checks
    pub kelpie: &'a Path,
    /// Kelpie's home, whose path the guard keeps off the forge
    pub kelpie_home: &'a Path,
    /// The project's checkout, whose path the guard keeps off the forge
    pub repo: &'a Path,
    /// The project's own guard hooks, which run after kelpie's
    pub guard_hooks: &'a [GuardHook],
    /// The domains the project's settings add to GitHub's
    pub allowed_domains: &'a [NonBlank],
    /// Variables the project's settings point into the build folder
    pub build_env: &'a BTreeMap<EnvName, BuildDir>,
    /// The shepherd's home, whose `dogs.toml` holds the webhook's URL. The
    /// worker reads none of it but its own folders in kelpie's.
    pub shep_home: &'a Path,
    /// The dog's door, the one Unix socket the worker may connect to
    pub door: &'a Path,
}

impl WorkerProfile<'_> {
    /// The worker's sandbox
    pub fn reach(&self) -> Reach {
        let git = |p: &str| self.git_common_dir.join(p);
        let branch_ref = git("refs/heads").join(self.branch);
        let tracking_ref = git("refs/remotes/origin").join(self.branch);
        let write = vec![
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
        let no_write: Vec<PathBuf> = GIT_DENY
            .iter()
            .map(|p| git(p))
            .chain([git("refs/heads").join(BASE)])
            .chain(fence::deny_write(self.worktree))
            .collect();
        // Kelpie's home is the shepherd's too unless `KELPIE_HOME` puts it elsewhere.
        let homes = [self.shep_home, self.kelpie_home]
            .into_iter()
            .enumerate()
            .filter(|&(i, home)| i == 0 || !home.starts_with(self.shep_home))
            .map(|(_, home)| format!("{}/**", home.display()));
        let no_read = CREDENTIALS
            .iter()
            .map(|&p| p.to_owned())
            .chain(homes)
            .collect();
        let read = [self.worktree, self.build, self.kelpie]
            .into_iter()
            .map(Path::to_owned)
            .collect();
        let no_commands = PM_ONLY
            .iter()
            .chain(&GH_DENY)
            .map(|&c| c.to_owned())
            .chain(push_to_base())
            .chain(PUSH_FLAGS.iter().map(|&c| c.to_owned()))
            .collect();
        let hosts = GITHUB
            .into_iter()
            .chain(self.allowed_domains.iter().map(NonBlank::as_str))
            .map(str::to_owned)
            .collect();
        let fence = Fence {
            write,
            no_write,
            no_read,
            read,
            hosts,
            no_commands,
            env: self.env(),
            sockets: vec![self.door.to_owned()],
            guard: Guard {
                kelpie: self.kelpie.to_owned(),
                worktree: self.worktree.to_owned(),
                build: self.build.to_owned(),
                git_common_dir: self.git_common_dir.to_owned(),
                folders: vec![self.kelpie_home.to_owned(), self.repo.to_owned()],
                issues: None,
            },
            hooks: self.guard_hooks.to_vec(),
        };
        Reach {
            read: Vec::new(),
            fence: Some(Box::new(fence)),
        }
    }

    fn env(&self) -> BTreeMap<String, PathBuf> {
        let cargo = ("CARGO_TARGET_DIR".to_owned(), self.build.to_owned());
        let door = (door::SOCKET_VAR.to_owned(), self.door.to_owned());
        let project = self
            .build_env
            .iter()
            .map(|(name, dir)| (name.as_str().to_owned(), self.build.join(dir.as_path())));
        [cargo, door].into_iter().chain(project).collect()
    }
}

// Pushes that name the branch kelpie cuts from, as `origin main`,
// `HEAD:main` or `refs/heads/main`. A repo without branch protection
// would otherwise take them. The guard reads every spelling; these are its backup.
fn push_to_base() -> impl Iterator<Item = String> {
    [" {b}", " {b} *", ":{b}*", "/{b}*"]
        .into_iter()
        .map(|p| format!("git push *{}", p.replace("{b}", BASE)))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    s.into()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::ports::Tools;
    use crate::settings::{HookEvent, NonBlank};

    impl WorkerProfile<'_> {
        // The profile as Claude Code's settings file carries it, with the
        // sandbox kelpie runs the call in under `sandbox`, as srt reads it
        fn settings(&self) -> Value {
            let reach = self.reach();
            let mut s = crate::adapters::claude_settings(Tools::Work, &reach);
            assert_eq!(s["sandbox"], json!({ "enabled": false }));
            let fence = reach.fence.as_deref().expect("a worker is fenced");
            s["sandbox"] = crate::adapters::srt_settings(&crate::adapters::fence_policy(fence));
            s
        }
    }

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
            kelpie_home: Path::new("/k"),
            repo: Path::new("/k/repos/shep"),
            guard_hooks: hooks,
            allowed_domains: domains,
            build_env: &BTreeMap::new(),
            shep_home: Path::new("/srv/shep"),
            door: Path::new("/k/dog/lease.sock"),
        }
        .settings()
    }

    // A `KELPIE_HOME` set outside the shepherd's home holds the TOTP secret
    // and other projects' state just the same.
    #[test]
    fn a_kelpie_home_outside_the_shepherds_is_denied_around_the_workers_own() {
        let dir = tempfile::tempdir().unwrap();
        let (kelpie, shep) = (dir.path().join("k"), dir.path().join("shep"));
        let (worktree, build) = (
            kelpie.join("koji/worktrees/7"),
            kelpie.join("koji/builds/7"),
        );
        for folder in [
            &worktree,
            &build,
            &kelpie.join("totp"),
            &kelpie.join("rotom"),
            &shep,
        ] {
            std::fs::create_dir_all(folder).unwrap();
        }
        let s = WorkerProfile {
            worktree: &worktree,
            build: &build,
            git_common_dir: Path::new("/r/.git"),
            git_dir: Path::new("/r/.git/worktrees/7"),
            branch: "kelpie/7",
            kelpie: Path::new("/opt/kelpie"),
            kelpie_home: &kelpie,
            repo: Path::new("/r"),
            guard_hooks: &[],
            allowed_domains: &[],
            build_env: &BTreeMap::new(),
            shep_home: &shep,
            door: Path::new("/s/kelpie/dog/lease.sock"),
        }
        .settings();

        let files = &s["sandbox"]["filesystem"];
        let denied = strings(&files["denyRead"]);
        for home in [&kelpie, &shep] {
            assert!(
                denied.contains(&format!("{}/**", home.display()).as_str()),
                "{denied:?}"
            );
        }
        assert!(strings(&files["allowRead"]).contains(&worktree.to_str().unwrap()));
        let deny = strings(&s["permissions"]["deny"]);
        for rule in ["totp/**", "rotom/**"] {
            let rule = format!("Read(/{}/{rule})", kelpie.display());
            assert!(deny.contains(&rule.as_str()), "{rule} not in {deny:?}");
        }
        let kelpie_rule = format!("Read(/{}/**)", kelpie.display());
        assert!(
            !deny.contains(&kelpie_rule.as_str()),
            "it would hide the worktree"
        );
    }

    fn strings(v: &Value) -> Vec<&str> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect()
    }

    // The whole process runs in kelpie's sandbox, which has no exceptions to
    // ask for: no command runs outside it, and a missing one runs nothing.
    #[test]
    fn the_sandbox_has_no_way_out() {
        let s = settings(&[]);
        for key in [
            "excludedCommands",
            "allowUnsandboxedCommands",
            "allowAllUnixSockets",
        ] {
            assert_eq!(s["sandbox"][key], Value::Null, "{key}");
            assert_eq!(s["sandbox"]["network"][key], Value::Null, "{key}");
        }
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
                "/k/wt/shep/7/.codex",
                "/k/wt/shep/7/**/.codex",
                "/k/wt/shep/7/.agents",
                "/k/wt/shep/7/**/.agents",
                "/k/wt/shep/7/.pi",
                "/k/wt/shep/7/**/.pi",
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
            kelpie_home: Path::new("/k"),
            repo: Path::new("/k/repos/shep"),
            guard_hooks: &[],
            allowed_domains: &[],
            build_env: &build_env,
            shep_home: Path::new("/srv/shep"),
            door: Path::new("/k/dog/lease.sock"),
        }
        .settings();
        assert_eq!(
            s["env"],
            json!({
                "CARGO_TARGET_DIR": "/k/targets/lab/7",
                "BUN_INSTALL_CACHE_DIR": "/k/targets/lab/7/bun",
                "KELPIE_LEASE_SOCKET": "/k/dog/lease.sock",
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

    #[test]
    fn a_worker_binds_no_port() {
        let s = settings(&[]);
        let network = s["sandbox"]["network"].as_object().unwrap();
        assert!(!network.contains_key("allowLocalBinding"), "{network:?}");
        assert!(!network.contains_key("allowMachLookup"), "{network:?}");
        assert_eq!(s["env"]["NODE_USE_ENV_PROXY"], Value::Null);
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
    fn a_worker_drops_the_features_it_never_uses() {
        let s = settings(&[]);
        for key in [
            "disableBundledSkills",
            "disableWorkflows",
            "disableClaudeAiConnectors",
            "disableArtifact",
        ] {
            assert_eq!(s[key], true, "{key}");
        }
        assert!(s.get("disableRemoteControl").is_none());
        let deny = strings(&s["permissions"]["deny"]).join(" ");
        for tool in [
            "EnterPlanMode",
            "CronCreate",
            "DesignSync",
            "ScheduleWakeup",
        ] {
            assert!(deny.contains(tool), "{tool}");
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

    // Every other Unix socket stays blocked, kelpie's shepherd socket
    // included, so a worker cannot `shep trigger` its own ruling's answer.
    #[test]
    fn a_worker_reaches_the_dogs_door_and_no_other_unix_socket() {
        let s = settings(&[]);
        let network = s["sandbox"]["network"].as_object().unwrap();
        assert_eq!(network["allowUnixSockets"], json!(["/k/dog/lease.sock"]));
        assert_eq!(
            s["env"]["KELPIE_LEASE_SOCKET"], network["allowUnixSockets"][0],
            "the worker asks at the one socket its sandbox lets it reach"
        );
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
                    "command": r"'/opt/kelpie'\''s bin/kelpie' 'guard' '/k/repos/shep/.git' '/k/wt/shep/7' '--folder=/k' '--folder=/k/repos/shep'",
                }],
            })
        );
        assert_eq!(s["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(s["hooks"]["PostToolUse"], Value::Null);
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
