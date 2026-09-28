//! The rework a pull request asks for itself, and the triage labels that say whose turn it is

use serde_json::json;

use super::*;
use crate::board::READY;
use crate::runner::report::ReworkBy;
use crate::runner::rework::HUMAN;

fn requesting_changes() -> MaintainerReview {
    MaintainerReview {
        id: "PRR_changes".into(),
        changes_requested: true,
        ..review()
    }
}

#[test]
fn ready_for_agent_on_a_pull_request_kelpie_opened_starts_its_rework_and_comes_off() {
    let rig = Rig::new("hazels-lab");
    reviewed_71(&rig);
    rig.forge.label_pull_request(71, READY);
    rig.forge.label_pull_request(71, HUMAN);
    rig.forge.label_pull_request(71, "design");
    let runner = running(&rig);
    let Some(StepReport::Reworked {
        issue: 7,
        pull_request: 71,
        by: ReworkBy::Label,
        ..
    }) = step(&runner).unwrap()
    else {
        panic!("the label started no rework");
    };
    assert_eq!(rig.forge.pull_request_labels(71), ["design"]);
    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
    step(&runner).unwrap();
    let [seen] = rig.claude.seen().try_into().unwrap();
    assert!(
        seen.call
            .prompt
            .starts_with("Your work item reworks your pull request #71")
    );
}

#[test]
fn a_review_requesting_changes_starts_a_rework_once() {
    let rig = Rig::new("reactmap");
    reviewed_71(&rig);
    rig.forge.review(71, requesting_changes());
    let runner = running(&rig);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Reworked {
            by: ReworkBy::Review,
            ..
        })
    ));
    rig.ask(&runner, "drop", None);
    assert_eq!(step(&runner).unwrap(), None, "that review asked once");

    let newer = MaintainerReview {
        id: "PRR_newer".into(),
        ..requesting_changes()
    };
    rig.forge.review(71, newer);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Reworked { .. })
    ));
}

#[test]
fn a_pull_request_the_forge_cannot_show_holds_up_none_of_the_others() {
    let rig = Rig::new("golbat");
    rig.forge.open_pull_request(70, "kelpie/6", &[6]);
    rig.forge.set_unreadable(70);
    reviewed_71(&rig);
    rig.forge.label_pull_request(71, READY);
    let runner = running(&rig);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Reworked {
            pull_request: 71,
            ..
        })
    ));
}

#[test]
fn a_forks_pull_request_on_a_kelpie_branch_is_left_alone() {
    let rig = Rig::new("chelone");
    reviewed_71(&rig);
    rig.forge.review(71, requesting_changes());
    rig.forge.label_pull_request(71, READY);
    rig.forge.set_from_fork(71);
    let runner = running(&rig);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.forge.comments(), []);
    assert_eq!(rig.forge.pull_request_labels(71), [READY]);
}

#[test]
fn a_label_that_will_not_come_off_starts_nothing_until_it_does() {
    let rig = Rig::new("zeus");
    reviewed_71(&rig);
    rig.forge.label_pull_request(71, READY);
    rig.forge.set_labels_down(true);
    let runner = running(&rig);
    let Some(StepReport::BoardFailed { reason }) = step(&runner).unwrap() else {
        panic!("a rework started with its label stuck on");
    };
    assert_eq!(
        reason,
        "cannot take the `ready-for-agent` label off #71: gh failed: labels are down"
    );
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    assert_eq!(
        rig.ask(&runner, "rework", Some("71")),
        json!({ "error": reason })
    );
    assert_eq!(
        rig.ask(&runner, "drop", None)["error"],
        "no work item is in flight"
    );

    rig.forge.set_labels_down(false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Reworked { .. })
    ));
    rig.forge.set_labels_down(true);
    let reply = rig.ask(&runner, "drop", None);
    assert!(
        reply["error"]
            .as_str()
            .unwrap()
            .starts_with("cannot hand #71 back: "),
        "{reply}"
    );
    assert_eq!(rig.ask(&runner, "status", None)["work_item"]["issue"], 7);
}

#[test]
fn a_manual_rework_uses_up_the_review_it_took() {
    let rig = Rig::new("golbat");
    reviewed_71(&rig);
    rig.forge.review(71, requesting_changes());
    let runner = running(&rig);
    rig.ask(&runner, "rework", Some("71"));
    rig.ask(&runner, "drop", None);
    assert_eq!(step(&runner).unwrap(), None);
}

#[test]
fn a_labelled_pull_request_with_nothing_to_rework_is_told_so_once() {
    let rig = Rig::new("chelone");
    reviewed_71(&rig);
    rig.forge.review(
        71,
        MaintainerReview {
            body: String::new(),
            comments: vec![],
            ..review()
        },
    );
    rig.forge.label_pull_request(71, READY);
    let runner = running(&rig);
    let reason = "nothing to rework: the latest review of #71 has no body \
                  and no unresolved comment";
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReworkRefused {
            pull_request: 71,
            reason: reason.into(),
            comment_failed: None,
        })
    );
    assert_eq!(
        rig.forge.comments(),
        [(
            71,
            format!("Kelpie cannot rework this pull request: {reason}.")
        )]
    );
    assert_eq!(rig.forge.pull_request_labels(71), Vec::<String>::new());
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.forge.comments().len(), 1);
}

#[test]
fn a_second_ask_while_a_rework_is_in_flight_changes_nothing() {
    let rig = Rig::new("koji");
    reviewed_71(&rig);
    rig.forge.label_pull_request(71, READY);
    let runner = running(&rig);
    step(&runner).unwrap(); // the rework starts
    rig.claude.script([
        Scripted::Push("fix.txt", "fixed\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // its turn
    rig.forge.label_pull_request(71, READY);
    rig.forge.review(71, requesting_changes());
    step(&runner).unwrap(); // review round 1
    step(&runner).unwrap(); // review round 2
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(&runner) else {
        panic!("the rework never reached its merge ruling");
    };
    assert!(!question.contains("label"), "{question}");
    assert_eq!(rig.claude.seen().len(), 1, "no second rework started");
}

#[test]
fn ready_for_human_goes_on_at_the_merge_ruling_and_comes_off_with_a_no() {
    let rig = Rig::new("zeus");
    reviewed_71(&rig);
    let runner = running(&rig);
    rig.ask(&runner, "rework", Some("71"));
    rig.claude.script([
        Scripted::Push("fix.txt", "fixed\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // its turn
    rig.forge.label_pull_request(71, READY);
    step(&runner).unwrap(); // review round 1
    step(&runner).unwrap(); // review round 2
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    assert_eq!(rig.forge.pull_request_labels(71), [HUMAN]);

    let status = rig.ask(&runner, "rule", Some("1 no move the dates up"));
    assert_eq!(status["work_item"]["turn"]["state"], "next");
    assert_eq!(rig.forge.pull_request_labels(71), Vec::<String>::new());
}

#[test]
fn a_dropped_pull_request_is_handed_back_ready_for_human() {
    let rig = Rig::new("rotom");
    reviewed_71(&rig);
    rig.forge.label_pull_request(71, READY);
    let runner = running(&rig);
    step(&runner).unwrap(); // the rework starts, taking the label off
    rig.forge.label_pull_request(71, READY);
    rig.ask(&runner, "drop", None);
    assert_eq!(rig.forge.pull_request_labels(71), [HUMAN]);
    assert_eq!(step(&runner).unwrap(), None, "nothing asks for a rework");
}
