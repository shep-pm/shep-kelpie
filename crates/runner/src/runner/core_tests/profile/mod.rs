use serde_json::{Value, json};

use crate::ports::Tools;
use crate::ports::{Fence, InstallationToken};
use crate::profile::housekeeping;
use crate::profile::*;
use crate::settings::HookEvent;
use crate::settings::{BuildDir, EnvName, GuardHook, NonBlank};
use std::collections::BTreeMap;
use std::path::Path;

mod home;
mod maintainer_login;

trait Rendered {
    fn settings(&self) -> Value;
}

impl Rendered for WorkerProfile<'_> {
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
    profile(hooks, domains).settings()
}

static NO_BUILD_ENV: BTreeMap<EnvName, BuildDir> = BTreeMap::new();

fn profile<'a>(hooks: &'a [GuardHook], domains: &'a [NonBlank]) -> WorkerProfile<'a> {
    WorkerProfile {
        worktree: Path::new("/k/wt/shep/7"),
        build: Path::new("/k/targets/shep/7"),
        git_common_dir: Path::new("/k/repos/shep/.git"),
        git_dir: Path::new("/k/repos/shep/.git/worktrees/7"),
        branch: "kelpie/7",
        base: "main",
        kelpie_pushes: false,
        kelpie: Path::new("/opt/kelpie's bin/kelpie"),
        skills: Path::new("/k/shep/skills"),
        kelpie_home: Path::new("/k"),
        repo: Path::new("/k/repos/shep"),
        guard_hooks: hooks,
        allowed_domains: domains,
        registries: &[],
        build_env: &NO_BUILD_ENV,
        home: Path::new("/Users/me"),
        shep_home: Path::new("/srv/shep"),
        door: Path::new("/k/dog/lease.sock"),
        token: None,
        login_token: None,
    }
}

fn covered_fence(token: &InstallationToken) -> Fence {
    let profile = WorkerProfile {
        token: Some(token),
        ..profile(&[], &[])
    };
    *profile.reach().fence.expect("a worker is fenced")
}

// On a repo kelpie's App covers, the worker's credential is a token for
// that repo alone, and the maintainer's login, `gh`'s config folder and
// the keychain are out of its reach. The guard parses commands and can
// miss one, so this is the boundary.
#[test]
fn on_a_repo_the_app_covers_a_worker_holds_the_apps_token_and_never_the_maintainers_login() {
    let fence = covered_fence(&InstallationToken::new("ghs_scoped".to_owned()));

    assert_eq!(
        fence.env,
        BTreeMap::from([
            ("CARGO_TARGET_DIR".to_owned(), "/k/targets/shep/7".to_owned()),
            ("KELPIE_LEASE_SOCKET".to_owned(), "/k/dog/lease.sock".to_owned()),
            ("GOCACHE".to_owned(), "/k/targets/shep/7/go-build".to_owned()),
            ("BUN_INSTALL_CACHE_DIR".to_owned(), "/k/targets/shep/7/bun-cache".to_owned()),
            (
                "GOLANGCI_LINT_CACHE".to_owned(),
                "/k/targets/shep/7/golangci-lint".to_owned(),
            ),
            ("GH_CONFIG_DIR".to_owned(), "/k/targets/shep/7/gh".to_owned()),
            ("GH_PROMPT_DISABLED".to_owned(), "1".to_owned()),
            ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
            (
                // Git's helpers are emptied, which drops the keychain's, and
                // github.com's is gh's, which answers from the config folder.
                // The sandbox's own proxy setting, which this replaces, stays.
                "GIT_CONFIG_PARAMETERS".to_owned(),
                "'http.proxyAuthMethod'='basic' 'gc.auto'='0' 'maintenance.auto'='false' 'fetch.prune'='false' 'credential.helper'='' 'credential.https://github.com.helper'='!gh auth git-credential'"
                    .to_owned()
            ),
        ])
    );
    // Codex drops a variable named for a key, a secret or a token.
    let names = fence.env.keys().map(|n| n.to_ascii_uppercase());
    for name in names {
        assert!(
            !["KEY", "SECRET", "TOKEN"].iter().any(|w| name.contains(w)),
            "{name}"
        );
    }
    assert!(
        !format!("{fence:?}").contains("ghs_scoped"),
        "the token is in no variable"
    );
    for path in ["~/.config/gh/**", "~/Library/Keychains/**"] {
        assert!(fence.no_read.contains(&path.to_owned()), "{path}");
    }
    for command in [
        "security",
        "security *",
        "git credential *",
        "git credential-*",
        "git -c credential*",
    ] {
        assert!(fence.no_commands.contains(&command.to_owned()), "{command}");
    }
}

