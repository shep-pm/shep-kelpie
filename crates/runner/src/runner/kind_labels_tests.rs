//! The kind labels the runner makes on a repo that lacks them, through the
//! runner's stand-ins

use crate::board::check;
use crate::test::Rig;

const ALL: [&str; 9] = [
    "kind:chore",
    "kind:ci",
    "kind:docs",
    "kind:feat",
    "kind:fix",
    "kind:perf",
    "kind:refactor",
    "kind:style",
    "kind:test",
];

fn made(rig: &Rig) -> Vec<String> {
    let mut have = rig.forge.repo_labels_now();
    have.retain(|l| check::kind_of(l).is_some());
    have.sort();
    have
}

#[test]
fn a_runner_opening_on_a_repo_with_no_kind_labels_makes_all_nine() {
    let rig = Rig::new("shep");
    let _runner = rig.open().unwrap();
    assert_eq!(made(&rig), ALL);
}

#[test]
fn a_kind_the_repo_has_in_any_spelling_is_not_made_again() {
    let rig = Rig::new("shep");
    rig.forge.set_repo_labels(&["Kind:Feat", "kind: fix"]);
    let _runner = rig.open().unwrap();
    let mut want = vec!["Kind:Feat", "kind: fix"];
    want.extend(
        ALL.iter()
            .filter(|l| !["kind:feat", "kind:fix"].contains(l)),
    );
    want.sort_unstable();
    assert_eq!(
        made(&rig),
        want,
        "feat and fix stand as the repo spells them"
    );
}

#[test]
fn a_project_on_the_local_store_makes_no_kind_labels() {
    let rig = Rig::new("shep");
    rig.tickets_local();
    let _runner = rig.open().unwrap();
    assert_eq!(made(&rig), Vec::<String>::new());
}
