//! What only shows across the batch: conflicts, rounds, findings, flakes and known gaps

use super::*;
use crate::ports::Severity;
use crate::runner::retro::journal::Held;

fn conflicted(issue: u64, files: &[&str]) -> Entry {
    Entry::Conflicted {
        issue: ticket(issue),
        files: files.iter().map(|f| (*f).to_owned()).collect(),
    }
}

fn outside(issue: u64, tests: &[&str]) -> Entry {
    Entry::CiOutsideDiff {
        issue: ticket(issue),
        tests: tests.iter().map(|t| (*t).to_owned()).collect(),
    }
}

fn round(issue: u64, round: u32, findings: usize) -> Entry {
    Entry::Round {
        issue: ticket(issue),
        round,
        reviewer: "claude".into(),
        findings,
    }
}

fn passed_over(issue: u64, round: u32, reviewer: &str, reason: &str) -> Entry {
    Entry::ReviewerSkipped {
        issue: ticket(issue),
        round,
        reviewer: reviewer.to_owned(),
        reason: reason.to_owned(),
    }
}

fn held(severity: Severity, file: &str, line: u32, what: &str) -> Held {
    Held {
        severity,
        file: file.to_owned(),
        line,
        what: what.to_owned(),
        again: false,
    }
}

// The known gaps the digest is told, with how many its limit left out
fn known(listed: &[&str], left_out: usize) -> Open {
    Open {
        listed: listed.iter().map(|g| (*g).to_owned()).collect(),
        left_out,
    }
}

// Three merged work items, all finished after the last retro
fn across(journal: &[Stamped], gaps: &Open) -> String {
    let batch = [
        finished(11, "fix(a): one", Some(71), 2000, 1200),
        finished(12, "fix(b): two", Some(72), 2100, 1200),
        finished(13, "fix(c): three", Some(73), 2200, 1200),
    ];
    build(&Records {
        project: "koji",
        batch: &batch,
        journal,
        rulings: &[],
        ledger: &[],
        since: 1000,
        gaps,
    })
}

#[test]
fn the_conflicts_per_file_match_the_hand_count_of_a_recorded_batch() {
    // By hand: design-log.md conflicted four times, on #11, #12 and #13;
    // src/settings.rs once, on #11, and src/state.rs once, on #13. #99 is
    // not in the batch.
    let journal = [
        stamped(
            1100,
            conflicted(11, &["docs/design-log.md", "src/settings.rs"]),
        ),
        stamped(1200, conflicted(12, &["docs/design-log.md"])),
        stamped(
            1300,
            conflicted(13, &["docs/design-log.md", "src/state.rs"]),
        ),
        stamped(1400, conflicted(13, &["docs/design-log.md"])),
        stamped(1500, conflicted(99, &["src/settings.rs", "src/state.rs"])),
    ];

    let digest = across(&journal, NO_GAPS);

    let table = section(&digest, "## Across the batch");
    let expected = "- Merge conflicts per file, most first:\n  \
        - docs/design-log.md: 4 conflicts on #11, #12, #13\n  \
        - src/settings.rs: 1 conflicts on #11\n  \
        - src/state.rs: 1 conflicts on #13\n";
    assert!(table.contains(expected), "{table}");
}

#[test]
fn a_batch_with_no_conflict_says_so() {
    let digest = across(&[], NO_GAPS);
    let table = section(&digest, "## Across the batch");
    assert!(
        table.contains("- Merge conflicts per file: none\n"),
        "{table}"
    );
}

#[test]
fn rounds_per_item_and_each_findings_round_are_given() {
    let journal = [
        stamped(1100, round(11, 1, 2)),
        stamped(
            1101,
            Entry::Findings {
                issue: ticket(11),
                round: 1,
                findings: vec![
                    held(Severity::Low, "src/a.rs", 4, "a nit"),
                    held(Severity::Medium, "src/b.rs", 0, "wrong wording"),
                ],
            },
        ),
        stamped(1200, round(11, 2, 0)),
        stamped(1300, round(13, 1, 1)),
        stamped(1400, round(13, 2, 1)),
        stamped(1500, round(13, 3, 1)),
        stamped(
            1501,
            Entry::Findings {
                issue: ticket(13),
                round: 3,
                findings: vec![held(Severity::High, "src/gate.rs", 88, "the design leaks")],
            },
        ),
    ];

    let digest = across(&journal, NO_GAPS);

    let table = section(&digest, "## Across the batch");
    let expected = "- Review rounds per work item:\n  \
        - #11: 2 read, 0 passed over\n  \
        - #12: 0 read, 0 passed over\n  \
        - #13: 3 read, 0 passed over\n";
    assert!(table.contains(expected), "{expected}: {table}");
    let item = section(&digest, "## #11:");
    for expected in [
        "- Review rounds: 2 read, 0 passed over, the latest numbered 2",
        "- Findings sent to the worker: 2\n  \
         - round 1 by claude, low, src/a.rs:4: a nit\n  \
         - round 1 by claude, medium, src/b.rs: wrong wording\n",
    ] {
        assert!(item.contains(expected), "{expected}: {item}");
    }
    let late = section(&digest, "## #13:");
    assert!(
        late.contains("  - round 3 (late) by claude, high, src/gate.rs:88: the design leaks"),
        "{late}"
    );
}

