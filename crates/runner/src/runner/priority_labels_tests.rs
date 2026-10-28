//! The priority labels the runner makes on a repo that lacks them, through the
//! runner's stand-ins

use crate::test::Rig;

const ALL: [&str; 5] = [
    "priority: P0",
    "priority: P1",
    "priority: P2",
    "priority: P3",
    "priority: P4",
];

fn made(rig: &Rig) -> Vec<String> {
    let mut have = rig.forge.repo_labels_now();
    have.retain(|l| crate::board::is_priority_label(l));
    have.sort();
    have
}

#[test]
fn a_runner_opening_on_a_repo_with_no_priority_labels_makes_all_five() {
    let rig = Rig::new("shep");
    let _runner = rig.open().unwrap();
    assert_eq!(made(&rig), ALL);
}

#[test]
fn a_priority_the_repo_has_under_either_name_is_not_made_again() {
    let rig = Rig::new("shep");
    rig.forge.set_repo_labels(&["priority: P2", "P3", "p4"]);
    let _runner = rig.open().unwrap();
    assert_eq!(
        made(&rig),
        ["P3", "p4", "priority: P0", "priority: P1", "priority: P2"],
        "P0 and P1 are made; the repo's own P2, P3 and P4 stand"
    );
}

#[test]
fn a_project_on_the_local_store_makes_no_priority_labels() {
    let rig = Rig::new("shep");
    rig.tickets_local();
    let _runner = rig.open().unwrap();
    assert_eq!(made(&rig), Vec::<String>::new());
}