#[test]
fn with_no_app_a_worker_runs_as_the_maintainer_and_its_git_still_skips_housekeeping() {
    let fence = profile(&[], &[]).reach().fence.expect("a worker is fenced");
    let names: Vec<&str> = fence.env.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "BUN_INSTALL_CACHE_DIR",
            "CARGO_TARGET_DIR",
            "GIT_CONFIG_PARAMETERS",
            "GOCACHE",
            "GOLANGCI_LINT_CACHE",
            "KELPIE_LEASE_SOCKET"
        ]
    );
    // The sandbox's own proxy setting, which this replaces, stays.
    assert_eq!(
        fence.env["GIT_CONFIG_PARAMETERS"],
        "'http.proxyAuthMethod'='basic' 'gc.auto'='0' 'maintenance.auto'='false' 'fetch.prune'='false'"
    );
    assert!(!fence.no_read.iter().any(|p| p.contains(".config/gh")));
    assert!(!fence.no_commands.contains(&"security".to_owned()));
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
            "/k/repos/shep/.git/packed-refs.lock",
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
            "/k/wt/shep/7/.goose",
            "/k/wt/shep/7/**/.goose",
            "/k/wt/shep/7/.config/goose",
            "/k/wt/shep/7/**/.config/goose",
        ]
    );
    assert_eq!(s["env"]["CARGO_TARGET_DIR"], "/k/targets/shep/7");
}

// A clone is the worker's own repo: its git dir sits outside the checkout's
// common git dir, so the checkout's git is not the worker's to write.
fn clone_settings() -> Value {
    WorkerProfile {
        git_dir: Path::new("/k/wt/shep/7/.git"),
        ..profile(&[], &[])
    }
    .settings()
}

#[test]
fn a_worker_on_a_clone_writes_its_git_dir_and_not_its_config_hooks_info_or_alternates() {
    let s = clone_settings();
    assert_eq!(
        strings(&s["sandbox"]["filesystem"]["allowWrite"]),
        ["/k/wt/shep/7", "/k/targets/shep/7", "/k/wt/shep/7/.git"]
    );
    let deny = strings(&s["sandbox"]["filesystem"]["denyWrite"]);
    for own in ["config", "hooks", "info", "modules", "objects/info"] {
        let path = format!("/k/wt/shep/7/.git/{own}");
        assert!(deny.contains(&path.as_str()), "{path} in {deny:?}");
    }
    assert!(deny.contains(&"/k/wt/shep/7/.claude"), "{deny:?}");
    let read = strings(&s["sandbox"]["filesystem"]["allowRead"]);
    assert!(read.contains(&"/k/wt/shep/7/.git"), "{read:?}");
}

#[test]
fn a_worker_on_a_clone_writes_nothing_of_the_checkouts_git() {
    let s = clone_settings();
    let allow = strings(&s["sandbox"]["filesystem"]["allowWrite"]);
    let deny = strings(&s["sandbox"]["filesystem"]["denyWrite"]);
    assert!(
        allow.iter().all(|p| !p.starts_with("/k/repos/shep/.git")),
        "{allow:?}"
    );
    assert!(
        deny.iter().all(|p| !p.starts_with("/k/repos/shep/.git")),
        "a clone keeps no deny on the checkout's refs: {deny:?}"
    );
}

