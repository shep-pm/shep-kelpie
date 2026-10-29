//! A worker turn with the browser is given the project's dev server from `main`'s launch file
//!
//! The plan tests prepare a second turn's call without opening its browser, so a worker's
//! sandbox runs them. The tests that run a turn open the browser's socket, which it cannot.

use std::path::Path;
use std::sync::Arc;

use super::browser::{install, instructions_of, turn_on};
use super::*;
use crate::ports::{Agents, DevRun, Ending, Role, Session};
use crate::settings::Harness;
use crate::test::{FakeClaude, FakeDevServers, ticket};

// Shep's own launch file: one configuration, `shep-web`, on port 4421 in `web`.
const SHEP: &str = include_str!("../../../../../../fixtures/launch.json");

// A project with the browser on, its tools installed, and `launch` on `main`.
fn project(launch: Option<&str>) -> Rig {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig);
    if let Some(launch) = launch {
        rig.land(".claude/launch.json", launch);
        rig.land("web/package.json", "{}\n");
    }
    rig
}

// The dev server planned for a second turn of work item 7, and what that turn
// is told. The first turn begins the work item. Where a sandbox cannot bind
// the browser's socket it fails, and the item and its worktree are there all
// the same. `edit` runs in the worktree before the second turn is prepared.
fn planned(rig: &Rig, edit: impl FnOnce(&Path)) -> (Option<DevRun>, String) {
    try_planned(rig, edit).unwrap()
}

// The same, or why the second turn's call cannot be prepared.
fn try_planned(rig: &Rig, edit: impl FnOnce(&Path)) -> Result<(Option<DevRun>, String), String> {
    let runner = rig.open().unwrap();
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    rig.ask(&runner, "add", Some("7"));
    let _ = step(&runner);
    let runner = runner.lock().unwrap();
    let item = runner.current().expect("an item").clone();
    edit(&item.worktree);
    let session = Session::Resume(item.session.clone());
    let call = runner.worker_call(&item, session, Some("again".into()), None, true)?;
    let told = fs::read_to_string(call.instructions.as_ref().unwrap()).unwrap();
    // The browser may open nothing until the server answers.
    assert_eq!(runner.dev_gate(ticket(7)).get(), None);
    // The keychain is blocked as the worker's own call blocks it.
    let blocked = runner
        .planned
        .get(ticket(7))
        .map(|run| run.policy.no_keychain);
    assert!(blocked.is_none_or(|blocked| blocked == runner.app_covers()));
    Ok((runner.planned.get(ticket(7)), told))
}

#[test]
fn shep_launch_file_on_main_plans_its_server_in_the_worktree_on_its_port() {
    let rig = project(Some(SHEP));
    let (run, told) = planned(&rig, |_| {});

    let run = run.expect("a plan");
    assert_eq!(run.program, "npm");
    assert_eq!(run.args, ["run", "dev", "--", "--port", "4421"]);
    assert_eq!(run.port, 4421);
    let worktree = rig.paths().worktree(ticket(7));
    assert_eq!(run.cwd, worktree.join("web"));
    assert_eq!(
        run.env.get("NODE_USE_ENV_PROXY").map(String::as_str),
        Some("1")
    );
    assert_eq!(run.env.get("PORT"), None);
    assert_eq!(run.log, rig.paths().build(ticket(7)).join("dev-server.log"));
    assert_eq!(run.ready_within, crate::ports::READY_WITHIN);
    assert!(told.contains("`http://localhost:4421`"), "{told}");
    assert!(told.contains(&run.log.display().to_string()), "{told}");
}

#[test]
fn the_server_gets_a_writable_temp_folder_in_the_build_folder() {
    let rig = project(Some(SHEP));
    let (run, _) = planned(&rig, |_| {});

    let temp = rig.paths().build(ticket(7)).join("dev-server-tmp");
    let run = run.expect("a plan");
    assert_eq!(run.env["TMPDIR"], temp.display().to_string());
    assert!(temp.is_dir());
    assert!(run.policy.write.iter().any(|w| temp.starts_with(w)));
}

