use serde_json::json;

use super::*;
use crate::runner::step;
use crate::test::{Rig, git};

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
        Some(StepReport::Dispatched { issue: 7, .. })
    ));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["issue"], 7);
    assert_eq!((status.get("run"), status.get("since")), (None, None));
    assert_eq!(status["rulings"][0]["id"], 3);
    assert_eq!(status["history"][0]["issue"], 5);
    drop(runner);
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    assert_eq!(saved["version"], 19);
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
            rig.open().unwrap_err().to_string(),
            format!(
                "setting `agents.reviewers`: coderabbit: shep-pm/shep is {seen_by}, \
                 and CodeRabbit's free plan reviews public repos only"
            )
        );
    }
}

#[test]
fn coderabbit_listed_for_a_public_repo_asks_the_forge_once() {
    let rig = Rig::new("shep");
    rig.coderabbit_on();
    rig.open().unwrap();
    assert_eq!(rig.forge.calls(), 1);
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
