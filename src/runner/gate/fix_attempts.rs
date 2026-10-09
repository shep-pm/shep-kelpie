//! `ci.fix_attempts`: how many fix turns red runs get before the maintainer decides

use serde_json::json;

use super::tests::ruling_report;
use crate::ports::Checks;
use crate::runner::{StepReport, step};
use crate::test::{Rig, Scripted};

// A red run of `lint` at `head`, as the gate's next verdict on it.
fn red(rig: &Rig, runner: &std::sync::Mutex<crate::runner::Runner>, head: &str) -> StepReport {
    rig.forge
        .set_checks(head, Checks::Failed(vec!["lint".into()]));
    rig.verdict(runner).expect("the gate read the red run")
}

// The worker's fix turn pushes a new head, which goes back to CI.
fn fixed(
    rig: &Rig,
    runner: &std::sync::Mutex<crate::runner::Runner>,
    file: &'static str,
) -> String {
    rig.claude.script([Scripted::Push(file, "fixed\n")]);
    step(runner).unwrap();
    rig.forge.head_of("kelpie/7").unwrap()
}

fn sent_back(report: &StepReport, head: &str) -> bool {
    matches!(report, StepReport::CiFailed { head: h, .. } if h == head)
}

#[test]
fn fix_attempts_0_raises_the_ruling_on_the_first_red_run_with_no_fix_turn() {
    let (rig, runner, head) = Rig::with_pull_request_set("koji", |rig| rig.fix_attempts(0));
    let calls = rig.claude.calls().len();
    let (id, question) = ruling_report(Some(red(&rig, &runner, &head)));
    assert_eq!(
        question,
        format!(
            "CI failed on pull request #71 at {}: lint. The worker has had no fix turn on red \
             runs, and the cap `ci.fix_attempts` sets is reached. `shep kelpie rule {id} yes` \
             has kelpie look again, and `shep kelpie rule {id} no <note>` sends the worker \
             your note.",
            &head[..7]
        )
    );
    assert_eq!(rig.claude.calls().len(), calls, "no fix turn went out");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["rulings"][0]["kind"],
        json!({
            "kind": "stuck",
            "reason": "still-red",
            "head": head,
            "checks": ["lint"],
            "fix_turns": 0,
        })
    );
    assert_eq!(
        rig.forge.comments(),
        [(
            71,
            format!(
                "CI failed at {}: lint. The worker has had no fix turn on red runs.\n\n\
                 Waiting on the maintainer.",
                &head[..7]
            )
        )]
    );
}

#[test]
fn a_cap_reached_raises_the_ruling_after_that_many_fix_turns() {
    let (rig, runner, head) = Rig::with_pull_request_set("koji", |rig| rig.fix_attempts(2));
    assert!(sent_back(&red(&rig, &runner, &head), &head));
    let second = fixed(&rig, &runner, "fix-1.txt");
    assert!(sent_back(&red(&rig, &runner, &second), &second));
    let third = fixed(&rig, &runner, "fix-2.txt");

    let (_, question) = ruling_report(Some(red(&rig, &runner, &third)));
    let parked = format!(
        "CI failed on pull request #71 at {}: lint. The worker has had 2 fix turns on red \
         runs, and the cap `ci.fix_attempts` sets is reached.",
        &third[..7]
    );
    assert!(question.starts_with(&parked), "{question}");

    // A no sends the worker the maintainer's note, past the cap.
    rig.ask(&runner, "rule", Some("1 no try the other lint config"));
    rig.claude.script([Scripted::Push("fix-3.txt", "fixed\n")]);
    step(&runner).unwrap();
    let prompt = rig.claude.calls().last().unwrap().prompt.clone();
    assert!(prompt.contains("try the other lint config"), "{prompt}");
}

#[test]
fn fix_attempts_minus_1_sends_every_red_run_on_a_new_head_back_to_the_worker() {
    let (rig, runner, mut head) = Rig::with_pull_request("koji");
    for (turn, file) in ["fix-0.txt", "fix-1.txt", "fix-2.txt", "fix-3.txt"]
        .into_iter()
        .enumerate()
    {
        assert!(sent_back(&red(&rig, &runner, &head), &head), "{turn}");
        head = fixed(&rig, &runner, file);
    }
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn a_head_red_twice_with_nothing_pushed_still_raises_the_ruling_under_a_cap() {
    let (rig, runner, head) = Rig::with_pull_request_set("koji", |rig| rig.fix_attempts(5));
    assert!(sent_back(&red(&rig, &runner, &head), &head));
    rig.claude.script([Scripted::Text("I could not find it.")]);
    step(&runner).unwrap();
    let (_, question) = ruling_report(rig.verdict(&runner));
    assert!(
        question.contains("and the worker pushed no fix: lint."),
        "{question}"
    );
}
