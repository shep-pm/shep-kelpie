//! The review loop under each kind of local round, and the check at start

use serde_json::json;

use crate::ports::{Finding, Role, Severity};
use crate::runner::report::StepReport;
use crate::runner::step;
use crate::settings::LocalRound;
use crate::settings::ReviewerName;
use crate::test::{Answer, Rig, Scripted, ScriptedRound, StandInEndpoint, unreachable_url};

// A rig whose project runs `table` as its local round.
fn rig_with(table: &str) -> Rig {
    let rig = Rig::new("koji");
    rig.edit_settings(|s| crate::test::with_tables(&s, table));
    rig
}

fn endpoint(url: &str) -> String {
    format!(
        "[app.dogs.kelpie.review.local]\nkind = \"endpoint\"\nurl = \"{url}\"\n\
         model = \"stand-in\"\ncontext = 8192\n"
    )
}

#[test]
fn with_the_local_round_off_one_clean_claude_round_reaches_ci() {
    let rig = rig_with("[app.dogs.kelpie.review.local]\nkind = \"off\"\n");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn
    assert_eq!(
        step(&runner).unwrap(), // round 1, claude: scripted clean above
        Some(StepReport::ReviewFindingsSent {
            issue: 7,
            pull_request: 71,
            round: 1,
            held: 0,
            clean: true,
        })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    assert_eq!(status["work_item"]["by_role"]["reviewer"]["calls"], 1);
    assert!(rig.reviewer.seen().is_empty(), "no local round ran");
}

#[test]
fn with_the_local_round_off_a_round_after_a_fix_is_claudes_again() {
    let rig = rig_with("[app.dogs.kelpie.review.local]\nkind = \"off\"\n");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("HIGH|src/lib.rs:9|racy|two writers"),
        Scripted::Text(r#"{"holds": true, "severity": "high", "reason": "it is"}"#),
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, claude: one finding
    step(&runner).unwrap(); // the judge holds it
    let Some(StepReport::ReviewFindingsSent { clean, .. }) = step(&runner).unwrap() else {
        panic!("the held finding was not sent to the worker");
    };
    assert!(!clean);
    step(&runner).unwrap(); // the worker's fix turn
    step(&runner).unwrap(); // the fix is pushed
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({
            "state": "review",
            "round": 2,
            "consecutive_clean": 0,
            "guard_cleared": false,
            "stage": { "stage": "round" },
            "last": "claude",
        }),
    );
    step(&runner).unwrap(); // round 2, claude: scripted clean above
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    assert_eq!(status["work_item"]["by_role"]["reviewer"]["calls"], 2);
    assert!(rig.reviewer.seen().is_empty(), "no local round ran");
}

#[test]
fn the_endpoint_reviewers_findings_reach_the_judge() {
    let server = StandInEndpoint::start([Answer::Says(
        "MEDIUM|work.txt:1|leftover placeholder text|ships to users",
    )]);
    let rig = rig_with(&endpoint(server.url()));
    rig.reviewer.pass_through();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    assert!(matches!(
        step(&runner).unwrap(), // round 1, the endpoint
        Some(StepReport::ReviewRound {
            round: 1,
            findings: 1,
            ..
        })
    ));
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["model"], "stand-in");
    let diff = requests[0]["messages"][1]["content"].as_str().unwrap();
    assert!(diff.contains("### work.txt\n"), "{diff}");
    assert!(diff.contains("     1 +work\n"), "{diff}");

    rig.claude.script([Scripted::Text(
        r#"{"holds": false, "severity": "low", "reason": "it is a test file"}"#,
    )]);
    step(&runner).unwrap(); // the judge
    let calls = rig.claude.all_calls();
    let judge = calls.iter().find(|c| c.role == Role::Judge).unwrap();
    assert!(
        judge.prompt.contains("leftover placeholder text"),
        "{}",
        judge.prompt
    );
}

#[test]
fn a_named_command_runs_in_place_of_the_script() {
    let rig = Rig::new("koji");
    let command = rig.home.path().join("bin/review");
    std::fs::create_dir_all(command.parent().unwrap()).unwrap();
    crate::test::write_script(
        &command,
        "#!/bin/sh\nmkdir -p \"$QWEN_REVIEW_OUT\"\n\
         echo 'MEDIUM|work.txt:1|named command ran|it did' > \"$QWEN_REVIEW_OUT/round-1.txt\"\n\
         : > \"$QWEN_REVIEW_OUT/round-1.txt.done\"\n",
    );
    let table = format!(
        "[app.dogs.kelpie.review.local]\nkind = \"command\"\ncommand = \"{}\"\n",
        command.display()
    );
    rig.edit_settings(|s| crate::test::with_tables(&s, &table));
    rig.reviewer.pass_through();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    assert!(matches!(
        step(&runner).unwrap(), // round 1, the named command
        Some(StepReport::ReviewRound {
            round: 1,
            findings: 1,
            ..
        })
    ));
    let seen = rig.reviewer.seen();
    assert_eq!(seen.len(), 1);
    let LocalRound::Command(local) = &seen[0].local else {
        panic!("{:?}", seen[0].local);
    };
    assert_eq!(local.command, command);
}