#[test]
fn the_server_gets_the_workers_build_env() {
    let rig = project(Some(SHEP));
    rig.edit_settings(|s| {
        s.replace(
            "build_env = {}",
            "build_env = { BUN_INSTALL_CACHE_DIR = \"bun/cache\" }",
        )
    });
    let (run, _) = planned(&rig, |_| {});

    let cache = rig.paths().build(ticket(7)).join("bun/cache");
    assert_eq!(
        run.expect("a plan").env["BUN_INSTALL_CACHE_DIR"],
        cache.display().to_string()
    );
}

#[test]
fn a_worktree_with_no_packages_installs_them_before_its_server_starts() {
    let rig = project(Some(SHEP));
    rig.land("web/bun.lock", "{}\n");
    let (run, _) = planned(&rig, |_| {});

    let run = run.expect("a plan");
    let script = &run.args[1];
    assert_eq!(run.program, crate::spawn::sh().to_string_lossy());
    assert!(script.contains("bun install --frozen-lockfile"), "{script}");
    assert_eq!(run.args[5..], ["npm", "run", "dev", "--", "--port", "4421"]);
    assert!(run.policy.hosts.contains(&"registry.npmjs.org".to_owned()));
    assert!(run.ready_within > crate::ports::READY_WITHIN);
}

#[test]
fn a_worktree_that_has_its_packages_gets_its_server_started_at_once() {
    let rig = project(Some(SHEP));
    rig.land("web/bun.lock", "{}\n");
    let (run, _) = planned(&rig, |worktree| {
        fs::create_dir_all(worktree.join("web/node_modules")).unwrap();
    });

    assert_eq!(
        run.expect("a plan").ready_within,
        crate::ports::READY_WITHIN
    );
}

#[test]
fn the_servers_sandbox_writes_the_worktree_and_build_folder_and_reaches_the_browsers_hosts() {
    let rig = project(Some(SHEP));
    let (run, _) = planned(&rig, |_| {});

    let policy = run.expect("a plan").policy;
    assert_eq!(
        policy.write,
        [
            rig.paths().worktree(ticket(7)),
            rig.paths().build(ticket(7))
        ]
    );
    assert_eq!(policy.hosts, ["app.example.com"]);
    assert!(policy.listen);
    assert_eq!(policy.services, ["com.apple.FSEvents"]);
    assert!(policy.sockets.is_empty());
    assert!(
        policy.no_write.iter().any(|p| p.ends_with("7.git/config")),
        "{:?}",
        policy.no_write
    );
    assert!(!policy.no_read.is_empty());
}

#[test]
fn an_edit_of_the_launch_file_in_the_worktree_stops_the_turn_before_any_server_is_planned() {
    let rig = project(Some(SHEP));
    let edit = r#"{"configurations":[{"name":"mine","runtimeExecutable":"sh","port":9999}]}"#;
    let refused = try_planned(&rig, |worktree| {
        fs::write(worktree.join(".claude/launch.json"), edit).unwrap();
    });

    let why = refused.unwrap_err();
    assert!(why.contains(".claude/launch.json"), "{why}");
}

#[test]
fn a_named_configuration_is_the_one_planned_else_the_first() {
    let two = r#"{"configurations":[
        {"name":"api","runtimeExecutable":"cargo","runtimeArgs":["run"],"port":8080},
        {"name":"web","runtimeExecutable":"npm","port":3000}]}"#;
    let first = planned(&project(Some(two)), |_| {}).0.expect("a plan");
    assert_eq!((first.program.as_str(), first.port), ("cargo", 8080));

    let rig = project(Some(two));
    rig.edit_settings(|s| {
        s.replace(
            "domains = [\"app.example.com\"]",
            "domains = [\"app.example.com\"]\nconfiguration = \"web\"",
        )
    });
    let web = planned(&rig, |_| {}).0.expect("a plan");
    assert_eq!((web.program.as_str(), web.port), ("npm", 3000));
}

