//! A worker turn on a project with a browser is given one, and no other call is

use super::*;
use crate::ports::{CLEAN, Role, Session};
use crate::runner::flight::{Pass, advance};
use crate::settings::AgentHarness;
use crate::test::{CALL_PATIENCE, Hold, ticket};

const OFF: &str = "[app.dogs.kelpie.browser]\nenabled = false\nheaded = false\ndomains = []";

pub(super) fn turn_on(rig: &Rig) {
    rig.edit_settings(|s| {
        assert!(s.contains(OFF), "the example's browser moved");
        s.replace(
            OFF,
            "[app.dogs.kelpie.browser]\nenabled = true\nheaded = false\n\
             domains = [\"app.example.com\"]",
        )
    });
}

// The server and a Chromium build, as `shep kelpie tools install` leaves them
pub(super) fn install(rig: &Rig) {
    let tools = rig.paths().tools;
    fs::create_dir_all(tools.playwright_mcp().parent().unwrap()).unwrap();
    fs::write(tools.playwright_mcp(), "").unwrap();
    fs::create_dir_all(tools.browsers().join("chromium-1200")).unwrap();
}

pub(super) fn instructions_of(seen: &crate::test::Seen) -> String {
    fs::read_to_string(seen.call.instructions.as_ref().unwrap()).unwrap()
}

#[test]
fn a_worker_turn_on_a_project_with_the_browser_on_gets_a_socket_and_is_told_its_hosts() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, qwen: clean by default
    step(&runner).unwrap(); // round 2, claude: scripted clean above

    let seen = rig.claude.all_seen();
    let worker = seen.iter().find(|s| s.call.role == Role::Worker).unwrap();
    let browser = worker.call.reach.browser.as_ref().expect("a browser");
    assert_eq!(browser.kelpie, std::path::Path::new(Rig::KELPIE));
    assert_eq!(browser.socket.parent(), Some(rig.paths().worker.as_path()));
    let config = fs::read_to_string(worker.call.settings.with_extension("mcp.json")).unwrap();
    assert!(config.contains("mcp-connect"), "{config}");
    let told = instructions_of(worker);
    assert!(told.contains("`app.example.com`"), "{told}");
    assert!(told.contains("`playwright` MCP server"), "{told}");
    // The turn is over, so its bridge is dropped and its socket is gone.
    assert!(!browser.socket.exists(), "{}", browser.socket.display());
    let reviewers = seen.iter().filter(|s| s.call.role == Role::Reviewer);
    for reviewer in reviewers {
        assert_eq!(reviewer.call.reach.browser, None);
    }
}

#[test]
fn a_bridge_is_closed_when_its_turn_ends() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);

    step(&runner).unwrap(); // the worker's first turn

    let asked = rig.bridges.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    let tools = rig.paths().tools;
    assert_eq!(asked[0].worker, rig.paths().worker);
    assert_eq!(asked[0].server.mcp, tools.playwright_mcp());
    assert_eq!(asked[0].server.browsers, tools.browsers());
    assert!(!asked[0].server.headed);
    assert_eq!(asked[0].filter.domains, ["app.example.com"]);
    // The turn is over, so the flight dropped the handle, and that closed the bridge.
    assert_eq!(rig.bridges.open_now(), 0);
}

#[test]
fn a_bridge_is_open_while_its_turn_runs() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);

    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE));
    let open = rig.bridges.open_now();
    hold.release();

    assert_eq!(open, 1);
}

#[test]
fn a_project_without_the_table_gets_no_browser_though_the_tools_are_installed() {
    let rig = Rig::new("shep");
    install(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();

    let seen = rig.claude.all_seen();
    let worker = seen.iter().find(|s| s.call.role == Role::Worker).unwrap();
    assert_eq!(worker.call.reach.browser, None);
    assert!(!instructions_of(worker).contains("playwright"));
}

#[test]
fn a_codex_worker_turn_on_a_project_with_the_browser_on_gets_one_and_is_told_of_it() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig);
    let agent =
        "---\nrole: implementer\nharness: codex\nmodel: gpt-6-sol\neffort: low\n---\nWork.\n";
    rig.write_agent("codex-box", agent);
    rig.agent_board.ranks(&["codex-box"]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();

    let seen = rig.claude.all_seen();
    let worker = seen.iter().find(|s| s.call.role == Role::Worker).unwrap();
    assert_eq!(worker.call.harness, AgentHarness::Codex);
    assert!(worker.call.reach.browser.is_some());
    assert!(instructions_of(worker).contains("`playwright` MCP server"));
}