#[test]
fn a_claude_round_that_says_neither_findings_nor_clean_fails() {
    let rig = rig_with("[app.dogs.kelpie.review.local]\nkind = \"off\"\n");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text(""),
        Scripted::Text("Looks good to me."),
    ]);
    step(&runner).unwrap(); // the worker's first turn
    for said in ["", "Looks good to me."] {
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::GateFailed {
                issue: 7,
                reason: format!("the Claude round's reply is neither findings nor CLEAN: {said}"),
            })
        );
    }
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "review",
        "nothing reviewed, so the round is still due"
    );
}

#[test]
fn a_missing_command_stops_the_runner_naming_it() {
    let table =
        "[app.dogs.kelpie.review.local]\nkind = \"command\"\ncommand = \"~/bin/no-such-review\"\n";
    let err = rig_with(table).open().unwrap_err().to_string();
    assert!(
        err.starts_with("setting `review.local`: cannot run "),
        "{err}"
    );
    assert!(
        err.contains("bin/no-such-review: entity not found"),
        "{err}"
    );
}

#[test]
fn an_unreachable_endpoint_stops_the_runner_naming_it() {
    let url = unreachable_url();
    let err = rig_with(&endpoint(&url)).open().unwrap_err().to_string();
    assert!(
        err.starts_with(&format!(
            "setting `review.local`: cannot reach {url}/models: "
        )),
        "{err}"
    );
}

#[test]
fn with_the_local_round_off_no_command_is_needed() {
    let rig = Rig::new("koji");
    std::fs::remove_file(rig.home.path().join(".claude/scripts/qwen-review.sh")).unwrap();
    assert!(rig.open().is_err(), "the default command is checked");
    rig.edit_settings(|s| {
        crate::test::with_tables(&s, "[app.dogs.kelpie.review.local]\nkind = \"off\"\n")
    });
    assert!(rig.open().is_ok());
}

#[test]
fn past_its_local_rounds_every_round_is_claudes_and_one_clean_one_ends_the_loop() {
    let rig = Rig::new("koji");
    rig.edit_settings(|s| {
        assert!(s.contains("loop_guard = 8\n"), "the example's guard moved");
        s.replace("loop_guard = 8\n", "loop_guard = 8\nlocal_rounds = 1\n")
    });
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
        severity: Severity::Medium,
        file: "src/lib.rs".into(),
        line: 3,
        what: "the flag is misnamed".into(),
        why: "it reads as its opposite".into(),
    }])]);
    let holds = r#"{"holds": true, "severity": "high", "reason": "it is"}"#;
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text(holds),
        Scripted::Push("named.txt", "named\n"),
        Scripted::Text("HIGH|src/lib.rs:9|racy|two writers"),
        Scripted::Text(holds),
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn
    let mut reviewers = Vec::new();
    for _ in 0..12 {
        match step(&runner).unwrap() {
            Some(StepReport::ReviewRound { reviewer, .. }) => reviewers.push(reviewer),
            // Only a Claude round is left to come back with nothing.
            Some(StepReport::ReviewFindingsSent { held: 0, .. }) => {
                reviewers.push(ReviewerName::claude());
                break;
            }
            _ => {}
        }
    }
    let names: Vec<&str> = reviewers.iter().map(ReviewerName::as_str).collect();
    assert_eq!(names, ["qwen", "claude", "claude"]);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    assert_eq!(rig.reviewer.seen().len(), 1, "one local round");
}

#[test]
fn local_rounds_spent_before_a_no_stay_spent_in_the_next_pass() {
    let (rig, runner, _) = Rig::parked_set("koji", |rig| {
        rig.edit_settings(|s| s.replace("loop_guard = 8\n", "loop_guard = 8\nlocal_rounds = 1\n"));
    });
    assert_eq!(
        rig.reviewer.seen().len(),
        1,
        "the first pass ran its local round"
    );
    rig.ask(&runner, "rule", Some("1 no rename it"));
    rig.claude.script([
        Scripted::Push("rename.txt", "renamed\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the noted turn: pushes, enters round 1
    step(&runner).unwrap(); // round 1, claude: scripted clean above
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    assert_eq!(rig.reviewer.seen().len(), 1, "no second local round");
}

#[test]
fn a_local_round_with_no_limit_set_leaves_the_state_file_as_it_was() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, local: clean by default
    assert_eq!(rig.reviewer.seen().len(), 1, "the local round ran");
    let saved = std::fs::read_to_string(rig.paths().state).unwrap();
    assert!(!saved.contains("local_rounds"), "{saved}");
}