#[test]
fn the_configured_port_is_used_when_it_is_free() {
    let rig = project(Some(SHEP));
    let (run, _) = planned(&rig, |_| {});
    assert_eq!(run.expect("a plan").port, 4421);
}

#[test]
fn a_port_that_is_taken_is_replaced_under_auto_port_in_the_variable_and_the_arguments() {
    let rig = project(Some(SHEP));
    rig.dev_servers.take(4421);
    // One pick for the first turn, one for the second.
    rig.dev_servers.pick_next(5301);
    rig.dev_servers.pick_next(5301);
    let (run, told) = planned(&rig, |_| {});

    let run = run.expect("a plan");
    assert_eq!(run.port, 5301);
    assert_eq!(run.env.get("PORT").map(String::as_str), Some("5301"));
    assert_eq!(run.args, ["run", "dev", "--", "--port", "5301"]);
    assert!(told.contains("localhost:5301"), "{told}");
}

#[test]
fn a_port_that_is_taken_without_auto_port_plans_no_server_and_the_instructions_say_so() {
    let fixed = r#"{"configurations":[{"name":"web","runtimeExecutable":"npm","port":4421}]}"#;
    let rig = project(Some(fixed));
    rig.dev_servers.take(4421);
    let (run, told) = planned(&rig, |_| {});

    assert_eq!(run, None);
    assert!(
        told.contains("runs no dev server for this turn: port 4421 is taken"),
        "{told}"
    );
    assert!(!told.contains("http://localhost"), "{told}");
}

#[test]
fn a_project_without_a_launch_file_on_main_plans_nothing() {
    let (run, told) = planned(&project(None), |_| {});
    assert_eq!(run, None);
    assert!(
        !told.contains("dev server") && !told.contains("localhost"),
        "{told}"
    );
}

#[test]
fn a_project_without_the_browser_plans_nothing_though_main_has_a_launch_file() {
    let rig = Rig::new("shep");
    install(&rig);
    rig.land(".claude/launch.json", SHEP);
    let (run, told) = planned(&rig, |_| {});
    assert_eq!(run, None);
    assert!(!told.contains("localhost"), "{told}");
}

#[test]
fn a_cwd_that_leaves_the_worktree_plans_nothing_and_the_instructions_say_why() {
    let escape =
        r#"{"configurations":[{"name":"web","runtimeExecutable":"npm","port":4421,"cwd":".."}]}"#;
    let (run, told) = planned(&project(Some(escape)), |_| {});
    assert_eq!(run, None);
    assert!(told.contains("leaves the worktree"), "{told}");
}

// Records how many dev servers were running while each call ran.
struct Watching {
    inner: FakeClaude,
    servers: FakeDevServers,
    live: Mutex<Vec<usize>>,
}

impl Agents for Watching {
    fn prepare(&self, call: &AgentCall) -> Result<(), AgentError> {
        self.inner.prepare(call)
    }

    fn run(&self, call: &AgentCall, ending: &Ending) -> Result<AgentReply, AgentError> {
        self.live.lock().unwrap().push(self.servers.live());
        self.inner.run(call, ending)
    }
}

// A runner on the stand-in Claude, and what it saw of the dev servers during each call.
fn watching(rig: &Rig) -> (Mutex<crate::runner::Runner>, Arc<Watching>) {
    let watching = Arc::new(Watching {
        inner: rig.claude.clone(),
        servers: rig.dev_servers.clone(),
        live: Mutex::default(),
    });
    let runner = rig.open_agents(watching.clone()).unwrap();
    (runner, watching)
}

fn worker_turn(rig: &Rig, runner: &Mutex<crate::runner::Runner>) -> crate::test::Seen {
    rig.ask(runner, "add", Some("7"));
    let _ = step(runner);
    let seen = rig.claude.all_seen();
    seen.into_iter()
        .find(|s| s.call.role == Role::Worker)
        .expect("a worker call")
}

