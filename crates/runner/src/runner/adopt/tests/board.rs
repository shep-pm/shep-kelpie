//! The agent board's pick for an adopted pull request, and the pacer and the dog it waits on

use serde_json::json;

use super::*;

#[test]
fn an_adopted_pull_request_opens_on_the_agent_boards_pick() {
    let rig = Rig::new("shep");
    opened_80(&rig);
    rig.agent_board.ranks(&["opus-high", "sonnet-high"]);
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));

    assert_eq!(step(&runner).unwrap(), Some(adopted_80_on("opus-high")));
    let [ask] = rig.agent_board.asks().try_into().unwrap();
    assert_eq!(ask.pin, None);
}

#[test]
fn a_waiting_pull_request_pinned_to_an_implementer_is_held_by_its_account() {
    let rig = Rig::new("shep");
    rig.ci_on();
    opened_80(&rig);
    rig.write_codex_implementer();
    rig.forge.label(5, "agent:codex!");
    rig.codex_day_began(0);
    // 15% of Codex's week since the day began, past its allowance of 100 / 7.
    rig.codex_meter.set(Rig::utilization(15, 0));
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));

    let Some(StepReport::Held { kind, reason, .. }) = step(&runner).unwrap() else {
        panic!("expected a hold");
    };
    assert_eq!(kind, crate::pacer::HoldKind::Allowance);
    assert!(reason.starts_with("today's Codex allowance"), "{reason}");
    assert_eq!(rig.codex_meter.reads(), 1);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["adopted"], waiting(&[80], false), "still queued");
    assert_eq!(status["work_item"], json!(null));
    assert_eq!(status["rulings"], json!([]), "a hold is no `adopt-failed`");
    assert_eq!(status["skipped"], json!([]));
}

#[test]
fn an_adoption_whose_top_pick_has_spent_its_day_opens_on_the_next_the_pacer_lets_start() {
    let rig = Rig::new("shep");
    rig.ci_on();
    opened_80(&rig);
    rig.write_codex_implementer();
    rig.agent_board.ranks(&["codex", "sonnet-high"]);
    rig.codex_day_began(0);
    // 15% of Codex's week since the day began, past its allowance of 100 / 7.
    rig.codex_meter.set(Rig::utilization(15, 0));
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));

    assert_eq!(step(&runner).unwrap(), Some(adopted_80()));
}

#[test]
fn a_pull_request_held_by_its_labelled_account_leaves_the_ones_behind_it_to_start() {
    let rig = Rig::new("shep");
    rig.ci_on();
    opened_80(&rig);
    rig.push_by_hand("fix/other", "other.txt");
    rig.forge.open_pull_request(81, "fix/other", &[6]);
    rig.write_codex_implementer();
    rig.forge.label(5, "agent:codex!");
    rig.agent_board.ranks(&["sonnet-high", "codex"]);
    rig.codex_day_began(0);
    rig.codex_meter.set(Rig::utilization(15, 0));
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));
    rig.ask(&runner, "adopt", Some("81"));

    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Adopted {
            pull_request: 81,
            ..
        })
    ));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["adopted"], waiting(&[80], false), "80 stays queued");
}

#[test]
fn an_unreachable_board_adopts_nothing_asks_once_and_the_next_poll_that_reaches_it_adopts() {
    let rig = Rig::new("shep");
    opened_80(&rig);
    rig.push_by_hand("fix/other", "other.txt");
    rig.forge.open_pull_request(81, "fix/other", &[6]);
    rig.agent_board.unreachable();
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));
    rig.ask(&runner, "adopt", Some("81"));

    assert_eq!(
        step(&runner).unwrap(),
        None,
        "the poll is idle, as dispatch's is"
    );

    assert_eq!(rig.agent_board.asks().len(), 1, "the dog is asked once");
    assert_eq!(rig.ask(&runner, "status", None)["skipped"], json!([]));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["adopted"],
        waiting(&[80, 81], false),
        "both stay queued"
    );
    assert_eq!(status["work_item"], json!(null));
    assert_eq!(status["rulings"], json!([]));

    rig.agent_board.ranks_first(&["sonnet-high"]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), Some(adopted_80()));
}
