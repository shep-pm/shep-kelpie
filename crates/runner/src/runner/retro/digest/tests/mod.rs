//! The digest for a recorded set of work items

use serde_json::{Value, json};

use super::{Open, Records, build, span};
use crate::ports::Timestamp;
use crate::runner::retro::journal::{Entry, Stamped};
use crate::runner::ruling::Record;
use crate::state::Finished;
use crate::test::ticket;
use crate::usage::Line;
use crate::work_item::Seconds;

mod across;
mod spend;

// No known gap is open
const NO_GAPS: &Open = &Open {
    listed: Vec::new(),
    left_out: 0,
};

fn finished(issue: u64, title: &str, pull_request: Option<u64>, at: u64, wall: u64) -> Finished {
    Finished {
        issue: ticket(issue),
        title: title.to_owned(),
        pull_request,
        merged: pull_request.is_some(),
        closed: pull_request.is_none(),
        at: Timestamp(at),
        wall,
        seconds: Seconds::default(),
        spend: None,
    }
}

fn call(at: u64, issue: Option<u64>, role: &str, kind: &str, units: u64, cost: f64) -> Line {
    line(json!({
        "line": "call", "at": at, "issue": issue, "role": role, "kind": kind,
        "usage": {"input": 0, "cache_write": 0, "cache_read": 0, "output": 0},
        "units": units, "cost_usd": cost, "ended": "answered",
    }))
}

fn line(value: Value) -> Line {
    serde_json::from_value(value).unwrap()
}

// A call whose harness reported no cost, as a local model's does
fn unpriced(at: u64, issue: Option<u64>, role: &str, kind: &str, units: u64) -> Line {
    line(json!({
        "line": "call", "at": at, "issue": issue, "role": role, "kind": kind,
        "usage": {"input": 0, "cache_write": 0, "cache_read": 0, "output": 0},
        "units": units, "unpriced": true, "ended": "answered",
    }))
}

// A call ended at its ceiling, which read no usage
fn timed_out(at: u64, issue: Option<u64>, role: &str, kind: &str) -> Line {
    line(json!({
        "line": "call", "at": at, "issue": issue, "role": role, "kind": kind,
        "usage": {"input": 0, "cache_write": 0, "cache_read": 0, "output": 0},
        "units": 0, "unpriced": true, "ended": "timed-out",
    }))
}

fn role(calls: u32, units: u64, cost: f64) -> Value {
    json!({
        "calls": calls, "units": units, "cost_usd": cost,
        "tokens": {"input": 0, "cache_write": 0, "cache_read": 0, "output": 0},
    })
}

fn ruling(value: Value) -> Record {
    serde_json::from_value(value).unwrap()
}

fn stamped(at: u64, entry: Entry) -> Stamped {
    Stamped {
        at: Timestamp(at),
        entry,
    }
}

fn wake(for_: &str, acted: &[&str]) -> Entry {
    Entry::Wake {
        woke_for: vec![for_.to_owned()],
        acted: acted.iter().map(|a| (*a).to_owned()).collect(),
        dropped: Vec::new(),
    }
}

// The wake whose answer started nothing
fn picked_none(for_: &str) -> Entry {
    Entry::Wake {
        woke_for: vec![for_.to_owned()],
        acted: vec!["starts nothing now".to_owned(), "holds #12".to_owned()],
        dropped: Vec::new(),
    }
}

// Two work items finished after a retro that began at 1000: #11 merged
// with a pull request, #12 closed with no change.
fn digest() -> String {
    let batch = [
        finished(11, "fix(retro): the digest", Some(71), 2000, 1500),
        finished(12, "Already done upstream", None, 3000, 42),
    ];
    let journal = [
        stamped(500, wake("#11 CI failed", &[])),
        stamped(1200, wake("#11 CI failed", &["marked #11 ready"])),
        stamped(2990, wake("#12 labels", &[])),
        stamped(1400, wake("#11 conflict, #110 too", &[])),
        stamped(
            1450,
            Entry::CiFailed {
                issue: ticket(11),
                checks: vec!["test".into()],
            },
        ),
        stamped(1500, Entry::Rebased { issue: ticket(11) }),
        stamped(
            1550,
            Entry::Conflicted {
                issue: ticket(11),
                files: vec!["docs/design-log.md".into()],
            },
        ),
        stamped(
            1600,
            Entry::Round {
                issue: ticket(11),
                round: 1,
                reviewer: "claude".into(),
                findings: 3,
            },
        ),
        stamped(
            1601,
            Entry::FindingsSent {
                issue: ticket(11),
                round: 1,
                held: 2,
            },
        ),
        stamped(
            1990,
            Entry::Friction {
                issue: ticket(11),
                text: "the lockfile\nfought me".into(),
            },
        ),
    ];
    let rulings = [
        ruling(json!({
            "id": 1, "at": 1900, "raised": 1600, "issue": 11, "pull_request": 71,
            "asked": "Merge?", "answer": "yes", "kind": "merge",
            "ballots": [
                {"voter": "worker", "vote": {"choice": "A", "why": "ok", "design": false}},
                {"voter": "advisor", "vote": null},
            ],
        })),
        ruling(json!({
            "id": 2, "at": 2900, "raised": 2810, "issue": 12, "pull_request": null,
            "asked": "Close?", "answer": "yes", "by_pm": true, "kind": "merge",
        })),
        ruling(json!({
            "id": 3, "at": 2950, "issue": 99, "pull_request": null,
            "asked": "Other?", "answer": "yes", "kind": "merge",
        })),
    ];
    let ledger = [
        call(1100, Some(11), "worker", "turn", 100, 1.50),
        call(1700, Some(11), "worker", "turn", 50, 0.50),
        call(1800, Some(11), "reviewer", "review", 30, 0.25),
        call(1250, None, "pm", "wake", 20, 0.10),
        call(500, None, "pm", "wake", 7, 0.07),
        call(2500, None, "pm", "retro", 400, 4.00),
        call(1900, Some(99), "worker", "turn", 9, 0.09),
        line(json!({
            "line": "finished", "at": 2000, "issue": 11, "title": "fix(retro): the digest",
            "pull_request": 71, "merged": true, "wall": 3725, "units": 180, "cost_usd": 2.25,
            "worker": role(2, 150, 2.0), "reviewer": role(1, 30, 0.25),
            "qwen": {"rounds": 0, "seconds": 0},
            "review_rounds": 1, "fix_turns": 1, "rulings": 1,
        })),
    ];
    build(&Records {
        project: "koji",
        batch: &batch,
        journal: &journal,
        rulings: &rulings,
        ledger: &ledger,
        since: 1000,
        gaps: NO_GAPS,
    })
}

