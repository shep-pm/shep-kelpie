//! The board briefing, as the runner writes it through its stand-ins

use std::sync::Mutex;

use crate::ports::{AgentError, CallActivity, Cost, Timestamp, Usage};
use crate::runner::flight::advance;
use crate::runner::flight::tests::Driven;
use crate::runner::{Pass, Runner, step};
use crate::settings::Harness;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted, git};

fn board(rig: &Rig) -> String {
    std::fs::read_to_string(rig.paths().board).unwrap()
}

// `origin/main`'s commit as the board shortens it
fn main_at(rig: &Rig) -> String {
    git(&rig.repo(), &["rev-parse", "origin/main"])[..10].to_owned()
}

// A running project with three slots and issues 7 and 8 open, whose pull
// requests will be 71 and 81, and ready issue 9, a P1 blocked by #4 that
// names the README.
fn seven_and_eight(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 3"));
    rig.forge.list_ready(9, false);
    rig.forge.label(9, "priority: P1");
    rig.forge.block(9, 4);
    rig.forge.set_ready_body(
        9,
        "Reword `README.md`, then `src/missing.rs:12`.\n\n\n\nSee `x y` and `--flag`.\n\n\
         Touches: `README.md`\n\n## What to build\n\n- The reword.\n\n\
         ## Acceptance criteria\n\n- [ ] It reads well.",
    );
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.forge.open_pull_request(81, "kelpie/8", &[8]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "add", Some("8"));
    (rig, runner)
}

#[test]
fn a_new_project_gets_its_board_at_start_with_nothing_on_it() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    let path = rig.ask(&runner, "status", None)["board"].clone();
    assert_eq!(path, serde_json::json!(rig.paths().board));
    assert_eq!(
        board(&rig),
        format!(
            "# Board: acme, 2026-09-21 14:13 UTC\n\
             \n\
             0 work items open: 0 of 1 slots taken, and 0 of 2 parked on rulings. Main is at {}.\n\
             \n\
             ## Open work\n\
             \n\
             (none)\n\
             \n\
             ## Ready queue\n\
             \n\
             Not read since the runner started. The board reads it while a slot is free.\n\
             \n\
             ## Agents\n\
             \n\
             Implementers by name, then reviewers in their order. The agent board picks a work \
             item's implementer by its kind, tier and slack, and a pick's agent or an \
             `agent:<name>!` label pins it.\n\
             \n\
             - opus-high (implementer): heavy tier, every kind; Builds work that is hard to undo, \
             such as a data migration, a release step or a change to a public interface.\n\
             - sonnet-high (implementer): standard tier, every kind; Builds everyday \
             features and anything with state, such as a store, a queue or a migration.\n\
             - qwen (reviewer): light tier, every kind; Reads a diff for defects with the \
             maintainer's local qwen-review script, as a first pass.\n\
             - claude (reviewer): standard tier, every kind; no description\n\
             \n\
             ## Recent events\n\
             \n\
             (none)\n\
             \n\
             ## Overlap\n\
             \n\
             Files both sides touch, and the files `git merge-tree` finds in conflict between \
             two open branches. A branch's files are its diff from main; a ready issue's are \
             the paths its body names. Pairs not listed share nothing.\n\
             \n\
             (nothing shared)\n",
            main_at(&rig)
        )
    );
    assert_eq!(rig.claude.calls().len(), 0, "a board calls no model");
}

#[test]
fn two_branches_editing_one_file_read_as_a_conflict_and_a_ready_issue_naming_it_as_overlap() {
    let (rig, runner) = seven_and_eight("acme");
    rig.claude.script([
        Scripted::Push("README.md", "seven\n"),
        Scripted::Push("README.md", "eight\n"),
    ]);
    // Each worker's turn, then each one's first review round.
    for _ in 0..4 {
        step(&runner).unwrap();
    }
    rig.clock.advance(5 * 60);
    super::brief(&runner);
    assert_eq!(rig.claude.calls().len(), 2, "the board added no call");
    let text = board(&rig).replace(&main_at(&rig), "MAIN");
    assert_eq!(text, TWO_BRANCHES, "{text}");
}

const TWO_BRANCHES: &str = r#"# Board: acme, 2026-09-21 14:18 UTC

2 work items open: 2 of 3 slots taken, and 0 of 2 parked on rulings. Main is at MAIN.