#[test]
fn a_goose_worker_turn_on_a_project_with_the_browser_on_gets_one_and_is_told_of_it() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig);
    let agent = "---\nrole: implementer\nharness: goose\nmodel: qwen3.8:27b\neffort: low\n\
                 url: http://box:11434/v1\ncontext: 65536\n---\nWork.\n";
    rig.write_agent("goose-box", agent);
    rig.agent_board.ranks(&["goose-box"]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();

    let seen = rig.claude.all_seen();
    let worker = seen.iter().find(|s| s.call.role == Role::Worker).unwrap();
    assert!(matches!(worker.call.harness, AgentHarness::Goose(_)));
    assert!(worker.call.reach.browser.is_some());
    assert!(instructions_of(worker).contains("`playwright` MCP server"));
}

// Runs a goose worker's turn, kills the runner in it, and resumes the turn in a new runner. Says
// the session of each of the two calls and whether it had a browser and was told of one.
fn goose_turn_killed_and_resumed(
    rig: &Rig,
    install_before_resuming: bool,
) -> Vec<(Session, bool, bool)> {
    let agent = "---\nrole: implementer\nharness: goose\nmodel: qwen3.8:27b\neffort: low\n\
                 url: http://box:11434/v1\ncontext: 65536\n---\nWork.\n";
    rig.write_agent("goose-box", agent);
    rig.agent_board.ranks(&["goose-box"]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Kill]);
    let killed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| step(&runner)));
    assert!(killed.is_err(), "the scripted kill did not happen");
    drop(runner);
    if install_before_resuming {
        install(rig);
    }
    let runner = rig.open().unwrap();
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    (rig.claude.seen().iter())
        .map(|s| {
            (
                s.call.session.clone(),
                s.call.reach.browser.is_some(),
                instructions_of(s).contains("`playwright` MCP server"),
            )
        })
        .collect()
}

#[test]
fn a_resumed_goose_session_that_began_without_a_browser_is_given_none_and_told_of_none() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    let calls = goose_turn_killed_and_resumed(&rig, true);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(matches!(calls[0].0, Session::New(_)));
    assert!(matches!(calls[1].0, Session::Resume(_)), "{calls:?}");
    assert!(!calls[0].1);
    assert_eq!((calls[1].1, calls[1].2), (false, false), "{calls:?}");
}

#[test]
fn a_resumed_goose_session_that_began_with_a_browser_is_given_one_again() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig);
    let calls = goose_turn_killed_and_resumed(&rig, false);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(matches!(calls[1].0, Session::Resume(_)), "{calls:?}");
    assert_eq!((calls[0].1, calls[0].2), (true, true));
    assert_eq!((calls[1].1, calls[1].2), (true, true), "{calls:?}");
}

#[test]
fn a_project_whose_tools_lack_the_browser_runs_without_one_and_a_note_says_to_install() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();

    let seen = rig.claude.all_seen();
    let worker = seen.iter().find(|s| s.call.role == Role::Worker).unwrap();
    assert_eq!(worker.call.reach.browser, None);
    assert!(!instructions_of(worker).contains("playwright"));
    let notes = runner.lock().unwrap().take_notes();
    let note = notes.iter().find(|n| n.contains("without a browser"));
    let note = note.unwrap_or_else(|| panic!("{notes:?}"));
    assert!(note.contains("issue #7's worker"), "{note}");
    assert!(note.contains("shep kelpie tools install"), "{note}");
}

