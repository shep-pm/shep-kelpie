use serde_json::json;

use std::num::NonZeroU64;
use std::sync::Mutex;

use super::*;
use crate::ports::{RateBucket, RateLimits, Visibility};
use crate::runner::step;
use crate::state::VERSION;
use crate::test::ticket;
use crate::test::{Rig, git};
use crate::ticket::TicketId;

// A project saved paused by an older kelpie runs once its runner starts.
#[test]
fn a_project_saved_paused_reads_the_board_on_its_first_pass() {
    let rig = Rig::new("reactmap");
    let epoch = Rig::EPOCH;
    let old = json!({
        "version": 9,
        "run": "paused",
        "since": epoch,
        "work_items": [],
        "rulings": [{
            "id": 3,
            "issue": null,
            "question": "q",
            "pull_request": 30,
            "kind": { "kind": "stuck", "reason": "closed" },
            "alerted": true,
        }],
        "last_ruling": 3,
        "finished": [5],
        "history": [{
            "issue": 5,
            "title": "Five",
            "pull_request": 50,
            "merged": true,
            "at": epoch,
            "wall": 100,
            "seconds": { "worker": 60, "review": 0, "ci": 40, "ruling": 0, "merge": 0, "other": 0 },
        }],
        "reworked": [],
        "adopted": [],
        "leases": [],
        "pacing": null,
        "notices": [],
        "replies": { "last": null },
        "events": [
            { "id": 1, "at": epoch, "what": "project started" },
            { "id": 2, "at": epoch, "what": "#5: PR #50 merged, work item done" },
            { "id": 3, "at": epoch, "what": "project paused" },
        ],
        "last_event": 3,
        "pm_seen": 2,
        "pm_session": "0e2c6a52-5b0e-4c5f-9a43-3c1f1d8b7e10",
    });
    let state = rig.paths().state;
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(&state, old.to_string()).unwrap();

    let runner = rig.open().unwrap();
    rig.forge.list_ready(7, false);
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Dispatched { issue, .. }) if issue == ticket(7)
    ));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["issue"], "7");
    assert_eq!((status.get("run"), status.get("since")), (None, None));
    assert_eq!(status["rulings"][0]["id"], "30");
    assert_eq!(status["history"][0]["issue"], "5");
    drop(runner);
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(
        saved["events"][2]["what"], "project paused",
        "old events stay"
    );
    assert_eq!(saved["pm_seen"], 2);
    assert_eq!(saved["pm_session"], "0e2c6a52-5b0e-4c5f-9a43-3c1f1d8b7e10");
}

#[test]
fn a_failed_save_is_reported_and_changes_nothing() {
    let rig = Rig::new("xilriws");
    let runner = rig.open().unwrap();
    let folder = rig.paths().state.parent().unwrap().to_owned();
    std::fs::remove_dir_all(&folder).unwrap();
    // A file where the folder was, which a save cannot make a folder of.
    std::fs::write(&folder, "").unwrap();
    let reply = rig.ask(&runner, "add", Some("7"));
    assert!(
        reply["error"]
            .as_str()
            .unwrap()
            .contains("cannot write state file"),
        "{reply}"
    );
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
}

#[test]
fn a_repo_that_is_not_a_git_work_tree_stops_the_runner() {
    let rig = Rig::new("chelone");
    let elsewhere = rig.home.path().join("not-a-repo");
    std::fs::create_dir(&elsewhere).unwrap();
    rig.edit_settings(|s| {
        s.replace(
            &rig.repo().display().to_string(),
            &elsewhere.display().to_string(),
        )
    });
    let err = rig.open().unwrap_err();
    assert!(
        err.to_string().starts_with("setting `git.checkout`: "),
        "{err}"
    );
    assert!(
        err.to_string()
            .ends_with("not-a-repo is not a git work tree"),
        "{err}"
    );
}

#[test]
fn a_repo_without_an_origin_stops_the_runner() {
    let rig = Rig::new("koji");
    git(&rig.repo(), &["remote", "remove", "origin"]);
    let err = rig.open().unwrap_err().to_string();
    assert!(err.starts_with("setting `git.checkout`: "), "{err}");
    assert!(
        err.ends_with("koji has no `origin` remote to cut branches from"),
        "{err}"
    );
}

