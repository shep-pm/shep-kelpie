//! The planning call's worker picks landing as `worker:` labels, on a
//! forge that refuses a label its repo lacks, and every fallback's comment

use super::*;
use crate::board::READY;

// A repo with none of the picks' labels, whose forge refuses an issue any
// label it lacks, as GitHub does.
fn strict(rig: &Rig) {
    rig.forge.set_repo_labels(&[READY]);
    rig.forge.set_strict_labels(true);
}

const MEDIUM_THEN_OPUS: &str = r#"{"split": true, "why": "Two slices.", "pieces": [
    {"title": "Store the thing", "body": "Build the store.", "worker": "sonnet-medium"},
    {"title": "Show the thing", "body": "Build the screen.", "blocked_by": [1],
     "worker": "opus-high"}]}"#;

const SONNET_TWICE: &str = r#"{"split": true, "why": "Two slices.", "pieces": [
    {"title": "Store the thing", "body": "Build the store.", "worker": "sonnet-high"},
    {"title": "Show the thing", "body": "Build the screen.", "blocked_by": [1],
     "worker": "sonnet-high"}]}"#;

const SONNET_THEN_NONE: &str = r#"{"split": true, "why": "Two slices.", "pieces": [
    {"title": "Store the thing", "body": "Build the store.", "worker": "sonnet-high"},
    {"title": "Show the thing", "body": "Build the screen.", "blocked_by": [1]}]}"#;

#[test]
fn a_whole_issue_s_pick_lands_on_a_repo_that_lacks_its_label() {
    let (rig, runner) = planning("lapras");
    strict(&rig);
    rig.forge.list_ready(5, false);
    rig.claude.script([Scripted::Text(
        r#"{"split": false, "why": "x", "worker": "opus-high"}"#,
    )]);
    let Some(StepReport::Planned {
        outcome: PlanOutcome::Whole { worker, .. },
        ..
    }) = step(&runner).unwrap()
    else {
        panic!("expected a whole plan")
    };
    assert_eq!(
        worker,
        WholeWorker::Picked {
            label: "worker:opus-high".into()
        }
    );
    assert_eq!(rig.forge.issue_labels(5), [READY, "worker:opus-high"]);
    assert_eq!(rig.forge.repo_labels_now(), [READY, "worker:opus-high"]);
    assert!(rig.forge.comments().is_empty());
}

#[test]
fn each_piece_of_a_split_gets_its_own_pick_on_a_repo_that_lacks_them() {
    let (rig, runner) = auto_planning("dratini");
    strict(&rig);
    rig.forge.list_ready(5, false);
    rig.claude.script([Scripted::Text(MEDIUM_THEN_OPUS)]);
    step(&runner).unwrap();
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Split { issue: 5, .. })
    ));
    assert_eq!(rig.forge.issue_labels(900), [READY, "worker:sonnet-medium"]);
    assert_eq!(rig.forge.issue_labels(901), [READY, "worker:opus-high"]);
    assert_eq!(
        rig.forge.repo_labels_now(),
        [READY, "worker:sonnet-medium", "worker:opus-high"]
    );
    assert_eq!(rig.forge.label_reads(), 1);
    let [(_, comment)] = rig.forge.comments().try_into().unwrap();
    assert!(!comment.contains("defaulted"), "{comment}");
    assert!(!comment.contains("not confirmed"), "{comment}");
}

#[test]
fn a_pick_two_pieces_name_is_made_on_the_repo_once() {
    let (rig, runner) = auto_planning("seadra");
    strict(&rig);
    rig.forge.list_ready(5, false);
    rig.claude.script([Scripted::Text(SONNET_TWICE)]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    assert_eq!(rig.forge.label_creates(), ["worker:sonnet-high"]);
    assert_eq!(rig.forge.label_reads(), 1);
    assert_eq!(rig.forge.issue_labels(900), [READY, "worker:sonnet-high"]);
    assert_eq!(rig.forge.issue_labels(901), [READY, "worker:sonnet-high"]);
}

#[test]
fn a_pick_whose_label_cannot_be_made_defaults_and_the_split_comment_says_why() {
    let (rig, runner) = auto_planning("seel");
    strict(&rig);
    rig.forge.set_label_creates_down(true);
    rig.forge.list_ready(5, false);
    rig.claude.script([Scripted::Text(SONNET_THEN_NONE)]);
    step(&runner).unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Split {
            issue: 5,
            sub_issues: vec![900, 901],
            comment_failed: None,
        })
    );
    assert_eq!(rig.forge.issue_labels(900), [READY]);
    let [(_, comment)] = rig.forge.comments().try_into().unwrap();
    assert!(
        comment.contains(
            "- #900: Store the thing, worker defaulted to the project's: could not make label \
             `worker:sonnet-high`: gh failed: HTTP 403: Resource not accessible by integration\n"
        ),
        "{comment}"
    );
    assert!(
        comment.contains(
            "- #901: Show the thing, after #900, worker defaulted to the project's: the plan \
             named no worker"
        ),
        "{comment}"
    );
}