// Every call ran with one server up, and none is left up after.
fn assert_served_each_call(rig: &Rig, watching: &Watching) {
    let live = watching.live.lock().unwrap();
    assert!(!live.is_empty() && live.iter().all(|&n| n == 1), "{live:?}");
    assert_eq!(rig.dev_servers.started().len(), live.len());
    assert_eq!(rig.dev_servers.live(), 0, "stopped after the call");
}

#[test]
fn the_server_starts_before_the_call_and_stops_after_it() {
    let rig = project(Some(SHEP));
    let (runner, watching) = watching(&rig);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    worker_turn(&rig, &runner);

    assert_served_each_call(&rig, &watching);
    assert_eq!(rig.dev_servers.started()[0].port, 4421);
}

#[test]
fn the_server_stops_when_the_turn_times_out_or_fails() {
    for error in [
        AgentError::TimedOut(Harness::ClaudeCode),
        AgentError::Failed(Harness::ClaudeCode, "exit 1".into()),
    ] {
        let rig = project(Some(SHEP));
        let (runner, watching) = watching(&rig);
        rig.claude.script([Scripted::Fail(error.clone())]);
        worker_turn(&rig, &runner);

        assert_served_each_call(&rig, &watching);
    }
}

#[test]
fn a_server_that_never_answers_leaves_the_turn_running_and_the_instructions_say_so() {
    let rig = project(Some(SHEP));
    rig.dev_servers.never_answer();
    let (runner, watching) = watching(&rig);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    let worker = worker_turn(&rig, &runner);

    assert_served_each_call(&rig, &watching);
    let told = instructions_of(&worker);
    assert!(told.contains("had not answered after 60 seconds"), "{told}");
    let log = rig.paths().build(ticket(7)).join("dev-server.log");
    assert!(told.contains(&log.display().to_string()), "{told}");
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes.iter().any(|n| n.contains("had not answered")),
        "{notes:?}"
    );
}

#[test]
fn a_server_that_cannot_start_leaves_the_turn_running_and_the_instructions_say_so() {
    let rig = project(Some(SHEP));
    rig.dev_servers.cannot_start("npm is not installed");
    let (runner, watching) = watching(&rig);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    let worker = worker_turn(&rig, &runner);

    let live = watching.live.lock().unwrap();
    assert!(!live.is_empty() && live.iter().all(|&n| n == 0), "{live:?}");
    let told = instructions_of(&worker);
    assert!(
        told.contains("could not start the dev server: npm is not installed"),
        "{told}"
    );
}

#[test]
fn a_project_without_a_launch_file_starts_nothing_during_the_turn() {
    let rig = project(None);
    let (runner, watching) = watching(&rig);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    worker_turn(&rig, &runner);

    assert!(rig.dev_servers.started().is_empty());
    assert!(watching.live.lock().unwrap().iter().all(|&n| n == 0));
}

// What the browser may open before, while and after the planned server is served.
fn gate_around_serving(rig: &Rig) -> ([Option<u16>; 3], String) {
    let runner = rig.open().unwrap();
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    rig.ask(&runner, "add", Some("7"));
    let _ = step(&runner);
    let runner = runner.lock().unwrap();
    let item = runner.current().expect("an item").clone();
    let session = Session::Resume(item.session.clone());
    let mut call = runner
        .worker_call(&item, session, Some("again".into()), None, true)
        .unwrap();
    let run = runner.planned.get(ticket(7)).expect("a plan");
    let before = runner.dev_gate(ticket(7)).get();
    let served = crate::runner::dev_server::started(
        &rig.dev_servers,
        &run,
        &mut call,
        &runner.notes,
        &runner.metrics,
    );
    let during = runner.dev_gate(ticket(7)).get();
    drop(served);
    (
        [before, during, runner.dev_gate(ticket(7)).get()],
        call.prompt,
    )
}

#[test]
fn the_browser_may_open_the_port_only_while_the_server_kelpie_started_answers() {
    let rig = project(Some(SHEP));
    assert_eq!(gate_around_serving(&rig).0, [None, Some(4421), None]);
}