#[test]
fn with_no_remote_set_the_repo_is_read_from_origin() {
    for url in [
        "git@github.com:shep-pm/from-origin.git",
        "https://github.com/shep-pm/from-origin",
    ] {
        let rig = Rig::new("koji");
        rig.edit_settings(|s| s.replace("remote = \"shep-pm/shep\"\n", ""));
        git(&rig.repo(), &["remote", "set-url", "origin", url]);
        let runner = rig.open().unwrap();
        let runner = runner.lock().unwrap();
        assert_eq!(runner.remote().as_str(), "shep-pm/from-origin", "{url}");
        assert_eq!(runner.settings().git.remote, None);
    }
}

#[test]
fn with_no_remote_set_an_origin_off_github_stops_the_runner_naming_the_setting() {
    let rig = Rig::new("koji");
    rig.edit_settings(|s| s.replace("remote = \"shep-pm/shep\"\n", ""));
    let err = rig.open().unwrap_err().to_string();
    assert_eq!(
        err,
        "setting `git.remote`: it is not set, and the checkout's `origin` is not a GitHub \
         repo over HTTPS or SSH: set it to the repo as `owner/name`"
    );
}

#[test]
fn a_repo_that_does_not_exist_stops_the_runner() {
    let rig = Rig::new("reactmap");
    let gone = rig.repo().display().to_string();
    rig.edit_settings(|s| s.replace(&gone, &format!("{gone}-gone")));
    let err = rig.open().unwrap_err().to_string();
    assert!(err.starts_with("setting `git.checkout`: "), "{err}");
    assert!(err.ends_with("reactmap-gone is not a folder"), "{err}");
}

fn open_listed(rig: &Rig, names: &[&str]) -> Result<Mutex<Runner>, OpenError> {
    let mut settings = rig.settings();
    settings.agents.reviewers = Some(
        names
            .iter()
            .map(|n| crate::settings::AgentName::try_from((*n).to_owned()).unwrap())
            .collect(),
    );
    let ports = rig.open()?.into_inner().unwrap().ports;
    Runner::open(
        rig.project.clone(),
        settings,
        rig.kelpie_settings(),
        &rig.paths(),
        rig.home.path(),
        Path::new(Rig::KELPIE),
        ports,
    )
    .map(Mutex::new)
}

#[test]
fn coderabbit_listed_for_a_repo_that_is_not_public_stops_the_runner() {
    for (visibility, seen_by) in [
        (Visibility::Private, "private"),
        (Visibility::Internal, "internal"),
    ] {
        let rig = Rig::new("shep");
        rig.coderabbit_on();
        rig.forge.set_visibility(visibility);
        assert_eq!(
            open_listed(&rig, crate::test::RIG_REVIEWERS_AND_CODERABBIT)
                .unwrap_err()
                .to_string(),
            format!(
                "setting `agents.reviewers`: coderabbit: shep-pm/shep is {seen_by}, \
                 and CodeRabbit's free plan reviews public repos only"
            )
        );
    }
}

#[test]
fn a_private_repo_with_no_reviewers_list_opens_with_coderabbit_among_the_candidates() {
    let rig = Rig::new("shep");
    rig.write_agent(
        "coderabbit",
        "---\nrole: reviewer\nharness: bot\nbot: coderabbit\nreviews: 1\nhours: 1\n---\n",
    );
    rig.forge.set_visibility(Visibility::Private);
    let runner = rig.open().expect("a candidate is not a listed reviewer");
    let bots = runner.lock().unwrap().listed_bots();
    assert!(!bots.is_empty(), "it is still a candidate");
}

#[test]
fn a_local_reviewer_that_is_missing_does_not_stop_a_project_with_no_reviewers_list() {
    let rig = Rig::new("shep");
    let gone = rig.home.path().join("bin/gone");
    rig.write_agent(
        "gone",
        &format!(
            "---\nrole: reviewer\nharness: command\ncommand: {}\n---\n",
            gone.display()
        ),
    );
    rig.open().expect("a candidate is not a listed reviewer");
}

