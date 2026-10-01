use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::ports::Cost;
use crate::runner::{StepReport, step};
use crate::test::{Rig, Scripted};

const WHOLE: &str = r#"{"split": false, "why": "One small change."}"#;

const TWO: &str = r#"{"split": true, "why": "Two slices.", "pieces": [
    {"title": "Store the thing", "body": "Build the store."},
    {"title": "Show the thing", "body": "Build the screen.", "blocked_by": [1]}]}"#;

fn planning(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    rig.planning_on();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    (rig, runner)
}

fn planner_calls(rig: &Rig) -> Vec<AgentCall> {
    let calls = rig.claude.all_calls().into_iter();
    calls.filter(|c| c.role == Role::Planner).collect()
}

// Plans issue 5 under `ask`, and returns the ruling's id.
fn asked(rig: &Rig, runner: &Mutex<Runner>) -> u64 {
    rig.forge.list_ready(5, false);
    rig.claude.script([Scripted::Text(TWO)]);
    let Some(StepReport::Planned {
        outcome: PlanOutcome::Asked { ruling, .. },
        ..
    }) = step(runner).unwrap()
    else {
        panic!("the split was not asked");
    };
    ruling
}

#[test]
fn a_small_issue_is_planned_whole_and_then_worked() {
    let (rig, runner) = planning("rotom");
    rig.forge.list_ready(5, false);
    rig.claude
        .script([Scripted::Billed(WHOLE, Cost(2_000_000_000))]);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Planned {
            issue: 5,
            outcome: PlanOutcome::Whole {
                why: "One small change.".into()
            },
            usage: Usage::default(),
            cost_usd: Some(2.0),
        })
    );
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));

    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched { issue: 5, .. })
    ));
    let [plan] = planner_calls(&rig).try_into().unwrap();
    assert!(
        plan.prompt.starts_with("/mattpocock:to-tickets "),
        "{}",
        plan.prompt
    );
    assert!(plan.prompt.contains("Plan issue #5: "));
    assert_eq!(plan.model, "claude-opus-5-5");
    assert!(rig.forge.created().is_empty());
}

#[test]
fn the_planning_call_reads_main_and_may_change_nothing() {
    let (rig, runner) = planning("golbat");
    rig.land("docs/notes.md", "on main\n");
    rig.forge.list_ready(5, false);
    rig.claude.script([Scripted::Text(WHOLE)]);
    step(&runner).unwrap();

    let seen = rig.claude.all_seen();
    let plan = seen.iter().find(|s| s.call.role == Role::Planner).unwrap();
    assert_eq!(
        plan.call.session,
        Session::New(plan.call.session.id().clone())
    );
    let deny = plan.settings["permissions"]["deny"].as_array().unwrap();
    for tool in ["Bash", "Agent", "Task"] {
        assert!(deny.contains(&json!(tool)), "{tool}");
    }
    for tool in ["Read", "Grep", "Glob"] {
        assert!(!deny.contains(&json!(tool)), "{tool}");
    }
    // The detached worktree it read is gone once it answers.
    assert!(!plan.call.cwd.exists());
    assert!(!rig.paths().worktree(5).exists());
}

#[test]
fn a_big_issue_under_auto_becomes_sub_issues_with_blockers_and_one_comment() {
    let rig = Rig::new("eevee");
    rig.planning_on();
    rig.merge_auto();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.forge.list_ready(5, false);
    rig.forge.label(5, "priority: P1");
    rig.claude.script([Scripted::Text(TWO)]);

    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Planned {
            issue: 5,
            outcome: PlanOutcome::Split { pieces: 2 },
            ..
        })
    ));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Split {
            issue: 5,
            sub_issues: vec![900, 901],
            comment_failed: None,
        })
    );
    let created = rig.forge.created();
    let titles: Vec<&str> = created.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, ["Store the thing", "Show the thing"]);
    assert!(
        created
            .iter()
            .all(|c| c.labels == ["ready-for-agent", "priority: P1"])
    );
    assert_eq!(rig.forge.sub_issues_of(5), [900, 901]);
    assert_eq!(rig.forge.blockers(900), Vec::<u64>::new());
    assert_eq!(rig.forge.blockers(901), [900]);
    let [(on, comment)] = rig.forge.comments().try_into().unwrap();
    assert_eq!(on, 5);
    assert!(comment.contains("- #900: Store the thing\n- #901: Show the thing, after #900"));

    // The frontier is worked first, and the split issue never is.
    let Some(StepReport::Dispatched { issue, skipped, .. }) = step(&runner).unwrap() else {
        panic!("nothing was dispatched");
    };
    assert_eq!(issue, 900);
    assert_eq!(skipped, [Skip::Split { issue: 5, open: 2 }]);
    assert_eq!(planner_calls(&rig).len(), 1);
}

