use std::collections::BTreeMap;
use std::time::Duration;

use super::*;
use crate::ports::{DevPort, Policy};

fn run(cwd: &Path) -> DevRun {
    DevRun {
        program: "npm".to_owned(),
        args: vec!["run".to_owned(), "dev".to_owned()],
        cwd: cwd.to_owned(),
        port: 4421,
        env: BTreeMap::new(),
        policy: Policy {
            hosts: vec!["app.example.com".to_owned()],
            ..Policy::default()
        },
        settings: PathBuf::from("settings.json"),
        log: PathBuf::from("log"),
        ready_within: Duration::from_secs(60),
        gate: DevPort::default(),
    }
}

fn checkout(files: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for file in files {
        let at = dir.path().join(file);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, "").unwrap();
    }
    dir
}

#[test]
fn the_packages_are_installed_from_the_lockfile_before_the_server_starts() {
    let dir = checkout(&["web/package-lock.json"]);
    let cwd = dir.path().join("web");
    let mut run = run(&cwd);

    before(&mut run, dir.path(), Path::new("/build"));

    assert_eq!(run.program, crate::spawn::sh().to_string_lossy());
    assert_eq!(run.args[0], "-c");
    assert!(
        run.args[1].contains("npm ci --ignore-scripts"),
        "{:?}",
        run.args
    );
    assert!(run.args[1].ends_with("shift 2; exec \"$@\""));
    // The folder, then the server's own command.
    assert_eq!(run.args[3], cwd.display().to_string());
    assert_eq!(run.args[4], "/build/install-unfinished");
    assert_eq!(run.args[5..], ["npm", "run", "dev"]);
    assert_eq!(run.env["NPM_CONFIG_CACHE"], "/build/npm-cache");
    assert_eq!(run.policy.hosts, ["app.example.com", "registry.npmjs.org"]);
    assert!(run.ready_within > Duration::from_secs(60));
}

#[test]
fn a_lockfile_above_the_servers_folder_is_found() {
    let dir = checkout(&["pnpm-lock.yaml", "web/src/main.ts"]);
    let mut run = run(&dir.path().join("web/src"));

    before(&mut run, dir.path(), Path::new("/build"));

    assert_eq!(run.args[3], dir.path().display().to_string());
    assert!(run.args[1].contains("pnpm install --frozen-lockfile"));
}

#[test]
fn a_checkout_with_no_lockfile_starts_its_server_as_it_is() {
    let dir = checkout(&["web/package.json"]);
    let mut run = run(&dir.path().join("web"));

    before(&mut run, dir.path(), Path::new("/build"));

    assert_eq!(run.program, "npm");
    assert_eq!(run.args, ["run", "dev"]);
    assert_eq!(run.policy.hosts, ["app.example.com"]);
}

#[test]
fn the_script_stops_when_the_install_fails_and_runs_the_server_when_it_does_not() {
    let dir = checkout(&["package-lock.json"]);
    let build = dir.path().join("b");
    for (install, ran) in [("true", true), ("false", false)] {
        let mut run = run(dir.path());
        run.program = "echo".to_owned();
        run.args = vec!["served".to_owned()];
        before(&mut run, dir.path(), &build);
        let script = run.args[1].replace("npm ci --ignore-scripts --no-audit --no-fund", install);
        std::fs::create_dir_all(&build).unwrap();
        let out = std::process::Command::new(crate::spawn::sh())
            .arg("-c")
            .arg(script)
            .args(&run.args[2..])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).contains("served"),
            ran,
            "{install}"
        );
    }
}

// Runs `run`'s script with `install` standing in for the package manager's command, in
// `dir`, and says whether the stand-in ran.
fn installed(run: &DevRun, dir: &Path, install: &str) -> bool {
    std::fs::create_dir_all(Path::new(&run.args[4]).parent().unwrap()).unwrap();
    let manager = MANAGERS.iter().filter_map(|m| m.install);
    let script = manager.fold(run.args[1].clone(), |s, m| s.replace(m, install));
    std::process::Command::new(crate::spawn::sh())
        .arg("-c")
        .arg(script)
        .args(&run.args[2..])
        .current_dir(dir)
        .status()
        .unwrap();
    dir.join("installed").exists()
}

