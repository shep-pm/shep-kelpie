//! Whether the head a timed-out or failed turn's ruling was raised at moved
//! since the last of either kind on the item, as its case says it

use super::rig::{append_lines, case, part, past_at, timed_out, timed_out_committing, wake};
use crate::test::git;
#[test]
fn a_first_timeout_says_it_is_the_first() {
    let (rig, runner, head) = timed_out("case-first-timeout");

    wake(&rig, &runner);

    let part = part(&rig, "## Past rulings");
    assert!(
        part.contains(&format!("Its worktree stood at `{head}`")),
        "{part}"
    );
    assert!(
        part.contains("This is the first turn-timeout or turn-failed on this item"),
        "{part}"
    );
}

#[test]
fn a_third_timeout_says_the_head_did_not_move() {
    let (rig, runner, head) = timed_out("case-still-timeout");
    let again = |at| past_at(7, at, "turn-timeout", &head);
    append_lines(rig.paths().rulings, &[again(1), again(2)]);

    wake(&rig, &runner);

    let part = part(&rig, "## Past rulings");
    assert!(
        part.contains("this is the 3rd turn-timeout on this item"),
        "{part}"
    );
    assert!(part.contains("The head has not moved since."), "{part}");
}

#[test]
fn a_moved_head_names_its_commits_and_files() {
    let (rig, runner, head) = timed_out_committing("case-moved-timeout");
    let before = git(&rig.repo(), &["rev-parse", "origin/main"]);
    assert_ne!(before, head, "the worker committed without pushing");
    append_lines(
        rig.paths().rulings,
        &[past_at(7, 1, "turn-timeout", &before)],
    );

    wake(&rig, &runner);

    let part = part(&rig, "## Past rulings");
    assert!(part.contains("1 commit and 1 file since"), "{part}");
    assert!(part.contains("- `done.rs`"), "{part}");
}

#[test]
fn a_head_the_clone_no_longer_holds_says_the_change_could_not_be_read() {
    let (rig, runner, _) = timed_out("case-gone-head");
    let gone = "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f";
    append_lines(rig.paths().rulings, &[past_at(7, 1, "turn-timeout", gone)]);

    wake(&rig, &runner);

    let part = part(&rig, "## Past rulings");
    assert!(
        part.contains(&format!("at `{gone}` when the last")),
        "{part}"
    );
    assert!(
        part.contains("Kelpie could not read what changed between them"),
        "{part}"
    );
    assert!(case(&rig, 7).is_some(), "the case goes on without it");
}

#[test]
fn a_timeout_after_a_failed_turn_compares_with_that_turns_head() {
    let (rig, runner, head) = timed_out("case-after-failed");
    let older = past_at(
        7,
        1,
        "turn-timeout",
        "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f",
    );
    let failed = past_at(7, 2, "turn-failed", &head);
    append_lines(rig.paths().rulings, &[older, failed]);

    wake(&rig, &runner);

    let part = part(&rig, "## Past rulings");
    assert!(part.contains("The head has not moved since."), "{part}");
    assert!(
        !part.contains("0f0f0f0f"),
        "the older head was compared with: {part}"
    );
}
