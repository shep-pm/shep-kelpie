//! Several work items open at once, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use crate::board::Skip;
use crate::lease::LeaseKind;
use crate::ports::{CLEAN, Checks, Cost, Usage};
use crate::runner::coderabbit::HEARD_WAIT;
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted, Told, ticket};
use crate::ticket::TicketId;

// A running project that may hold two work items open, with issues 7 and 8
// ready to open pull requests 71 and 81
fn two_slots(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = running(two_slot_rig(project));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.forge.open_pull_request(81, "kelpie/8", &[8]);
    (rig, runner)
}

// A running project with two slots and nothing open or opened yet
fn two_free_slots(project: &str) -> (Rig, Mutex<Runner>) {
    running(two_slot_rig(project))
}

fn two_slot_rig(project: &str) -> Rig {
    let rig = Rig::new(project);
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    rig
}

fn running(rig: Rig) -> (Rig, Mutex<Runner>) {
    let runner = rig.open().unwrap();
    (rig, runner)
}

// Both work items through their first turn and a finished review,
// taking turns, and parked on merge rulings 71 (#7) and 81 (#8)
fn both_parked(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = two_slots(project);
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "add", Some("8"));
    rig.claude.script([
        Scripted::Push("seven.txt", "seven\n"),
        Scripted::Push("eight.txt", "eight\n"),
        Scripted::Text(CLEAN),
        Scripted::Text(CLEAN),
    ]);
    let acted: Vec<TicketId> = (0..6).map(|_| issue_of(step(&runner).unwrap())).collect();
    assert_eq!(
        acted,
        [
            ticket(7),
            ticket(8),
            ticket(7),
            ticket(8),
            ticket(7),
            ticket(8)
        ],
        "the items take turns"
    );
    for branch in ["kelpie/7", "kelpie/8"] {
        let head = rig.forge.head_of(branch).unwrap();
        rig.forge.set_checks(&head, Checks::Passed);
    }
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling {
            issue,
            id,
            ..
        })
     if issue == ticket(7) && id == ticket(71)));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(71) })
    );
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ruling {
            issue,
            id,
            ..
        })
     if issue == ticket(8) && id == ticket(81)
    ));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(81) })
    );
    (rig, runner)
}

// A worker's reply that ends on a question, which keeps its work item open
// and waiting on the maintainer
const ASKS: &str = "<kelpie-question>\nWhich flag?\n</kelpie-question>\n";

// The issue of the work item a step's report is about, as the runner logs it
fn issue_of(report: Option<StepReport>) -> TicketId {
    let logged = serde_json::to_value(&report).unwrap();
    serde_json::from_value(logged["issue"].clone())
        .unwrap_or_else(|e| panic!("no work item acted: {logged}: {e}"))
}

fn phases(rig: &Rig, runner: &Mutex<Runner>) -> Vec<(u64, String)> {
    let status = rig.ask(runner, "status", None);
    let items = status["work_items"].as_array().unwrap();
    items
        .iter()
        .map(|item| {
            let state = item["phase"]["state"].as_str().unwrap().to_owned();
            (
                item["issue"].as_str().unwrap().parse::<u64>().unwrap(),
                state,
            )
        })
        .collect()
}

// Two slots with CodeRabbit listed last and the dog granting nothing, and
// #7 through its turn and its qwen and Claude rounds, its draft marked
// ready for CodeRabbit's round, waiting on the lease. Returns #7's head.
fn seven_waits_on_the_lease(project: &str) -> (Rig, Mutex<Runner>, String) {
    let rig = two_slot_rig(project);
    rig.coderabbit_on();
    let (rig, runner) = running(rig);
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.forge.open_pull_request(81, "kelpie/8", &[8]);
    rig.leases.withhold(true);
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([
        Scripted::Push("seven.txt", "seven\n"),
        Scripted::Text(CLEAN),
    ]);
    for _ in 0..3 {
        assert_eq!(issue_of(step(&runner).unwrap()), ticket(7));
    }
    let seven = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&seven, Checks::Passed);
    assert_eq!(issue_of(rig.verdict(&runner)), ticket(7), "marks #71 ready");
    assert_eq!(step(&runner).unwrap(), None, "#7 waits on the lease");
    (rig, runner, seven)
}

