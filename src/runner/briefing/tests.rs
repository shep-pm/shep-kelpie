//! The board briefing, as the runner writes it through its stand-ins

use std::sync::Mutex;
use std::time::Duration;

use crate::ports::{AgentError, CallActivity, Timestamp};
use crate::runner::flight::advance;
use crate::runner::{Pass, Runner, step};
use crate::settings::Harness;
use crate::test::{Hold, Rig, Scripted, git};

// Real threads on real time, so a held call's wait has this ceiling.
const PATIENCE: Duration = Duration::from_secs(30);

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
    rig.edit_settings(|s| s.replace("max_items = 1", "max_items = 3"));
    rig.forge.list_ready(9, false);
    rig.forge.label(9, "priority: P1");
    rig.forge.block(9, 4);
    rig.forge.set_ready_body(
        9,
        "Reword `README.md`, then `src/missing.rs:12`.\n\n\n\nSee `x y` and `--flag`.",
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
             0 of 1 work items open. Main is at {}.\n\
             \n\
             ## Open work\n\
             \n\
             (none)\n\
             \n\
             ## Ready queue\n\
             \n\
             Not read since the runner started. The board reads it while a slot is free.\n\
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

2 of 3 work items open. Main is at MAIN.

## Open work

- #7 "Title of #7": review round 2; PR #71, opened 5m ago, on sonnet-high; touches 1 file
  - Session: no call in flight; the worker's last turn ended 5m ago
  - Last turn: "pushed"
- #8 "Title of #8": review round 2; PR #81, opened 5m ago, on sonnet-high; touches 1 file
  - Session: no call in flight; the worker's last turn ended 5m ago
  - Last turn: "pushed"

## Ready queue, read 5m ago (board rule order: priority, then oldest by number)

1. #9 [P1] "Title of #9"; waits: blocked by #4; names README.md, src/missing.rs

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
"#;

#[test]
fn a_worker_silent_past_ten_minutes_reads_as_idle_until_it_shows_something_again() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(PATIENCE), "the turn never began");
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
    assert!(hold.answered(PATIENCE), "the turn never answered");
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
    assert!(hold.entered(PATIENCE), "the turn never began");
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
    assert!(hold.answered(PATIENCE), "the turn never answered");
}

#[test]
fn a_summary_naming_a_private_name_is_withheld_from_the_board() {
    let rig = Rig::new("acme");
    rig.edit_settings(|s| {
        s.replace(
            "# private_names = [\"Acme Corp\"]",
            "private_names = [\"Kestrel\"]",
        )
    });
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude
        .script([Scripted::Say("Done for Kestrel, nothing pushed yet.")]);
    step(&runner).unwrap();
    let text = board(&rig);
    let withheld =
        "  - Last turn: \"(withheld: it names a name on this project's private list)\"\n";
    assert!(text.contains(withheld), "{text}");
    assert!(!text.contains("Kestrel"), "{text}");
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
