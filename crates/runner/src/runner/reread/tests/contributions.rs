use std::sync::Mutex;

use crate::ports::CLEAN;
use crate::runner::Runner;
use crate::runner::contributions::pass_tests::{asked, listing, read_through, write};
use crate::runner::contributions::security_tests::{commit_on, contribute};
use crate::runner::contributions::tests::{followed, opened, picking, poll};
use crate::settings::Settings;
use crate::test::{Rig, Scripted};

use super::{list_local, paddock_keyed_by_cargo, settings_with};

const TABLE: &str = "\n[app.dogs.kelpie.contributions]\n";

fn reread(rig: &Rig, runner: &Mutex<Runner>, next: Settings) -> Option<String> {
    let mut runner = runner.lock().unwrap();
    runner.reread(next, rig.kelpie_settings()).unwrap()
}

fn refused(rig: &Rig, runner: &Mutex<Runner>, next: Settings) -> String {
    let mut runner = runner.lock().unwrap();
    runner
        .reread(next, rig.kelpie_settings())
        .unwrap_err()
        .to_string()
}

#[test]
fn pick_turned_on_takes_up_an_open_contribution_without_a_restart() {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.maintainer("Boss");
    let runner = rig.open().unwrap();
    let head = opened(&rig, 30, "newcomer");
    poll(&rig, &runner);
    assert_eq!(followed(&rig, &runner), []);

    let next = settings_with(&rig, |s| format!("{s}{TABLE}pick = \"all\"\n"));
    let line = reread(&rig, &runner, next);
    assert_eq!(
        line.as_deref(),
        Some("settings changed: contributions now in effect")
    );

    poll(&rig, &runner);
    assert_eq!(followed(&rig, &runner), [(30, "newcomer".to_owned(), head)]);
}

#[test]
fn pick_turned_off_forgets_the_contributions_followed() {
    let (rig, runner) = picking("all");
    opened(&rig, 30, "newcomer");
    poll(&rig, &runner);
    assert_eq!(followed(&rig, &runner).len(), 1);

    let next = settings_with(&rig, |s| s.replace("pick = \"all\"", "pick = \"off\""));
    let line = reread(&rig, &runner, next);
    assert_eq!(
        line.as_deref(),
        Some("settings changed: contributions now in effect")
    );

    poll(&rig, &runner);
    assert_eq!(followed(&rig, &runner), []);
}

#[test]
fn a_changed_reviewer_is_the_one_the_next_contribution_call_uses() {
    let (rig, runner) = listing(&["mine"]);
    // The security reads of the two heads
    rig.claude
        .script(std::iter::repeat_n(Scripted::Text(CLEAN), 2));
    let head = contribute(&rig, 30, |h| write(h, "one.txt", "one\n"));
    read_through(&rig, &runner, &head);
    assert_eq!(asked(&rig), ["mine"]);

    let next = settings_with(&rig, |s| s.replace("[\"mine\"]", "[\"other\"]"));
    let line = reread(&rig, &runner, next);
    assert_eq!(
        line.as_deref(),
        Some("settings changed: contributions now in effect")
    );

    let pushed = commit_on(&rig, 30, "topic/30", |h| write(h, "two.txt", "two\n"));
    read_through(&rig, &runner, &pushed);
    assert_eq!(asked(&rig), ["mine", "other"]);
}

#[test]
fn an_app_that_does_not_cover_the_repo_refuses_the_change_whatever_else_changes() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let before = rig.settings();
    // A start would refuse `label` too.
    let next = settings_with(&rig, |s| {
        format!("{s}{TABLE}pick = \"label\"\n").replace("per_item_usd = 10", "per_item_usd = 20")
    });

    let err = refused(&rig, &runner, next);

    assert!(err.contains("contributions.pick"), "{err}");
    assert_eq!(
        runner.lock().unwrap().settings(),
        &before,
        "the old settings stay"
    );
}

#[test]
fn a_missing_agent_file_refuses_the_change_naming_its_setting() {
    let (rig, runner) = listing(&["mine"]);
    let before = rig.settings();
    let next = settings_with(&rig, |s| s.replace("[\"mine\"]", "[\"ghost\"]"));

    let err = refused(&rig, &runner, next);

    assert!(err.contains("contributions.reviewers"), "{err}");
    assert_eq!(
        runner.lock().unwrap().settings(),
        &before,
        "the old settings stay"
    );
}

#[test]
fn a_missing_security_reviewer_file_refuses_the_change_naming_its_setting() {
    let (rig, runner) = listing(&["mine"]);
    let next = settings_with(&rig, |s| format!("{s}security_reviewer = \"ghost\"\n"));

    let err = refused(&rig, &runner, next);

    assert!(err.contains("contributions.security_reviewer"), "{err}");
}

#[test]
fn an_app_that_lost_the_repo_does_not_refuse_an_edit_to_another_setting() {
    let (rig, runner) = picking("all");
    rig.without_app();
    let next = settings_with(&rig, |s| {
        s.replace("per_item_usd = 10", "per_item_usd = 20")
    });

    let line = reread(&rig, &runner, next);

    assert_eq!(
        line.as_deref(),
        Some("settings changed: budget now in effect")
    );
}

#[test]
fn reviewers_on_a_gateway_added_with_them_wait_with_the_agents() {
    let (rig, runner) = picking("all");
    paddock_keyed_by_cargo(&rig);
    list_local(&rig);
    let reviewer = "---\nrole: reviewer\nharness: endpoint\ngateway: paddock\nmodel: coder\n\
                    context: 32768\n---\n";
    rig.write_agent("paddocked", reviewer);
    let next = settings_with(&rig, |s| {
        s.replace(
            "pick = \"all\"\n",
            "pick = \"all\"\nreviewers = [\"paddocked\"]\n",
        )
    });

    let line = reread(&rig, &runner, next);

    assert_eq!(
        line.as_deref(),
        Some(
            "settings changed: reviews now in effect; gateways and agents from the runner's next start"
        )
    );
}
