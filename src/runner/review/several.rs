//! The review loop over a project's list of reviewers

use std::sync::Mutex;

use crate::ports::{Finding, Role, Severity};
use crate::runner::{Runner, step};
use crate::settings::LocalRound;
use crate::test::{Rig, Scripted, ScriptedRound};

// A rig whose project lists `list`, over kelpie's definitions of `mine`, a
// stand-in command, and `opus`, a Claude session limited to `src/`.
fn listing(list: &str) -> Rig {
    let rig = Rig::new("koji");
    rig.edit_settings(|s| {
        assert!(s.contains("loop_guard = 8\n"), "the example's guard moved");
        s.replace(
            "loop_guard = 8\n",
            &format!("loop_guard = 8\nreviewers = {list}\n"),
        )
    });
    let script = rig.home.path().join("bin/review");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    crate::test::write_script(&script, "#!/bin/sh\nexit 0\n");
    let kelpie = std::fs::read_to_string(rig.paths().kelpie_settings).unwrap();
    rig.set_kelpie_settings(&format!(
        "{kelpie}[local_reviewers.mine]\nkind = \"command\"\ncommand = \"{}\"\n\
         [local_reviewers.opus]\nkind = \"claude\"\nmodel = \"claude-opus-5-5\"\n\
         effort = \"high\"\npaths = [\"src/**\"]\n",
        script.display()
    ));
    rig
}

// Steps until the loop leaves review, naming who reviewed each round by the
// stand-in it reached: the local reviewer's command, or the Claude model.
fn reviewers_until_ci(rig: &Rig, runner: &Mutex<Runner>) -> Vec<&'static str> {
    let mut seen = Vec::new();
    for _ in 0..20 {
        if rig.ask(runner, "status", None)["work_item"]["phase"]["state"] != "review" {
            return seen;
        }
        let (local, claude) = (rig.reviewer.seen().len(), reviewer_models(rig).len());
        step(runner).unwrap();
        if let Some(round) = rig.reviewer.seen().get(local) {
            let mine = round.local.paths().is_empty()
                && matches!(&round.local, LocalRound::Command(c) if c.command.ends_with("bin/review"));
            seen.push(if mine { "mine" } else { "qwen" });
        }
        if let Some(model) = reviewer_models(rig).get(claude) {
            seen.push(if model == "claude-opus-5-5" {
                "opus"
            } else {
                "claude"
            });
        }
    }
    panic!("the loop never left review: {seen:?}");
}

fn at_review(rig: &Rig, push: &'static str) -> Mutex<Runner> {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push(push, "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    runner
}

fn reviewer_models(rig: &Rig) -> Vec<String> {
    let calls = rig.claude.all_calls();
    let reviews = calls.iter().filter(|c| c.role == Role::Reviewer);
    reviews.map(|c| c.model.clone()).collect()
}

#[test]
fn no_list_runs_today_s_loop_of_qwen_then_claude() {
    let rig = Rig::new("koji");
    let runner = at_review(&rig, "work.txt");
    rig.claude.script([Scripted::Text("CLEAN")]);
    assert_eq!(reviewers_until_ci(&rig, &runner), ["qwen", "claude"]);
    assert_eq!(rig.reviewer.seen().len(), 1);
    assert_eq!(reviewer_models(&rig), ["claude-sonnet-5"]);
}

#[test]
fn two_clean_rounds_from_different_reviewers_end_the_loop() {
    let rig = listing(r#"["claude", "mine"]"#);
    let runner = at_review(&rig, "work.txt");
    rig.claude.script([Scripted::Text("CLEAN")]);
    assert_eq!(reviewers_until_ci(&rig, &runner), ["claude", "mine"]);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    let seen = rig.reviewer.seen();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].local.lease().is_none());
}