fn section<'a>(digest: &'a str, heading: &str) -> &'a str {
    let from = digest
        .find(heading)
        .unwrap_or_else(|| panic!("{heading}: {digest}"));
    let rest = &digest[from..];
    let to = rest[heading.len()..]
        .find("\n## ")
        .map_or(rest.len(), |n| n + heading.len());
    &rest[..to]
}

// Every section with `heading`, in the order the digest lists them
fn sections<'a>(digest: &'a str, heading: &str) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut rest = digest;
    while let Some(at) = rest.find(heading) {
        let part = section(rest, heading);
        found.push(part);
        rest = &rest[at + heading.len()..];
    }
    found
}

// A digest of `batch` over `journal` and `ledger`, with the last retro at `since`
fn built(
    batch: &[Finished],
    journal: &[Stamped],
    ledger: &[Line],
    since: u64,
    gaps: &Open,
) -> String {
    build(&Records {
        project: "koji",
        batch,
        journal,
        rulings: &[],
        ledger,
        since,
        gaps,
    })
}

#[test]
fn the_header_names_the_project_and_the_batchs_issues() {
    let digest = digest();
    assert_eq!(
        digest.lines().next().unwrap(),
        "# Batch digest: koji, 2 finished work items (#11, #12)"
    );
}

#[test]
fn an_item_carries_its_title_type_and_ledger_line() {
    let digest = digest();
    let item = section(&digest, "## #11:");
    assert!(
        item.starts_with("## #11: fix(retro): the digest (fix)"),
        "{item}"
    );
    assert!(
        item.contains("- Ended: merged, pull request #71; wall time 25m00s"),
        "{item}"
    );
    assert!(
        item.contains(
            "- Ledger: 180 units, $2.25; worker 2 calls $2.00; reviewer 1 calls $0.25; \
             1 review rounds, 1 fix turns, 1 rulings"
        ),
        "{item}"
    );
}

#[test]
fn an_item_carries_the_wakes_that_named_it_and_what_they_did() {
    let digest = digest();
    let item = section(&digest, "## #11:");
    assert!(
        item.contains("- Project manager wakes that named it: 3"),
        "{item}"
    );
    assert!(
        item.contains("  - woke for #11 CI failed: changed nothing"),
        "{item}"
    );
    assert!(
        item.contains("  - woke for #11 CI failed: marked #11 ready"),
        "{item}"
    );
    // "#110" is another issue
    assert!(
        item.contains("  - woke for #11 conflict, #110 too: changed nothing"),
        "{item}"
    );
    let other = section(&digest, "## #12:");
    assert!(
        other.contains("- Project manager wakes that named it: 1"),
        "{other}"
    );
}

#[test]
fn an_item_carries_each_ruling_with_who_answered_it() {
    let digest = digest();
    let item = section(&digest, "## #11:");
    assert!(item.contains("- Rulings: 1"), "{item}");
    assert!(
        item.contains("  - ruling 1 (merge), waited 5m00s; answered by the maintainer: yes"),
        "{item}"
    );
    let other = section(&digest, "## #12:");
    assert!(
        other.contains("  - ruling 2 (merge), waited 1m30s; answered by the project manager: yes"),
        "{other}"
    );
}

