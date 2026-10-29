//! Which failing issues the triager reaches whatever their age or author, through
//! the runner's stand-ins

use std::sync::Mutex;

use serde_json::{Value, json};

use super::check_tests::{FAILING, REPO_LABELS, STILL, checks, filed, told};
use super::pick_tests::{MAINTAINER, due, issue, list, rig_with};
use super::research_tests::{APP, WORDS, calls, reply};
use crate::ports::{OpenIssue, Role, Timestamp};
use crate::runner::{Runner, step};
use crate::state::StateStore;
use crate::test::{Rig, Scripted};

// A rig whose triage was turned on at second 100, with `[triage]` set to `lines`
// and the repo holding `REPO_LABELS`
fn turned_on_late(lines: &str) -> Rig {
    let rig = rig_with(lines);
    rig.forge.set_repo_labels(&REPO_LABELS);
    rig.clock.advance(100);
    rig
}

// Issue `number` by `author`, opened before triage was turned on, failing the
// check, with `labels`
fn old(rig: &Rig, number: u64, author: &str, labels: &[&str]) -> OpenIssue {
    let mut old = issue(rig, number, author, 0);
    old.created = Timestamp(1);
    old.title = format!("Crash {number}");
    old.body = FAILING.to_owned();
    old.labels = labels.iter().map(|&l| l.to_owned()).collect();
    old
}

// The issues the App commented on, in order
fn commented(rig: &Rig) -> Vec<u64> {
    (rig.github.writes().into_iter())
        .filter(|(_, call)| call.path.ends_with("/comments"))
        .filter_map(|(_, call)| {
            call.path
                .split("/issues/")
                .nth(1)?
                .split('/')
                .next()?
                .parse()
                .ok()
        })
        .collect()
}

fn script_one_pass(rig: &Rig) {
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
}

#[test]
fn a_held_ready_issue_opened_before_triage_was_on_gets_a_pass_and_an_unlabelled_one_gets_none() {
    let rig = turned_on_late("pick = \"all\"");
    list(&rig, old(&rig, 30, "newcomer", &["ready-for-agent"]));
    list(&rig, old(&rig, 31, "newcomer", &[]));
    script_one_pass(&rig);
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(commented(&rig), [30]);
    assert_eq!(due(&rig, &runner), Vec::<u64>::new());
}

#[test]
fn a_held_ready_issue_opened_before_triage_was_on_is_due_under_maintainer_pick() {
    let rig = turned_on_late("pick = \"maintainer\"");
    list(&rig, old(&rig, 30, MAINTAINER, &["ready-for-agent"]));
    list(&rig, old(&rig, 31, MAINTAINER, &[]));
    script_one_pass(&rig);
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(commented(&rig), [30]);
}

#[test]
fn a_held_issue_the_projects_app_opened_gets_a_pass_and_another_bots_gets_none() {
    for pick in ["all", "maintainer"] {
        let rig = turned_on_late(&format!("pick = \"{pick}\""));
        let mut ours = old(&rig, 30, APP, &["ready-for-agent"]);
        ours.author_is_bot = true;
        ours.created = Timestamp(150);
        let mut theirs = old(&rig, 31, "dependabot[bot]", &["ready-for-agent"]);
        theirs.author_is_bot = true;
        theirs.created = Timestamp(150);
        list(&rig, ours);
        list(&rig, theirs);
        script_one_pass(&rig);
        let runner = rig.open().unwrap();

        assert_eq!(step(&runner).unwrap(), None, "{pick}");

        assert_eq!(calls(&rig, Role::Triager).len(), 1, "{pick}");
        assert_eq!(commented(&rig), [30], "{pick}");
        let kept = checks(&rig);
        let kept: Vec<u64> = (kept.as_array().unwrap().iter())
            .map(|c| c["issue"].as_u64().unwrap())
            .collect();
        assert_eq!(kept, [30], "the other bot's issue is not checked: {pick}");
    }
}

// The saved check result of issue 30 given a key from an older check
fn stale(rig: &Rig, runner: Mutex<Runner>) {
    drop(runner);
    let store = StateStore::new(rig.paths().state);
    let mut saved: Value = serde_json::to_value(store.load().unwrap().unwrap()).unwrap();
    let key = saved["checks"][0]["key"].as_u64().unwrap();
    saved["checks"][0]["key"] = (key ^ 1).into();
    store.save(&serde_json::from_value(saved).unwrap()).unwrap();
}

#[test]
fn a_done_issue_still_failing_gets_one_new_pass_when_the_check_version_changes() {
    let (rig, runner) = told();
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    stale(&rig, runner);
    script_one_pass(&rig);
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        calls(&rig, Role::Triager).len(),
        2,
        "the new check, a new pass"
    );

    // The same version again: the issue is checked, told and left alone.
    list(&rig, filed(&rig, MAINTAINER, STILL, &["needs-triage"]));
    for _ in 0..3 {
        rig.next_look();
        assert_eq!(step(&runner).unwrap(), None);
    }
    assert_eq!(calls(&rig, Role::Triager).len(), 2, "no second pass");
}

#[test]
fn the_daily_cap_still_stops_the_passes_of_old_held_issues() {
    let rig = turned_on_late("pick = \"all\"\nper_day = 1");
    for number in 30..33 {
        list(&rig, old(&rig, number, "newcomer", &["ready-for-agent"]));
    }
    script_one_pass(&rig);
    let runner = rig.open().unwrap();

    for _ in 0..4 {
        assert_eq!(step(&runner).unwrap(), None);
        rig.next_look();
    }

    assert_eq!(calls(&rig, Role::Triager).len(), 1);
}