## Open work

- #7 "Title of #7": review round 2; PR #71, opened 5m ago, on sonnet-high; touches 1 file
  - Session: no call in flight; the worker's last turn ended 5m ago
  - Spend: $0.00 of $10.00
  - Last turn: "pushed"
- #8 "Title of #8": review round 2; PR #81, opened 5m ago, on sonnet-high; touches 1 file
  - Session: no call in flight; the worker's last turn ended 5m ago
  - Spend: $0.00 of $10.00
  - Last turn: "pushed"

## Ready queue, read 5m ago (the board's order, first taken first; a new issue joins after the last of its priority)

1. #9 [P1] "Title of #9"; waits: blocked by #4; names README.md, src/missing.rs

## Agents

Implementers by name, then reviewers in their order. The agent board picks a work item's implementer by its kind, tier and slack, and a pick's agent or an `agent:<name>!` label pins it.

- opus-high (implementer): heavy tier, every kind; Builds work that is hard to undo, such as a data migration, a release step or a change to a public interface.
- sonnet-high (implementer): standard tier, every kind; Builds everyday features and anything with state, such as a store, a queue or a migration.
- qwen (reviewer): light tier, every kind; Reads a diff for defects with the maintainer's local qwen-review script, as a first pass.
- claude (reviewer): standard tier, every kind; no description

## Recent events

- [1] 14:13 #7: work item opened on sonnet-high: "Title of #7"
- [2] 14:13 #8: work item opened on sonnet-high: "Title of #8"
- [3] 14:13 #7: worker turn started
- [4] 14:13 #7: worker turn ended
- [5] 14:13 #7: pull request #71 opened
- [6] 14:13 #7: implement to review round 1
- [7] 14:13 #8: worker turn started
- [8] 14:13 #8: worker turn ended
- [9] 14:13 #8: pull request #81 opened
- [10] 14:13 #8: implement to review round 1
- [11] 14:13 #7: review call started
- [12] 14:13 #7: review call ended
- [13] 14:13 #7: review round 1 to review round 2
- [14] 14:13 #8: review call started
- [15] 14:13 #8: review call ended
- [16] 14:13 #8: review round 1 to review round 2

## Overlap

Files both sides touch, and the files `git merge-tree` finds in conflict between two open branches. A branch's files are its diff from main; a ready issue's are the paths its body names. Pairs not listed share nothing.

| A | B | In conflict | Both touch |
| --- | --- | --- | --- |
| #7 (PR #71) | #8 (PR #81) | README.md | README.md |
| #9 (ready) | #7 (PR #71) |  | README.md |
| #9 (ready) | #8 (PR #81) |  | README.md |

## Files

- #7 (kelpie/7): README.md
- #8 (kelpie/8): README.md

## Ready issues, the first 600 characters of each body

### #9 Title of #9

> Reword `README.md`, then `src/missing.rs:12`.
>
> See `x y` and `--flag`.
>
> Touches: `README.md`
>
> ## What to build
>
> - The reword.
>
> ## Acceptance criteria
>
> - [ ] It reads well.
"#;

#[test]
fn a_worker_silent_past_ten_minutes_reads_as_idle_until_it_shows_something_again() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the turn never began");
    rig.claude
        .set_activity(CallActivity::At(Timestamp(Rig::EPOCH + 60)));
    rig.clock.advance(5 * 60);
    advance(&runner).unwrap();
    let text = board(&rig);
    let busy = "  - Session: worker turn running for 5m, last tool call or output 4m ago\n";
    assert!(text.contains(busy), "{text}");

    rig.clock.advance(7 * 60);
    advance(&runner).unwrap();
    let text = board(&rig);
    let idle = "  - Session: worker turn running for 12m, idle: no tool call or output for 11m\n";
    assert!(text.contains(idle), "{text}");
    let event = "] 14:25 #7: worker idle, no tool call or output for 11m\n";
    assert!(text.contains(event), "{text}");

    rig.claude
        .set_activity(CallActivity::At(Timestamp(Rig::EPOCH + 12 * 60)));
    advance(&runner).unwrap();
    let text = board(&rig);
    assert!(text.contains("] 14:25 #7: worker active again\n"), "{text}");
    hold.release();
    assert!(hold.answered(CALL_PATIENCE), "the turn never answered");
}

