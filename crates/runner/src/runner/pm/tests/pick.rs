//! A project manager's pick through a board that changes before dispatch
//! takes it

use std::sync::Mutex;

use serde_json::{Value, json};

use super::{PICK_12, answered, pm_on, pm_seen, step_until, with_pm};
use crate::agents::Tier;
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted, ticket};

// A project with #9 and #12 ready, whose project manager picked #12 and has
// not been stepped past it.
fn picked_12() -> (Rig, Mutex<Runner>) {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude.script([Scripted::Say(PICK_12)]);
    let (_, acted, _) = answered(step(&runner).unwrap());
    assert_eq!(acted, ["picked #12"]);
    (rig, runner)
}

fn dispatched_in_order<const N: usize>(runner: &Mutex<Runner>, issues: [u64; N]) {
    for issue in issues {
        let report = step_until(runner, |r| matches!(r, StepReport::Dispatched { .. }));
        assert!(matches!(report, StepReport::Dispatched { issue: i, .. } if i == ticket(issue)));
    }
}

fn dispatched_12(runner: &Mutex<Runner>) {
    let report = step_until(runner, |r| matches!(r, StepReport::Dispatched { .. }));
    assert!(matches!(report, StepReport::Dispatched { issue, .. } if issue == ticket(12)));
}

// The steps logged in the middle of a pass that name a dropped pick.
fn pick_dropped_steps(runner: &Mutex<Runner>) -> Vec<Value> {
    let started = runner.lock().unwrap().take_started();
    let all = started.iter().map(|s| serde_json::to_value(s).unwrap());
    all.filter(|s| s["step"] == "pick-dropped").collect()
}

#[test]
fn a_pick_survives_an_unrelated_item_opening_and_is_dispatched() {
    let (rig, runner) = picked_12();

    rig.ask(&runner, "add", Some("7"));

    dispatched_12(&runner);
    assert_eq!(pick_dropped_steps(&runner), Vec::<Value>::new());
}

#[test]
fn a_pick_survives_an_unrelated_item_closing_and_is_dispatched() {
    let (rig, runner) = with_pm("koji");
    rig.ask(&runner, "add", Some("7"));
    step_until(&runner, |r| matches!(r, StepReport::Failed { .. }));
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude.script([Scripted::Say(PICK_12)]);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    assert!(matches!(report, StepReport::PmAnswered { acted, .. } if acted == ["picked #12"]));

    rig.ask(&runner, "drop", Some("7"));

    dispatched_12(&runner);
    assert_eq!(pick_dropped_steps(&runner), Vec::<Value>::new());
}

#[test]
fn a_pick_the_board_can_no_longer_take_leaves_a_step_and_is_told_to_the_next_wake() {
    let (rig, runner) = picked_12();

    // The maintainer opens the picked issue before dispatch takes it.
    rig.ask(&runner, "add", Some("12"));
    step_until(&runner, |r| matches!(r, StepReport::Dispatched { .. }));
    // The worker's turn has no scripted reply, which the project manager's
    // next wake must not take for its own.
    step_until(
        &runner,
        |r| matches!(r, StepReport::Failed { issue, .. } if *issue == ticket(9)),
    );

    assert_eq!(
        pick_dropped_steps(&runner),
        [json!({
            "step": "pick-dropped",
            "issue": "12",
            "why": "it is not an issue the board can take now",
        })]
    );

    rig.claude.script([Scripted::Say(PICK_12)]);
    rig.ask(&runner, "tell", Some("anything"));
    step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let wakes = pm_seen(&rig);
    let prompt = &wakes.last().unwrap().call.prompt;
    assert!(
        prompt.contains(
            "- kelpie dropped your pick #12 and the board rule picked: it is not an issue \
             the board can take now"
        ),
        "{prompt}"
    );
}

// A project with three slots and #9, #12 and #14 ready.
fn three_slots() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("koji");
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 3"));
    let (rig, runner) = pm_on(rig);
    for n in [9, 12, 14] {
        rig.forge.list_ready(n, false);
    }
    (rig, runner)
}

const PICKS_14_AND_12: &str =
    "```json\n{\"pick\": [14, 12], \"why\": \"Both are clear of each other.\"}\n```";