#[test]
fn a_turn_whose_call_cannot_be_prepared_leaves_no_socket_behind() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    // A folder where the call's settings file goes, so writing it fails.
    fs::create_dir_all(rig.paths().worker.join("settings-7.json")).unwrap();
    let _ = step(&runner);

    let sockets = fs::read_dir(rig.paths().worker).unwrap();
    let left: Vec<_> = (sockets.flatten().map(|e| e.file_name()))
        .filter(|name| name.to_string_lossy().ends_with(".sock"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

// Linux waits on #135, so there a reread that turns the browser on is refused as the start is.
#[test]
fn a_reread_that_turns_the_browser_on_is_checked_as_the_start_checks_it() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    turn_on(&rig);
    let read = (runner.lock().unwrap()).reread(rig.settings(), rig.kelpie_settings());
    if cfg!(target_os = "macos") {
        let line = read.unwrap().expect("a change");
        assert!(line.contains("browser"), "{line}");
    } else {
        let why = read.unwrap_err().to_string();
        assert!(
            why.contains("browser.enabled") && why.contains("#135"),
            "{why}"
        );
    }
}

#[test]
fn a_turn_retried_after_its_host_was_down_gets_a_browser_of_its_own_on_the_same_socket() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let down = AgentError::Unreachable(crate::settings::Harness::ClaudeCode, "502".into());
    rig.claude
        .script([Scripted::Fail(down), Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the host is down
    rig.clock.advance(30);
    step(&runner).unwrap(); // the retry

    let seen = rig.claude.all_seen();
    let workers: Vec<_> = seen
        .iter()
        .filter(|s| s.call.role == Role::Worker)
        .collect();
    assert_eq!(workers.len(), 2, "the failed call and its retry");
    let socket = |s: &crate::test::Seen| {
        s.call
            .reach
            .browser
            .as_ref()
            .expect("a browser")
            .socket
            .clone()
    };
    let (first, retry) = (socket(workers[0]), socket(workers[1]));
    // A session started on the first call keeps the command that names this path.
    assert_eq!(first, retry, "the retry moved the socket its session names");
    let config = fs::read_to_string(workers[1].call.settings.with_extension("mcp.json")).unwrap();
    assert!(config.contains(&retry.display().to_string()), "{config}");
}

#[test]
fn an_attached_session_is_not_told_of_a_browser_it_lacks() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    let runner = runner.lock().unwrap();
    let item = runner.current().expect("an item").clone();
    let session = crate::ports::Session::Resume(item.session.clone());
    let attach = runner
        .prepare(&item, session, Some(String::new()), None)
        .unwrap();
    let told = fs::read_to_string(attach.instructions.as_ref().unwrap()).unwrap();
    assert!(!told.contains("playwright"), "{told}");
    assert_eq!(attach.reach.browser, None);
}

// How many runs of the browser the metrics endpoint counted as `outcome`
fn browser_runs(rig: &Rig, outcome: &str) -> Option<f64> {
    let labels = [("feature", "browser"), ("outcome", outcome)];
    rig.samples.value("kelpie_feature_runs_total", &labels)
}

#[test]
fn a_bridge_opened_for_a_turn_counts_as_an_ok_run() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    let _ = step(&runner);

    assert!(browser_runs(&rig, "ok") >= Some(1.0));
    assert_eq!(browser_runs(&rig, "skipped"), None);
    assert_eq!(browser_runs(&rig, "failed"), None);
}

#[test]
fn a_turn_whose_tools_lack_the_browser_counts_as_a_skipped_run() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();

    assert_eq!(browser_runs(&rig, "skipped"), Some(1.0));
    assert_eq!(browser_runs(&rig, "ok"), None);
    assert_eq!(browser_runs(&rig, "failed"), None);
}

#[test]
fn a_bridge_that_cannot_be_opened_counts_as_a_failed_run() {
    let rig = Rig::new("shep");
    turn_on(&rig);
    install(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    // A file where the work item's browser folder goes, which kelpie will not use.
    let folder = rig.paths().browser(ticket(7));
    fs::create_dir_all(folder.parent().unwrap()).unwrap();
    fs::write(&folder, "").unwrap();
    let _ = step(&runner);

    assert_eq!(browser_runs(&rig, "failed"), Some(1.0));
    assert_eq!(browser_runs(&rig, "ok"), None);
}

#[test]
fn a_project_without_the_browser_counts_no_browser_runs() {
    let rig = Rig::new("shep");
    install(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();

    for outcome in ["ok", "skipped", "failed"] {
        assert_eq!(browser_runs(&rig, outcome), None, "{outcome}");
    }
}

#[test]
fn a_worker_turn_on_a_local_ticket_with_the_browser_on_gets_a_socket_and_is_told_its_hosts() {
    let rig = Rig::new("koji");
    rig.tickets_local();
    rig.ticket_verb(&["new", "Add a thing", "--label", "ready-for-agent"]);
    turn_on(&rig);
    install(&rig);
    let runner = rig.open().unwrap();
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // dispatches k1
    step(&runner).unwrap(); // the worker's first turn

    let seen = rig.claude.all_seen();
    let worker = seen.iter().find(|s| s.call.role == Role::Worker).unwrap();
    assert_eq!(worker.call.issue, Some("k1".parse().unwrap()));
    let browser = worker.call.reach.browser.as_ref().expect("a browser");
    assert_eq!(browser.socket.parent(), Some(rig.paths().worker.as_path()));
    let told = instructions_of(worker);
    assert!(told.contains("`app.example.com`"), "{told}");
    assert!(told.contains("`playwright` MCP server"), "{told}");
    assert!(rig.paths().browser("k1".parse().unwrap()).exists());
}