#[test]
fn a_turn_queued_for_its_lease_is_never_idle_and_its_quiet_starts_at_the_grant() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let queue = Hold::default();
    let work = Hold::default();
    rig.claude.script([Scripted::Queued(
        queue.clone(),
        Box::new(Scripted::Hold(work.clone())),
    )]);
    rig.claude.set_activity(CallActivity::Nothing);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(
        queue.entered(CALL_PATIENCE),
        "the turn never queued for its lease"
    );
    assert_eq!(driven.heard(), None, "the wait is no report");

    rig.clock.advance(30 * 60);
    driven.pass();
    let text = board(&rig);
    let waiting = "  - Session: worker turn running for 30m, waiting for its model\n";
    assert!(text.contains(waiting), "{text}");
    let lease = "  - Waiting for its model since 14:13 (30m): for a lease on its model\n";
    assert!(text.contains(lease), "{text}");
    assert!(
        text.contains("#7: worker waits 30m for its model's lease\n"),
        "{text}"
    );
    assert!(!text.contains("worker idle"), "{text}");

    queue.release();
    assert_eq!(driven.heard(), None, "the grant is no report");
    assert!(work.entered(CALL_PATIENCE), "the turn never began");
    rig.clock.advance(60);
    driven.pass();
    let text = board(&rig);
    let quiet = "  - Session: worker turn running for 31m, no tool call or output yet\n";
    assert!(text.contains(quiet), "{text}");
    assert!(
        text.contains("worker has its model's lease, after waiting 30m\n"),
        "{text}"
    );

    rig.clock.advance(10 * 60);
    driven.pass();
    let text = board(&rig);
    let idle = "  - Session: worker turn running for 41m, idle: no tool call or output for 11m\n";
    assert!(text.contains(idle), "{text}");
    assert!(
        text.contains("] 14:54 #7: worker idle, no tool call or output for 11m\n"),
        "{text}"
    );
    work.release();
    assert!(work.answered(CALL_PATIENCE), "the turn never answered");
}

/// A wait the gateway answered busy is not a wait for a lease, and the board
/// says what it waits for rather than the lease the wake names no holder for
#[test]
fn a_turn_the_gateway_answered_busy_waits_on_its_gateway_and_not_on_a_lease() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let busy = Hold::default();
    let work = Hold::default();
    rig.claude.script([Scripted::Busy(
        busy.clone(),
        Box::new(Scripted::Hold(work.clone())),
    )]);
    rig.claude.set_activity(CallActivity::Nothing);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(
        busy.entered(CALL_PATIENCE),
        "the turn never said its gateway was busy"
    );
    assert_eq!(driven.heard(), None, "the wait is no report");

    rig.clock.advance(11 * 60);
    driven.pass();
    let text = board(&rig);
    let waiting = "  - Session: worker turn running for 11m, waiting for its model\n";
    assert!(text.contains(waiting), "{text}");
    let gateway = "  - Waiting for its model since 14:13 (11m): for its gateway, \
                    which answered busy\n";
    assert!(text.contains(gateway), "{text}");
    assert!(
        text.contains("#7: worker waits 11m for its gateway, which answered busy\n"),
        "{text}"
    );
    assert!(!text.contains(" lease"), "{text}");

    busy.release();
    assert_eq!(driven.heard(), None, "the grant is no report");
    assert!(work.entered(CALL_PATIENCE), "the turn never began");
    driven.pass();
    let text = board(&rig);
    assert!(
        text.contains("#7: worker has its gateway back, after waiting 11m\n"),
        "{text}"
    );
    assert!(!text.contains(" lease"), "{text}");
    work.release();
    assert!(work.answered(CALL_PATIENCE), "the turn never answered");
}

#[test]
fn the_events_shown_start_after_the_project_managers_cursor() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "drop", None);
    drop(runner);
    let path = rig.paths().state;
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(state["last_event"], 2);
    state["pm_seen"] = 1.into();
    std::fs::write(&path, state.to_string()).unwrap();

    rig.open().unwrap();
    let text = board(&rig);
    let events = "## Since your last wake\n\n\
                  - [2] 14:13 #7: work item dropped\n\n";
    assert!(text.contains(events), "{text}");
}