#[test]
fn an_item_ending_leaves_a_grant_its_sibling_waits_for() {
    let (rig, runner, _) = seven_waits_on_the_lease("koji");
    rig.ask(&runner, "add", Some("8"));
    rig.leases.grant(&LeaseKind::coderabbit());

    // #8 never took the lease, so dropping it gives back nothing of #7's.
    rig.ask(&runner, "drop", Some("8"));
    assert!(rig.leases.held(&LeaseKind::coderabbit()));
    assert!(
        !rig.leases
            .told()
            .contains(&Told::Return(LeaseKind::coderabbit()))
    );
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Summoned { issue, .. }) if issue == ticket(7)
    ));
}

#[test]
fn a_restart_leaves_no_resend_under_a_grant_another_item_took() {
    let (rig, runner, _) = seven_waits_on_the_lease("lugia");
    rig.leases.withhold(false);
    assert_eq!(issue_of(step(&runner).unwrap()), ticket(7), "#7 summons");

    // The restart clears the book of leases, and #8 takes the grant.
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("8"));
    rig.claude.script([
        Scripted::Push("eight.txt", "eight\n"),
        Scripted::Text(CLEAN),
    ]);
    for _ in 0..3 {
        assert_eq!(issue_of(step(&runner).unwrap()), ticket(8));
    }
    let eight = rig.forge.head_of("kelpie/8").unwrap();
    rig.forge.set_checks(&eight, Checks::Passed);
    assert_eq!(issue_of(rig.verdict(&runner)), ticket(8), "marks #81 ready");
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Summoned { issue, .. }) if issue == ticket(8)
    ));

    // #7's fifteen minutes pass with no sign; the grant is #8's.
    rig.clock.advance(HEARD_WAIT);
    for _ in 0..4 {
        if let Some(report) = step(&runner).unwrap() {
            assert!(
                !matches!(report, StepReport::SummonedAgain { issue, .. } if issue == ticket(7)),
                "re-sent under another item's grant: {report:?}"
            );
        }
    }
}

#[test]
fn an_item_waiting_on_coderabbit_yields_to_one_implementing_and_reviewing() {
    let (rig, runner, seven) = seven_waits_on_the_lease("shep");

    // #8 opens while #7 waits, and its turn and qwen and Claude rounds go ahead.
    rig.ask(&runner, "add", Some("8"));
    rig.claude.script([
        Scripted::Push("eight.txt", "eight\n"),
        Scripted::Text(CLEAN),
    ]);
    for _ in 0..3 {
        assert_eq!(issue_of(step(&runner).unwrap()), ticket(8));
    }
    assert_eq!(
        phases(&rig, &runner),
        [(7, "review".into()), (8, "review".into())]
    );
    let eight = rig.forge.head_of("kelpie/8").unwrap();
    rig.forge.set_checks(&eight, Checks::Passed);
    assert_eq!(issue_of(rig.verdict(&runner)), ticket(8), "marks #81 ready");
    assert_eq!(step(&runner).unwrap(), None, "both wait on the lease");

    // One grant is one summon, for whichever item asked first in the rotation.
    rig.leases.withhold(false);
    assert_eq!(issue_of(step(&runner).unwrap()), ticket(7));
    assert_eq!(step(&runner).unwrap(), None, "#8 waits for its own grant");
    let summons: Vec<u64> = rig
        .forge
        .coderabbit
        .label_log()
        .into_iter()
        .filter(|(_, _, on)| *on)
        .map(|(number, _, _)| number)
        .collect();
    assert_eq!(summons, [71]);
    assert_eq!(
        rig.ask(&runner, "status", None)["leases"][0]["issue"],
        json!("7")
    );

    // #7's review lands, the lease goes back, and #8 asks for it anew.
    let summon = crate::ports::Clock::now(&rig.clock).0;
    rig.forge.coderabbit.review(71, &seven, summon + 60, &[]);
    rig.clock.advance(60);
    let returned = |rig: &Rig| {
        let told = rig.leases.told();
        told.iter()
            .filter(|t| **t == Told::Return(LeaseKind::coderabbit()))
            .count()
    };
    step(&runner).unwrap();
    assert_eq!(returned(&rig), 1);
    let mut summoned_8 = false;
    for _ in 0..4 {
        let report = step(&runner).unwrap();
        if matches!(&report, Some(StepReport::Summoned { issue, .. }) if *issue == ticket(8)) {
            summoned_8 = true;
            break;
        }
    }
    assert!(summoned_8, "#8 never summoned once #7 gave the lease back");
}

