//! A project that lists the `spec` reviewer, through the runner and its stand-ins

use crate::ports::{CLEAN, Finding, Role, Severity};
use crate::runner::step;
use crate::test::{Rig, Scripted, ScriptedRound};

#[test]
fn a_listed_spec_reviewer_gets_the_issue_s_criteria_after_its_body() {
    let rig = Rig::new("shep");
    rig.reviewers(&["spec"]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    rig.claude.script([Scripted::Text(CLEAN)]);
    step(&runner).unwrap(); // round 1, spec: clean

    let seen = rig.claude.all_seen();
    let reviews: Vec<_> = seen
        .iter()
        .filter(|s| s.call.role == Role::Reviewer)
        .collect();
    let [spec] = reviews.as_slice() else {
        panic!("one spec read: {reviews:#?}");
    };
    let prompt = &spec.call.prompt;
    let (body, criteria) = prompt
        .split_once("\n--- end ---\n\n")
        .expect("the criteria follow the body's last line");
    assert!(body.starts_with("You are checking"), "{prompt}");
    assert!(
        criteria.starts_with("The issue this pull request resolves asks for the following."),
        "{prompt}"
    );
    assert!(
        criteria.ends_with("#7 Title of #7\n\nBody of #7."),
        "{prompt}"
    );
}

#[test]
fn a_spec_reviewer_in_round_two_is_told_what_round_one_sent_the_worker() {
    let asked = Finding::new(
        Severity::Medium,
        "src/lib.rs",
        3,
        "the flag is read first",
        "a caller sees nothing",
    );
    let rig = Rig::new("shep");
    rig.reviewers(&["qwen", "spec"]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
    rig.reviewer.script([ScriptedRound::Findings(vec![asked])]);
    step(&runner).unwrap(); // round 1, qwen: one MEDIUM finding
    step(&runner).unwrap(); // the finding goes to the worker
    rig.claude.script([Scripted::Push("fixed.txt", "fixed\n")]);
    step(&runner).unwrap(); // the fix turn pushes
    step(&runner).unwrap(); // the fix turn ends: round 2 is next
    rig.claude.script([Scripted::Text(CLEAN)]);
    step(&runner).unwrap(); // round 2, spec: clean

    let prompt = rig.claude.all_calls().pop().unwrap().prompt;

    assert!(prompt.starts_with("You are checking"), "{prompt}");
    assert!(
        prompt.ends_with(
            "--- the findings earlier rounds sent ---\n\
             MEDIUM|src/lib.rs:3|the flag is read first|a caller sees nothing\n--- end ---"
        ),
        "{prompt}"
    );
    assert!(prompt.contains("counts as asked for"), "{prompt}");
}
