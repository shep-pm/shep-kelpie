//! A pass that ends with some reads but fewer than `reviews.need`: marked
//! unreviewed when a reviewer was down or passed over, so `auto` will not
//! merge it, and left alone when the project's own list gave it too few

use std::sync::Mutex;

use crate::lease::LeaseKind;
use crate::ports::{AgentError, CLEAN, Checks, Timestamp, parse_findings};
use crate::runner::{Runner, StepReport, step};
use crate::settings::Harness;
use crate::test::ticket;
use crate::test::{Rig, Scripted, ScriptedRound};

const SHORT: &str = "1 of 2 reviewers read it: cubic was passed over: its window is closed";

// Pull request 71 for issue 7 is open, its worker's first turn done, on a
// project whose review is `listed`, needing `need` reads, and whose `cubic`
// window is closed.
fn at_review(rig: &Rig, listed: &[&str], need: usize) -> Mutex<Runner> {
    rig.reviewers(listed);
    rig.reviews_need(Some(need));
    rig.allow(&[&["sonnet-high"][..], listed].concat());
    rig.leases.opens_at(
        &LeaseKind::try_from("cubic").unwrap(),
        Timestamp(Rig::EPOCH + 600),
    );
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

fn green(rig: &Rig) {
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
}

fn unreviewed(reports: &[StepReport]) -> bool {
    (reports.iter()).any(|r| matches!(r, StepReport::Unreviewed { .. }))
}

#[test]
fn a_pass_with_one_read_of_two_needed_is_marked_unreviewed() {
    let rig = Rig::new("shep");
    let runner = at_review(&rig, &["qwen", "cubic"], 2);
    let reports = through_review(&rig, &runner);
    assert_eq!(
        reports.last(),
        Some(&StepReport::Unreviewed {
            issue: ticket(7),
            pull_request: 71,
            reason: SHORT.into(),
        })
    );
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(item["unreviewed"], SHORT);
    assert_eq!(item["phase"]["state"], "ci");

    green(&rig);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(&runner) else {
        panic!("no merge ruling");
    };
    assert!(
        question.contains(&format!("No reviewer read it in its last review: {SHORT}.")),
        "{question}"
    );
}

#[test]
fn under_auto_a_short_pass_raises_the_merge_ruling_instead_of_merging() {
    let rig = Rig::new("shep");
    rig.merge_auto();
    let runner = at_review(&rig, &["qwen", "cubic"], 2);
    through_review(&rig, &runner);
    green(&rig);
    let report = rig.verdict(&runner);
    assert!(
        matches!(&report, Some(StepReport::Ruling { question, .. }) if question.contains(SHORT)),
        "{report:?}"
    );
    assert!(rig.forge.merges().is_empty(), "nothing merged");
}

#[test]
fn a_pass_that_reaches_need_is_not_marked() {
    let rig = Rig::new("shep");
    rig.merge_auto();
    let runner = at_review(&rig, &["qwen", "cubic"], 1);
    let reports = through_review(&rig, &runner);
    assert!(!unreviewed(&reports), "{reports:#?}");
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(item.get("unreviewed"), None);
}

const OPUS: &str = "---\nrole: reviewer\nharness: claude-code\nmodel: claude-opus-5-5\n\
                    effort: high\npaths: [\"src/**\"]\n---\nRead {{DIFF}}.\n";

#[test]
fn a_list_that_leaves_the_pass_short_is_noted_and_not_marked() {
    for (listed, noted) in [(&["opus"][..], true), (&["qwen", "opus"][..], false)] {
        let rig = Rig::new("koji");
        rig.merge_auto();
        rig.write_agent("opus", OPUS);
        let runner = at_review(&rig, listed, 2);
        let reports = through_review(&rig, &runner);
        assert!(!unreviewed(&reports), "{listed:?}: {reports:#?}");
        let notes = runner.lock().unwrap().take_notes();
        let none_read = |n: &String| n.contains("goes to CI with no reviewer's read");
        assert_eq!(notes.iter().any(none_read), noted, "{listed:?}: {notes:?}");
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
fn a_local_round_that_reviewed_no_file_does_not_count_as_a_read() {
    let rig = Rig::new("koji");
    let runner = at_review(&rig, &["qwen", "claude"], 2);
    let unreachable = "LOW|work.txt:0|not reviewed: curl: (7) Failed to connect|raw kept\n";
    rig.reviewer
        .script([ScriptedRound::Findings(parse_findings(unreachable))]);
    rig.claude.script([Scripted::Text(CLEAN)]);
    let reports = through_review(&rig, &runner);
    assert_eq!(
        reports.last(),
        Some(&StepReport::Unreviewed {
            issue: ticket(7),
            pull_request: 71,
            reason: "1 of 2 reviewers read it: qwen reviewed no file".into(),
        })
    );
}

#[test]
fn a_reviewer_passed_over_in_one_pass_counts_when_it_reads_the_next() {
    let rig = Rig::new("koji");
    let runner = at_review(&rig, &["qwen", "claude"], 2);
    let outage = || Scripted::Fail(AgentError::TimedOut(Harness::ClaudeCode));
    rig.claude.script([outage(), outage(), outage()]);
    let reports = through_review(&rig, &runner);
    assert!(unreviewed(&reports), "claude kept failing: {reports:#?}");

    green(&rig);
    let Some(StepReport::Ruling { id, .. }) = rig.verdict(&runner) else {
        panic!("no merge ruling");
    };
    rig.ask(&runner, "rule", Some(&format!("{id} rework fix it")));
    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
    step(&runner).unwrap(); // the noted turn: pushes, and a pass begins
    rig.claude.script([Scripted::Text(CLEAN)]);
    let reports = through_review(&rig, &runner);
    assert!(
        !unreviewed(&reports),
        "both read the second pass: {reports:#?}"
    );
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(item.get("unreviewed"), None);
}
