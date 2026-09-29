//! The review loop under each kind of local round, and the check at start

use serde_json::json;

use crate::ports::{ReviewerError, Role};
use crate::runner::report::StepReport;
use crate::runner::step;
use crate::settings::LocalRound;
use crate::state::StateStore;
use crate::test::{Answer, Rig, Scripted, ScriptedRound, StandInEndpoint, unreachable_url};

const TABLE: &str = "[app.dogs.kelpie.review.local]\n\
                     kind = \"command\"\n\
                     command = \"~/.claude/scripts/qwen-review.sh\"\n";

// A rig whose project runs `table` as its local round.
fn rig_with(table: &str) -> Rig {
    let rig = Rig::new("koji");
    rig.edit_settings(|s| {
        assert!(s.contains(TABLE), "the example's local round moved");
        s.replace(TABLE, table)
    });
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
    rig.edit_settings(|s| s.replace(TABLE, &table));
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
    rig.edit_settings(|s| s.replace(TABLE, "[app.dogs.kelpie.review.local]\nkind = \"off\"\n"));
    assert!(rig.open().is_ok());
}

// A running project at its first local round, past the worker's turn.
fn at_the_local_round() -> (Rig, std::sync::Mutex<crate::runner::Runner>) {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    (rig, runner)
}

// The open work item's timings once the round is saved.
fn timings_after(rig: &Rig) -> crate::work_item::Timings {
    let state = StateStore::new(rig.paths().state).load().unwrap().unwrap();
    state.work_items[0].timings
}

fn assert_sums_to_wall_time(timings: &crate::work_item::Timings) {
    let started = timings.started.expect("counted from the start");
    let charged = timings.charged.expect("charged by the round");
    assert_eq!(timings.seconds.total(), charged.0 - started.0);
}

#[test]
fn a_local_rounds_wait_for_the_gpu_is_counted_apart_from_the_round() {
    let (rig, runner) = at_the_local_round();
    rig.reviewer.script([ScriptedRound::Waited {
        findings: Vec::new(),
        gpu_wait_seconds: 600,
        took_seconds: 900,
    }]);
    step(&runner).unwrap();
    let timings = timings_after(&rig);
    assert_eq!(timings.seconds.gpu_wait, 600);
    assert_eq!(timings.seconds.local_round, 300);
    assert_sums_to_wall_time(&timings);
}

#[test]
fn a_wait_longer_than_the_round_moves_only_the_rounds_own_length() {
    let (rig, runner) = at_the_local_round();
    rig.reviewer.script([ScriptedRound::Waited {
        findings: Vec::new(),
        gpu_wait_seconds: 5000,
        took_seconds: 100,
    }]);
    step(&runner).unwrap();
    let timings = timings_after(&rig);
    assert_eq!(timings.seconds.gpu_wait, 100);
    assert_eq!(timings.seconds.local_round, 0);
    assert_sums_to_wall_time(&timings);
}

#[test]
fn a_failed_local_round_keeps_the_wait_it_had() {
    let (rig, runner) = at_the_local_round();
    rig.reviewer.script([ScriptedRound::FailedWaiting {
        error: ReviewerError::Failed("gave up".into()),
        gpu_wait_seconds: 40,
        took_seconds: 50,
    }]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::GateFailed { .. })
    ));
    let timings = timings_after(&rig);
    assert_eq!(timings.seconds.gpu_wait, 40);
    assert_eq!(timings.seconds.local_round, 10);
    assert_sums_to_wall_time(&timings);
}