#[test]
fn the_browser_may_not_open_the_port_of_a_server_that_never_answered_or_never_started() {
    let silent = project(Some(SHEP));
    silent.dev_servers.never_answer();
    let (gates, prompt) = gate_around_serving(&silent);
    assert_eq!(gates, [None, None, None]);
    // The resumed session keeps its first instructions, so the prompt carries the news.
    assert!(
        prompt.contains("had not answered after 60 seconds"),
        "{prompt}"
    );

    let broken = project(Some(SHEP));
    broken.dev_servers.cannot_start("the port is taken");
    let (gates, prompt) = gate_around_serving(&broken);
    assert_eq!(gates, [None, None, None]);
    assert!(
        prompt.contains("could not start the dev server: the port is taken"),
        "{prompt}"
    );
}

#[test]
fn port_zero_is_replaced_under_auto_port_and_refused_without_it() {
    let auto =
        r#"{"configurations":[{"name":"web","runtimeExecutable":"npm","port":0,"autoPort":true}]}"#;
    let rig = project(Some(auto));
    rig.dev_servers.pick_next(5302);
    rig.dev_servers.pick_next(5302);
    let (run, _) = planned(&rig, |_| {});
    assert_eq!(run.expect("a plan").port, 5302);

    let fixed = r#"{"configurations":[{"name":"web","runtimeExecutable":"npm","port":0}]}"#;
    let (run, told) = planned(&project(Some(fixed)), |_| {});
    assert_eq!(run, None);
    assert!(told.contains("`port` is 0"), "{told}");
}

// The port planned for issue 7's second turn while issue 99's plan holds 4421 unbound.
fn replanned_beside_a_plan_on_4421(rig: &Rig) -> (Option<DevRun>, String) {
    let runner = rig.open().unwrap();
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    rig.ask(&runner, "add", Some("7"));
    let _ = step(&runner);
    let runner = runner.lock().unwrap();
    let item = runner.current().expect("an item").clone();
    let session = || Session::Resume(item.session.clone());
    runner
        .worker_call(&item, session(), Some("again".into()), None, true)
        .unwrap();
    // Nothing listens on 4421: the other item's server has not bound it yet.
    let other = runner.planned.get(ticket(7)).expect("a plan");
    assert_eq!(other.port, 4421);
    runner.planned.hold(ticket(99), Some(other));
    let call = runner
        .worker_call(&item, session(), Some("again".into()), None, true)
        .unwrap();
    let told = fs::read_to_string(call.instructions.as_ref().unwrap()).unwrap();
    (runner.planned.get(ticket(7)), told)
}

#[test]
fn a_port_another_work_items_plan_holds_is_not_planned_again_though_nothing_listens() {
    let rig = project(Some(SHEP));
    rig.dev_servers.pick_next(5303);
    let run = replanned_beside_a_plan_on_4421(&rig).0.expect("a plan");
    assert_eq!(run.port, 5303);
    assert_eq!(run.args, ["run", "dev", "--", "--port", "5303"]);

    let fixed = r#"{"configurations":[{"name":"web","runtimeExecutable":"npm","port":4421}]}"#;
    let rig = project(Some(fixed));
    let (run, told) = replanned_beside_a_plan_on_4421(&rig);
    assert_eq!(run, None);
    assert!(told.contains("port 4421 is taken"), "{told}");
}

#[test]
fn a_zero_port_is_not_swapped_into_the_arguments_that_say_zero() {
    let zero = r#"{"configurations":[{"name":"web","runtimeExecutable":"npm","runtimeArgs":["dev","--retries","0"],"port":0,"autoPort":true}]}"#;
    let rig = project(Some(zero));
    rig.dev_servers.pick_next(5304);
    rig.dev_servers.pick_next(5304);
    let run = planned(&rig, |_| {}).0.expect("a plan");
    assert_eq!(run.port, 5304);
    assert_eq!(run.args, ["dev", "--retries", "0"]);
    assert_eq!(run.env.get("PORT").map(String::as_str), Some("5304"));
}

