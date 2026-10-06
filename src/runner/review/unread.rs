//! A pass no reviewer read: marked unreviewed when one was down or failing,
//! which the merge ruling names and `auto` will not merge, and only noted
//! when the project's own list gave it nobody to read it

use std::sync::Mutex;

use serde_json::json;

use crate::ports::{AgentError, Checks};
use crate::runner::{Runner, StepReport, step};
use crate::settings::Harness;
use crate::test::{Rig, Scripted};

// Pull request 71 for issue 7 is open, its worker's first turn done, on a
// project whose review is `defect-hunter` alone, or `listed`.
fn at_review(rig: &Rig, listed: &[&str]) -> Mutex<Runner> {
    rig.reviewers(listed);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    runner
}

// Steps until the review is over, and returns what the steps reported.
fn through_review(rig: &Rig, runner: &Mutex<Runner>) -> Vec<StepReport> {
    let mut reports = Vec::new();
    for _ in 0..20 {
        if rig.ask(runner, "status", None)["work_item"]["phase"]["state"] != "review" {
            return reports;
        }
        reports.extend(step(runner).unwrap());
    }
    panic!("the review never ended: {reports:#?}");
}

const DOWN: &str = "defect-hunter was passed over after its calls kept failing";

// `defect-hunter`'s every call fails, as through an API outage.
fn an_outage(rig: &Rig) {
    let outage = || Scripted::Fail(AgentError::TimedOut(Harness::ClaudeCode));
    rig.claude.script([outage(), outage(), outage()]);
}

fn green(rig: &Rig) {
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
}

#[test]
fn a_pass_whose_only_reviewer_kept_failing_marks_the_work_item_unreviewed() {
    let rig = Rig::new("koji");
    let runner = at_review(&rig, &["defect-hunter"]);
    an_outage(&rig);
    let reports = through_review(&rig, &runner);
    assert_eq!(
        reports.last(),
        Some(&StepReport::Unreviewed {
            issue: 7,
            pull_request: 71,
            reason: DOWN.into(),
        })
    );
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(item["unreviewed"], DOWN);
    assert_eq!(item["phase"]["state"], "ci");

    green(&rig);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(&runner) else {
        panic!("no merge ruling");
    };
    assert!(
        question.contains(&format!("No reviewer read it in its last review: {DOWN}.")),
        "{question}"
    );
}

#[test]
fn under_auto_a_pull_request_no_reviewer_read_raises_the_merge_ruling_instead_of_merging() {
    let rig = Rig::new("koji");
    rig.merge_auto();
    let runner = at_review(&rig, &["defect-hunter"]);
    an_outage(&rig);
    through_review(&rig, &runner);
    green(&rig);
    let report = rig.verdict(&runner);
    assert!(
        matches!(&report, Some(StepReport::Ruling { question, .. }) if question.contains(DOWN)),
        "{report:?}"
    );
    assert!(rig.forge.merges().is_empty(), "nothing merged");
}

#[test]
fn a_list_that_gives_nobody_to_read_it_is_noted_and_not_marked() {
    for listed in [&[][..], &["opus"][..]] {
        let rig = Rig::new("koji");
        rig.merge_auto();
        rig.write_agent(
            "opus",
            "---\nrole: reviewer\nharness: claude-code\nmodel: claude-opus-5-5\n\
             effort: high\npaths: [\"src/**\"]\n---\nRead {{DIFF}}.\n",
        );
        let runner = at_review(&rig, listed);
        let reports = through_review(&rig, &runner);
        assert!(
            !reports
                .iter()
                .any(|r| matches!(r, StepReport::Unreviewed { .. })),
            "{listed:?}: {reports:#?}"
        );
        let notes = runner.lock().unwrap().take_notes();
        assert!(
            notes
                .iter()
                .any(|n| n.contains("goes to CI with no reviewer's read")),
            "{listed:?}: {notes:?}"
        );
        let item = &rig.ask(&runner, "status", None)["work_item"];
        assert_eq!(item.get("unreviewed"), None, "{listed:?}");
        green(&rig);
        for _ in 0..10 {
            rig.verdict(&runner);
        }
        assert_eq!(rig.forge.merges().len(), 1, "{listed:?}: auto merges it");
    }
}

#[test]
fn a_later_pass_a_reviewer_reads_clears_the_mark() {
    let rig = Rig::new("koji");
    let runner = at_review(&rig, &["qwen", "claude"]);
    drop(runner);
    let state = rig.paths().state;
    let mut saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    saved["work_items"][0]["unreviewed"] = json!("qwen is down for this work item");
    std::fs::write(&state, saved.to_string()).unwrap();
    let runner = rig.open().unwrap();
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(item["unreviewed"], "qwen is down for this work item");

    step(&runner).unwrap(); // round 1, qwen: clean by default, a read
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(item.get("unreviewed"), None, "{item}");
}

#[test]
fn a_pass_whose_local_reviewer_reviewed_no_file_is_marked_unreviewed() {
    let rig = Rig::new("koji");
    let runner = at_review(&rig, &["qwen"]);
    let unreachable = "LOW|work.txt:0|not reviewed: curl: (7) Failed to connect|raw kept\n";
    let down = crate::ports::parse_findings(unreachable);
    rig.reviewer
        .script([crate::test::ScriptedRound::Findings(down)]);
    let reports = through_review(&rig, &runner);
    assert_eq!(
        reports.last(),
        Some(&StepReport::Unreviewed {
            issue: 7,
            pull_request: 71,
            reason: "qwen reviewed no file".into(),
        })
    );
}