#[test]
fn an_item_carries_its_review_rounds_ci_fixes_merges_and_friction() {
    let digest = digest();
    let item = section(&digest, "## #11:");
    for expected in [
        "- Review rounds: 1 read, 0 passed over, the latest numbered 1",
        "  - #11 round 1 by claude: 3 findings, 2 sent to the worker",
        "- Fix turns after a failed check: 1",
        "  - failed: test",
        "- Merges of main: 1 rebased, 1 conflicted",
        "  - conflict in docs/design-log.md",
        "- Friction the worker reported: the lockfile fought me",
    ] {
        assert!(item.contains(expected), "{expected}: {item}");
    }
}

#[test]
fn an_item_closed_with_no_change_says_so_and_has_no_ledger_line() {
    let digest = digest();
    let item = section(&digest, "## #12:");
    assert!(
        item.contains("- Ended: closed with no change; wall time 42s"),
        "{item}"
    );
    assert!(!item.contains("- Ledger:"), "{item}");
}

#[test]
fn the_totals_match_the_sums_by_hand() {
    let digest = digest();
    let totals = section(&digest, "## Batch totals");
    // Worker turns 100 + 50 units and $1.50 + $0.50, the reviewer's 30 and
    // $0.25, the project manager's one wake in the window (20, $0.10). Left
    // out: the wake before the retro, the retro's own call and #99's turn.
    // Rulings 1 and 2 waited 300s and 90s.
    let expected = "## Batch totals\n\
        - Spend by role and kind:\n  \
        - pm wake: 1 calls, 20 units, $0.10\n  \
        - reviewer review: 1 calls, 30 units, $0.25\n  \
        - worker turn: 2 calls, 150 units, $2.00\n\
        - Project manager wakes: 3, of which 2 changed nothing\n\
        - Rulings: 2, waiting 6m30s in all and 5m00s at the longest\n";
    assert_eq!(totals, expected);
}

#[test]
fn a_span_reads_in_the_largest_units() {
    assert_eq!(span(42), "42s");
    assert_eq!(span(310), "5m10s");
    assert_eq!(span(3900), "1h05m");
}

#[test]
fn a_first_retro_counts_only_the_calls_from_its_batchs_first_work_item_on() {
    // #11 began at 1400 and #12 at 1500. The wake at 1000 is before the
    // batch's first work item, so a project's first retro leaves it out.
    let batch = [
        finished(11, "fix(a): one", Some(71), 2000, 600),
        finished(12, "fix(b): two", Some(72), 2100, 600),
    ];
    let ledger = [
        call(1000, None, "pm", "wake", 500, 5.00),
        call(1500, None, "pm", "wake", 20, 0.10),
        call(1900, None, "pm", "wake", 30, 0.30),
    ];

    let digest = built(&batch, &[], &ledger, 0, NO_GAPS);

    let totals = section(&digest, "## Batch totals");
    assert!(
        totals.contains("- pm wake: 2 calls, 50 units, $0.40"),
        "{totals}"
    );
    assert!(!totals.contains("550 units"), "{totals}");
}

#[test]
fn a_work_item_that_ran_twice_shows_as_two_runs_each_with_its_own_entries() {
    // #11 ran from 1400 to 2000 and again, revived, from 2400 to 3000
    let batch = [
        finished(11, "fix(a): one", Some(71), 2000, 600),
        finished(11, "fix(a): one", Some(72), 3000, 600),
    ];
    let journal = [
        stamped(1500, wake("#11 CI failed", &["marked #11 ready"])),
        stamped(2050, wake("#11 reopened", &[])),
        stamped(2500, wake("#11 conflict", &[])),
    ];

    let digest = built(&batch, &journal, &[], 1000, NO_GAPS);

    let runs = sections(&digest, "## #11:");
    let [first, second] = runs.as_slice() else {
        panic!("one section per run: {runs:#?}");
    };
    assert!(first.contains("run 1 of 2"), "{first}");
    assert!(second.contains("run 2 of 2"), "{second}");
    assert!(
        first.contains("- Project manager wakes that named it: 1"),
        "{first}"
    );
    assert!(
        second.contains("- Project manager wakes that named it: 1"),
        "{second}"
    );
    assert!(
        second.contains("  - woke for #11 conflict: changed nothing"),
        "{second}"
    );
    assert!(!first.contains("#11 conflict"), "{first}");
    assert!(!second.contains("marked #11 ready"), "{second}");
    // A wake between the two runs belongs to neither
    assert!(!digest.contains("#11 reopened"), "{digest}");
}

#[test]
fn an_item_keeps_the_wake_that_picked_it_just_before_its_creation() {
    // #11 was created at 1000, and the wake that picked it ran a minute on
    let batch = [finished(11, "fix(a): one", Some(71), 2000, 1000)];
    let journal = [
        stamped(940, wake("#11 is ready", &["picked #11"])),
        stamped(2010, wake("#11 CI failed", &["marked #11 ready"])),
    ];

    let digest = built(&batch, &journal, &[], 0, NO_GAPS);

    let item = section(&digest, "## #11:");
    assert!(
        item.contains("- Project manager wakes that named it: 1"),
        "{item}"
    );
    assert!(
        item.contains("  - woke for #11 is ready: picked #11"),
        "{item}"
    );
    // A wake after the finish is another run's
    assert!(!item.contains("marked #11 ready"), "{item}");
}
