//! The review under each kind of local round, and the check at start

use std::sync::Mutex;

use serde_json::json;

use crate::ports::Checks;
use crate::runner::report::StepReport;
use crate::runner::{Runner, step};
use crate::settings::AgentName;
use crate::settings::LocalRound;
use crate::test::{Answer, Rig, Scripted, ScriptedRound, StandInEndpoint, unreachable_url};

// A rig whose project lists only `mine`, the reviewer `file` defines.
fn rig_with(file: &str) -> Rig {
    let rig = Rig::new("koji");
    rig.reviewers(&["mine"]);
    rig.write_agent("mine", file);
    rig
}

// A rig whose project lists only its Claude session, and no local round.
fn claude_alone() -> Rig {
    let rig = Rig::new("koji");
    rig.reviewers(&["claude"]);
    rig
}

fn endpoint(url: &str) -> String {
    format!(
        "---\nrole: reviewer\nharness: endpoint\nurl: {url}\nmodel: stand-in\n\
         context: 8192\n---\n"
    )
}

fn command(path: &str) -> String {
    format!("---\nrole: reviewer\nharness: command\ncommand: {path}\n---\n")
}

#[test]
fn with_no_local_round_listed_one_clean_claude_round_reaches_ci() {
    let rig = claude_alone();
    let runner = rig.open().unwrap();
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
        })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    assert_eq!(status["work_item"]["by_role"]["reviewer"]["calls"], 1);
    assert!(rig.reviewer.seen().is_empty(), "no local round ran");
}

#[test]
fn with_no_local_round_listed_claudes_fix_goes_on_to_ci() {
    let rig = claude_alone();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("HIGH|src/lib.rs:9|racy|two writers"),
        Scripted::Push("fixed.txt", "fixed\n"),
    ]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, claude: one finding
    let Some(StepReport::ReviewFindingsSent { held: 1, .. }) = step(&runner).unwrap() else {
        panic!("the finding was not sent to the worker");
    };
    step(&runner).unwrap(); // the worker's fix turn
    step(&runner).unwrap(); // the fix is pushed
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    assert_eq!(status["work_item"]["by_role"]["reviewer"]["calls"], 1);
    assert!(rig.reviewer.seen().is_empty(), "no local round ran");
}

#[test]
fn the_endpoint_reviewers_findings_reach_the_worker() {
    let server = StandInEndpoint::start([Answer::Says(
        "MEDIUM|work.txt:1|leftover placeholder text|ships to users",
    )]);
    let rig = rig_with(&endpoint(server.url()));
    rig.reviewer.pass_through();
    let runner = rig.open().unwrap();
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

    step(&runner).unwrap(); // the finding goes to the worker
    let text = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(text.contains("leftover placeholder text"), "{text}");
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
    rig.reviewers(&["mine"]);
    rig.write_agent("mine", &self::command(&command.display().to_string()));
    rig.reviewer.pass_through();
    let runner = rig.open().unwrap();
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
fn a_session_that_says_neither_findings_nor_clean_fails() {
    let rig = claude_alone();
    let runner = rig.open().unwrap();
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
                reason: format!("the reviewer's reply is neither findings nor CLEAN: {said}"),
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
    let err = rig_with(&command("~/bin/no-such-review"))
        .open()
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("setting `agents.reviewers`: mine: cannot run "),
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
            "setting `agents.reviewers`: mine: cannot reach {url}/models: "
        )),
        "{err}"
    );
}

#[test]
fn with_no_local_round_listed_no_command_is_needed() {
    let rig = Rig::new("koji");
    std::fs::remove_file(rig.home.path().join(".claude/scripts/qwen-review.sh")).unwrap();
    assert!(rig.open().is_err(), "the listed qwen command is checked");
    rig.reviewers(&["claude"]);
    assert!(rig.open().is_ok());
}

// What qwen-review.sh itself writes, one line per file, when the GPU box
// cannot be reached, and when it answers with nothing.
const UNREACHABLE: &str = "\
LOW|src/a.rs:0|not reviewed: curl: (7) Failed to connect to gpu.box port 8080|raw response kept at /tmp/qwen-review/raw/src_a.rs.txt
LOW|src/b.rs:0|not reviewed: empty response|raw response kept at /tmp/qwen-review/raw/src_b.rs.txt
";

fn found(lines: &str) -> ScriptedRound {
    ScriptedRound::Findings(crate::ports::parse_findings(lines))
}

fn qwen() -> AgentName {
    AgentName::try_from("qwen".to_owned()).unwrap()
}