#[test]
fn a_ruling_answered_for_one_item_leaves_the_other_alone() {
    let (rig, runner) = both_parked("koji");
    let status = rig.ask(&runner, "rule", Some("81 no rename the flag"));
    let rulings = status["rulings"].as_array().unwrap();
    assert_eq!(rulings.len(), 1);
    assert_eq!(
        (&rulings[0]["id"], &rulings[0]["issue"]),
        (&json!("71"), &json!("7"))
    );
    assert_eq!(
        phases(&rig, &runner),
        [(7, "ruling".into()), (8, "implement".into())]
    );

    rig.claude
        .script([Scripted::Push("renamed.txt", "renamed\n")]);
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Ended { issue, .. }) if issue == ticket(8)
    ));
    let call = rig.claude.calls().pop().unwrap();
    assert_eq!(call.cwd, rig.paths().worktree(ticket(8)));
    assert!(call.prompt.contains("rename the flag"), "{}", call.prompt);
    assert_eq!(phases(&rig, &runner)[0], (7, "ruling".into()));
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn a_merge_of_one_item_sends_the_other_to_catch_up_with_main() {
    let (rig, runner) = both_parked("rotom");
    let seven = rig.forge.head_of("kelpie/7").unwrap();
    rig.ask(&runner, "rule", Some("71 yes"));
    let mut finished = false;
    for _ in 0..4 {
        match step(&runner).unwrap() {
            Some(StepReport::Finished {
                issue,
                merged: true,
                ..
            }) if issue == ticket(7) => {
                finished = true;
                break;
            }
            _ => rig.clock.advance(crate::runner::CHECKS_SETTLE),
        }
    }
    assert!(finished, "#7 never merged");
    assert_eq!(rig.forge.merges(), [(71, seven)]);
    assert_eq!(phases(&rig, &runner), [(8, "ruling".into())]);

    // The stand-in forge records a merge without moving `main`, so #71's
    // merge lands on `main` the way GitHub would put it there.
    rig.land_on_origin("seven.txt");
    rig.ask(&runner, "rule", Some("81 yes"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::YesWithdrawn { issue, reason, .. })
            if reason == "main moved since the question" && issue == ticket(8)
    ));
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Rebased { issue, .. }) if issue == ticket(8)
    ));
}

#[test]
fn drop_and_gate_name_the_item_when_more_than_one_is_open() {
    let (rig, runner) = two_slots("golbat");
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "add", Some("8"));
    let several = "the work items for #7 and #8 are open, so name the issue of one";
    assert_eq!(rig.ask(&runner, "drop", None), json!({ "error": several }));
    assert_eq!(rig.ask(&runner, "gate", None), json!({ "error": several }));
    assert_eq!(
        rig.ask(&runner, "drop", Some("9")),
        json!({ "error": "no work item for #9 is open" })
    );

    let status = rig.ask(&runner, "drop", Some("8"));
    let open: Vec<_> = status["work_items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["issue"].clone())
        .collect();
    assert_eq!(open, [json!("7")]);
    assert_eq!(status["work_item"]["issue"], "7");
}