#[test]
fn a_big_issue_under_ask_waits_on_a_ruling_and_a_yes_splits_it() {
    let (rig, runner) = planning("zeus");
    let id = asked(&rig, &runner);
    let status = rig.ask(&runner, "status", None);
    let question = status["rulings"][0]["question"].as_str().unwrap();
    assert!(question.starts_with("Planning would split issue #5 into 2 pull requests."));
    assert!(question.contains("1. Store the thing\n2. Show the thing (after 1)"));
    assert!(question.contains(&format!("`shep kelpie rule {id} answer <note>`")));
    assert!(rig.forge.created().is_empty());

    // Nothing else on the board, so once the ruling is out the project waits on it.
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id }));
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        rig.ask(&runner, "status", None)["skipped"],
        json!([{ "reason": "planning", "issue": 5, "ruling": id }])
    );

    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Split { issue: 5, .. })
    ));
    assert_eq!(rig.forge.sub_issues_of(5), [900, 901]);
    assert_eq!(planner_calls(&rig).len(), 1);
}

#[test]
fn a_no_on_the_split_works_the_issue_whole() {
    let (rig, runner) = planning("ditto");
    let id = asked(&rig, &runner);
    rig.ask(
        &runner,
        "rule",
        Some(&format!("{id} no one pull request is fine")),
    );
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched { issue: 5, .. })
    ));
    assert_eq!(planner_calls(&rig).len(), 1);
    assert!(rig.forge.created().is_empty());
}

#[test]
fn an_answer_on_the_split_plans_again_with_the_note() {
    let (rig, runner) = planning("koji");
    let id = asked(&rig, &runner);
    rig.ask(
        &runner,
        "rule",
        Some(&format!("{id} answer keep the store and screen together")),
    );
    rig.claude.script([Scripted::Text(WHOLE)]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Planned {
            outcome: PlanOutcome::Whole { .. },
            ..
        })
    ));
    let [_, again] = planner_calls(&rig).try_into().unwrap();
    assert!(
        again
            .prompt
            .contains("with this note:\n\nkeep the store and screen together\n")
    );
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched { issue: 5, .. })
    ));
}

#[test]
fn the_parent_closes_when_its_last_sub_issue_does() {
    let (rig, runner) = planning("xilriws");
    rig.forge.list_ready(5, false);
    rig.forge.list_ready(6, false);
    rig.forge.link_sub_issue(5, 6);
    rig.forge.link_sub_issue(5, 7);
    rig.forge.close_issue(7);
    rig.forge.label(6, "worker:haiku-low");
    // A sub-issue is not planned again.
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched { issue: 6, .. })
    ));
    assert!(planner_calls(&rig).is_empty());
    assert!(rig.forge.closings().is_empty());

    rig.forge.close_issue(6);
    rig.ask(&runner, "drop", Some("6"));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ParentClosed { issue: 5 })
    );
    let [(closed, why)] = rig.forge.closings().try_into().unwrap();
    assert_eq!((closed, why.as_str()), (5, PARENT_CLOSED));
    assert_eq!(step(&runner).unwrap(), None);
}

#[test]
fn a_parent_with_sub_issues_is_never_dispatched_even_with_planning_off() {
    let rig = Rig::new("chelone");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.forge.list_ready(5, false);
    rig.forge.list_ready(8, false);
    rig.forge.link_sub_issue(5, 8);
    rig.forge.block(8, 40);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        rig.ask(&runner, "status", None)["skipped"],
        json!([
            { "reason": "split", "issue": 5, "open": 1 },
            { "reason": "blocked", "issue": 8, "by": [40] },
        ])
    );
}

#[test]
fn a_reply_that_is_not_a_plan_works_the_issue_whole() {
    let (rig, runner) = planning("reactmap");
    rig.forge.list_ready(5, false);
    rig.claude.script([Scripted::Text("It looks fine as one.")]);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Planned {
            issue: 5,
            outcome: PlanOutcome::Failed {
                reason: "no JSON object".into()
            },
            usage: Usage::default(),
            cost_usd: Some(0.0),
        })
    );
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched { issue: 5, .. })
    ));
}

#[test]
fn a_split_the_forge_cuts_short_carries_on_without_opening_a_piece_twice() {
    let rig = Rig::new("rotom");
    rig.planning_on();
    rig.merge_auto();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.forge.list_ready(5, false);
    rig.claude.script([Scripted::Text(TWO)]);
    step(&runner).unwrap();
    rig.forge.set_creates_left(1);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::SplitFailed {
            issue: 5,
            reason: "cannot open piece 2: gh failed: issues are down".into(),
            ruling: None,
        })
    );
    assert_eq!(rig.forge.sub_issues_of(5), [900]);

    // A restart carries on from the saved state.
    drop(runner);
    let runner = rig.open().unwrap();
    rig.forge.set_creates_left(5);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Split { issue: 5, .. })
    ));
    assert_eq!(rig.forge.created().len(), 2);
    assert_eq!(rig.forge.sub_issues_of(5), [900, 901]);
    assert_eq!(rig.forge.blockers(901), [900]);
}

