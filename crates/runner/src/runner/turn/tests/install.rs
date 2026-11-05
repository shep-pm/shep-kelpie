//! A fresh worktree's packages, installed in the worker's sandbox before its first turn

use super::*;
use crate::ports::TestOutcome;

// The first turn of issue 7 in a project whose `main` keeps `files`, with the install
// answering `outcomes`, and the runner after it
fn first_turn(files: &[&str], outcomes: Vec<TestOutcome>) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    for file in files {
        rig.land(file, "");
    }
    rig.tests.script(outcomes);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude
        .script([Scripted::Reply(usage(1), Cost(20_085_300))]);
    step(&runner).unwrap();
    (rig, runner)
}

fn steps(runner: &Mutex<Runner>) -> Vec<serde_json::Value> {
    let started = runner.lock().unwrap().take_started();
    started
        .iter()
        .map(|s| serde_json::to_value(s).unwrap())
        .collect()
}

#[test]
fn a_worktree_with_a_bun_lock_and_no_node_modules_is_installed_before_the_first_turn() {
    let (rig, runner) = first_turn(&["bun.lock"], Vec::new());

    let [ran] = rig.tests.ran().try_into().unwrap();
    let build = rig.home.path().join("shep/kelpie/shep/builds/7");
    assert_eq!(ran.run.cwd, build);
    assert!(
        ran.run
            .command
            .contains("bun install --frozen-lockfile --ignore-scripts"),
        "{}",
        ran.run.command
    );
    assert_eq!(ran.run.door, None, "an install takes no test lease");
    let fence = ran.run.reach.fence.as_deref().expect("the worker's fence");
    assert_eq!(
        fence.env["BUN_INSTALL_CACHE_DIR"],
        build.join("bun-cache").display().to_string()
    );
    // The install is a step of its own, ahead of the call it was made for.
    let steps = steps(&runner);
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert_eq!(
        steps[0],
        json!({"step": "packages-installed", "issue": "7", "folders": ["."], "failed": null})
    );
    assert_eq!(steps[1]["step"], "session-started");
    let [seen] = rig.claude.seen().try_into().unwrap();
    assert!(
        !seen.call.prompt.contains("install"),
        "a worker is told only of an install that failed: {}",
        seen.call.prompt
    );
}

#[test]
fn a_lockfile_below_the_root_is_installed_from_its_own_folder() {
    let (rig, _) = first_turn(&["web/pnpm-lock.yaml"], Vec::new());

    let [ran] = rig.tests.ran().try_into().unwrap();
    let web = rig.home.path().join("shep/kelpie/shep/worktrees/7/web");
    assert!(
        ran.run.command.contains(&format!("cd '{}'", web.display())),
        "{}",
        ran.run.command
    );
    assert!(
        ran.run.command.contains("pnpm install --frozen-lockfile"),
        "{}",
        ran.run.command
    );
}

// pnpm 12 runs without its store operation lock where its folder is missing, and a project's
// `build_env` names folders the build folder does not hold yet.
#[test]
fn the_folders_an_install_names_exist_before_the_install_runs() {
    let rig = Rig::new("shep");
    rig.edit_settings(|s| {
        s.replace(
            "build_env = {}",
            "build_env = { XDG_RUNTIME_DIR = \"runtime\" }",
        )
    });
    rig.land("pnpm-lock.yaml", "");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude
        .script([Scripted::Reply(usage(1), Cost(20_085_300))]);
    step(&runner).unwrap();

    let build = rig.build_7();
    assert!(build.join("runtime").is_dir(), "the project's own folder");
    for folder in ["pnpm-store", "pnpm-cache", "pnpm-state"] {
        assert!(build.join(folder).is_dir(), "{folder}");
    }
    assert!(
        !build.join("pnpm-run").exists(),
        "the project's own folder stands in for the manager's"
    );
}