#[test]
fn a_local_reviewer_the_project_lists_that_is_missing_stops_the_runner() {
    let rig = Rig::new("shep");
    let gone = rig.home.path().join("bin/gone");
    rig.write_agent(
        "gone",
        &format!(
            "---\nrole: reviewer\nharness: command\ncommand: {}\n---\n",
            gone.display()
        ),
    );
    rig.reviewers(&["qwen", "gone"]);
    let err = open_listed(&rig, &["qwen", "gone"])
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("setting `agents.reviewers`: gone: cannot run "),
        "{err}"
    );
}

#[test]
fn coderabbit_listed_for_a_public_repo_asks_the_forge_once() {
    let rig = Rig::new("shep");
    rig.coderabbit_on();
    open_listed(&rig, crate::test::RIG_REVIEWERS_AND_CODERABBIT).unwrap();
    assert_eq!(rig.forge.calls(), 1);
}

#[test]
fn the_runner_records_its_e2e_table_for_the_dog_and_removes_it_when_unset() {
    let rig = Rig::new("acme");
    let file = rig.paths().folder.join("e2e.json");
    rig.open().unwrap();
    assert!(!file.exists(), "no table, no file");

    rig.edit_settings(|s| s + "\n[app.dogs.kelpie.e2e]\nhost = \"e2e-box\"\n");
    rig.open().unwrap();
    let recorded: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(
        recorded,
        json!({
            "host": "e2e-box",
            "command": "scripts/e2e-remote.sh",
            "results": "test-results",
        })
    );

    rig.edit_settings(|s| s.replace("host = \"e2e-box\"", "host = \"other\""));
    rig.open().unwrap();
    let recorded = std::fs::read_to_string(&file).unwrap();
    assert!(recorded.contains("other"), "{recorded}");

    rig.edit_settings(|s| s.replace("\n[app.dogs.kelpie.e2e]\nhost = \"other\"\n", ""));
    rig.open().unwrap();
    assert!(!file.exists(), "a stale file is removed");
}

#[test]
fn a_project_listing_no_bot_never_asks_the_forge() {
    let rig = Rig::new("acme");
    rig.forge.set_visibility(Visibility::Private);
    rig.open().unwrap();
    assert_eq!(rig.forge.calls(), 0);
}

#[test]
fn a_repo_github_marks_private_may_list_cubic() {
    let rig = Rig::new("acme");
    rig.reviewers(&["qwen", "claude", "cubic"]);
    rig.forge.set_visibility(Visibility::Private);
    rig.open().unwrap();
    assert_eq!(rig.forge.calls(), 0);
}

fn is_json_object(line: &str) -> bool {
    matches!(
        serde_json::from_str::<serde_json::Value>(line),
        Ok(serde_json::Value::Object(_))
    )
}

#[test]
fn a_runner_logs_no_json_object_at_its_start_or_a_dispatch() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.forge.list_ready(7, false);
    let report = step(&runner).unwrap().unwrap();
    let mut runner = runner.lock().unwrap();
    let lines = [runner.up_line(), runner.log_step(&report)];
    for line in &lines {
        assert!(!is_json_object(line), "{line}");
        assert!(!line.contains('\n'), "{line}");
    }
    assert!(lines[0].starts_with("up: koji"), "{}", lines[0]);
    assert!(lines[1].starts_with("#7 dispatched to "), "{}", lines[1]);
    assert!(lines[1].ends_with(')'), "the model's effort: {}", lines[1]);
}

#[test]
fn a_dispatch_on_an_unlisted_implementer_logs_the_model_and_effort_it_runs_on() {
    let rig = Rig::new("koji");
    rig.write_agent(
        "gpt",
        "---\nrole: implementer\nharness: stand-in\nmodel: gpt-5-codex\neffort: medium\n\
         usage: none\n---\n",
    );
    let runner = rig.open().unwrap();
    rig.forge.list_ready(7, false);
    rig.forge.label(7, "agent:gpt!");
    let report = step(&runner).unwrap().unwrap();
    let line = runner.lock().unwrap().log_step(&report);
    assert_eq!(line, "#7 dispatched to gpt-5-codex (medium)");
}