// How many runs of the dev server the metrics endpoint counted as `outcome`
fn dev_server_runs(rig: &Rig, outcome: &str) -> Option<f64> {
    let labels = [("feature", "dev_server"), ("outcome", outcome)];
    rig.samples.value("kelpie_feature_runs_total", &labels)
}

#[test]
fn a_server_that_answers_counts_as_an_ok_run() {
    let rig = project(Some(SHEP));
    // The first turn's own flight starts a server too where it can open its browser.
    gate_around_serving(&rig);
    assert!(dev_server_runs(&rig, "ok") >= Some(1.0));
    assert_eq!(dev_server_runs(&rig, "failed"), None);
}

#[test]
fn a_server_that_never_answers_or_never_starts_counts_as_a_failed_run() {
    let silent = project(Some(SHEP));
    silent.dev_servers.never_answer();
    gate_around_serving(&silent);
    assert!(dev_server_runs(&silent, "failed") >= Some(1.0));
    assert_eq!(dev_server_runs(&silent, "ok"), None);

    let broken = project(Some(SHEP));
    broken.dev_servers.cannot_start("the port is taken");
    gate_around_serving(&broken);
    assert!(dev_server_runs(&broken, "failed") >= Some(1.0));
    assert_eq!(dev_server_runs(&broken, "ok"), None);
}

#[test]
fn a_server_that_cannot_be_planned_counts_as_a_failed_run() {
    let fixed = r#"{"configurations":[{"name":"web","runtimeExecutable":"npm","port":4421}]}"#;
    let rig = project(Some(fixed));
    rig.dev_servers.take(4421);
    planned(&rig, |_| {});
    assert!(dev_server_runs(&rig, "failed") >= Some(1.0));
    assert_eq!(dev_server_runs(&rig, "ok"), None);
}

#[test]
fn a_project_without_a_launch_file_on_main_counts_a_skipped_run() {
    let rig = project(None);
    planned(&rig, |_| {});
    assert!(dev_server_runs(&rig, "skipped") >= Some(1.0));
    assert_eq!(dev_server_runs(&rig, "ok"), None);
    assert_eq!(dev_server_runs(&rig, "failed"), None);
}

#[test]
fn a_launch_file_with_no_configuration_counts_a_skipped_run() {
    let rig = project(Some(r#"{"configurations":[]}"#));
    planned(&rig, |_| {});
    assert!(dev_server_runs(&rig, "skipped") >= Some(1.0));
}

#[test]
fn a_project_without_the_browser_counts_no_dev_server_runs() {
    let rig = Rig::new("shep");
    install(&rig);
    rig.land(".claude/launch.json", SHEP);
    planned(&rig, |_| {});
    for outcome in ["ok", "skipped", "failed"] {
        assert_eq!(dev_server_runs(&rig, outcome), None, "{outcome}");
    }
}

// What `kelpie_feature_enabled{feature="dev_server"}` read once the runner opened
fn dev_server_enabled(rig: &Rig) -> Option<f64> {
    rig.open().unwrap();
    rig.samples
        .value("kelpie_feature_enabled", &[("feature", "dev_server")])
}

#[test]
fn the_dev_server_is_on_when_the_browser_is_on_and_main_has_a_launch_file() {
    assert_eq!(dev_server_enabled(&project(Some(SHEP))), Some(1.0));
    assert_eq!(dev_server_enabled(&project(None)), Some(0.0));
    let empty = r#"{"configurations":[]}"#;
    let none = dev_server_enabled(&project(Some(empty)));
    assert_eq!(
        none,
        Some(0.0),
        "a file that gives no server skips by design"
    );
    let rig = Rig::new("shep");
    rig.land(".claude/launch.json", SHEP);
    assert_eq!(dev_server_enabled(&rig), Some(0.0), "the browser is off");
}