fn at_round_one(rig: &Rig) -> Mutex<Runner> {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    runner
}

// Takes the work item, its review done, through green CI to the merge
// ruling, and answers it rework: the worker's fix starts a fresh pass.
fn next_pass(rig: &Rig, runner: &Mutex<Runner>, fix: &'static str) {
    assert_eq!(phase(rig, runner)["state"], "ci");
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Ruling { id, .. }) = rig.verdict(runner) else {
        panic!("no merge ruling");
    };
    rig.ask(runner, "rule", Some(&format!("{id} rework fix it")));
    rig.claude.script([Scripted::Push(fix, "fixed\n")]);
    step(runner).unwrap(); // the noted turn: pushes, and a pass begins
    assert_eq!(phase(rig, runner)["round"], 1);
}

#[test]
fn a_local_round_that_reviewed_nothing_goes_on_to_the_next_reviewer() {
    let rig = Rig::new("koji");
    let runner = at_round_one(&rig);
    rig.reviewer.script([found(UNREACHABLE)]);
    rig.claude.script([Scripted::Text("CLEAN")]);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::LocalRoundFailed {
            issue: 7,
            pull_request: 71,
            round: 1,
            reviewer: qwen(),
            unreviewed: vec!["src/a.rs".into(), "src/b.rs".into()],
        })
    );
    let phase = phase(&rig, &runner);
    assert_eq!(phase["round"], 2, "{phase}");
    assert_eq!(phase["ran"], serde_json::json!(["qwen"]), "{phase}");
    assert_eq!(rig.claude.all_calls().len(), 1, "only the worker's turn");

    step(&runner).unwrap(); // round 2, claude: scripted clean above
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    assert_eq!(rig.reviewer.seen().len(), 1, "qwen ran once");
}

#[test]
fn a_local_round_that_fails_in_two_passes_is_left_out_of_the_next_and_status_says_so() {
    let rig = Rig::new("koji");
    let runner = at_round_one(&rig);
    rig.reviewer
        .script([found(UNREACHABLE), found(UNREACHABLE)]);
    rig.claude.script([Scripted::Text("CLEAN")]);
    step(&runner).unwrap(); // qwen fails
    step(&runner).unwrap(); // claude: clean
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"].get("local_reviewers_down"), None);

    next_pass(&rig, &runner, "second.txt");
    rig.claude.script([Scripted::Text("CLEAN")]);
    step(&runner).unwrap(); // qwen fails again
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["local_reviewers_down"], json!(["qwen"]));
    step(&runner).unwrap(); // claude: clean

    next_pass(&rig, &runner, "third.txt");
    rig.claude.script([Scripted::Text("CLEAN")]);
    step(&runner).unwrap(); // claude, the only reviewer left: clean
    assert_eq!(phase(&rig, &runner)["state"], "ci");
    assert_eq!(rig.reviewer.seen().len(), 2, "no third local round");
}

#[test]
fn a_round_with_some_files_unreviewed_keeps_its_real_findings() {
    let rig = Rig::new("koji");
    let runner = at_round_one(&rig);
    let lines = format!("MEDIUM|src/c.rs:4|leftover debug print|noisy logs\n{UNREACHABLE}");
    rig.reviewer.script([found(&lines)]);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewRound {
            issue: 7,
            pull_request: 71,
            round: 1,
            reviewer: qwen(),
            findings: 1,
            unreviewed: vec!["src/a.rs".into(), "src/b.rs".into()],
        })
    );
    let status = rig.ask(&runner, "status", None);
    let findings = &status["work_item"]["phase"]["stage"]["findings"];
    assert_eq!(findings.as_array().unwrap().len(), 1, "{findings}");
    assert_eq!(findings[0]["file"], "src/c.rs");
    assert_eq!(status["work_item"].get("local_reviewers_down"), None);
}

// A round that found a nit and left two files unreviewed.
const MIXED: &str = "\
LOW|src/c.rs:4|leftover debug print|noisy logs
LOW|src/a.rs:0|not reviewed: curl: (7) Failed to connect to gpu.box port 8080|raw response kept at /tmp/qwen-review/raw/src_a.rs.txt
LOW|src/b.rs:0|not reviewed: empty response|raw response kept at /tmp/qwen-review/raw/src_b.rs.txt
";

fn phase(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
    rig.ask(runner, "status", None)["work_item"]["phase"].clone()
}