#[test]
fn one_wake_tells_the_free_slots_and_its_list_of_picks_fills_them_in_order() {
    let (rig, runner) = three_slots();
    rig.claude.script([Scripted::Say(PICKS_14_AND_12)]);

    let (woke_for, acted, dropped) = answered(step(&runner).unwrap());

    assert_eq!(
        woke_for,
        [
            "3 slots are free and 3 ready issues could fill them: pick up to 3 as a list, in \
             the order to start them"
        ]
    );
    assert_eq!(
        (acted, dropped),
        (vec!["picked #14, #12".to_owned()], vec![])
    );
    dispatched_in_order(&runner, [14, 12]);
    assert_eq!(pm_seen(&rig).len(), 1, "one wake filled both slots");
}

#[test]
fn a_pick_in_the_list_the_board_can_no_longer_take_is_dropped_and_the_next_goes() {
    let (rig, runner) = three_slots();
    rig.claude.script([Scripted::Say(PICKS_14_AND_12)]);
    answered(step(&runner).unwrap());

    // The maintainer opens #14 before dispatch takes it.
    rig.ask(&runner, "add", Some("14"));

    let report = step_until(
        &runner,
        |r| matches!(r, StepReport::Dispatched { issue, .. } if *issue == ticket(12)),
    );
    assert!(matches!(report, StepReport::Dispatched { .. }));
    assert_eq!(pick_dropped_steps(&runner).len(), 1);
}

#[test]
fn a_list_longer_than_the_free_slots_drops_the_rest() {
    let (rig, runner) = three_slots();
    rig.forge.list_ready(15, false);
    rig.claude.script([Scripted::Say(
        "```json\n{\"pick\": [14, 12, 9, 15], \"why\": \"All of them.\"}\n```",
    )]);

    let (_, acted, dropped) = answered(step(&runner).unwrap());

    assert_eq!(acted, ["picked #14, #12, #9"]);
    assert_eq!(dropped, ["pick #15: only 3 can start from this wake"]);
}

#[test]
fn a_picks_agent_pins_its_work_item_over_the_issues_pinned_label_and_its_tier_and_slack_are_kept() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.forge.label(12, "agent:sonnet-high!");
    rig.forge.label(12, "kind:docs");
    rig.claude.script([Scripted::Say(
        "```json\n{\"pick\": {\"issue\": 12, \"agent\": \"opus-high\", \"tier\": \"heavy\", \
         \"slack\": 0}, \"why\": \"#12 is hard to undo.\"}\n```",
    )]);
    let (_, acted, dropped) = answered(step(&runner).unwrap());
    assert_eq!((acted, dropped), (vec!["picked #12".to_owned()], vec![]));

    let report = step_until(&runner, |r| matches!(r, StepReport::Dispatched { .. }));

    assert!(
        matches!(&report, StepReport::Dispatched { issue, agent, .. }
            if *issue == ticket(12) && agent.as_str() == "opus-high"),
        "{report:?}"
    );
    let held = runner.lock().unwrap();
    let item = held.state.item(ticket(12)).unwrap();
    assert_eq!(
        (item.pinned, item.tier, item.slack, item.kind.as_deref()),
        (true, Some(Tier::Heavy), Some(0), Some("docs"))
    );
    assert!(
        rig.forge
            .labels_of(12)
            .contains(&"agent:sonnet-high!".to_owned()),
        "the issue's label is left as it was"
    );
    let asks = rig.agent_board.asks();
    let ask = asks.last().unwrap();
    assert_eq!(
        (ask.pin.as_ref().map(|p| p.as_str()), ask.tier, ask.slack),
        (Some("opus-high"), Some(Tier::Heavy), Some(0))
    );
}

#[test]
fn issues_no_agent_can_start_never_wake_the_project_manager_again() {
    let (rig, runner) = picked_12();
    rig.agent_board.ranks(&[]);

    assert_eq!(step(&runner).unwrap(), None);
    let dropped = pick_dropped_steps(&runner);
    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert_eq!(dropped[0]["issue"], "12");
    let calls = rig.claude.calls().len();
    for _ in 0..3 {
        rig.next_look();
        assert_eq!(step(&runner).unwrap(), None);
    }

    assert_eq!(rig.claude.calls().len(), calls, "no wake for the same two");
    rig.agent_board.ranks_listed();
    rig.next_look();
    let report = step(&runner).unwrap();
    assert!(
        matches!(report, Some(StepReport::Dispatched { .. })),
        "{report:?}"
    );
}

