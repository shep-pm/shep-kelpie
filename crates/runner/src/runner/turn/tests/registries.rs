//! The registries and caches of the package managers a worktree keeps files for, in the
//! worker's sandbox

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::*;

// The domains of the sandbox policy of the first turn of issue 7, in a project whose
// worktree keeps `files` and whose `allowed_domains` is empty.
fn first_turn_hosts(files: &[&str]) -> Vec<String> {
    let rig = Rig::new("shep");
    rig.edit_settings(|s| {
        s.replace(
            "allowed_domains = [\"crates.io\", \"index.crates.io\", \"static.crates.io\"]",
            "allowed_domains = []",
        )
    });
    for file in files {
        rig.land(file, "");
    }
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude
        .script([Scripted::Reply(usage(1), Cost(20_085_300))]);
    step(&runner).unwrap();
    let [seen] = rig.claude.seen().try_into().unwrap();
    let all = seen.sandbox["network"]["allowedDomains"]
        .as_array()
        .unwrap();
    // GitHub's own hosts are the same with or without a registry.
    let github = all.iter().position(|h| h == "api.github.com").unwrap();
    let after_github = all.iter().skip(github + 1);
    after_github
        .map(|h| h.as_str().unwrap().to_owned())
        .filter(|h| !h.contains("githubusercontent.com") && !h.contains("blob.core.windows.net"))
        .collect()
}

#[test]
fn a_bun_lock_reaches_the_npm_registry_with_no_allowed_domains() {
    let hosts = first_turn_hosts(&["bun.lock"]);

    assert_eq!(hosts, ["registry.npmjs.org"]);
}

#[test]
fn each_lockfile_adds_the_hosts_of_its_manager() {
    let hosts = first_turn_hosts(&["bun.lock", "Cargo.lock", "go.mod", "uv.lock"]);

    assert_eq!(
        hosts,
        [
            "registry.npmjs.org",
            "crates.io",
            "index.crates.io",
            "static.crates.io",
            "proxy.golang.org",
            "sum.golang.org",
            "pypi.org",
            "files.pythonhosted.org",
        ]
    );
}

#[test]
fn a_worktree_with_no_lockfile_or_manifest_reaches_no_registry() {
    assert_eq!(first_turn_hosts(&["README.md"]), Vec::<String>::new());
}

// The env of the first turn of issue 7, and the build folder it points into, in a project
// whose worktree keeps `files` and whose settings give `build_env` (a TOML inline table).
fn first_turn_env(files: &[&str], build_env: &str) -> (BTreeMap<String, String>, PathBuf) {
    let rig = Rig::new("shep");
    rig.edit_settings(|s| s.replace("build_env = {}", &format!("build_env = {build_env}")));
    for file in files {
        rig.land(file, "");
    }
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude
        .script([Scripted::Reply(usage(1), Cost(20_085_300))]);
    step(&runner).unwrap();
    let [seen] = rig.claude.seen().try_into().unwrap();
    let env = seen.settings["env"].as_object().unwrap();
    let env = (env.iter())
        .map(|(name, value)| (name.clone(), value.as_str().unwrap().to_owned()))
        .collect();
    (env, rig.home.path().join("shep/kelpie/shep/builds/7"))
}

#[test]
fn a_workers_call_carries_the_caches_of_its_worktrees_managers() {
    let (env, build) = first_turn_env(&["pnpm-lock.yaml", "go.mod"], "{}");

    for (name, folder) in [
        ("NPM_CONFIG_STORE_DIR", "pnpm-store"),
        ("PNPM_CONFIG_STORE_DIR", "pnpm-store"),
        ("PNPM_CONFIG_CACHE_DIR", "pnpm-cache"),
        ("PNPM_CONFIG_STATE_DIR", "pnpm-state"),
        ("XDG_RUNTIME_DIR", "pnpm-run"),
        ("GOCACHE", "go-build"),
        ("GOMODCACHE", "go-mod"),
    ] {
        assert_eq!(
            env[name],
            build.join(folder).display().to_string(),
            "{name}"
        );
    }
    assert!(
        !env.contains_key("BUN_INSTALL_CACHE_DIR"),
        "bun is not kept"
    );
}

#[test]
fn a_worktree_that_keeps_no_manager_carries_no_manager_cache() {
    let (env, _) = first_turn_env(&["README.md"], "{}");

    for name in ["GOCACHE", "BUN_INSTALL_CACHE_DIR", "PNPM_CONFIG_STORE_DIR"] {
        assert!(!env.contains_key(name), "{name}");
    }
}

#[test]
fn a_projects_own_build_env_moves_a_managers_cache_in_a_workers_call() {
    let (env, build) = first_turn_env(&["go.mod"], "{ GOCACHE = \"mine\" }");

    assert_eq!(env["GOCACHE"], build.join("mine").display().to_string());
    assert_eq!(
        env["GOMODCACHE"],
        build.join("go-mod").display().to_string()
    );
}
