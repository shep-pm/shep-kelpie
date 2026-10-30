use std::sync::Mutex;

use serde_json::Value;

use crate::ports::{AuthorAssociation, Clock, OpenIssue, Role, Timestamp};
use crate::review_bot::Bot;
use crate::runner::contributions::pass_tests::listing;
use crate::runner::contributions::tests::{opened, poll};
use crate::runner::{OpenError, Pass, Runner, advance, step};
use crate::settings::AgentName;
use crate::state::{BotTurn, StateError, Walking};
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted};
use crate::work_item::ReviewStage;

use super::settings_with;

const TABLE: &str = "\n[app.dogs.kelpie.triage]\n";

// A rig whose App covers the repo, with a runner opened under `pick`
fn rig_picking(pick: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.edit_settings(|s| format!("{s}{TABLE}pick = \"{pick}\"\n"));
    let runner = rig.open().unwrap();
    (rig, runner)
}

fn pick_now(rig: &Rig, from: &str, to: &str) -> crate::settings::Settings {
    settings_with(rig, |s| {
        s.replace(
            &format!("{TABLE}pick = \"{from}\""),
            &format!("{TABLE}pick = \"{to}\""),
        )
    })
}

// An issue by someone else made `after` seconds after the rig's clock now reads
fn issue(rig: &Rig, number: u64, after: u64) -> OpenIssue {
    OpenIssue {
        number,
        title: format!("issue {number}"),
        body: String::new(),
        author: "newcomer".to_owned(),
        author_is_bot: false,
        author_association: Some(AuthorAssociation::Contributor),
        labels: Vec::new(),
        created: Timestamp(rig.clock.now().0 + after),
    }
}

fn due(rig: &Rig, runner: &Mutex<Runner>) -> Vec<u64> {
    let status = rig.ask(runner, "status", None);
    let listed = status["triage"]["due"].as_array().cloned();
    let numbers = listed.unwrap_or_default();
    numbers
        .iter()
        .map(|i| i["issue"].as_u64().unwrap())
        .collect()
}

fn refusal(reply: &Value) -> Option<&str> {
    reply["error"].as_str()
}

#[test]
fn pick_turned_on_takes_effect_at_once_and_the_verb_is_taken() {
    let (rig, runner) = rig_picking("off");
    rig.forge.list_open_issue(issue(&rig, 30, 5));
    let refused = rig.ask(&runner, "triage", Some("30"));
    assert_eq!(
        refusal(&refused),
        Some("`triage.pick` is `off`, so no issue is triaged")
    );

    let next = pick_now(&rig, "off", "all");
    let line = runner
        .lock()
        .unwrap()
        .reread(next, rig.kelpie_settings())
        .unwrap();
    assert_eq!(
        line.as_deref(),
        Some("settings changed: triage now in effect")
    );

    let reply = rig.ask(&runner, "triage", Some("30"));
    assert_eq!(refusal(&reply), None, "{reply}");
}

#[test]
fn the_next_poll_lists_the_issues_opened_after_the_reread() {
    let (rig, runner) = rig_picking("off");
    rig.forge.list_open_issue(issue(&rig, 29, 0));
    rig.clock.advance(100);
    let next = pick_now(&rig, "off", "all");
    runner
        .lock()
        .unwrap()
        .reread(next, rig.kelpie_settings())
        .unwrap();
    rig.forge.list_open_issue(issue(&rig, 31, 20));
    rig.forge.list_open_issue(issue(&rig, 30, 10));

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        due(&rig, &runner),
        [30, 31],
        "the older issue is left alone"
    );
}

#[test]
fn pick_turned_off_forgets_the_listing_and_the_verb_refuses() {
    let (rig, runner) = rig_picking("all");
    rig.forge.list_open_issue(issue(&rig, 30, 5));
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(due(&rig, &runner), [30]);

    let next = pick_now(&rig, "all", "off");
    let line = runner
        .lock()
        .unwrap()
        .reread(next, rig.kelpie_settings())
        .unwrap();
    assert_eq!(
        line.as_deref(),
        Some("settings changed: triage now in effect")
    );

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(due(&rig, &runner), Vec::<u64>::new());
    let reply = rig.ask(&runner, "triage", Some("30"));
    assert_eq!(
        refusal(&reply),
        Some("`triage.pick` is `off`, so no issue is triaged")
    );

    // Turned on again, it begins from the reread and not from the first start.
    rig.clock.advance(100);
    let next = pick_now(&rig, "off", "all");
    runner
        .lock()
        .unwrap()
        .reread(next, rig.kelpie_settings())
        .unwrap();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(due(&rig, &runner), Vec::<u64>::new());
}

#[test]
fn a_change_between_maintainer_and_all_keeps_since() {
    let (rig, runner) = rig_picking("maintainer");
    rig.clock.advance(100);
    rig.forge.list_open_issue(issue(&rig, 30, 0));
    let next = pick_now(&rig, "maintainer", "all");
    runner
        .lock()
        .unwrap()
        .reread(next, rig.kelpie_settings())
        .unwrap();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(due(&rig, &runner), [30], "since is still the first start");
}