#[test]
fn a_finding_after_two_passed_over_rounds_is_not_late() {
    let mut journal = vec![
        stamped(1100, passed_over(11, 1, "cubic", "no review")),
        stamped(1200, passed_over(11, 2, "greptile", "no review")),
    ];
    for (at, number, what) in [
        (1300, 3, "first read"),
        (1400, 4, "second read"),
        (1500, 5, "third read"),
    ] {
        journal.push(stamped(at, round(11, number, 1)));
        journal.push(stamped(
            at + 1,
            Entry::Findings {
                issue: ticket(11),
                round: number,
                findings: vec![held(Severity::High, "src/gate.rs", 88, what)],
            },
        ));
    }

    let digest = across(&journal, NO_GAPS);

    let item = section(&digest, "## #11:");
    let expected = "- Findings sent to the worker: 3\n  \
        - round 1 by claude, high, src/gate.rs:88: first read\n  \
        - round 2 by claude, high, src/gate.rs:88: second read\n  \
        - round 3 (late) by claude, high, src/gate.rs:88: third read\n";
    assert!(item.contains(expected), "{item}");
    for expected in [
        "- Review rounds: 3 read, 2 passed over, the latest numbered 5",
        "  - #11 round 3 by claude: 1 findings",
        "  - #11 round 4 by claude: 1 findings",
        "  - #11 round 5 by claude: 1 findings",
    ] {
        assert!(item.contains(expected), "{expected}: {item}");
    }
    let table = section(&digest, "## Across the batch");
    assert!(
        table.contains("  - #11: 3 read, 2 passed over\n"),
        "{table}"
    );
}

#[test]
fn findings_count_reads_in_journal_order_across_review_passes() {
    let mut journal = Vec::new();
    for (at, number, reviewer, what) in [
        (1100, 1, "claude", "first read"),
        (1200, 2, "claude", "second read"),
        (1300, 1, "codex", "third read"),
        (1400, 1, "claude", "fourth read"),
    ] {
        journal.push(stamped(
            at,
            Entry::Round {
                issue: ticket(11),
                round: number,
                reviewer: reviewer.into(),
                findings: 1,
            },
        ));
        journal.push(stamped(
            at,
            Entry::Findings {
                issue: ticket(11),
                round: number,
                findings: vec![held(Severity::High, "src/gate.rs", 88, what)],
            },
        ));
    }

    let digest = across(&journal, NO_GAPS);

    let item = section(&digest, "## #11:");
    let expected = "- Findings sent to the worker: 4\n  \
        - round 1 by claude, high, src/gate.rs:88: first read\n  \
        - round 2 by claude, high, src/gate.rs:88: second read\n  \
        - round 3 (late) by codex, high, src/gate.rs:88: third read\n  \
        - round 4 (late) by claude, high, src/gate.rs:88: fourth read\n";
    assert!(item.contains(expected), "{item}");
}

#[test]
fn a_clean_round_and_a_passed_over_reviewer_each_show_numbered() {
    let journal = [
        stamped(
            1100,
            passed_over(11, 1, "codex", "its window opens more than an hour on"),
        ),
        stamped(1200, round(11, 2, 0)),
        stamped(1300, round(11, 3, 2)),
        stamped(
            1301,
            Entry::FindingsSent {
                issue: ticket(11),
                round: 3,
                held: 2,
            },
        ),
    ];

    let digest = across(&journal, NO_GAPS);

    let item = section(&digest, "## #11:");
    for expected in [
        "- Review rounds: 2 read, 1 passed over, the latest numbered 3",
        "  - #11 round 1 by codex: passed over, its window opens more than an hour on",
        "  - #11 round 2 by claude: 0 findings",
        "  - #11 round 3 by claude: 2 findings, 2 sent to the worker",
    ] {
        assert!(item.contains(expected), "{expected}: {item}");
    }
    let table = section(&digest, "## Across the batch");
    assert!(
        table.contains("- Review rounds per work item:\n  - #11: 2 read, 1 passed over\n"),
        "{table}"
    );
}