#[test]
fn a_dirty_round_between_two_clean_ones_restarts_the_count() {
    let rig = listing(r#"["mine", "claude"]"#);
    let runner = at_review(&rig, "work.txt");
    rig.reviewer.script([
        ScriptedRound::Findings(Vec::new()),
        ScriptedRound::Findings(Vec::new()),
    ]);
    let holds = r#"{"holds": true, "severity": "high", "reason": "it is"}"#;
    rig.claude.script([
        Scripted::Text("HIGH|work.txt:1|wrong|it is"),
        Scripted::Text(holds),
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text("CLEAN"),
    ]);
    // mine clean, claude holds a finding and the worker fixes it, mine
    // clean, claude clean: the last two are a clean pair from two reviewers.
    assert_eq!(
        reviewers_until_ci(&rig, &runner),
        ["mine", "claude", "mine", "claude"]
    );
}

#[test]
fn one_listed_reviewer_ends_the_loop_on_one_clean_round() {
    let rig = listing(r#"["mine"]"#);
    let runner = at_review(&rig, "work.txt");
    assert_eq!(reviewers_until_ci(&rig, &runner), ["mine"]);
    assert!(reviewer_models(&rig).is_empty(), "no Claude round ran");

    let rig = listing(r#"["claude"]"#);
    let runner = at_review(&rig, "work.txt");
    rig.claude.script([Scripted::Text("CLEAN")]);
    assert_eq!(reviewers_until_ci(&rig, &runner), ["claude"]);
    assert!(rig.reviewer.seen().is_empty(), "no local round ran");
}

// What qwen-review.sh writes when the model behind it cannot be reached.
const UNREACHABLE: &str = "\
LOW|work.txt:0|not reviewed: curl: (7) Failed to connect to gpu.box port 8080|raw response kept at /tmp/qwen-review/raw/work.txt.txt
";

// A rig whose project lists two local reviewers, `mine` then `other`, each a
// command of its own.
fn two_local() -> Rig {
    let rig = listing(r#"["mine", "other"]"#);
    let other = rig.home.path().join("bin/other");
    crate::test::write_script(&other, "#!/bin/sh\nexit 0\n");
    let kelpie = std::fs::read_to_string(rig.paths().kelpie_settings).unwrap();
    rig.set_kelpie_settings(&format!(
        "{kelpie}[local_reviewers.other]\nkind = \"command\"\ncommand = \"{}\"\n",
        other.display()
    ));
    rig
}

// A round that found something real and left `work.txt` unreviewed.
const MIXED: &str = "\
MEDIUM|src/c.rs:4|leftover debug print|noisy logs
LOW|work.txt:0|not reviewed: curl: (7) Failed to connect to gpu.box port 8080|raw response kept at /tmp/qwen-review/raw/work.txt.txt
";

#[test]
fn another_reviewers_first_miss_on_a_file_is_not_a_failure_against_it() {
    let rig = two_local();
    let runner = at_review(&rig, "work.txt");
    let mixed = crate::ports::parse_findings(MIXED);
    rig.reviewer
        .script((0..4).map(|_| ScriptedRound::Findings(mixed.clone())));
    let rejects = r#"{"holds": false, "severity": "low", "reason": "it is a test file"}"#;
    rig.claude.script((0..4).map(|_| Scripted::Text(rejects)));
    // mine, other, mine, other each leave the same file unreviewed, and the
    // judge rejects each round's one finding. No reviewer misses it twice
    // running, so neither is down.
    for _ in 0..4 {
        step(&runner).unwrap(); // the local round
        step(&runner).unwrap(); // the judge rejects its finding
        step(&runner).unwrap(); // the round is not clean
    }
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"].get("local_reviewers_down"), None);
    assert_eq!(rig.reviewer.seen().len(), 4);
}

#[test]
fn a_local_reviewer_that_is_down_leaves_the_other_local_reviewers_to_run() {
    let rig = two_local();
    let runner = at_review(&rig, "work.txt");
    let down = crate::ports::parse_findings(UNREACHABLE);
    rig.reviewer.script([
        ScriptedRound::Findings(down.clone()),
        ScriptedRound::Findings(down),
    ]);
    step(&runner).unwrap(); // mine reviews nothing, and is retried
    step(&runner).unwrap(); // mine reviews nothing again: it is down
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["local_reviewers_down"],
        serde_json::json!(["mine"])
    );

    // `other` is a different command and still runs, and as the only one
    // left, one clean round from it ends the loop with no Claude round.
    step(&runner).unwrap();
    let seen = rig.reviewer.seen();
    let commands: Vec<_> = seen
        .iter()
        .map(|round| match &round.local {
            LocalRound::Command(c) => c
                .command
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            _ => unreachable!("every listed local reviewer here is a command"),
        })
        .collect();
    assert_eq!(commands, ["review", "review", "other"]);
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci"
    );
    assert!(reviewer_models(&rig).is_empty(), "no Claude round ran");
}

#[test]
fn a_reviewer_limited_to_paths_runs_only_where_the_pull_request_changes_them() {
    let rig = listing(r#"["claude", "opus"]"#);
    let runner = at_review(&rig, "docs/notes.md");
    rig.claude.script([Scripted::Text("CLEAN")]);
    assert_eq!(reviewers_until_ci(&rig, &runner), ["claude"]);
    assert_eq!(reviewer_models(&rig), ["claude-sonnet-5"]);

    let rig = listing(r#"["claude", "opus"]"#);
    let runner = at_review(&rig, "src/merge.rs");
    rig.claude
        .script([Scripted::Text("CLEAN"), Scripted::Text("CLEAN")]);
    assert_eq!(reviewers_until_ci(&rig, &runner), ["claude", "opus"]);
    assert_eq!(
        reviewer_models(&rig),
        ["claude-sonnet-5", "claude-opus-5-5"]
    );
    let calls = rig.claude.all_calls();
    let opus = calls.iter().find(|c| c.model == "claude-opus-5-5").unwrap();
    assert_eq!(opus.effort, crate::settings::Effort::High);
}

#[test]
fn every_round_s_prompt_carries_the_issue_s_acceptance_criteria() {
    let rig = listing(r#"["mine", "claude"]"#);
    let runner = at_review(&rig, "work.txt");
    rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
        severity: Severity::Low,
        file: "work.txt".into(),
        line: 1,
        what: "a nit".into(),
        why: "it is".into(),
    }])]);
    rig.claude.script([
        Scripted::Text(r#"{"holds": false, "severity": "low", "reason": "no"}"#),
        Scripted::Text("CLEAN"),
    ]);
    reviewers_until_ci(&rig, &runner);
    assert_eq!(
        rig.reviewer.seen()[0].criteria,
        "#7 Title of #7\n\nBody of #7."
    );
    let calls = rig.claude.all_calls();
    let review = calls.iter().find(|c| c.role == Role::Reviewer).unwrap();
    assert!(
        review.prompt.contains("asks for the following") && review.prompt.contains("Body of #7."),
        "{}",
        review.prompt
    );
}
