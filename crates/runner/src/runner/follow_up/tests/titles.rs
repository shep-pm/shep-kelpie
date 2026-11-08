//! The title and duplicate check of a filed follow-up, through the runner's stand-ins

use super::*;

// Each `what` is a clause or two, as the reviewer prompt asks for
const WHATS: [(&str, &str); 5] = [
    (
        "Distinguish publication failures from directory-sync failures: On Unix, if the parent sync fails after the rename, the file is published.",
        "Distinguish publication failures from directory-sync failures",
    ),
    (
        "The issue's first acceptance criterion is not met. The diff never changes the Windows path.",
        "The issue's first acceptance criterion is not met",
    ),
    (
        "The issue says `Harness` is matched only in the facts table, the serde names and the docs; the diff also renames it.",
        "The issue says `Harness` is matched only in the facts table, the serde …",
    ),
    (
        "The issue is done only when a pi worker turn on a project with the code graph has run, which no test does.",
        "The issue is done only when a pi worker turn on a project with the …",
    ),
    (
        "The new Codex unreachable fixtures are explicitly written from recall rather than recorded from a run.",
        "The new Codex unreachable fixtures are explicitly written from recall …",
    ),
];

#[test]
fn a_title_is_the_findings_first_clause_cut_at_a_whole_word_within_72_characters() {
    let found: Vec<Finding> = (WHATS.iter().enumerate())
        .map(|(n, (what, _))| finding(&format!("src/f{n}.rs"), what))
        .collect();
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));

    assert_eq!(
        after_yes(&runner),
        filed(&[900, 901, 902, 903, 904], &[], 0)
    );
    let titles: Vec<String> = rig.forge.created().into_iter().map(|i| i.title).collect();
    assert_eq!(titles, WHATS.map(|(_, title)| title));
    assert!(titles.iter().all(|t| t.chars().count() <= 72), "{titles:?}");
}

#[test]
fn a_what_with_no_clause_break_over_72_characters_ends_on_a_whole_word_and_an_ellipsis() {
    let what = "Two threads write the same field without a lock so a reader may see half of it";
    let found = [finding("src/lib.rs", what)];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));

    assert_eq!(after_yes(&runner), filed(&[900], &[], 0));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(
        issue.title,
        "Two threads write the same field without a lock so a reader may see …"
    );
    assert!(issue.body.contains(what), "the full what stays in the body");
}

#[test]
fn a_dotted_abbreviation_does_not_end_a_title() {
    let what =
        "Guard the shared map with a lock, e.g. a Mutex, so two workers cannot write it at once";
    let found = [finding("src/lib.rs", what)];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));

    assert_eq!(after_yes(&runner), filed(&[900], &[], 0));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(
        issue.title,
        "Guard the shared map with a lock, e.g. a Mutex, so two workers cannot …"
    );
}

#[test]
fn a_finding_filed_before_under_its_80_character_title_is_not_filed_again() {
    let what = "Distinguish publication failures from directory-sync failures: On Unix, if the parent sync fails";
    let found = [finding("src/lib.rs", what)];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
    let old_title: String = what.chars().take(80).collect();
    rig.forge
        .open_issue(61, &old_title, &format!("In `src/lib.rs`: {what}"));

    assert_eq!(after_yes(&runner), filed(&[], &[61], 0));
    assert_eq!(rig.forge.created(), []);
}