#[test]
fn add_opens_up_to_active_items_and_never_the_same_issue_twice() {
    let (rig, runner) = two_slots("xilriws");
    assert_eq!(rig.ask(&runner, "add", Some("7"))["active_items"], 2);
    assert_eq!(
        rig.ask(&runner, "add", Some("7")),
        json!({ "error": "the work item for #7 is in flight" })
    );
    rig.ask(&runner, "add", Some("8"));
    assert_eq!(
        rig.ask(&runner, "add", Some("9")),
        json!({ "error": "the work items for #7 and #8 are in flight" })
    );
}

#[test]
fn with_one_slot_a_second_add_is_refused_as_before() {
    let rig = Rig::new("chelone");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    assert_eq!(
        rig.ask(&runner, "add", Some("8")),
        json!({ "error": "the work item for #7 is in flight" })
    );
    assert_eq!(rig.ask(&runner, "drop", None)["work_items"], json!([]));
}

fn dispatched(report: Option<StepReport>) -> (TicketId, Vec<Skip>) {
    match report {
        Some(StepReport::Dispatched { issue, skipped, .. }) => (issue, skipped),
        other => panic!("nothing was dispatched: {other:?}"),
    }
}

fn open_items(rig: &Rig, runner: &Mutex<Runner>) -> Vec<u64> {
    let status = rig.ask(runner, "status", None);
    let items = status["work_items"].as_array().unwrap().clone();
    items
        .iter()
        .map(|i| i["issue"].as_str().unwrap().parse::<u64>().unwrap())
        .collect()
}

#[test]
fn the_board_fills_a_free_slot_and_never_takes_an_open_issue_again() {
    let (rig, runner) = two_free_slots("acme");
    for issue in [7, 8, 9] {
        rig.forge.list_ready(issue, false);
    }
    rig.claude
        .script([Scripted::Say(ASKS), Scripted::Say(ASKS)]);
    assert_eq!(dispatched(step(&runner).unwrap()), (ticket(7), vec![]));
    assert_eq!(
        issue_of(step(&runner).unwrap()),
        ticket(7),
        "#7's first turn"
    );
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(7) })
    );
    // #7 is still ready on the forge, and in flight here.
    assert_eq!(dispatched(step(&runner).unwrap()), (ticket(8), vec![]));
    assert_eq!(
        issue_of(step(&runner).unwrap()),
        ticket(8),
        "#8's first turn"
    );
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(8) })
    );

    // Both workers are parked on their questions, which fills `concurrency.pending_rulings`, so #9 waits.
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(open_items(&rig, &runner), [7, 8]);

    rig.ask(&runner, "drop", Some("7"));
    let (issue, skipped) = dispatched(step(&runner).unwrap());
    assert_eq!(issue, ticket(9));
    assert_eq!(skipped, [Skip::Dropped { issue: ticket(7) }]);
    assert_eq!(open_items(&rig, &runner), [8, 9]);
}

#[test]
fn an_adoption_goes_before_the_board_into_a_free_slot() {
    let (rig, runner) = two_free_slots("shep");
    rig.forge.list_ready(3, false);
    rig.push_by_hand("fix/timeline", "work.txt");
    rig.forge.open_pull_request(80, "fix/timeline", &[5]);
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "adopt", Some("80"));

    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    assert_eq!(
        issue_of(step(&runner).unwrap()),
        ticket(7),
        "#7's first turn"
    );
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Adopted {
            issue,
            pull_request: 80,
            ..
        })
     if issue == ticket(5)));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["adopted"], json!([]));
    assert_eq!(status["work_items"][1]["phase"]["state"], "ci");
    assert_eq!(open_items(&rig, &runner), [7, 5], "no slot is left for #3");
}