#[test]
fn packages_already_installed_are_left_alone_and_the_server_gets_its_usual_wait() {
    let dir = checkout(&["bun.lock", "node_modules/vite/package.json"]);
    let mut run = run(dir.path());
    run.program = "true".to_owned();

    before(&mut run, dir.path(), &dir.path().join("b"));

    assert!(!installed(&run, dir.path(), "touch installed"));
    assert_eq!(run.ready_within, Duration::from_secs(60));
}

#[test]
fn a_checkout_with_no_packages_is_installed_into() {
    let dir = checkout(&["bun.lock"]);
    let mut run = run(dir.path());
    run.program = "true".to_owned();

    before(&mut run, dir.path(), &dir.path().join("b"));

    assert!(installed(&run, dir.path(), "touch installed"));
}

#[test]
fn an_install_that_did_not_finish_is_run_again_with_time_to_finish() {
    let dir = checkout(&["bun.lock", "node_modules/vite/package.json"]);
    let build = dir.path().join("b");
    std::fs::create_dir_all(&build).unwrap();
    std::fs::write(build.join(UNFINISHED), "").unwrap();
    let mut run = run(dir.path());
    run.program = "true".to_owned();

    before(&mut run, dir.path(), &build);

    assert!(installed(&run, dir.path(), "touch installed"));
    assert!(run.ready_within > Duration::from_secs(60));
}

#[test]
fn only_an_install_that_succeeded_clears_its_mark() {
    for (install, mark) in [("true", false), ("false", true)] {
        let dir = checkout(&["bun.lock"]);
        let build = dir.path().join("b");
        let mut run = run(dir.path());
        run.program = "true".to_owned();
        before(&mut run, dir.path(), &build);

        installed(&run, dir.path(), install);

        assert_eq!(build.join(UNFINISHED).exists(), mark, "{install}");
    }
}

#[test]
fn a_cache_the_server_already_names_is_kept() {
    let dir = checkout(&["bun.lock"]);
    let mut run = run(dir.path());
    run.program = "true".to_owned();
    run.env.insert(
        "BUN_INSTALL_CACHE_DIR".to_owned(),
        "/b/bun/cache".to_owned(),
    );

    before(&mut run, dir.path(), Path::new("/build"));

    assert_eq!(run.env["BUN_INSTALL_CACHE_DIR"], "/b/bun/cache");
}

#[test]
fn the_folders_an_install_names_exist_before_it_runs() {
    let dir = checkout(&["pnpm-lock.yaml"]);
    let build = dir.path().join("b");
    let outside = dir.path().join("outside");
    let mut run = run(dir.path());
    run.program = "true".to_owned();
    // A project's own `build_env` folder, which the server's plan carries, and a cache the
    // server named for a folder outside the build folder.
    run.env.insert(
        "XDG_RUNTIME_DIR".to_owned(),
        build.join("runtime").display().to_string(),
    );
    run.env.insert(
        "PNPM_CONFIG_STATE_DIR".to_owned(),
        outside.display().to_string(),
    );

    before(&mut run, dir.path(), &build);

    assert!(build.join("runtime").is_dir(), "the project's own folder");
    for name in ["PNPM_CONFIG_STORE_DIR", "PNPM_CONFIG_CACHE_DIR"] {
        let folder = Path::new(&run.env[name]).to_owned();
        assert!(folder.is_dir(), "{name}: {}", folder.display());
    }
    assert!(!outside.exists(), "a folder outside the build folder");
}

// Runs a worktree plan's command with `install` standing in for each manager's install, in
// the worktree, and says whether it succeeded.
fn ran(plan: &Plan, dir: &Path, install: &str) -> bool {
    let manager = MANAGERS.iter().filter_map(|m| m.install);
    let command = manager.fold(plan.command.clone(), |s, m| s.replace(m, install));
    std::process::Command::new(crate::spawn::sh())
        .arg("-c")
        .arg(command)
        .current_dir(dir)
        .status()
        .unwrap()
        .success()
}

#[test]
fn a_worktree_with_a_lockfile_and_no_packages_is_planned_for_install() {
    let dir = checkout(&["bun.lock"]);

    let plan = worktree(dir.path(), Path::new("/build")).unwrap();

    assert!(
        plan.command
            .contains("bun install --frozen-lockfile --ignore-scripts")
    );
    assert_eq!(plan.folders, ["."]);
    assert_eq!(plan.env["BUN_INSTALL_CACHE_DIR"], "/build/bun-cache");
}

