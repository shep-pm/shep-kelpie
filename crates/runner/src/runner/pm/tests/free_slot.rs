//! A free slot goes to the board rule when the project manager gives no pick,
//! and the board waits on a pick wake only until its call ends

use serde_json::json;

use super::{NOTHING, answered, dispatched, pm_on, pm_seen, step_until, with_pm};
use crate::ports::{AgentError, Role};
use crate::runner::flight::advance;
use crate::runner::{BOARD_POLL, Pass, StepReport, step};
use crate::settings::Harness;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted, ticket};

const FREE_SLOT: &str = "a slot is free and 2 ready issues could fill it: pick one";

#[test]
fn a_pick_of_null_with_nothing_open_starts_the_rule_issue_at_once() {
    let (rig, runner) = with_pm("ditto");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude.script([Scripted::Say(
        "{\"pick\": null, \"hold\": [12], \"why\": \"no preference.\"}",
    )]);

    let (woke_for, acted, dropped) = answered(step(&runner).unwrap());

    assert_eq!(woke_for, [FREE_SLOT]);
    assert_eq!((acted, dropped), (vec!["holds #12".to_owned()], vec![]));
    assert_eq!(dispatched(step(&runner).unwrap()), ticket(9));
    assert_eq!(pm_seen(&rig).len(), 1, "no second wake for the same slot");
}

#[test]
fn a_pick_of_null_with_an_item_open_starts_the_rule_issue_at_once() {
    let rig = Rig::new("ditto");
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    let (rig, runner) = pm_on(rig);
    rig.ask(&runner, "add", Some("7"));
    let work = Hold::default();
    rig.claude
        .script([Scripted::Hold(work.clone()), Scripted::Say(NOTHING)]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(work.entered(CALL_PATIENCE), "#7's turn never began");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.clock.advance(BOARD_POLL.as_secs());

    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (woke_for, acted, dropped) = answered(Some(report));
    assert_eq!(woke_for, [FREE_SLOT]);
    assert_eq!((acted, dropped), (vec![], vec![]));
    let report = step_until(&runner, |r| matches!(r, StepReport::Dispatched { .. }));
    assert_eq!(dispatched(Some(report)), ticket(9));

    work.release();
    assert!(work.answered(CALL_PATIENCE), "#7's turn never answered");
}

#[test]
fn a_pick_of_null_covers_every_free_slot_of_its_wake_with_one_call() {
    let rig = Rig::new("ditto");
    rig.edit_settings(|s| {
        let s = s.replace("active_items = 1", "active_items = 3");
        s.replace("pending_rulings = 2", "pending_rulings = 3")
    });
    let (rig, runner) = pm_on(rig);
    for issue in [9, 12, 14] {
        rig.forge.list_ready(issue, false);
    }
    // A second answer is scripted so a second call would show.
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(NOTHING), Scripted::Say(NOTHING)]);

    let (woke_for, _, _) = answered(step(&runner).unwrap());
    assert_eq!(
        woke_for,
        [
            "3 slots are free and 3 ready issues could fill them: pick up to 3 as a list, in the order to start them"
        ]
    );
    for issue in [9, 12, 14] {
        let report = step_until(&runner, |r| matches!(r, StepReport::Dispatched { .. }));
        assert_eq!(dispatched(Some(report)), ticket(issue));
    }
    assert_eq!(pm_seen(&rig).len(), 1, "one call filled all three slots");
}

