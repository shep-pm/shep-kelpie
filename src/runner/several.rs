//! Several work items open at once, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use crate::board::Skip;
use crate::lease::LeaseKind;
use crate::ports::{Checks, Cost, Usage};
use crate::runner::coderabbit::HEARD_WAIT;
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted, Told};

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
    rig.edit_settings(|s| s.replace("max_items = 1", "max_items = 2"));
    rig
}

fn running(rig: Rig) -> (Rig, Mutex<Runner>) {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    (rig, runner)
}

// Both work items through their first turn and a settled qwen-review loop,
// taking turns, and parked on merge rulings 1 (#7) and 2 (#8)
fn both_parked(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = two_slots(project);
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "add", Some("8"));
    rig.claude.script([
        Scripted::Push("seven.txt", "seven\n"),
        Scripted::Push("eight.txt", "eight\n"),
        Scripted::Text("CLEAN"),
        Scripted::Text("CLEAN"),
    ]);
    let acted: Vec<u64> = (0..6).map(|_| issue_of(step(&runner).unwrap())).collect();
    assert_eq!(acted, [7, 8, 7, 8, 7, 8], "the items take turns");
    for branch in ["kelpie/7", "kelpie/8"] {
        let head = rig.forge.head_of(branch).unwrap();
        rig.forge.set_checks(&head, Checks::Passed);
    }
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling {
            issue: 7,
            id: 1,
            ..
        })
    ));
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert!(matches!(
        rig.step_past_audits(&runner),
        Some(StepReport::Ruling {
            issue: 8,
            id: 2,
            ..
        })
    ));
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 2 }));
    (rig, runner)
}

// A worker's reply that ends on a question, which keeps its work item open
// and waiting on the maintainer
const ASKS: &str = "<kelpie-question>\nWhich flag?\n</kelpie-question>\n";

// The issue of the work item a step's report is about, as the runner logs it
fn issue_of(report: Option<StepReport>) -> u64 {
    let logged = serde_json::to_value(&report).unwrap();
    logged["issue"]
        .as_u64()
        .unwrap_or_else(|| panic!("no work item acted: {logged}"))
}

fn phases(rig: &Rig, runner: &Mutex<Runner>) -> Vec<(u64, String)> {
    let status = rig.ask(runner, "status", None);
    let items = status["work_items"].as_array().unwrap();
    items
        .iter()
        .map(|item| {
            let state = item["phase"]["state"].as_str().unwrap().to_owned();
            (item["issue"].as_u64().unwrap(), state)
        })
        .collect()
}

// Two slots with CodeRabbit on and the dog granting nothing, and #7 through
// its turn, its review rounds and green CI, waiting on the lease. Returns
// #7's head.
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
        Scripted::Text("CLEAN"),
    ]);
    for _ in 0..3 {
        assert_eq!(issue_of(step(&runner).unwrap()), 7);
    }
    let seven = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&seven, Checks::Passed);
    assert_eq!(issue_of(rig.verdict(&runner)), 7, "marks #71 ready");
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
        Some(StepReport::Summoned { issue: 7, .. })
    ));
}

#[test]
fn a_restart_leaves_no_resend_under_a_grant_another_item_took() {
    let (rig, runner, _) = seven_waits_on_the_lease("lugia");
    rig.leases.withhold(false);
    assert_eq!(issue_of(step(&runner).unwrap()), 7, "#7 summons");

    // The restart clears the book of leases, and #8 takes the grant.
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("8"));
    rig.claude.script([
        Scripted::Push("eight.txt", "eight\n"),
        Scripted::Text("CLEAN"),
    ]);
    for _ in 0..3 {
        assert_eq!(issue_of(step(&runner).unwrap()), 8);
    }
    let eight = rig.forge.head_of("kelpie/8").unwrap();
    rig.forge.set_checks(&eight, Checks::Passed);
    assert_eq!(issue_of(rig.verdict(&runner)), 8, "marks #81 ready");
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { issue: 8, .. })
    ));

    // #7's fifteen minutes pass with no sign; the grant is #8's.
    rig.clock.advance(HEARD_WAIT);
    for _ in 0..4 {
        if let Some(report) = step(&runner).unwrap() {
            assert!(
                !matches!(report, StepReport::SummonedAgain { issue: 7, .. }),
                "re-sent under another item's grant: {report:?}"
            );
        }
    }
}

#[test]
fn an_item_waiting_on_coderabbit_yields_to_one_implementing_and_reviewing() {
    let (rig, runner, seven) = seven_waits_on_the_lease("shep");

    // #8 opens while #7 waits, and its turn and review rounds go ahead.
    rig.ask(&runner, "add", Some("8"));
    rig.claude.script([
        Scripted::Push("eight.txt", "eight\n"),
        Scripted::Text("CLEAN"),
    ]);
    for _ in 0..3 {
        assert_eq!(issue_of(step(&runner).unwrap()), 8);
    }
    assert_eq!(
        phases(&rig, &runner),
        [(7, "coderabbit".into()), (8, "ci".into())]
    );
    let eight = rig.forge.head_of("kelpie/8").unwrap();
    rig.forge.set_checks(&eight, Checks::Passed);
    assert_eq!(issue_of(rig.verdict(&runner)), 8, "marks #81 ready");
    assert_eq!(step(&runner).unwrap(), None, "both wait on the lease");

    // One grant is one summon, for whichever item asked first in the rotation.
    rig.leases.withhold(false);
    assert_eq!(issue_of(step(&runner).unwrap()), 7);
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
        json!(7)
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
        if let Some(StepReport::Summoned { issue: 8, .. }) = step(&runner).unwrap() {
            summoned_8 = true;
            break;
        }
    }
    assert!(summoned_8, "#8 never summoned once #7 gave the lease back");
}

