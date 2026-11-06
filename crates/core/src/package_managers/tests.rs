use super::*;

fn worktree(files: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for file in files {
        let at = dir.path().join(file);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, "").unwrap();
    }
    dir
}

fn names(files: &[&str]) -> Vec<&'static str> {
    let dir = worktree(files);
    found_in(dir.path()).iter().map(|m| m.name).collect()
}

fn hosts(files: &[&str]) -> Vec<&'static str> {
    let dir = worktree(files);
    registries(&found_in(dir.path()))
}

#[test]
fn a_lockfile_names_its_manager_and_its_registries() {
    assert_eq!(names(&["bun.lock"]), ["bun"]);
    assert_eq!(hosts(&["bun.lock"]), ["registry.npmjs.org"]);
    assert_eq!(
        hosts(&["Cargo.lock"]),
        ["crates.io", "index.crates.io", "static.crates.io"]
    );
    assert_eq!(hosts(&["go.mod"]), ["proxy.golang.org", "sum.golang.org"]);
    assert_eq!(hosts(&["uv.lock"]), ["pypi.org", "files.pythonhosted.org"]);
}

#[test]
fn a_manifest_names_its_manager_where_no_lockfile_is_kept() {
    assert_eq!(names(&["package.json"]), ["npm"]);
    assert_eq!(names(&["requirements.txt"]), ["pip"]);
    assert_eq!(names(&["pyproject.toml"]), ["pip"]);
}

#[test]
fn managers_that_share_a_registry_name_it_once() {
    assert_eq!(
        hosts(&["yarn.lock", "bun.lock", "package-lock.json"]),
        ["registry.npmjs.org", "registry.yarnpkg.com"]
    );
    assert_eq!(hosts(&["uv.lock", "requirements.txt"]).len(), 2);
}

#[test]
fn a_folder_directly_under_the_worktree_is_read_and_a_deeper_one_is_not() {
    assert_eq!(names(&["web/pnpm-lock.yaml"]), ["pnpm"]);
    assert_eq!(names(&["web/app/pnpm-lock.yaml"]), Vec::<&str>::new());
}

#[test]
fn hidden_and_installed_folders_are_not_read() {
    for folder in [".git", "node_modules", "target", "vendor"] {
        let file = format!("{folder}/Cargo.lock");
        assert_eq!(names(&[&file]), Vec::<&str>::new(), "{folder}");
    }
}

#[test]
fn a_worktree_with_no_such_file_has_no_manager() {
    assert_eq!(names(&["README.md"]), Vec::<&str>::new());
}

#[test]
fn the_dev_servers_managers_each_have_a_lockfile_to_run_from() {
    for m in MANAGERS.iter().filter(|m| m.install.is_some()) {
        assert!(!m.lockfiles.is_empty(), "{}", m.name);
    }
}

#[test]
fn a_cache_variable_is_named_once_whatever_managers_name_it() {
    let dir = worktree(&["pnpm-lock.yaml", "package-lock.json", "go.mod"]);
    let found = found_in(dir.path());

    let names: Vec<_> = caches(&found).into_iter().map(|(name, _)| name).collect();

    let mut once = names.clone();
    once.sort_unstable();
    once.dedup();
    assert_eq!(names.len(), once.len(), "{names:?}");
    assert!(names.contains(&"NPM_CONFIG_CACHE"));
    assert!(names.contains(&"NPM_CONFIG_STORE_DIR"));
}

#[test]
fn the_caches_every_worker_gets_are_each_a_cache_of_the_table() {
    let always = always_caches();

    let names: Vec<_> = always.iter().map(|(name, _)| *name).collect();
    assert_eq!(names.len(), ALWAYS.len(), "{names:?}");
    assert!(names.contains(&"GOCACHE") && names.contains(&"XDG_RUNTIME_DIR"));
    assert!(
        !names.contains(&"GOMODCACHE"),
        "the home's module cache is read, not written"
    );
}