// The guard reads a commit's git through the dir it is given, and the
// checkout's has no entry for a clone.
#[test]
fn the_guard_of_a_worker_on_a_clone_checks_the_clones_git_dir() {
    let clone = WorkerProfile {
        git_dir: Path::new("/k/wt/shep/7/.git"),
        ..profile(&[], &[])
    };
    let fence = clone.reach().fence.expect("a worker is fenced");
    assert_eq!(fence.guard.git_common_dir, Path::new("/k/wt/shep/7/.git"));
    let linked = profile(&[], &[]).reach().fence.expect("a worker is fenced");
    assert_eq!(linked.guard.git_common_dir, Path::new("/k/repos/shep/.git"));
}

#[test]
fn build_env_points_into_the_build_folder_beside_cargo() {
    let build_env = BTreeMap::from([
        (
            EnvName::try_from("BUN_INSTALL_CACHE_DIR".to_owned()).unwrap(),
            BuildDir::try_from("bun".to_owned()).unwrap(),
        ),
        (
            EnvName::try_from("GOLANGCI_LINT_CACHE".to_owned()).unwrap(),
            BuildDir::try_from("lint".to_owned()).unwrap(),
        ),
    ]);
    let s = WorkerProfile {
        worktree: Path::new("/k/wt/lab/7"),
        build: Path::new("/k/targets/lab/7"),
        git_common_dir: Path::new("/k/repos/lab/.git"),
        git_dir: Path::new("/k/repos/lab/.git/worktrees/7"),
        branch: "kelpie/7",
        base: "main",
        kelpie_pushes: false,
        kelpie: Path::new("/opt/kelpie"),
        skills: Path::new("/k/lab/skills"),
        kelpie_home: Path::new("/k"),
        repo: Path::new("/k/repos/shep"),
        guard_hooks: &[],
        allowed_domains: &[],
        registries: &[],
        build_env: &build_env,
        home: Path::new("/Users/me"),
        shep_home: Path::new("/srv/shep"),
        door: Path::new("/k/dog/lease.sock"),
        token: None,
        login_token: None,
    }
    .settings();
    assert_eq!(
        s["env"],
        json!({
            "CARGO_TARGET_DIR": "/k/targets/lab/7",
            "BUN_INSTALL_CACHE_DIR": "/k/targets/lab/7/bun",
            "GIT_CONFIG_PARAMETERS": housekeeping::PARAMETERS,
            "GOCACHE": "/k/targets/lab/7/go-build",
            "GOLANGCI_LINT_CACHE": "/k/targets/lab/7/lint",
            "KELPIE_LEASE_SOCKET": "/k/dog/lease.sock",
        })
    );
}

#[test]
fn the_network_is_github_and_the_projects_own_domains_only() {
    let plain = settings(&[]);
    let hosts = strings(&plain["sandbox"]["network"]["allowedDomains"]);
    assert_eq!(hosts[..2], ["github.com", "api.github.com"]);
    let npm = [NonBlank::try_from("registry.npmjs.org".to_owned()).unwrap()];
    let extended = with_domains(&[], &npm);
    let with_npm = strings(&extended["sandbox"]["network"]["allowedDomains"]);
    assert_eq!(with_npm[..hosts.len()], hosts[..]);
    assert_eq!(with_npm[hosts.len()..], ["registry.npmjs.org"]);
}

#[test]
fn a_registry_the_project_lists_itself_is_named_once() {
    let npm = [NonBlank::try_from("registry.npmjs.org".to_owned()).unwrap()];
    let profile = WorkerProfile {
        registries: &["registry.npmjs.org", "crates.io"],
        ..profile(&[], &npm)
    };
    let s = profile.settings();
    let hosts = strings(&s["sandbox"]["network"]["allowedDomains"]);

    assert_eq!(
        hosts.iter().filter(|h| **h == "registry.npmjs.org").count(),
        1
    );
    assert_eq!(hosts.last(), Some(&"crates.io"));
}

