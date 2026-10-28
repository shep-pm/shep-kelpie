//! A changed `tickets` table reaching a running runner

use std::sync::Mutex;

use crate::ports::{CLEAN, Checks};
use crate::runner::slots_tests::dispatched;
use crate::runner::{Runner, step};
use crate::test::{Rig, Scripted};
use crate::ticket::TicketId;

const NOW: &str = "settings changed: tickets now in effect";

fn ticket(name: &str) -> TicketId {
    name.parse().unwrap()
}

// Has the project's table name `source`, and the open runner read it again.
fn change_source_to(rig: &Rig, runner: &Mutex<Runner>, source: &str) -> Option<String> {
    let (was, now) = match source {
        "local" => ("source = \"github\"", "source = \"local\""),
        _ => ("source = \"local\"", "source = \"github\""),
    };
    rig.edit_settings(|s| {
        assert!(s.contains(was), "the project was not on the other source");
        s.replace(was, now)
    });
    let (settings, kelpie) = (rig.settings(), rig.kelpie_settings());
    runner.lock().unwrap().reread(settings, kelpie).unwrap()
}

// Steps a work item that has had its first turn on to its merge, and returns the head merged.
fn on_to_the_merge(rig: &Rig, runner: &Mutex<Runner>, branch: &str) -> String {
    let mut head = None;
    for _ in 0..20 {
        step(runner).unwrap();
        if rig.ask(runner, "status", None)["work_item"]["phase"]["state"] == "ci" {
            head = rig.forge.head_of(branch);
            break;
        }
    }
    let head = head.expect("the review never reached CI");
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(
        rig.verdict(runner).is_some(),
        "the pull request was marked ready"
    );
    rig.clock.advance(crate::runner::CHECKS_SETTLE);
    step(runner).unwrap();
    head
}

fn local_ticket_filed(rig: &Rig) {
    let filed = rig.ticket_verb(&["new", "Add a thing", "--label", "ready-for-agent"]);
    assert_eq!(filed, ["k1"]);
}

#[test]
fn github_to_local_lists_the_local_tickets_at_the_next_board_read() {
    let rig = Rig::new("shep");
    local_ticket_filed(&rig);
    let runner = rig.open().unwrap();

    let line = change_source_to(&rig, &runner, "local");

    assert_eq!(line.as_deref(), Some(NOW));
    let (issue, skipped) = dispatched(step(&runner).unwrap());
    assert_eq!((issue, skipped), (ticket("k1"), vec![]));
}

#[test]
fn local_to_github_lists_the_issues_at_the_next_board_read() {
    let rig = Rig::new("shep");
    rig.tickets_local();
    local_ticket_filed(&rig);
    rig.forge.list_ready(7, false);
    let runner = rig.open().unwrap();

    let line = change_source_to(&rig, &runner, "github");

    assert_eq!(line.as_deref(), Some(NOW));
    let (issue, skipped) = dispatched(step(&runner).unwrap());
    assert_eq!((issue, skipped), (ticket("7"), vec![]));
}

#[test]
fn a_work_item_on_a_github_issue_finishes_on_it_across_a_change_to_local() {
    let rig = Rig::new("shep");
    rig.merge_auto();
    rig.forge.list_ready(7, false);
    let runner = rig.open().unwrap();
    rig.forge.open_pull_request(71, "kelpie/7", &[]);
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
    let (issue, _) = dispatched(step(&runner).unwrap());
    assert_eq!(issue, ticket("7"));
    step(&runner).unwrap(); // the worker's first turn
    assert!(
        rig.forge
            .issue_labels(7)
            .contains(&"in-progress".to_owned())
    );

    let line = change_source_to(&rig, &runner, "local");
    assert_eq!(line.as_deref(), Some(NOW));
    let head = on_to_the_merge(&rig, &runner, "kelpie/7");

    assert_eq!(rig.forge.merges(), [(71, head)]);
    assert!(
        !rig.forge
            .issue_labels(7)
            .contains(&"in-progress".to_owned()),
        "the held label came off the issue"
    );
    assert!(
        rig.forge
            .issue_writes()
            .contains(&"issue closed: #7".to_owned()),
        "{:?}",
        rig.forge.issue_writes()
    );
}

#[test]
fn a_work_item_on_a_local_ticket_finishes_on_it_across_a_change_to_github() {
    let rig = Rig::new("shep");
    rig.tickets_local();
    rig.merge_auto();
    local_ticket_filed(&rig);
    let runner = rig.open().unwrap();
    rig.forge.open_pull_request(71, "kelpie/k1", &[]);
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
    let (issue, _) = dispatched(step(&runner).unwrap());
    assert_eq!(issue, ticket("k1"));
    step(&runner).unwrap(); // the worker's first turn

    let line = change_source_to(&rig, &runner, "github");
    assert_eq!(line.as_deref(), Some(NOW));
    let head = on_to_the_merge(&rig, &runner, "kelpie/k1");

    assert_eq!(rig.forge.merges(), [(71, head)]);
    let shown = rig.ticket_verb(&["show", "k1"]);
    assert!(
        shown.iter().any(|line| line == "state: closed"),
        "{shown:?}"
    );
    // The change to GitHub made the agent and priority labels, and nothing was
    // written to the issues.
    let writes = rig.forge.issue_writes();
    let made =
        |w: &String| w.starts_with("label made: agent:") || w.starts_with("label made: priority:");
    assert!(writes.iter().all(made), "{writes:?}");
    assert!(
        rig.forge.comments().is_empty(),
        "nothing was said on GitHub"
    );
}

#[test]
fn a_change_to_github_makes_the_agent_labels_a_local_start_skipped() {
    let rig = Rig::new("shep");
    rig.tickets_local();
    let runner = rig.open().unwrap();
    let made = |rig: &Rig| {
        let writes = rig.forge.issue_writes();
        writes.iter().any(|w| w.starts_with("label made: agent:"))
    };
    assert!(!made(&rig), "a local start makes no label on the repo");

    change_source_to(&rig, &runner, "github");

    assert!(made(&rig), "{:?}", rig.forge.issue_writes());
}

#[test]
fn a_change_to_github_makes_the_priority_labels_a_local_start_skipped() {
    let rig = Rig::new("shep");
    rig.tickets_local();
    let runner = rig.open().unwrap();
    let made = |rig: &Rig| {
        let writes = rig.forge.issue_writes();
        writes
            .iter()
            .filter(|w| w.starts_with("label made: priority:"))
            .count()
    };
    assert_eq!(made(&rig), 0, "a local start makes no label on the repo");

    change_source_to(&rig, &runner, "github");

    assert_eq!(made(&rig), 5, "{:?}", rig.forge.issue_writes());
}
