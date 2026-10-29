//! `[app.dogs.kelpie.contributions]`, which says whether and how kelpie picks up
//! other people's pull requests

use std::num::NonZeroU32;
use std::path::Path;

use crate::settings::Gateways;

const EXAMPLE: &str = include_str!("../../../../../settings.example.toml");

const HOME: &str = "/home/me";
const FOLDER: &str = "/home/me/.shep/kelpie/shep";

fn parse(entry: &str) -> Result<Settings, String> {
    let table = crate::test::project_table(entry);
    Settings::from_table(&table, "shep", Path::new(HOME), Path::new(FOLDER))
        .map_err(|e| e.to_string())
}

fn parse_err(text: &str) -> String {
    parse(text).expect_err("the settings should be refused")
}
use crate::agents::Agents;
use crate::settings::{Pick, Settings};
use crate::test::Rig;

const TABLE: &str = "[app.dogs.kelpie.contributions]\n";

const REVIEWER: &str = "---\nrole: reviewer\nharness: claude-code\nmodel: claude-opus-5-5\n\
                        effort: high\n---\nRead it for defects.\n";

// The example with `lines` under a contributions table.
fn with(lines: &str) -> String {
    format!("{EXAMPLE}\n{TABLE}{lines}")
}

fn days(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}

#[test]
fn an_absent_contributions_table_means_pick_off_quiet_7_nudge_3() {
    let c = parse(EXAMPLE).unwrap().contributions;
    assert_eq!(c.pick, Pick::Off);
    assert_eq!(c.quiet_days, days(7));
    assert_eq!(c.nudge_days, days(3));
    assert_eq!(c.reviewers, None);
    assert_eq!(c.security_reviewer.as_str(), "security-reviewer");
    assert_eq!(parse(&with("")).unwrap().contributions, c, "an empty table");
}

#[test]
fn pick_takes_off_label_or_all_and_refuses_any_other_word() {
    for (word, pick) in [
        ("off", Pick::Off),
        ("label", Pick::Label),
        ("all", Pick::All),
    ] {
        let settings = parse(&with(&format!("pick = \"{word}\"\n"))).unwrap();
        assert_eq!(settings.contributions.pick, pick, "{word}");
    }
    let err = parse_err(&with("pick = \"some\"\n"));
    assert!(err.contains("pick") && err.contains("some"), "{err}");
}

#[test]
fn an_unknown_key_in_contributions_is_refused_naming_it() {
    let err = parse_err(&with("pik = \"all\"\n"));
    assert!(err.contains("pik"), "{err}");
}

#[test]
fn zero_quiet_days_or_nudge_days_is_refused() {
    for key in ["quiet_days", "nudge_days"] {
        let err = parse_err(&with(&format!("{key} = 0\n")));
        assert!(err.contains(key), "{err}");
    }
}

#[test]
fn nudge_days_at_or_past_quiet_days_is_refused() {
    for lines in [
        "quiet_days = 5\nnudge_days = 5\n",
        "quiet_days = 5\nnudge_days = 9\n",
        "quiet_days = 2\n",
    ] {
        let err = parse_err(&with(lines));
        assert!(err.contains("contributions.nudge_days"), "{err}");
        assert!(err.contains("below"), "{err}");
    }
    let c = parse(&with("quiet_days = 5\nnudge_days = 4\n")).unwrap();
    assert_eq!(
        (c.contributions.quiet_days, c.contributions.nudge_days),
        (days(5), days(4))
    );
}

#[test]
fn absent_reviewers_are_the_projects_agents_reviewers() {
    let binding = "harness: claude-code\nmodel: claude-opus-5-5\neffort: high\n";
    let book = Agents::embedded()
        .bind("defect-hunter", binding)
        .with("opus", REVIEWER);
    let home = tempfile::tempdir().unwrap();
    let mut settings = parse(EXAMPLE).unwrap();
    settings.agents.reviewers = Some(vec![
        crate::settings::AgentName::try_from("opus".to_owned()).unwrap(),
    ]);
    let lineup = settings
        .contribution_lineup(&book, &Gateways::default(), home.path())
        .unwrap();
    let names: Vec<&str> = lineup.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["opus"]);

    let mut own = parse(&with("reviewers = [\"defect-hunter\", \"opus\"]\n")).unwrap();
    own.agents.reviewers = settings.agents.reviewers.clone();
    let lineup = own
        .contribution_lineup(&book, &Gateways::default(), home.path())
        .unwrap();
    let names: Vec<&str> = lineup.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["defect-hunter", "opus"]);
}

#[test]
fn a_reviewer_with_no_agent_file_stops_the_runner_when_pick_is_label() {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.edit_settings(|s| format!("{s}\n{TABLE}pick = \"label\"\nreviewers = [\"outsider\"]\n"));
    let err = rig.open().unwrap_err().to_string();
    assert!(
        err.contains("`contributions.reviewers` names outsider, which has no agent file"),
        "{err}"
    );
    rig.write_agent("outsider", REVIEWER);
    rig.open().unwrap();
}

#[test]
fn a_reviewer_with_no_agent_file_is_not_checked_when_pick_is_off() {
    let rig = Rig::new("shep");
    rig.edit_settings(|s| format!("{s}\n{TABLE}reviewers = [\"outsider\"]\n"));
    rig.open().unwrap();
}

#[test]
fn pick_label_on_a_repo_the_app_does_not_cover_stops_the_runner_naming_contributions_pick() {
    for pick in ["label", "all"] {
        let rig = Rig::new("shep");
        rig.edit_settings(|s| format!("{s}\n{TABLE}pick = \"{pick}\"\n"));
        let err = rig.open().unwrap_err().to_string();
        assert!(err.contains("contributions.pick"), "{err}");
        assert!(err.contains("as the maintainer"), "{err}");
    }
}

#[test]
fn pick_label_on_a_repo_the_app_covers_starts() {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.edit_settings(|s| format!("{s}\n{TABLE}pick = \"label\"\n"));
    rig.open().unwrap();
}

#[test]
fn pick_off_on_a_repo_the_app_does_not_cover_still_starts() {
    let rig = Rig::new("shep");
    rig.edit_settings(|s| format!("{s}\n{TABLE}pick = \"off\"\n"));
    rig.open().unwrap();
}
