//! `triage.pick = "label"`: someone else's issue is triaged only once the
//! maintainer puts `needs-triage` on it, through the runner's stand-ins

use super::check_tests::{FAILING, PASSING, PASSING_LABELS, checking};
use super::research_tests::{calls, moves};
use crate::board::TRIAGE;
use crate::ports::Role;
use crate::runner::step;

#[test]
fn someone_elses_issue_gets_needs_approval_with_no_call() {
    let (rig, runner) = checking("pick = \"label\"", "newcomer", PASSING, &PASSING_LABELS);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 0);
    assert_eq!(moves(&rig), [format!("label {TRIAGE}")]);
    assert_eq!(rig.github.comments(), []);
}

#[test]
fn a_failing_issue_of_someone_elses_also_waits_with_no_call() {
    let (rig, runner) = checking("pick = \"label\"", "newcomer", FAILING, &[]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 0);
    assert_eq!(moves(&rig), [format!("label {TRIAGE}")]);
    assert_eq!(rig.github.comments(), []);
}