// A worker's own `pnpm install` runs where kelpie planned none: the lockfile is two folders
// down, past what its plan reads.
#[test]
fn a_workers_own_call_finds_the_build_folders_its_env_names() {
    let rig = Rig::new("shep");
    rig.edit_settings(|s| {
        s.replace(
            "build_env = {}",
            "build_env = { PNPM_CONFIG_STATE_DIR = \"state\" }",
        )
    });
    rig.land("web/app/pnpm-lock.yaml", "");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude
        .script([Scripted::Reply(usage(1), Cost(20_085_300))]);
    step(&runner).unwrap();

    assert_eq!(rig.tests.ran(), [], "kelpie planned no install");
    let build = rig.build_7();
    assert!(build.join("state").is_dir(), "the project's own folder");
    for folder in ["pnpm-store", "pnpm-cache", "pnpm-run"] {
        assert!(build.join(folder).is_dir(), "{folder}");
    }
    assert!(
        !build.join("pnpm-state").exists(),
        "the project's own folder stands in for the manager's"
    );
}

#[test]
fn a_worktree_that_holds_node_modules_is_not_installed_again() {
    let (rig, runner) = first_turn(&["bun.lock", "node_modules/vite/package.json"], Vec::new());

    assert_eq!(rig.tests.ran(), []);
    let steps = steps(&runner);
    assert_eq!(steps.len(), 1, "only the call is logged: {steps:?}");
    assert_eq!(rig.claude.seen().len(), 1, "the turn ran");
}

#[test]
fn an_install_that_leaves_no_node_modules_is_not_run_again_on_the_next_turn() {
    // The stand-in installs nothing, as Yarn's Plug'n'Play and a project with no dependencies don't.
    let (rig, runner) = first_turn(&["bun.lock"], Vec::new());
    rig.claude
        .script([Scripted::Reply(usage(1), Cost(20_085_300))]);
    step(&runner).unwrap();

    assert_eq!(rig.tests.ran().len(), 1, "the second turn installed again");
    let installs = steps(&runner)
        .iter()
        .filter(|s| s["step"] == "packages-installed")
        .count();
    assert_eq!(installs, 1);
}

#[test]
fn a_worktree_with_no_lockfile_is_not_installed() {
    let (rig, _) = first_turn(&["README.md"], Vec::new());

    assert_eq!(rig.tests.ran(), []);
}

#[test]
fn a_failed_install_is_a_logged_step_and_the_first_prompt_names_it() {
    let failed = TestOutcome::Failed {
        output: "error: lockfile had changes, but lockfile is frozen".to_owned(),
    };
    let (rig, runner) = first_turn(&["bun.lock"], vec![failed]);

    let steps = steps(&runner);
    assert_eq!(
        steps[0],
        json!({
            "step": "packages-installed",
            "issue": "7",
            "folders": ["."],
            "failed": "error: lockfile had changes, but lockfile is frozen",
        })
    );
    // The work item goes on: the turn ran, and its prompt says what happened.
    let [seen] = rig.claude.seen().try_into().unwrap();
    let prompt = seen.call.prompt;
    assert!(prompt.starts_with("/mattpocock:implement "), "{prompt}");
    assert!(prompt.contains("failed"), "{prompt}");
    assert!(
        prompt.contains("lockfile had changes, but lockfile is frozen"),
        "{prompt}"
    );
}

#[test]
fn an_install_that_did_not_start_or_ran_too_long_fails_with_its_reason() {
    for (outcome, said) in [
        (
            TestOutcome::NotRun {
                reason: "the sandbox would not start".to_owned(),
            },
            "the sandbox would not start",
        ),
        (TestOutcome::TimedOut, "300 seconds"),
    ] {
        let (rig, _) = first_turn(&["bun.lock"], vec![outcome]);

        let [seen] = rig.claude.seen().try_into().unwrap();
        assert!(seen.call.prompt.contains(said), "{}", seen.call.prompt);
    }
}