#[test]
fn a_ruling_answered_for_one_item_leaves_the_other_alone() {
    let (rig, runner) = both_parked("koji");
    let status = rig.ask(&runner, "rule", Some("2 no rename the flag"));
    let rulings = status["rulings"].as_array().unwrap();
    assert_eq!(rulings.len(), 1);
    assert_eq!(
        (&rulings[0]["id"], &rulings[0]["issue"]),
        (&json!(1), &json!(7))
    );
    assert_eq!(
        phases(&rig, &runner),
        [(7, "ruling".into()), (8, "implement".into())]
    );

    rig.claude
        .script([Scripted::Push("renamed.txt", "renamed\n")]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ended { issue: 8, .. })
    ));
    let call = rig.claude.calls().pop().unwrap();
    assert_eq!(call.cwd, rig.paths().worktree(8));
    assert!(call.prompt.contains("rename the flag"), "{}", call.prompt);
    assert_eq!(phases(&rig, &runner)[0], (7, "ruling".into()));
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn a_merge_of_one_item_sends_the_other_to_catch_up_with_main() {
    let (rig, runner) = both_parked("rotom");
    let seven = rig.forge.head_of("kelpie/7").unwrap();
    rig.ask(&runner, "rule", Some("1 yes"));
    let mut finished = false;
    for _ in 0..4 {
        match step(&runner).unwrap() {
            Some(StepReport::Finished {
                issue: 7,
                merged: true,
                ..
            }) => {
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
    rig.ask(&runner, "rule", Some("2 yes"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::YesWithdrawn { issue: 8, reason, .. })
            if reason == "main moved since the question"
    ));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Rebased { issue: 8, .. })
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
    assert_eq!(open, [json!(7)]);
    assert_eq!(status["work_item"]["issue"], 7);
}

#[test]
fn add_opens_up_to_max_items_and_never_the_same_issue_twice() {
    let (rig, runner) = two_slots("xilriws");
    assert_eq!(rig.ask(&runner, "add", Some("7"))["max_items"], 2);
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

fn dispatched(report: Option<StepReport>) -> (u64, Vec<Skip>) {
    match report {
        Some(StepReport::Dispatched { issue, skipped, .. }) => (issue, skipped),
        other => panic!("nothing was dispatched: {other:?}"),
    }
}

fn open_items(rig: &Rig, runner: &Mutex<Runner>) -> Vec<u64> {
    let status = rig.ask(runner, "status", None);
    let items = status["work_items"].as_array().unwrap().clone();
    items.iter().map(|i| i["issue"].as_u64().unwrap()).collect()
}

#[test]
fn the_board_fills_a_free_slot_and_never_takes_an_open_issue_again() {
    let (rig, runner) = two_free_slots("zeus");
    for issue in [7, 8, 9] {
        rig.forge.list_ready(issue, false);
    }
    rig.claude
        .script([Scripted::Say(ASKS), Scripted::Say(ASKS)]);
    assert_eq!(dispatched(step(&runner).unwrap()), (7, vec![]));
    assert_eq!(issue_of(step(&runner).unwrap()), 7, "#7's first turn");
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    // #7 is still ready on the forge, and in flight here.
    assert_eq!(dispatched(step(&runner).unwrap()), (8, vec![]));
    assert_eq!(issue_of(step(&runner).unwrap()), 8, "#8's first turn");
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 2 }));

    // Both slots are taken, each by a worker waiting on its question, so #9 waits.
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(open_items(&rig, &runner), [7, 8]);

    rig.ask(&runner, "drop", Some("7"));
    let (issue, skipped) = dispatched(step(&runner).unwrap());
    assert_eq!(issue, 9);
    assert_eq!(skipped, [Skip::Finished { issue: 7 }]);
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
    assert_eq!(issue_of(step(&runner).unwrap()), 7, "#7's first turn");
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Adopted {
            issue: 5,
            pull_request: 80,
            ..
        })
    ));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["adopted"], json!([]));
    assert_eq!(status["work_items"][1]["phase"]["state"], "ci");
    assert_eq!(open_items(&rig, &runner), [7, 5], "no slot is left for #3");
}

#[test]
fn an_adoption_for_an_issue_in_flight_waits_for_it_to_end() {
    let (rig, runner) = two_free_slots("rotom");
    rig.push_by_hand("fix/seven", "work.txt");
    rig.forge.open_pull_request(90, "fix/seven", &[7]);
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "adopt", Some("90"));

    rig.claude.script([Scripted::Say(ASKS)]);
    assert_eq!(issue_of(step(&runner).unwrap()), 7, "#7's first turn");
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert_eq!(step(&runner).unwrap(), None, "#90 waits for #7");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["adopted"],
        json!([{ "pull_request": 90, "by_label": false }])
    );
    assert_eq!(status["skipped"], json!([]), "waiting is not failing");
    assert_eq!(open_items(&rig, &runner), [7]);
}