// A GitHub ticket's line keeps the `#` it has always had. A local ticket has
// no issue number to show, so its id stands on its own in both logs.
#[test]
fn a_step_names_its_work_item_by_ticket_id_in_words_and_in_json() {
    let rig = Rig::new("koji");
    let opened = rig.open().unwrap();
    let mut runner = opened.lock().unwrap();
    let local = TicketId::Local(NonZeroU64::new(12).unwrap());

    let github = StepReport::GateFailed {
        issue: ticket(7),
        reason: "the forge is down".to_owned(),
    };
    assert_eq!(
        runner.log_step(&github),
        "#7 gate-failed reason=the forge is down"
    );
    assert_eq!(serde_json::to_value(&github).unwrap()["issue"], json!("7"));

    let local_item = StepReport::GateFailed {
        issue: local,
        reason: "the forge is down".to_owned(),
    };
    assert_eq!(
        runner.log_step(&local_item),
        "k12 gate-failed reason=the forge is down"
    );
    assert_eq!(
        serde_json::to_value(&local_item).unwrap()["issue"],
        json!("k12")
    );
}

#[test]
fn the_steps_a_runner_logs_are_in_steps_jsonl_one_object_per_line() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.forge.list_ready(7, false);
    let reports: Vec<StepReport> = (0..3).filter_map(|_| step(&runner).unwrap()).collect();
    assert!(reports.len() >= 2, "{reports:?}");
    let mut runner = runner.lock().unwrap();
    for report in &reports {
        runner.log_step(report);
    }

    let file = rig.paths().folder.join("steps.jsonl");
    let text = std::fs::read_to_string(&file).unwrap();
    let logged: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let expected: Vec<serde_json::Value> = (reports.iter())
        .map(|report| serde_json::to_value(report).unwrap())
        .collect();
    assert_eq!(logged.len(), reports.len(), "{text}");
    for (line, mut want) in logged.into_iter().zip(expected) {
        let mut line = line.as_object().unwrap().clone();
        let at = line.remove("at").unwrap();
        assert!(
            at.as_str().unwrap().parse::<jiff::Timestamp>().is_ok(),
            "{at}"
        );
        let want = want.as_object_mut().unwrap();
        assert_eq!(&line, &*want);
    }
}

// The tickets port is a source's ticket half (ADR 0014). One wrapped source
// stands behind it and behind the forge port, so a ticket read or write
// reaches the rig's forge, is held and ledged there, and is refused by the
// same guard.
#[test]
fn the_tickets_port_and_the_forge_port_are_one_wrapped_source() {
    use crate::forge_ledger::{self, Kind};

    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    let runner = runner.lock().unwrap();
    let repo = runner.remote.clone();
    let ledger = |kind: Kind| {
        forge_ledger::read(&rig.paths().kelpie_home.join(forge_ledger::FILE))
            .unwrap()
            .iter()
            .filter(|line| line.kind == kind)
            .count()
    };

    let (asked, ledged) = (rig.forge.asked(), ledger(Kind::Issue));
    let read = runner.ports.tickets.issue(&repo, ticket(7)).unwrap();
    let again = runner.ports.tickets.issue(&repo, ticket(7)).unwrap();
    assert_eq!(read, again);
    assert_eq!(
        rig.forge.asked(),
        asked + 2,
        "each read reached the rig's forge"
    );
    assert_eq!(ledger(Kind::Issue), ledged + 2, "each read is ledged");

    // What the rig's forge no longer has is gone
    rig.forge.remove_issue(7);
    assert_eq!(
        runner.ports.tickets.issue(&repo, ticket(7)).unwrap_err(),
        ForgeError::Failed("no issue #7".into()),
    );

    // What the ticket port opens, the ticket port reads back
    let opened = runner
        .ports
        .tickets
        .create_issue(&repo, "A ticket", "Its body", &[])
        .unwrap();
    assert_eq!(
        runner.ports.tickets.issue(&repo, opened).unwrap().title,
        "A ticket",
    );

    // The forge port keeps the pull requests, and reaches the same rig forge
    // through the same hold
    let (asked, ledged) = (rig.forge.asked(), ledger(Kind::OpenPullRequests));
    runner.ports.forge.open_pull_requests(&repo).unwrap();
    assert_eq!(rig.forge.asked(), asked + 1, "one call on one forge");
    assert_eq!(
        ledger(Kind::OpenPullRequests),
        ledged + 1,
        "the forge read is ledged too"
    );

    // And the ticket port stands behind the same guard, so a body naming this
    // machine's folders never reaches the forge
    let asked = rig.forge.asked();
    let refused = runner
        .ports
        .tickets
        .create_issue(
            &repo,
            "A ticket",
            &rig.home.path().display().to_string(),
            &[],
        )
        .unwrap_err();
    assert!(matches!(refused, ForgeError::LocalPath { .. }), "{refused}");
    assert_eq!(rig.forge.asked(), asked, "the forge was never asked");
}