// One pass in which qwen's round is `round` and claude's is clean, ending at
// CI. A round with a finding gets the worker's fix turn first, which pushes.
fn pass(rig: &Rig, runner: &Mutex<Runner>, round: ScriptedRound) {
    let found = matches!(&round, ScriptedRound::Findings(f) if f.iter().any(|f| !f.is_unreviewed() && !f.is_skipped_for_size()));
    rig.reviewer.script([round]);
    if found {
        let fix = format!("tidy-{}.txt", rig.reviewer.seen().len());
        rig.claude
            .script([Scripted::Push(Box::leak(fix.into_boxed_str()), "tidied\n")]);
    }
    rig.claude.script([Scripted::Text("CLEAN")]);
    for _ in 0..6 {
        if phase(rig, runner)["state"] != "review" {
            return;
        }
        step(runner).unwrap();
    }
    panic!("the pass never reached CI: {}", phase(rig, runner));
}

fn down(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
    let status = rig.ask(runner, "status", None);
    status["work_item"]["local_reviewers_down"].clone()
}

#[test]
fn files_left_unreviewed_again_count_against_the_reviewer_until_it_is_left_out() {
    let rig = Rig::new("koji");
    let runner = at_round_one(&rig);
    pass(&rig, &runner, found(MIXED)); // first time, nothing yet counts
    assert_eq!(down(&rig, &runner), json!(null));
    next_pass(&rig, &runner, "second.txt");
    pass(&rig, &runner, found(MIXED)); // the same files again, one failure
    assert_eq!(down(&rig, &runner), json!(null));
    next_pass(&rig, &runner, "third.txt");
    pass(&rig, &runner, found(MIXED)); // again, a second in a row
    assert_eq!(down(&rig, &runner), json!(["qwen"]));
    assert_eq!(rig.reviewer.seen().len(), 3);
}

#[test]
fn a_clean_local_round_clears_that_reviewers_failures() {
    let rig = Rig::new("koji");
    let runner = at_round_one(&rig);
    pass(&rig, &runner, found(UNREACHABLE)); // a first failure
    next_pass(&rig, &runner, "second.txt");
    pass(&rig, &runner, ScriptedRound::Findings(Vec::new())); // clean: cleared
    next_pass(&rig, &runner, "third.txt");
    pass(&rig, &runner, found(UNREACHABLE)); // a first failure again
    assert_eq!(down(&rig, &runner), json!(null));
}

#[test]
fn a_round_that_leaves_a_failed_rounds_files_unreviewed_again_takes_its_reviewer_down() {
    let rig = Rig::new("koji");
    let runner = at_round_one(&rig);
    pass(&rig, &runner, found(UNREACHABLE)); // nothing reviewed
    next_pass(&rig, &runner, "second.txt");
    pass(&rig, &runner, found(MIXED)); // a nit, and a.rs and b.rs missed again
    assert_eq!(down(&rig, &runner), json!(["qwen"]));
}

#[test]
fn a_file_skipped_for_its_size_stays_a_finding_and_is_not_unreviewed() {
    let rig = Rig::new("koji");
    let runner = at_round_one(&rig);
    rig.reviewer.script([found(
        "LOW|src/big.rs:0|not reviewed: 900 lines exceeds the chunk limit|split the file or review it by hand\n",
    )]);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewRound {
            issue: 7,
            pull_request: 71,
            round: 1,
            reviewer: qwen(),
            findings: 1,
            unreviewed: Vec::new(),
        })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["stage"]["stage"], "found");
    assert_eq!(status["work_item"].get("local_reviewers_down"), None);
    // No fix of the worker's reviews the file, so it goes to no fix turn.
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent {
            issue: 7,
            pull_request: 71,
            round: 1,
            held: 0,
        })
    );
    assert_eq!(phase(&rig, &runner)["ran"], json!(["qwen"]));
    assert_eq!(rig.claude.calls().len(), 1, "no fix turn");
}

#[test]
fn a_file_skipped_for_its_size_is_left_out_of_a_fix_turn_sent_other_findings() {
    let rig = Rig::new("koji");
    let runner = at_round_one(&rig);
    rig.reviewer.script([found(
        "MEDIUM|src/c.rs:4|the flag is misnamed|it reads as its opposite\n\
         LOW|src/big.rs:0|not reviewed: 900 lines exceeds the chunk limit|split the file or review it by hand\n",
    )]);
    step(&runner).unwrap(); // round 1, qwen: a finding and a skipped file
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent {
            issue: 7,
            pull_request: 71,
            round: 1,
            held: 1,
        })
    );
    let text = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(text.contains("MEDIUM|src/c.rs:4"), "{text}");
    assert!(!text.contains("src/big.rs"), "{text}");
}
