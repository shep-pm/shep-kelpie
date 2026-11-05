//! `worker.package_registries`, which is gone: detection is always on, and the caches the
//! managers a worktree keeps get with it

use super::*;

#[test]
fn the_old_key_is_refused_by_name() {
    let text = EXAMPLE.replace(
        "build_env = {}\n",
        "build_env = {}\npackage_registries = true\n",
    );
    assert!(
        text.contains("package_registries = true"),
        "the example's worker table moved"
    );

    let err = parse_err(&text);

    assert!(
        err.contains("`worker.package_registries` is no longer a setting"),
        "{err}"
    );
    assert!(err.contains("delete it"), "{err}");
}

// The worker's folders for `build_env_in` over a worktree that keeps `files`, by variable.
fn build_env_in(settings: &str, files: &[&str]) -> BTreeMap<String, PathBuf> {
    let dir = tempfile::tempdir().unwrap();
    for file in files {
        std::fs::write(dir.path().join(file), "").unwrap();
    }
    let worker = parse(settings).unwrap().worker;
    (worker.build_env_in(dir.path()).into_iter())
        .map(|(name, folder)| (name.as_str().to_owned(), folder.as_path().to_owned()))
        .collect()
}

#[test]
fn build_env_in_gives_the_caches_of_the_managers_the_worktree_keeps() {
    let env = build_env_in(EXAMPLE, &["pnpm-lock.yaml", "go.mod"]);

    let folders: Vec<_> = env
        .iter()
        .map(|(n, f)| (n.as_str(), f.to_str().unwrap()))
        .collect();
    assert_eq!(
        folders,
        [
            ("GOCACHE", "go-build"),
            ("GOMODCACHE", "go-mod"),
            ("NPM_CONFIG_STORE_DIR", "pnpm-store"),
            ("PNPM_CONFIG_CACHE_DIR", "pnpm-cache"),
            ("PNPM_CONFIG_STATE_DIR", "pnpm-state"),
            ("PNPM_CONFIG_STORE_DIR", "pnpm-store"),
            ("XDG_RUNTIME_DIR", "pnpm-run"),
        ]
    );
}

#[test]
fn a_worktree_that_keeps_no_manager_gets_no_cache() {
    assert!(build_env_in(EXAMPLE, &["README.md"]).is_empty());
}

#[test]
fn a_projects_build_env_beats_a_managers_cache() {
    let text = EXAMPLE.replace(
        "build_env = {}",
        r#"build_env = { GOCACHE = "mine", CARGO_HOME = "cargo" }"#,
    );

    let env = build_env_in(&text, &["go.mod"]);

    assert_eq!(env["GOCACHE"], Path::new("mine"));
    assert_eq!(env["GOMODCACHE"], Path::new("go-mod"));
    assert_eq!(env["CARGO_HOME"], Path::new("cargo"));
}

#[test]
fn every_cache_in_the_table_is_a_valid_env_name() {
    let all: Vec<_> = crate::package_managers::MANAGERS.iter().collect();

    for (name, folder) in crate::package_managers::caches(&all) {
        assert!(EnvName::try_from(name.to_owned()).is_ok(), "{name}");
        assert!(
            BuildDir::try_from(folder.to_owned()).is_ok(),
            "{name}: {folder}"
        );
    }
}
