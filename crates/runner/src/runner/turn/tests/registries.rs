//! The registries of the package managers a worktree keeps files for, in the worker's sandbox

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