#[test]
fn a_whole_issue_s_pick_outside_the_three_defaults_and_its_comment_says_so() {
    let (rig, runner) = planning("horsea");
    rig.forge.list_ready(5, false);
    rig.claude.script([Scripted::Text(
        r#"{"split": false, "why": "x", "worker": "opus-max"}"#,
    )]);
    let reason = "`opus-max` is not one of the planner's picks (sonnet-high, sonnet-medium, \
                  opus-high)";
    let Some(StepReport::Planned {
        outcome: PlanOutcome::Whole { worker, .. },
        ..
    }) = step(&runner).unwrap()
    else {
        panic!("expected a whole plan")
    };
    assert_eq!(
        worker,
        WholeWorker::Defaulted {
            reason: reason.into(),
            comment_failed: None,
        }
    );
    assert_eq!(rig.forge.issue_labels(5), [READY]);
    let [(on, comment)] = rig.forge.comments().try_into().unwrap();
    assert_eq!(on, 5);
    assert_eq!(
        comment,
        format!(
            "Kelpie kept this issue whole, and it runs on the project's default worker, \
             `claude-sonnet-5-5` at high effort: {reason}."
        )
    );
}

#[test]
fn a_no_on_the_split_says_the_issue_runs_whole_on_the_project_s_worker() {
    let (rig, runner) = planning("goldeen");
    let id = asked(&rig, &runner);
    rig.ask(&runner, "rule", Some(&format!("{id} no keep it whole")));
    let [(on, comment)] = rig.forge.comments().try_into().unwrap();
    assert_eq!(on, 5);
    assert_eq!(
        comment,
        "Kelpie works this issue whole, as the maintainer answered, on the project's default \
         worker, `claude-sonnet-5-5` at high effort."
    );
}

#[test]
fn a_no_on_the_split_of_an_issue_with_a_worker_label_names_that_label() {
    let (rig, runner) = planning("seaking");
    rig.forge.label(5, "worker:opus-high");
    let id = asked(&rig, &runner);
    rig.ask(&runner, "rule", Some(&format!("{id} no keep it whole")));
    let [(_, comment)] = rig.forge.comments().try_into().unwrap();
    assert_eq!(
        comment,
        "Kelpie works this issue whole, as the maintainer answered, on its `worker:opus-high` \
         label."
    );
}

#[test]
fn a_sub_issue_the_forge_cannot_read_back_reads_as_a_failure_in_the_split_comment() {
    let (rig, runner) = auto_planning("staryu");
    rig.forge.list_ready(5, false);
    rig.forge.remove_issue(901);
    let piece = |title: &str| Piece {
        title: title.into(),
        body: "Build it.".into(),
        blocked_by: Vec::new(),
        worker: None,
    };
    let pieces = [piece("Store the thing"), piece("Show the thing")];
    let report = runner
        .lock()
        .unwrap()
        .finish_split(5, "Two slices.", &pieces, vec![900, 901], &[])
        .unwrap();
    assert_eq!(
        report,
        StepReport::Split {
            issue: 5,
            sub_issues: vec![900, 901],
            comment_failed: None,
        }
    );
    let [(_, comment)] = rig.forge.comments().try_into().unwrap();
    assert!(
        comment.contains(
            "- #901: Show the thing, worker not confirmed: could not read #901's labels: gh \
             failed: no issue #901\n"
        ),
        "{comment}"
    );
    assert!(!comment.contains("- #901: Show the thing, worker defaulted"));
}