// A project manager's project with #12 ready and pinned to `codex`, whose 5-hour window
// holds it, after the pass that found no agent could start #12
fn codex_holds_12() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("koji");
    rig.write_codex_implementer();
    let (rig, runner) = pm_on(rig);
    rig.codex_meter.set(Rig::utilization(0, 55));
    rig.forge.list_ready(12, false);
    rig.forge.label(12, "agent:codex!");
    step(&runner).unwrap();
    assert!(pm_seen(&rig).is_empty(), "one issue wakes no pick");
    (rig, runner)
}

const PICK_9: &str = "```json\n{\"pick\": 9, \"why\": \"w\"}\n```";

#[test]
fn a_pick_wake_counts_only_the_issues_an_agent_could_start() {
    let (rig, runner) = codex_holds_12();
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(14, false);
    rig.claude.script([Scripted::Say(PICK_9)]);
    rig.next_look();

    let (woke_for, _, _) = answered(step(&runner).unwrap());

    assert_eq!(
        woke_for,
        ["a slot is free and 2 ready issues could fill it: pick one"]
    );
}

#[test]
fn an_issue_whose_pacer_hold_lapsed_counts_toward_a_pick_wake_again() {
    let (rig, runner) = codex_holds_12();
    rig.clock.advance(5 * 3600);
    rig.codex_meter.set(Rig::utilization(0, 0));
    rig.forge.list_ready(9, false);
    rig.claude.script([Scripted::Say(PICK_9)]);
    rig.next_look();

    let (woke_for, _, _) = answered(step(&runner).unwrap());

    assert_eq!(
        woke_for,
        ["a slot is free and 2 ready issues could fill it: pick one"]
    );
}

#[test]
fn a_pick_of_an_agent_that_is_no_implementer_file_is_dropped() {
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude.script([Scripted::Say(
        "```json\n{\"pick\": [{\"issue\": 12, \"agent\": \"pm\"}, {\"issue\": 9, \"agent\": \
         \"nobody\"}], \"why\": \"w\"}\n```",
    )]);

    let (_, acted, dropped) = answered(step(&runner).unwrap());

    assert_eq!(acted, Vec::<String>::new());
    assert_eq!(
        dropped,
        [
            "pick #12: `pm` is no implementer agent file `agents.allow` holds",
            "pick #9: `nobody` is no implementer agent file `agents.allow` holds",
        ]
    );
}

#[test]
fn a_picks_agent_that_allow_drops_before_dispatch_opens_on_the_boards_pick_keeping_tier_and_slack()
{
    let (rig, runner) = with_pm("koji");
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.forge.label(12, "agent:sonnet-high");
    rig.claude.script([Scripted::Say(
        "```json\n{\"pick\": {\"issue\": 12, \"agent\": \"opus-high\", \"tier\": \"light\", \
         \"slack\": 45}, \"why\": \"w\"}\n```",
    )]);
    let (_, acted, _) = answered(step(&runner).unwrap());
    assert_eq!(acted, ["picked #12"]);
    rig.edit_settings(|s| {
        let listed = "\nimplementers = [\"sonnet-high\"]\n";
        assert!(s.contains(listed), "the example's implementers moved");
        let allow = "\nallow = [\"sonnet-high\", \"pm\", \"qwen\", \"claude\"]";
        s.replace(listed, &format!("{allow}{listed}"))
    });
    let (settings, kelpie) = (rig.settings(), rig.kelpie_settings());
    runner.lock().unwrap().reread(settings, kelpie).unwrap();
    runner.lock().unwrap().take_notes();

    let report = step_until(&runner, |r| matches!(r, StepReport::Dispatched { .. }));

    assert!(
        matches!(&report, StepReport::Dispatched { issue, agent, .. }
            if *issue == ticket(12) && agent.as_str() == "sonnet-high"),
        "{report:?}"
    );
    let mut held = runner.lock().unwrap();
    let item = held.state.item(ticket(12)).unwrap();
    assert_eq!(
        (item.pinned, item.tier, item.slack),
        (false, Some(Tier::Light), Some(45))
    );
    let notes = held.take_notes();
    assert!(
        notes.iter().any(|n| n
            == "#12: the project manager picked it for opus-high, which is no longer an \
                implementer `agents.allow` holds, so the agent board picks"),
        "{notes:?}"
    );
}