#[test]
fn a_fresh_profile_allows_the_hosts_a_log_download_redirects_to() {
    let settings = settings(&[]);
    let hosts = strings(&settings["sandbox"]["network"]["allowedDomains"]);
    let storage = (0..20).map(|n| format!("productionresultssa{n}.blob.core.windows.net"));
    for host in ["results-receiver.actions.githubusercontent.com".to_owned()]
        .into_iter()
        .chain(storage)
    {
        assert!(
            hosts.contains(&host.as_str()),
            "{host} missing from {hosts:?}"
        );
    }
}

#[test]
fn a_fresh_profile_allows_no_other_blob_host() {
    let settings = settings(&[]);
    let hosts = strings(&settings["sandbox"]["network"]["allowedDomains"]);
    let blobs: Vec<_> = hosts.iter().filter(|h| h.contains("windows.net")).collect();
    assert_eq!(blobs.len(), 20, "{blobs:?}");
    assert!(!hosts.iter().any(|h| h.contains('*')), "{hosts:?}");
    assert!(!hosts.contains(&"evil.blob.core.windows.net"));
    assert!(!hosts.contains(&"productionresultssa20.blob.core.windows.net"));
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
        "disableRemoteControl",
    ] {
        assert_eq!(s[key], true, "{key}");
    }
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
fn another_base_branch_is_denied_in_place_of_main() {
    let on_v2 = WorkerProfile {
        base: "v2",
        kelpie_pushes: false,
        ..profile(&[], &[])
    };
    let s = on_v2.settings();
    let deny = strings(&s["permissions"]["deny"]);
    assert!(deny.contains(&"Bash(git push * v2)"), "{deny:?}");
    assert!(!deny.contains(&"Bash(git push * main)"), "{deny:?}");
    let no_write = strings(&s["sandbox"]["filesystem"]["denyWrite"]);
    assert!(
        no_write.contains(&"/k/repos/shep/.git/refs/heads/v2"),
        "{no_write:?}"
    );
    assert!(
        !no_write.contains(&"/k/repos/shep/.git/refs/heads/main"),
        "{no_write:?}"
    );
    let reach = on_v2.reach();
    let guard = &reach.fence.unwrap().guard;
    assert_eq!(guard.base_flag().as_deref(), Some("--base=v2"));
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
    // The one thing a worker's PostToolUse hook does: end the turn at a pause.
    assert_eq!(
        s["hooks"]["PostToolUse"],
        json!([{
            "hooks": [{
                "type": "command",
                "command": r"'/opt/kelpie'\''s bin/kelpie' 'guard' '/k/repos/shep/.git' '/k/wt/shep/7' '--pause=/k/targets/shep/7'",
            }],
        }])
    );
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
        s["hooks"]["PostToolUse"][0],
        json!({ "hooks": [{ "type": "command", "command": "~/bin/after" }] })
    );
    assert_eq!(
        s["hooks"]["PostToolUse"].as_array().unwrap().len(),
        2,
        "kelpie's pause hook follows the project's"
    );
}

// A commit locks `packed-refs` on its own, the fence denies the lock, and a
// worker that took the message for a failure spent turns checking `git log`.
#[test]
fn the_instructions_say_the_packed_refs_lock_message_is_no_failure() {
    assert!(INSTRUCTIONS.contains("packed-refs.lock"), "{INSTRUCTIONS}");
}

// 29 refused `/tmp` writes in four days of transcripts, 51 of 93 worker
// retros. Every harness already runs its call with `TMPDIR` at the call's
// own scratch folder; nothing told the model it exists (#951).
#[test]
fn the_instructions_send_a_worker_to_its_own_temp_folder_for_scratch_files() {
    assert!(
        INSTRUCTIONS.contains("the folder `TMPDIR` names"),
        "{INSTRUCTIONS}"
    );
    assert!(INSTRUCTIONS.contains("`/tmp` is refused"), "{INSTRUCTIONS}");
}

