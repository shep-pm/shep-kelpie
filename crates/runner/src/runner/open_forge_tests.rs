//! The forge calls a runner's start makes after its socket opened, through the
//! runner's stand-ins

use std::sync::Mutex;

use super::Runner;
use crate::board::check;
use crate::ports::ForgeError;
use crate::test::Rig;

// The rig's project is `shep-pm/shep`, so its owner's repository is this
const OWNERS: &str = "shep-pm/.github";

const KINDS: [&str; 9] = [
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

const PRIORITIES: [&str; 5] = [
    "priority: P0",
    "priority: P1",
    "priority: P2",
    "priority: P3",
    "priority: P4",
];

const AGENTS: [&str; 2] = ["agent:opus-high", "agent:sonnet-high"];

// The repo's labels under `keep`, oldest spelling first
fn labels(rig: &Rig, keep: impl Fn(&str) -> bool) -> Vec<String> {
    let mut have = rig.forge.repo_labels_now();
    have.retain(|l| keep(l));
    have.sort();
    have
}

fn kinds(rig: &Rig) -> Vec<String> {
    labels(rig, |l| check::kind_of(l).is_some())
}

fn priorities(rig: &Rig) -> Vec<String> {
    labels(rig, crate::board::is_priority_label)
}

fn agents(rig: &Rig) -> Vec<String> {
    labels(rig, |l| {
        !crate::board::is_priority_label(l) && check::kind_of(l).is_none()
    })
}

// Where the runner's pull request template comes from, as `status` says
fn template_of(runner: &Mutex<Runner>) -> serde_json::Value {
    let runner = runner.lock().unwrap();
    serde_json::to_value(runner.status()).unwrap()["pull_request_template"].clone()
}

#[test]
fn an_opened_runner_has_made_no_paced_forge_call() {
    let rig = Rig::new("shep");
    rig.forge
        .put_file(OWNERS, ".github/pull_request_template.md", "## Summary\n");
    let runner = rig.opened().unwrap();
    assert_eq!(rig.forge.asked(), 0, "{:?}", rig.forge.issue_writes());
    assert_eq!(rig.forge.repo_labels_now(), Vec::<String>::new());
    assert_eq!(rig.forge.issue_writes(), Vec::<String>::new());
    assert_eq!(template_of(&runner), "none");
}

#[test]
fn settling_on_the_forge_makes_the_labels_and_reads_the_owner_template() {
    let rig = Rig::new("shep");
    rig.forge
        .put_file(OWNERS, ".github/pull_request_template.md", "## Summary\n");
    let runner = rig.opened().unwrap();
    assert_eq!(rig.forge.asked(), 0, "the start asked the forge");

    runner.lock().unwrap().settle_on_forge();

    assert_eq!(template_of(&runner), serde_json::json!({ "owner": OWNERS }));
    assert_eq!(agents(&rig), AGENTS);
    assert_eq!(priorities(&rig), PRIORITIES);
    assert_eq!(kinds(&rig), KINDS);
}

#[test]
fn a_visibility_read_that_fails_is_noted_and_the_runner_goes_on() {
    let rig = Rig::new("shep");
    rig.reviewers(&["coderabbit"]);
    rig.forge.set_visibility_down(ForgeError::Failed(
        "gh: error connecting to api.github.com".into(),
    ));
    let runner = rig
        .open_allowing(&["sonnet-high", "coderabbit"])
        .expect("the runner opens");

    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes.iter().any(|line| {
            line.starts_with("cannot read shep-pm/shep's visibility: ")
                && line.ends_with(
                    "so kelpie cannot tell whether reviewer coderabbit is left out of the \
                     agent board's ask",
                )
        }),
        "{notes:?}"
    );
    assert_eq!(kinds(&rig), KINDS, "the labels the repo lacks are made");
}