#[test]
fn a_turn_hung_before_its_transcript_exists_reads_as_idle() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    rig.claude.set_activity(CallActivity::Nothing);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the turn never began");
    rig.clock.advance(4 * 60);
    advance(&runner).unwrap();
    let text = board(&rig);
    let quiet = "  - Session: worker turn running for 4m, no tool call or output yet\n";
    assert!(text.contains(quiet), "{text}");

    rig.clock.advance(7 * 60);
    advance(&runner).unwrap();
    let text = board(&rig);
    let idle = "  - Session: worker turn running for 11m, idle: no tool call or output for 11m\n";
    assert!(text.contains(idle), "{text}");
    let event = "] 14:24 #7: worker idle, no tool call or output for 11m\n";
    assert!(text.contains(event), "{text}");
    hold.release();
    assert!(hold.answered(CALL_PATIENCE), "the turn never answered");
}

#[test]
fn a_failure_naming_a_path_on_this_machine_is_withheld_from_the_board() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let repo = rig.repo().display().to_string();
    let reason = format!("cannot read {repo}/src/lib.rs");
    let failed = AgentError::Failed(Harness::ClaudeCode, reason);
    rig.claude.script([Scripted::Fail(failed)]);
    step(&runner).unwrap();
    let text = board(&rig);
    assert!(!text.contains(&repo), "{text}");
    let failed = "] 14:13 #7: worker turn failed: (withheld: it names a path on this machine, \
                  which names its user)\n";
    assert!(text.contains(failed), "{text}");
}

#[test]
fn a_dropped_item_still_listed_ready_is_named_as_dropped_with_the_command_that_takes_it_back() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.forge.list_ready(7, false);
    step(&runner).unwrap();
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(&runner).unwrap();
    rig.ask(&runner, "drop", None);
    step(&runner).unwrap();
    rig.clock.advance(5 * 60);
    super::brief(&runner);
    let text = board(&rig);
    let waits = "waits: stopped with no merged pull request, and an edit to the issue, a label put back or `shep kelpie add 7` takes it back";
    assert!(text.contains(waits), "{text}");
    assert!(!text.contains("kelpie finished it"), "{text}");
}

#[test]
fn a_due_turn_that_has_not_started_names_what_it_waits_on_in_place_of_being_due() {
    let (rig, runner) = seven_and_eight("acme");
    rig.ask(&runner, "drain", None);
    assert!(matches!(
        crate::runner::flight::advance(&runner).unwrap(),
        Pass::Idle
    ));
    rig.clock.advance(5 * 60);
    assert!(matches!(
        crate::runner::flight::advance(&runner).unwrap(),
        Pass::Idle
    ));

    let text = board(&rig);
    let held = "  - Session: no call in flight; its first turn has waited 5m: \
                the runner is draining, so no turn starts\n";
    assert_eq!(text.matches(held).count(), 2, "{text}");
    assert!(!text.contains("turn is due"), "{text}");
}

#[test]
fn the_board_lists_the_projects_agents_from_their_files() {
    let rig = Rig::new("acme");
    rig.write_agent(
        "heavy",
        "---\nrole: implementer\nharness: claude-code\nmodel: claude-opus-5-5\neffort: high\n\
         description: Design work that\n  needs the strongest agent.\nkinds: [design, migration]\n\
         tier: heavy\n---\n",
    );
    rig.reviewers(&["claude", "qwen"]);
    let runner = rig.open().unwrap();
    let text = board(&rig);
    let section = text.split("## Agents\n").nth(1).unwrap();
    let section = section.split("\n## ").next().unwrap();
    let lines: Vec<&str> = section.lines().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(
        lines,
        [
            "- heavy (implementer): heavy tier, kinds design, migration; \
             Design work that needs the strongest agent.",
            "- opus-high (implementer): heavy tier, every kind; Builds work that is hard to \
             undo, such as a data migration, a release step or a change to a public interface.",
            "- sonnet-high (implementer): standard tier, every kind; Builds everyday features \
             and anything with state, such as a store, a queue or a migration.",
            "- claude (reviewer): standard tier, every kind; no description",
            "- qwen (reviewer): light tier, every kind; Reads a diff for defects with the \
             maintainer's local qwen-review script, as a first pass.",
        ],
        "{text}"
    );
    assert!(
        !text.contains("claude-opus-5-5"),
        "no model reaches the board"
    );
    drop(runner);
}