// The log lines the runner has left, clearing them
fn said(runner: &Mutex<Runner>) -> Vec<String> {
    runner.lock().unwrap().take_notes()
}

// The issues the board dispatches over the next `turns` steps, in order
fn dispatched_in(runner: &Mutex<Runner>, turns: usize) -> Vec<TicketId> {
    (0..turns)
        .filter_map(|_| match step(runner).unwrap() {
            Some(StepReport::Dispatched { issue, .. }) => Some(issue),
            _ => None,
        })
        .take(2)
        .collect()
}

// Two slots with #7 dropped and #8 and #9 holding both
//
// The board has already said #7 is dropped, so the runner's `skipped` holds the skip
// `add` is asked to clear. #8's worker answers with a question, so #8 stays where it is.
fn seven_dropped_two_slots_held(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = two_free_slots(project);
    rig.forge.list_ready(7, false);
    rig.forge.list_ready(8, false);
    rig.claude.script([Scripted::Say(ASKS)]);
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "drop", Some("7"));
    assert!(
        matches!(
            step(&runner).unwrap(),
            Some(StepReport::Dispatched { issue, skipped, .. })
                if issue == ticket(8) && skipped == [Skip::Dropped { issue: ticket(7) }]
        ),
        "the board takes #8 into the freed slot and says #7 is dropped"
    );
    rig.ask(&runner, "add", Some("9"));
    assert_eq!(open_items(&rig, &runner), [8, 9], "every slot is held");
    (rig, runner)
}

#[test]
fn a_dropped_issue_added_with_every_slot_busy_is_queued_and_starts_when_one_frees() {
    let (rig, runner) = seven_dropped_two_slots_held("mawile");
    assert_eq!(
        rig.ask(&runner, "status", None)["skipped"],
        json!([{ "reason": "dropped", "issue": "7" }])
    );

    let status = rig.ask(&runner, "add", Some("7"));
    assert_eq!(open_items(&rig, &runner), [8, 9], "#7 waits for a slot");
    assert_eq!(
        status["skipped"],
        json!([]),
        "its skip is cleared: {status}"
    );

    rig.ask(&runner, "drop", Some("9"));
    assert_eq!(
        dispatched_in(&runner, 8),
        [ticket(7)],
        "#7 takes the freed slot"
    );
}

#[test]
fn a_queued_add_clears_the_dropped_skip_once_in_the_log_and_in_status() {
    let (rig, runner) = seven_dropped_two_slots_held("numel");
    rig.ask(&runner, "add", Some("7"));

    let cleared = |lines: &[String]| {
        lines
            .iter()
            .filter(|line| line.starts_with("#7 skip-cleared"))
            .count()
    };
    let lines = said(&runner);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("#7 skipped reason=")),
        "{lines:?}"
    );
    assert_eq!(cleared(&lines), 1, "{lines:?}");

    rig.ask(&runner, "drop", Some("9"));
    assert_eq!(dispatched_in(&runner, 8), [ticket(7)]);
    let lines = said(&runner);
    assert_eq!(cleared(&lines), 0, "the next pass says it again: {lines:?}");
    assert_eq!(rig.ask(&runner, "status", None)["skipped"], json!([]));
}