// A bare `git fetch` updates every `refs/remotes/origin/*` of the shared
// clone, and a worker may write only its own branch's: 36 tool errors in the
// transcripts of 13 to 16 October 2026, and 83 of the 93 retros of 10 to 12
// October, were that failure.
#[test]
fn the_instructions_tell_a_worker_that_kelpie_fetches_the_base() {
    assert!(
        INSTRUCTIONS.contains("Run no `git fetch`, `git pull` or `git remote update`"),
        "{INSTRUCTIONS}"
    );
    assert!(
        INSTRUCTIONS.contains("fetches `origin/{base}`"),
        "{INSTRUCTIONS}"
    );
    assert!(
        INSTRUCTIONS.contains("`git merge origin/{base}`"),
        "{INSTRUCTIONS}"
    );
    assert!(
        INSTRUCTIONS.contains("A repo you make inside your worktree is yours to fetch in"),
        "{INSTRUCTIONS}"
    );
    assert!(!INSTRUCTIONS.contains("fetch and merge"), "{INSTRUCTIONS}");
}

// The stash is shared by every worktree of a repo: a worker's `git stash`
// could not lock `refs/stash` in the sandbox, and the `git stash pop` after it
// applied the maintainer's entry (shep-pm/shep#1123).
#[test]
fn the_instructions_forbid_git_stash_in_the_shared_clone() {
    assert!(
        INSTRUCTIONS.contains("Run no `git stash`"),
        "{INSTRUCTIONS}"
    );
}

// #1010's worker ran three one-hour turns on a 223-file refactor with nothing
// committed; "as you go" left the point of each commit to the worker.
#[test]
fn the_instructions_ask_for_a_commit_wherever_the_build_compiles() {
    assert!(
        INSTRUCTIONS.contains("Commit whenever the build compiles"),
        "{INSTRUCTIONS}"
    );
}

// The first-turn skill names no docs, so a worker read the domain docs in
// about a third of its sessions.
#[test]
fn the_instructions_send_a_worker_to_the_domain_docs_before_code() {
    assert!(
        INSTRUCTIONS.contains("`docs/agents/domain.md`"),
        "{INSTRUCTIONS}"
    );
}

// The deferred and disputed findings rules reach a worker in the header of the
// findings file a fix turn is sent to, which names both paths there.
#[test]
fn the_worker_instructions_name_no_findings_file() {
    for name in [
        "deferred-findings.md",
        "disputed-findings.md",
        "{deferred}",
        "{disputed}",
    ] {
        assert!(!INSTRUCTIONS.contains(name), "{name}: {INSTRUCTIONS}");
    }
}

// A bare verb on a choice only the maintainer can carry out read as "kelpie,
// do this", and the answer reached a worker that could not (shep#829).
#[test]
fn the_worker_instructions_say_a_choice_the_maintainer_carries_out_starts_with_you() {
    assert!(INSTRUCTIONS.contains("starts with `You`"), "{INSTRUCTIONS}");
    assert!(
        INSTRUCTIONS.contains("- You run the command => "),
        "{INSTRUCTIONS}"
    );
    assert!(
        INSTRUCTIONS.contains("Never offer a choice you cannot carry out"),
        "{INSTRUCTIONS}"
    );
}

// The question block is told once, with one worked example that shows the
// block, the choices, `Problem:` and ` => ` together.
#[test]
fn the_instructions_tell_the_question_block_once_with_one_example() {
    for rule in [
        "Open the block with a line that starts `Problem: `",
        "Write after each choice ` => `",
        "starts with `You`",
        "- You run the command => ",
        "Never offer a choice you cannot carry out",
    ] {
        assert_eq!(INSTRUCTIONS.matches(rule).count(), 1, "{rule}");
    }
    assert_eq!(
        INSTRUCTIONS
            .lines()
            .filter(|line| line.trim() == "<kelpie-question>")
            .count(),
        1,
        "one worked example of the block"
    );
}