#[test]
fn a_pnpm_plan_names_its_store_cache_state_and_lock_folders() {
    let dir = checkout(&["pnpm-lock.yaml"]);

    let plan = worktree(dir.path(), Path::new("/build")).unwrap();

    for (name, folder) in [
        ("NPM_CONFIG_STORE_DIR", "pnpm-store"),
        ("PNPM_CONFIG_STORE_DIR", "pnpm-store"),
        ("PNPM_CONFIG_CACHE_DIR", "pnpm-cache"),
        ("PNPM_CONFIG_STATE_DIR", "pnpm-state"),
        ("XDG_RUNTIME_DIR", "pnpm-run"),
    ] {
        assert_eq!(plan.env[name], format!("/build/{folder}"), "{name}");
    }
}

#[test]
fn a_worktree_that_holds_its_packages_is_planned_for_no_install() {
    let dir = checkout(&["bun.lock", "node_modules/vite/package.json"]);

    assert!(worktree(dir.path(), &dir.path().join("b")).is_none());
}

#[test]
fn a_worktree_with_no_lockfile_is_planned_for_no_install() {
    let dir = checkout(&["package.json", "Cargo.lock"]);

    assert!(worktree(dir.path(), &dir.path().join("b")).is_none());
}

#[test]
fn packages_an_install_left_unfinished_are_planned_again() {
    let dir = checkout(&["bun.lock", "node_modules/vite/package.json"]);
    let build = dir.path().join("b");
    std::fs::create_dir_all(&build).unwrap();
    std::fs::write(build.join(UNFINISHED), "").unwrap();

    assert!(worktree(dir.path(), &build).is_some());
}

#[test]
fn each_folder_that_lacks_packages_is_installed_by_its_own_manager() {
    let dir = checkout(&[
        "web/pnpm-lock.yaml",
        "api/package-lock.json",
        "docs/yarn.lock",
        "docs/node_modules/x/package.json",
    ]);

    let plan = worktree(dir.path(), Path::new("/build")).unwrap();

    let mut folders = plan.folders.clone();
    folders.sort();
    assert_eq!(folders, ["api", "web"]);
    assert!(plan.command.contains("pnpm install --frozen-lockfile"));
    assert!(plan.command.contains("npm ci --ignore-scripts"));
    assert!(!plan.command.contains("yarn"));
    assert_eq!(plan.env["NPM_CONFIG_STORE_DIR"], "/build/pnpm-store");
    assert_eq!(plan.env["NPM_CONFIG_CACHE"], "/build/npm-cache");
}

#[test]
fn the_mark_stays_when_any_install_fails_and_goes_when_all_succeed() {
    for (install, mark) in [("true", false), ("false", true)] {
        let dir = checkout(&["web/bun.lock", "api/package-lock.json"]);
        let build = dir.path().join("b");
        std::fs::create_dir_all(&build).unwrap();
        let plan = worktree(dir.path(), &build).unwrap();

        assert_eq!(ran(&plan, dir.path(), install), !mark, "{install}");

        assert_eq!(build.join(UNFINISHED).exists(), mark, "{install}");
    }
}

#[test]
fn a_folder_name_the_shell_would_read_is_quoted() {
    let dir = checkout(&["a b'c/bun.lock"]);
    let build = dir.path().join("b");
    std::fs::create_dir_all(&build).unwrap();
    let plan = worktree(dir.path(), &build).unwrap();

    assert!(ran(&plan, dir.path(), "touch installed"));

    assert!(dir.path().join("a b'c/installed").exists());
}

#[test]
fn the_dev_servers_install_skips_a_worktree_the_plan_installed() {
    let dir = checkout(&["bun.lock"]);
    let build = dir.path().join("b");
    std::fs::create_dir_all(&build).unwrap();
    let plan = worktree(dir.path(), &build).unwrap();
    assert!(ran(&plan, dir.path(), "mkdir -p node_modules"));
    let mut run = run(dir.path());
    run.program = "true".to_owned();

    before(&mut run, dir.path(), &build);

    assert!(!installed(&run, dir.path(), "touch installed"));
    assert_eq!(run.ready_within, Duration::from_secs(60));
}