fn limits(core_used: u64, core_reset: u64, graphql_used: u64, graphql_reset: u64) -> RateLimits {
    let bucket = |used, reset| RateBucket {
        used,
        remaining: 5_000 - used,
        reset: Timestamp(reset),
    };
    RateLimits {
        core: bucket(core_used, core_reset),
        graphql: bucket(graphql_used, graphql_reset),
        search: bucket(0, 0),
    }
}

fn gauges(rig: &Rig) -> [Option<f64>; 4] {
    let at = |name, bucket| rig.samples.value(name, &[("bucket", bucket)]);
    let used = "kelpie_forge_rate_used";
    let reset = "kelpie_forge_rate_reset_timestamp_seconds";
    [
        at(used, "core"),
        at(reset, "core"),
        at(used, "graphql"),
        at(reset, "graphql"),
    ]
}

#[test]
fn the_runner_reads_rate_limit_at_most_once_a_minute_and_sets_both_gauges() {
    let rig = Rig::new("reactmap");
    let runner = rig.open().unwrap();
    rig.forge.set_rate_limits(limits(120, 7_000, 7, 6_500));

    step(&runner).unwrap();
    assert_eq!(
        gauges(&rig),
        [Some(120.0), Some(7_000.0), Some(7.0), Some(6_500.0)]
    );
    assert_eq!(rig.forge.rate_limit_reads(), 1);

    rig.forge.set_rate_limits(limits(130, 7_000, 9, 6_500));
    rig.clock.advance(59);
    step(&runner).unwrap();
    assert_eq!(
        rig.forge.rate_limit_reads(),
        1,
        "not again within the minute"
    );
    assert_eq!(gauges(&rig)[0], Some(120.0));

    rig.clock.advance(1);
    step(&runner).unwrap();
    assert_eq!(rig.forge.rate_limit_reads(), 2);
    assert_eq!(
        gauges(&rig),
        [Some(130.0), Some(7_000.0), Some(9.0), Some(6_500.0)]
    );
}

#[test]
fn a_failed_rate_limit_read_keeps_the_gauges_and_stands_once() {
    let rig = Rig::new("reactmap");
    let runner = rig.open().unwrap();
    rig.forge.set_rate_limits(limits(120, 7_000, 7, 6_500));
    step(&runner).unwrap();
    let before = gauges(&rig);
    runner.lock().unwrap().take_notes();
    rig.forge.set_rate_limit_down(true);
    rig.forge.set_rate_limits(limits(500, 9_000, 50, 9_000));

    let mut told = Vec::new();
    for _ in 0..3 {
        rig.clock.advance(60);
        step(&runner).unwrap();
        told.extend(runner.lock().unwrap().take_notes());
    }

    assert_eq!(rig.forge.rate_limit_reads(), 4, "each minute asked again");
    assert_eq!(gauges(&rig), before);
    let told: Vec<_> = told.iter().filter(|n| n.contains("rate limits")).collect();
    assert_eq!(told.len(), 1, "{told:?}");

    rig.forge.set_rate_limit_down(false);
    rig.clock.advance(60);
    step(&runner).unwrap();
    assert_eq!(gauges(&rig)[0], Some(500.0), "a read that works moves them");
}

// The read of the rate limits waits its turn behind the login's pace, so it is made with the lock
// let go: a trigger sent meanwhile is answered.
#[test]
fn a_trigger_is_answered_while_the_rate_limits_are_read() {
    let rig = Rig::new("reactmap");
    let runner = rig.open().unwrap();
    let read = rig.forge.hold_next_rate_limit_read();

    let (reply, pass) = rig.asked_while_held(&runner, read, "add", Some("7"));

    assert!(
        reply.is_some(),
        "the trigger waited for the rate limit read"
    );
    pass.unwrap();
}