#[test]
fn a_finding_a_later_round_raised_again_is_marked() {
    let journal = [
        stamped(1100, round(11, 1, 1)),
        stamped(
            1101,
            Entry::Findings {
                issue: ticket(11),
                round: 1,
                findings: vec![held(Severity::High, "src/a.rs", 4, "the lock leaks")],
            },
        ),
        stamped(1200, round(11, 2, 2)),
        stamped(
            1201,
            Entry::Findings {
                issue: ticket(11),
                round: 2,
                findings: vec![
                    held(Severity::High, "src/a.rs", 4, "the lock leaks").again(true),
                    held(Severity::Medium, "src/b.rs", 9, "a new defect"),
                ],
            },
        ),
    ];

    let digest = across(&journal, NO_GAPS);

    let item = section(&digest, "## #11:");
    let expected = "- Findings sent to the worker: 3\n  \
        - round 1 by claude, high, src/a.rs:4: the lock leaks\n  \
        - round 2 (again) by claude, high, src/a.rs:4: the lock leaks\n  \
        - round 2 by claude, medium, src/b.rs:9: a new defect\n";
    assert!(item.contains(expected), "{expected}: {item}");
}

#[test]
fn a_test_failing_outside_two_items_diffs_is_a_known_flake_and_one_item_is_not() {
    let journal = [
        stamped(
            1100,
            outside(11, &["sheep::tests::reconnects", "only::here"]),
        ),
        stamped(1200, outside(12, &["sheep::tests::reconnects"])),
        stamped(1300, outside(12, &["sheep::tests::reconnects"])),
        stamped(1400, outside(99, &["only::here"])),
    ];

    let digest = across(&journal, NO_GAPS);

    let table = section(&digest, "## Across the batch");
    assert!(
        table.contains("- Known flakes:\n  - sheep::tests::reconnects: #11, #12\n"),
        "{table}"
    );
    assert!(!table.contains("only::here"), "{table}");
}

#[test]
fn a_known_gap_is_counted_by_the_items_that_name_it_and_not_described_again() {
    let gaps = known(&["sandbox cannot bind a socket", "stale lockfile"], 0);
    let journal = [
        stamped(
            1100,
            Entry::Friction {
                issue: ticket(11),
                text: "The Sandbox cannot bind a socket, so tests only ran in CI".into(),
            },
        ),
        stamped(1200, wake("#12 sandbox cannot bind a socket", &[])),
        stamped(1300, wake("#13 something else", &[])),
    ];

    let digest = across(&journal, &gaps);

    let table = section(&digest, "## Across the batch");
    assert!(
        table.contains(
            "- Known gaps named again in this batch:\n  \
             - sandbox cannot bind a socket: #11, #12\n"
        ),
        "{table}"
    );
    assert!(!table.contains("stale lockfile"), "{table}");
}

#[test]
fn the_across_section_names_the_known_gaps_the_digest_left_out() {
    let gaps = known(&["sandbox cannot bind a socket"], 5);
    let journal = [stamped(
        1100,
        Entry::Friction {
            issue: ticket(11),
            text: "The sandbox cannot bind a socket, so tests only ran in CI".into(),
        },
    )];

    let digest = across(&journal, &gaps);

    let table = section(&digest, "## Across the batch");
    assert!(
        table.contains(
            "- Known gaps named again in this batch, 5 of 6 known gaps left out:\n  \
             - sandbox cannot bind a socket: #11\n"
        ),
        "{table}"
    );
}

#[test]
fn a_wake_that_only_holds_an_item_back_counts_none_of_that_wake() {
    let journal = [stamped(
        1100,
        wake("#11 CI failed", &["holds #12, #13", "marked #11 ready"]),
    )];

    let digest = across(&journal, NO_GAPS);

    for (issue, wakes) in [(11, 1), (12, 0), (13, 0)] {
        let item = section(&digest, &format!("## #{issue}:"));
        let expected = format!("- Project manager wakes that named it: {wakes}");
        assert!(item.contains(&expected), "{expected}: {item}");
    }
}

#[test]
fn a_pick_of_none_counts_as_a_wake_that_changed_nothing() {
    let journal = [
        stamped(1100, picked_none("#11 stalled")),
        stamped(1200, wake("#12 CI failed", &["picked #13"])),
    ];

    let digest = across(&journal, NO_GAPS);

    let item = section(&digest, "## #11:");
    assert!(
        item.contains("  - woke for #11 stalled: changed nothing"),
        "{item}"
    );
    let totals = section(&digest, "## Batch totals");
    assert!(
        totals.contains("- Project manager wakes: 2, of which 1 changed nothing"),
        "{totals}"
    );
}