fn auto_planning(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    rig.planning_on();
    rig.merge_auto();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    (rig, runner)
}

#[test]
fn a_split_the_forge_keeps_refusing_waits_on_a_ruling_and_the_board_goes_on() {
    let (rig, runner) = auto_planning("golbat");
    rig.forge.list_ready(5, false);
    rig.forge.list_ready(6, false);
    rig.forge.set_links_down(true);
    rig.claude.script([Scripted::Text(TWO)]);
    step(&runner).unwrap();
    let refused = |ruling| StepReport::SplitFailed {
        issue: 5,
        reason: "cannot make #900 a sub-issue: gh failed: sub-issues are not enabled".into(),
        ruling,
    };
    assert_eq!(step(&runner).unwrap(), Some(refused(None)));
    assert_eq!(step(&runner).unwrap(), Some(refused(None)));
    assert_eq!(step(&runner).unwrap(), Some(refused(Some(1))));
    let status = rig.ask(&runner, "status", None);
    let question = status["rulings"][0]["question"].as_str().unwrap();
    assert!(
        question.starts_with("Splitting issue #5 keeps failing: "),
        "{question}"
    );
    assert!(question.contains("It opened #900 so far."), "{question}");

    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    rig.claude.script([Scripted::Text(WHOLE)]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Planned { issue: 6, .. })
    ));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched { issue: 6, .. })
    ));
    assert_eq!(rig.forge.created().len(), 1);

    // A yes tries again from where it stopped.
    rig.ask(&runner, "drop", Some("6"));
    rig.forge.set_links_down(false);
    rig.ask(&runner, "rule", Some("1 yes"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Split { issue: 5, .. })
    ));
    assert_eq!(rig.forge.sub_issues_of(5), [900, 901]);
}

#[test]
fn a_link_that_landed_but_lost_its_answer_is_not_asked_for_again() {
    let (rig, runner) = auto_planning("ditto");
    rig.forge.list_ready(5, false);
    rig.claude.script([Scripted::Text(TWO)]);
    step(&runner).unwrap();
    rig.forge.lose_next_link_answer();
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::SplitFailed { ruling: None, .. })
    ));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Split {
            issue: 5,
            sub_issues: vec![900, 901],
            comment_failed: None,
        })
    );
    assert_eq!(rig.forge.sub_issues_of(5), [900, 901]);
    assert_eq!(rig.forge.blockers(901), [900]);
}

#[test]
fn a_yes_on_a_split_whose_issue_was_closed_meanwhile_opens_nothing() {
    let (rig, runner) = planning("eevee");
    let id = asked(&rig, &runner);
    rig.forge.close_issue(5);
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::SplitDropped {
            issue: 5,
            reason: "#5 was closed".into()
        })
    );
    assert!(rig.forge.created().is_empty());
}

#[test]
fn a_parent_the_forge_will_not_close_waits_on_a_ruling_and_the_board_goes_on() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.forge.list_ready(5, false);
    rig.forge.list_ready(6, false);
    rig.forge.link_sub_issue(5, 8);
    rig.forge.close_issue(8);
    rig.forge.set_closes_down(true);
    let refused = |ruling| StepReport::ParentCloseFailed {
        issue: 5,
        reason: "gh failed: closing is refused".into(),
        ruling,
    };
    assert_eq!(step(&runner).unwrap(), Some(refused(None)));
    assert_eq!(step(&runner).unwrap(), Some(refused(None)));
    assert_eq!(step(&runner).unwrap(), Some(refused(Some(1))));
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched { issue: 6, .. })
    ));

    rig.forge.set_closes_down(false);
    rig.ask(&runner, "rule", Some("1 yes"));
    rig.ask(&runner, "drop", Some("6"));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ParentClosed { issue: 5 })
    );
}

#[test]
fn a_planning_call_that_times_out_is_tried_once_more_in_a_fresh_session() {
    let (rig, runner) = planning("zeus");
    rig.forge.list_ready(5, false);
    rig.claude
        .script([Scripted::Fail(AgentError::TimedOut), Scripted::Text(WHOLE)]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Planned {
            outcome: PlanOutcome::Whole { .. },
            ..
        })
    ));
    let [first, again] = planner_calls(&rig).try_into().unwrap();
    assert_ne!(first.session, again.session);
}