#[test]
fn queued_dropped_issues_take_a_freed_slot_in_the_boards_order() {
    let rig = Rig::new("shinx");
    // The workers here answer with questions, and a parked one gives its slot up, so
    // the board has to stay open past two of them.
    rig.edit_settings(|s| {
        s.replace("active_items = 1", "active_items = 2")
            .replace("pending_rulings = 2", "pending_rulings = 4")
    });
    let (rig, runner) = running(rig);
    for issue in [7, 8] {
        rig.forge.list_ready(issue, false);
    }
    // #7 outranks #8 on the board, whatever order they were added back in.
    rig.forge.label(7, "priority: P0");
    rig.claude.script([
        Scripted::Say(ASKS),
        Scripted::Say(ASKS),
        Scripted::Say(ASKS),
        Scripted::Say(ASKS),
    ]);
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "add", Some("8"));
    rig.ask(&runner, "drop", Some("7"));
    rig.ask(&runner, "drop", Some("8"));
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "the board leaves both dropped"
    );

    rig.ask(&runner, "add", Some("9"));
    rig.ask(&runner, "add", Some("10"));
    rig.ask(&runner, "add", Some("8"));
    rig.ask(&runner, "add", Some("7"));
    assert_eq!(open_items(&rig, &runner), [9, 10], "both wait for a slot");

    rig.ask(&runner, "drop", Some("9"));
    assert_eq!(
        dispatched_in(&runner, 10),
        [ticket(7), ticket(8)],
        "the board's own order, not the order they were added in"
    );
}

#[test]
fn add_with_a_free_slot_still_opens_a_dropped_issue_at_once() {
    let (rig, runner) = two_free_slots("krabby");
    rig.forge.list_ready(7, false);
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "drop", Some("7"));
    assert_eq!(step(&runner).unwrap(), None, "the board leaves #7 dropped");
    assert_eq!(
        rig.ask(&runner, "status", None)["skipped"],
        json!([{ "reason": "dropped", "issue": "7" }])
    );

    assert!(rig.ask(&runner, "add", Some("7"))["work_item"].is_object());
    assert_eq!(
        open_items(&rig, &runner),
        [7],
        "a free slot opens it at once"
    );
    assert_eq!(rig.dropped(), [7], "`add` leaves `dropped` as it is");
}

#[test]
fn add_refuses_a_dropped_issue_the_board_would_pass_over_for_its_open_pull_request() {
    let (rig, runner) = seven_dropped_two_slots_held("tropius");
    // #7's worker had opened its pull request before the drop, and it is still open
    rig.forge.open_pull_request(80, "fix/seven", &[7]);

    assert_eq!(
        rig.ask(&runner, "add", Some("7")),
        json!({ "error": "the board would pass #7 over: PR #80 is open for it" }),
        "queueing #7 would promise a slot the board never takes"
    );
    assert_eq!(rig.dropped(), [7], "#7 stays dropped");
    assert_eq!(open_items(&rig, &runner), [8, 9]);
    assert_eq!(
        said(&runner)
            .iter()
            .filter(|line| line.starts_with("#7 skip-cleared"))
            .count(),
        0,
        "no skip is cleared"
    );
}

#[test]
fn add_refuses_a_dropped_issue_the_board_would_pass_over_as_blocked() {
    let (rig, runner) = seven_dropped_two_slots_held("carkol");
    rig.forge.block(7, 32);

    assert_eq!(
        rig.ask(&runner, "add", Some("7")),
        json!({ "error": "the board would pass #7 over: blocked by #32" })
    );
    assert_eq!(rig.dropped(), [7], "#7 stays dropped");
    assert_eq!(open_items(&rig, &runner), [8, 9]);
}

#[test]
fn an_adoption_for_an_issue_in_flight_waits_for_it_to_end() {
    let (rig, runner) = two_free_slots("rotom");
    rig.push_by_hand("fix/seven", "work.txt");
    rig.forge.open_pull_request(90, "fix/seven", &[7]);
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "adopt", Some("90"));

    rig.claude.script([Scripted::Say(ASKS)]);
    assert_eq!(
        issue_of(step(&runner).unwrap()),
        ticket(7),
        "#7's first turn"
    );
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(7) })
    );
    assert_eq!(step(&runner).unwrap(), None, "#90 waits for #7");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["adopted"],
        json!([{ "pull_request": 90, "by_label": false }])
    );
    assert_eq!(status["skipped"], json!([]), "waiting is not failing");
    assert_eq!(open_items(&rig, &runner), [7]);
}