// A worker wrote a client for a third-party API it could not read the docs of,
// said so in the pull request and guessed the auth scheme and the fields.
#[test]
fn the_instructions_tell_a_worker_to_ask_when_an_external_apis_docs_are_out_of_reach() {
    assert!(INSTRUCTIONS.contains("external"), "{INSTRUCTIONS}");
    assert!(INSTRUCTIONS.contains("docs"), "{INSTRUCTIONS}");
    assert!(
        INSTRUCTIONS.contains("do not guess its shape"),
        "{INSTRUCTIONS}"
    );
    assert!(
        INSTRUCTIONS.contains("`worker.allowed_domains`"),
        "{INSTRUCTIONS}"
    );
    assert!(INSTRUCTIONS.contains("<kelpie-question>"), "{INSTRUCTIONS}");
}

#[test]
fn the_instructions_never_mention_money_or_limits() {
    let text = INSTRUCTIONS.to_lowercase();
    for word in ["budget", "cost", "spend", "token", "usage", "$", "limit"] {
        assert!(!text.contains(word), "the instructions mention {word:?}");
    }
}

// A worker that opens its own pull request is told the base, since `gh` would
// otherwise open it against the repo's default branch.
#[test]
fn a_worker_that_opens_its_own_pull_request_is_told_to_open_it_against_the_base() {
    let kelpie = Path::new("/bin/kelpie");
    let on_next = instructions_for(kelpie, None, true, "next", false);
    assert!(
        on_next.contains("`gh pr create --draft --base next`"),
        "{on_next}"
    );
    let default = instructions(kelpie, None);
    assert!(
        default.contains("`gh pr create --draft --base main`"),
        "{default}"
    );
}

// A question raised before a pull request exists is posted on the issue, which
// on a repo the maintainer does not own is noise, and the maintainer cannot
// see the work it is about (#1037).
#[test]
fn the_instructions_ask_for_the_draft_pull_request_before_a_question_with_commits() {
    let kelpie = Path::new("/bin/kelpie");
    let own = instructions(kelpie, None);
    let handed = instructions(kelpie, Some(Path::new("/build/pull-request.md")));
    for text in [&own, &handed] {
        assert!(
            text.contains("or before a question with commits made, push the branch"),
            "{text}"
        );
        assert!(
            text.contains(
                "With commits made, push and open the draft before you ask; with nothing to push, ask anyway."
            ),
            "{text}"
        );
    }
    assert!(
        own.contains("open a draft one with `gh pr create --draft"),
        "{own}"
    );
}

// The push to a contributor's fork is kelpie's, made outside the sandbox with
// the maintainer's login, so a worker on a fork gets no credential the
// worker of a repo's own branch lacks: it only loses the right to push.
#[test]
fn the_maintainers_credential_is_not_in_the_workers_environment_or_files() {
    let token = InstallationToken::new("ghs_scoped".to_owned());
    let own = WorkerProfile {
        token: Some(&token),
        ..profile(&[], &[])
    };
    let forked = WorkerProfile {
        kelpie_pushes: true,
        ..own
    };
    let (own_fence, fence) = (*own.reach().fence.unwrap(), *forked.reach().fence.unwrap());

    assert_eq!(fence.env, own_fence.env, "no variable is added");
    assert_eq!(fence.no_read, own_fence.no_read, "no folder is opened");
    for path in ["~/.config/gh/**", "~/Library/Keychains/**"] {
        assert!(fence.no_read.contains(&path.to_owned()), "{path}");
    }
    for command in ["security", "git credential *", "git push", "git push *"] {
        assert!(fence.no_commands.contains(&command.to_owned()), "{command}");
    }
    for text in [forked.settings().to_string(), format!("{fence:?}")] {
        assert!(!text.contains("ghs_scoped"), "the token is in no file");
    }
}
