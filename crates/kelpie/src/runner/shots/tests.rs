use std::path::PathBuf;

use serde_json::{Value, json};

use crate::ports::{Checks, Finding, Role, Severity};
use crate::profile::INSTRUCTIONS;
use crate::runner::{Runner, StepReport, step};
use crate::shots::publish::MARKER;
use crate::test::{Rig, Scripted, ScriptedShots, git};

/// A playground-like project: a launch file on `main`, and a preview table
fn with_preview(project: &str) -> Rig {
    let rig = Rig::new(project);
    rig.land_launch_file();
    rig.edit_settings(|s| {
        format!("{s}\n[preview]\nroutes = [\"/\", \"/events\"]\ndomains = [\"api.example.com\"]\n")
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
    assert_eq!(instructions.unwrap(), INSTRUCTIONS);
    assert_eq!(shots_comments(&rig), Vec::<String>::new());
    assert!(!rig.paths().worker.join("mcp.json").exists());
}

#[test]
fn a_launch_file_the_worker_adds_on_its_branch_opens_nothing() {
    let rig = Rig::new("koji");
    let runner = started(&rig);
    rig.claude.script([
        Scripted::Push(".claude/launch.json", r#"{"configurations": []}"#),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's turn adds the file
    step(&runner).unwrap(); // round 1, qwen
    step(&runner).unwrap(); // round 2, claude: no shots first
    assert!(rig.worktree_7().join(".claude/launch.json").is_file());
    assert_eq!(rig.shots.jobs(), []);
    let all = rig.claude.all_seen();
    let round = all.iter().find(|s| s.call.role == Role::Reviewer).unwrap();
    assert!(!round.call.prompt.contains("--- shots ---"));
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
    let instructions = std::fs::read_to_string(worker.join("instructions.md")).unwrap();
    assert!(instructions.starts_with(INSTRUCTIONS));
    assert!(
        instructions.contains("# Seeing what you build"),
        "{instructions}"
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
        json!({ "permissions": { "additionalDirectories": [dir] } })
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

// Up to green CI on the reviewed head, which the Claude round's shots cover
fn green(project: &str) -> (Rig, std::sync::Mutex<Runner>, String) {
    let rig = with_preview(project);
    let runner = started(&rig);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    for _ in 0..4 {
        step(&runner).unwrap(); // the turn, qwen, the shots, claude
    }
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    (rig, runner, head)
}

fn tree(rig: &Rig, commit: &str) -> Vec<String> {
    let origin = rig.home.path().join("origin.git");
    let names = git(&origin, &["ls-tree", "--name-only", commit]);
    names.lines().map(str::to_owned).collect()
}

#[test]
fn the_merge_ruling_puts_the_shots_on_one_comment_off_the_branch() {
    let (rig, runner, head) = green("lab");
    rig.shots.script([]);
    let Some(StepReport::ShotsPosted {
        comment, head: of, ..
    }) = rig.verdict(&runner)
    else {
        panic!("no shots were posted before the ruling");
    };
    assert_eq!(of, head);
    assert_eq!(
        rig.shots.jobs().len(),
        1,
        "the round's run of this head is reused"
    );
    let Some(StepReport::Ruling { id: 1, .. }) = step(&runner).unwrap() else {
        panic!("the merge ruling did not follow");
    };

    let [body] = shots_comments(&rig).try_into().unwrap();
    let shots = rig
        .forge
        .head_of("kelpie-shots/71")
        .expect("the shots branch");
    assert!(
        body.contains(&format!("Kelpie's shots of {}.", &head[..7])),
        "{body}"
    );
    assert!(body.contains("the `kelpie-shots/71` branch"), "{body}");
    let url =
        format!("https://github.com/shep-pm/shep/blob/{shots}/events-mobile-dark.png?raw=true");
    assert!(body.contains(&url), "{body}");
    assert_eq!(tree(&rig, &shots).len(), 8);
    assert!(tree(&rig, "kelpie/7").iter().all(|f| !f.ends_with(".png")));
    assert_eq!(
        rig.forge.head_of("kelpie/7").unwrap(),
        head,
        "the branch never moved"
    );
    assert_eq!(
        rig.forge.edits(),
        Vec::<u64>::new(),
        "posted once, {comment}, and not yet edited"
    );
}

#[test]
fn a_rework_edits_the_shots_comment_in_place() {
    let (rig, runner, first) = green("lab");
    let Some(StepReport::ShotsPosted { comment, .. }) = rig.verdict(&runner) else {
        panic!("no shots were posted");
    };
    step(&runner).unwrap(); // merge ruling 1
    let shots_1 = rig.forge.head_of("kelpie-shots/71").unwrap();

    rig.ask(&runner, "rule", Some("1 no make the header darker"));
    rig.claude.script([
        Scripted::Push("header.css", "dark\n"),
        Scripted::Text("CLEAN"),
    ]);
    for _ in 0..4 {
        step(&runner).unwrap(); // the fix, qwen, the shots, claude
    }
    let second = rig.forge.head_of("kelpie/7").unwrap();
    assert_ne!(second, first);
    rig.forge.set_checks(&second, Checks::Passed);
    let Some(StepReport::ShotsPosted {
        comment: again,
        head,
        ..
    }) = rig.verdict(&runner)
    else {
        panic!("the rework's shots were not posted");
    };
    assert_eq!((again, head.as_str()), (comment, second.as_str()));
    assert_eq!(rig.forge.edits(), [comment]);
    let [body] = shots_comments(&rig).try_into().unwrap();
    assert!(
        body.contains(&format!("Kelpie's shots of {}.", &second[..7])),
        "{body}"
    );
    let shots_2 = rig.forge.head_of("kelpie-shots/71").unwrap();
    let origin = rig.home.path().join("origin.git");
    assert_eq!(
        git(&origin, &["rev-parse", &format!("{shots_2}^")]),
        shots_1,
        "no force"
    );
}

#[test]
fn a_page_that_calls_a_domain_off_the_list_says_so_on_the_comment() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    let blocked = "blocked https://cdn.example/a.png: cdn.example is not a preview domain";
    rig.shots
        .script([ScriptedShots::Problems(vec![blocked.into()])]);
    for _ in 0..4 {
        step(&runner).unwrap();
    }
    let all = rig.claude.all_seen();
    let round = all.iter().find(|s| s.call.role == Role::Reviewer).unwrap();
    assert!(
        round
            .call
            .prompt
            .contains(&format!("/events at desktop, light: {blocked}"))
    );

    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    rig.verdict(&runner);
    let [body] = shots_comments(&rig).try_into().unwrap();
    assert!(
        body.contains(&format!(
            "What went wrong:\n\n- `/ at mobile, light: {blocked}`\n"
        )),
        "{body}"
    );
}

#[test]
fn a_failed_run_at_the_merge_is_on_the_comment_and_the_ruling_still_comes() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    rig.shots.script([ScriptedShots::Fail(
        "the dev server exited: bun: command not found",
    )]);
    for _ in 0..4 {
        step(&runner).unwrap();
    }
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let posted = rig.verdict(&runner);
    assert!(
        matches!(posted, Some(StepReport::ShotsPosted { .. })),
        "{posted:?}"
    );
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    let [body] = shots_comments(&rig).try_into().unwrap();
    assert!(body.ends_with(&format!(
        "Kelpie could not take shots of {}: the dev server exited: bun: command not found\n",
        &head[..7]
    )));
    assert_eq!(
        rig.forge.head_of("kelpie-shots/71"),
        None,
        "nothing to push"
    );
}

#[test]
fn a_comment_that_cannot_be_posted_does_not_hold_the_ruling() {
    let (rig, runner, _) = green("lab");
    rig.forge.set_comments_down(true);
    let report = rig.verdict(&runner);
    let Some(StepReport::ShotsNotPosted { reason, .. }) = report else {
        panic!("{report:?}");
    };
    assert_eq!(reason, "gh failed: comments are down");
    rig.forge.set_comments_down(false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ruling { .. })
    ));
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
    let _: PathBuf = job.out;
}