#[test]
fn a_null_covers_only_the_issues_its_wake_offered_so_later_ones_wake_it_again() {
    let rig = Rig::new("ditto");
    rig.edit_settings(|s| {
        let s = s.replace("active_items = 1", "active_items = 3");
        s.replace("pending_rulings = 2", "pending_rulings = 3")
    });
    let (rig, runner) = pm_on(rig);
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(NOTHING), Scripted::Say(NOTHING)]);
    let (woke_for, _, _) = answered(step(&runner).unwrap());
    assert_eq!(
        woke_for,
        [
            "3 slots are free and 2 ready issues could fill them: pick up to 2 as a list, in the order to start them"
        ]
    );
    for issue in [9, 12] {
        let report = step_until(&runner, |r| matches!(r, StepReport::Dispatched { .. }));
        assert_eq!(dispatched(Some(report)), ticket(issue));
    }

    // The two items parked, which gave their slots up, and two issues turn ready.
    rig.forge.list_ready(14, false);
    rig.forge.list_ready(15, false);
    rig.clock.advance(BOARD_POLL.as_secs());
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (woke_for, _, _) = answered(Some(report));
    let offered = "3 slots are free and 2 ready issues could fill them";
    assert!(
        woke_for.iter().any(|w| w.starts_with(offered)),
        "{woke_for:?}"
    );
}

#[test]
fn a_pick_wake_due_behind_another_call_goes_when_the_runner_finishes() {
    let (rig, runner) = with_pm("ditto");
    rig.ask(&runner, "tell", Some("first"));
    let call = Hold::default();
    rig.claude.script_for(
        Role::Pm,
        [
            Scripted::HoldThenSay(call.clone(), NOTHING),
            Scripted::Say(NOTHING),
        ],
    );
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(call.entered(CALL_PATIENCE), "the tell's call never began");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.clock.advance(BOARD_POLL.as_secs());
    assert!(matches!(advance(&runner).unwrap(), Pass::Idle));

    rig.ask(&runner, "finish", None);
    call.release();
    step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    for _ in 0..3 {
        step(&runner).unwrap();
    }
    assert_eq!(pm_seen(&rig).len(), 1, "the pick wake went with the finish");
}

#[test]
fn a_wake_shows_no_text_about_starting_nothing() {
    let (rig, runner) = with_pm("ditto");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude.script([Scripted::Say(NOTHING)]);
    answered(step(&runner).unwrap());

    let [wake] = pm_seen(&rig).try_into().unwrap();
    let prompt = wake.call.prompt;
    assert!(prompt.contains(FREE_SLOT), "{prompt}");
    assert!(!prompt.contains("start nothing"), "{prompt}");
    assert!(!prompt.contains("starts nothing"), "{prompt}");
}

#[test]
fn the_shipped_project_manager_file_never_says_to_start_nothing() {
    let shipped = crate::agents::baseline::named("pm").unwrap().text;
    assert!(!shipped.contains("start nothing"), "{shipped}");
}

// A call that fails, as one ended at `CEILING` does, gives the wake up through
// `give_up`, so the rule picks and the wait ends with the call.
#[test]
fn a_pick_wake_holds_the_board_until_its_call_ends_and_then_the_rule_picks() {
    let (rig, runner) = with_pm("ditto");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    let call = Hold::default();
    let ceiling = AgentError::TimedOut(Harness::ClaudeCode);
    rig.claude
        .script([Scripted::HoldThenFail(call.clone(), ceiling)]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(call.entered(CALL_PATIENCE), "the wake never began");

    assert!(matches!(advance(&runner).unwrap(), Pass::Idle));
    let skipped = rig.ask(&runner, "status", None)["skipped"].clone();
    assert_eq!(skipped, json!([{ "reason": "pm-waits", "issue": "9" }]));
    let board = std::fs::read_to_string(rig.paths().board).unwrap();
    assert!(
        board.contains("the project manager's pick waits"),
        "{board}"
    );

    call.release();
    let reason = "claude ran past its turn's ceiling".to_owned();
    let failed = step_until(&runner, |r| matches!(r, StepReport::PmFailed { .. }));
    assert_eq!(failed, StepReport::PmFailed { reason });
    rig.clock.advance(BOARD_POLL.as_secs());
    let report = step_until(&runner, |r| matches!(r, StepReport::Dispatched { .. }));
    assert_eq!(dispatched(Some(report)), ticket(9));
    assert_eq!(pm_seen(&rig).len(), 1, "it is not asked again while down");
}
