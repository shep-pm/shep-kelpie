//! The review over a project's list of reviewers

use std::sync::Mutex;

use crate::ports::{Finding, Role, Severity};
use crate::runner::{Runner, step};
use crate::settings::LocalRound;
use crate::test::{Rig, Scripted, ScriptedRound};

// A rig whose project lists `list`, over agent files for `mine`, a stand-in
// command, and `opus`, a Claude session limited to `src/`.
fn listing(list: &[&str]) -> Rig {
    let rig = Rig::new("koji");
    rig.reviewers(list);
    let script = rig.home.path().join("bin/review");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    crate::test::write_script(&script, "#!/bin/sh\nexit 0\n");
    let mine = format!(
        "---\nrole: reviewer\nharness: command\ncommand: {}\n---\n",
        script.display()
    );
    rig.write_agent("mine", &mine);
    rig.write_agent(
        "opus",
        "---\nrole: reviewer\nharness: claude-code\nmodel: claude-opus-5-5\neffort: high\n\
         paths: [\"src/**\"]\n---\nRead {{DIFF}} for defects.\n",
    );
    rig
}

// Steps until the review is done, naming who reviewed each round by the
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
            let mine =
                matches!(&round.local, LocalRound::Command(c) if c.command.ends_with("bin/review"));
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
    panic!("the review never ended: {seen:?}");
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
fn the_rigs_review_runs_qwen_then_claude() {
    let rig = Rig::new("koji");
    let runner = at_review(&rig, "work.txt");
    rig.claude.script([Scripted::Text("CLEAN")]);
    assert_eq!(reviewers_until_ci(&rig, &runner), ["qwen", "claude"]);
    assert_eq!(rig.reviewer.seen().len(), 1);
    assert_eq!(reviewer_models(&rig), ["claude-sonnet-5"]);
}

#[test]
fn each_listed_reviewer_runs_once_in_the_lists_order() {
    let rig = listing(&["claude", "mine"]);
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
fn a_fix_goes_to_the_next_reviewer_and_no_reviewer_runs_twice() {
    let rig = listing(&["claude", "mine"]);
    let runner = at_review(&rig, "work.txt");
    rig.claude.script([
        Scripted::Text("HIGH|work.txt:1|wrong|it is"),
        Scripted::Push("fixed.txt", "fixed\n"),
    ]);
    rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
        severity: Severity::High,
        file: "fixed.txt".into(),
        line: 1,
        what: "still wrong".into(),
        why: "it is".into(),
    }])]);
    rig.claude
        .script([Scripted::Push("again.txt", "fixed again\n")]);
    // claude finds one, the worker fixes it, mine reads the fix and finds
    // one, the worker fixes that, and the pass is over.
    assert_eq!(reviewers_until_ci(&rig, &runner), ["claude", "mine"]);
    let calls = rig.claude.all_calls();
    let workers = calls.iter().filter(|c| c.role == Role::Worker).count();
    assert_eq!(workers, 3, "the first turn and one fix per reviewer");
}

#[test]
fn one_listed_reviewer_runs_once() {
    let rig = listing(&["mine"]);
    let runner = at_review(&rig, "work.txt");
    assert_eq!(reviewers_until_ci(&rig, &runner), ["mine"]);
    assert!(reviewer_models(&rig).is_empty(), "no Claude round ran");

    let rig = listing(&["claude"]);
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
    let rig = listing(&["mine", "other"]);
    let other = rig.home.path().join("bin/other");
    crate::test::write_script(&other, "#!/bin/sh\nexit 0\n");
    let text = format!(
        "---\nrole: reviewer\nharness: command\ncommand: {}\n---\n",
        other.display()
    );
    rig.write_agent("other", &text);
    rig
}

// A round that found a nit and left `work.txt` unreviewed.
const MIXED: &str = "\
LOW|src/c.rs:4|leftover debug print|noisy logs
LOW|work.txt:0|not reviewed: curl: (7) Failed to connect to gpu.box port 8080|raw response kept at /tmp/qwen-review/raw/work.txt.txt
";

#[test]
fn another_reviewers_first_miss_on_a_file_is_not_a_failure_against_it() {
    let rig = two_local();
    let runner = at_review(&rig, "work.txt");
    let mixed = crate::ports::parse_findings(MIXED);
    rig.reviewer
        .script((0..2).map(|_| ScriptedRound::Findings(mixed.clone())));
    // mine, then other, each leave the same file unreviewed with one nit.
    // Neither missed it before, so neither has a failure against it.
    reviewers_until_ci(&rig, &runner);
    let failures = runner.lock().unwrap().state.work_items[0]
        .local_failures
        .clone();
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(rig.reviewer.seen().len(), 2);
}

#[test]
fn a_local_round_that_reviews_nothing_goes_on_to_the_next_local_reviewer() {
    let rig = two_local();
    let runner = at_review(&rig, "work.txt");
    let down = crate::ports::parse_findings(UNREACHABLE);
    rig.reviewer.script([ScriptedRound::Findings(down)]);
    step(&runner).unwrap(); // mine reviews nothing

    // `other` is the next in the list, and the last.
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
    assert_eq!(commands, ["review", "other"]);
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci"
    );
    assert!(reviewer_models(&rig).is_empty(), "no Claude round ran");
}

#[test]
fn a_reviewer_limited_to_paths_runs_only_where_the_pull_request_changes_them() {
    let rig = listing(&["claude", "opus"]);
    let runner = at_review(&rig, "docs/notes.md");
    rig.claude.script([Scripted::Text("CLEAN")]);
    assert_eq!(reviewers_until_ci(&rig, &runner), ["claude"]);
    assert_eq!(reviewer_models(&rig), ["claude-sonnet-5"]);

    let rig = listing(&["claude", "opus"]);
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
    let rig = listing(&["mine", "claude"]);
    let runner = at_review(&rig, "work.txt");
    rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
        severity: Severity::Low,
        file: "work.txt".into(),
        line: 1,
        what: "a nit".into(),
        why: "it is".into(),
    }])]);
    rig.claude.script([Scripted::Text("CLEAN")]);
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

// The pass so far ran `mine`; the list then changes before the next round.
fn after_mine_the_list_becomes(to: &[&str]) -> Vec<&'static str> {
    let rig = listing(&["mine", "claude"]);
    let runner = at_review(&rig, "work.txt");
    step(&runner).unwrap(); // round 1, mine: clean by default
    drop(runner);
    rig.edit_settings(|s| {
        s.replace(
            r#"reviewers = ["mine", "claude"]"#,
            &format!("reviewers = {to:?}"),
        )
    });
    let runner = rig.open().unwrap();
    rig.claude.script([Scripted::Text("CLEAN")]);
    let after = reviewers_until_ci(&rig, &runner);
    assert_eq!(rig.reviewer.seen().len(), 1, "mine ran once");
    after
}

#[test]
fn a_reviewer_taken_off_the_list_mid_pass_leaves_the_rest_to_run() {
    assert_eq!(after_mine_the_list_becomes(&["claude"]), ["claude"]);
}

#[test]
fn a_list_reordered_mid_pass_runs_what_the_pass_has_not() {
    assert_eq!(after_mine_the_list_becomes(&["claude", "mine"]), ["claude"]);
}