#[test]
fn pick_turned_on_where_the_app_does_not_cover_the_repo_is_refused_and_the_old_settings_stay() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let next = settings_with(&rig, |s| format!("{s}{TABLE}pick = \"maintainer\"\n"));
    let mut runner = runner.lock().unwrap();

    let err = runner.reread(next, rig.kelpie_settings()).unwrap_err();

    assert!(err.to_string().contains("triage.pick"), "{err}");
    assert!(runner.settings().triage.is_off(), "the old pick stays");
}

#[test]
fn a_missing_triager_file_is_refused_naming_triage_agent() {
    let (rig, runner) = rig_picking("off");
    let next = settings_with(&rig, |s| {
        s.replace(
            &format!("{TABLE}pick = \"off\"\n"),
            &format!("{TABLE}pick = \"all\"\nagent = \"nobody\"\n"),
        )
    });
    let mut runner = runner.lock().unwrap();

    let err = runner.reread(next, rig.kelpie_settings()).unwrap_err();

    assert!(err.to_string().contains("triage.agent"), "{err}");
    assert!(runner.settings().triage.is_off(), "the old pick stays");
}

#[test]
fn narrowing_pick_drops_the_listing_made_under_the_wider_one() {
    let (rig, runner) = rig_picking("all");
    rig.maintainer("Boss");
    rig.forge.list_open_issue(issue(&rig, 30, 5));
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(due(&rig, &runner), [30]);

    let next = pick_now(&rig, "all", "maintainer");
    runner
        .lock()
        .unwrap()
        .reread(next, rig.kelpie_settings())
        .unwrap();

    assert_eq!(due(&rig, &runner), Vec::<u64>::new());
}

#[test]
fn pick_turned_off_while_a_call_runs_posts_nothing_when_it_ends() {
    let (rig, runner) = rig_picking("all");
    rig.forge.list_open_issue(issue(&rig, 30, 5));
    let hold = Hold::default();
    let found = r#"{"type": null, "priority": null, "related": [], "files": [],
        "tests": [], "criteria": [], "open": [], "body": null}"#;
    rig.claude
        .script_for(Role::Triager, [Scripted::HoldThenSay(hold.clone(), found)]);
    rig.claude.script_for(
        Role::Representative,
        [Scripted::Say("Thanks for the report.")],
    );
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the research never began");

    let next = pick_now(&rig, "all", "off");
    runner
        .lock()
        .unwrap()
        .reread(next, rig.kelpie_settings())
        .unwrap();
    hold.release();
    assert!(hold.answered(CALL_PATIENCE), "the research never answered");
    let by = std::time::Instant::now() + CALL_PATIENCE;
    while rig.ask(&runner, "status", None)["triage"]["running"].is_u64() {
        assert!(std::time::Instant::now() < by, "the call never ended");
        advance(&runner).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.github.comments(), []);
}

#[test]
fn a_reread_that_fails_after_pick_changes_keeps_the_old_triage_state() {
    let (rig, runner) = listing(&["mine"]);
    rig.edit_settings(|s| format!("{s}{TABLE}pick = \"off\"\n"));
    opened(&rig, 30, "newcomer");
    poll(&rig, &runner);
    {
        // A walk at a bot the next lineup does not list, so the reread must end its turn.
        let mut runner = runner.lock().unwrap();
        assert_eq!(
            runner.state.contributions.len(),
            1,
            "the pull request is followed"
        );
        let (now, head) = (rig.clock.now(), runner.state.contributions[0].head.clone());
        runner.state.contributions[0].walking = Some(Walking {
            head: head.clone(),
            past: Vec::new(),
            ran: Vec::new(),
            missed: Vec::new(),
            bot: Some(BotTurn {
                reviewer: AgentName::try_from("coderabbit".to_owned()).unwrap(),
                stage: ReviewStage::Summon {
                    bot: Bot::Coderabbit,
                    started: now,
                    head,
                    readied: None,
                    full: false,
                },
            }),
        });
        // The triage save is the first the reread makes and the turn's the second.
        runner.store = runner.store.clone().refusing_after(1);
    }
    let before = rig.ask(&runner, "status", None)["triage"].clone();
    let saved = || -> Value {
        serde_json::from_str(&std::fs::read_to_string(&rig.paths().state).unwrap()).unwrap()
    };
    let stamped_before = saved()["triage"].clone();
    let next = settings_with(&rig, |s| {
        let edited = (s.replace("[\"mine\"]", "[\"other\"]")).replace(
            &format!("{TABLE}pick = \"off\""),
            &format!("{TABLE}pick = \"all\""),
        );
        assert!(edited.contains("[\"other\"]"), "the reviewers changed");
        assert!(
            edited.contains(&format!("{TABLE}pick = \"all\"")),
            "pick changed"
        );
        edited
    });

    let err = runner
        .lock()
        .unwrap()
        .reread(next, rig.kelpie_settings())
        .unwrap_err();

    assert!(
        matches!(err, OpenError::State(StateError::Write { .. })),
        "{err}"
    );
    assert_eq!(rig.ask(&runner, "status", None)["triage"], before);
    assert_eq!(
        saved()["triage"],
        stamped_before,
        "the saved state kept its triage"
    );
    assert!(runner.lock().unwrap().settings().triage.is_off());
}
