//! Work items from a state file older than review bots in the pass, through
//! the runner's stand-ins

use std::sync::Mutex;

use serde_json::{Value, json};

use crate::coderabbit::FULL_REVIEW;
use crate::ports::Checks;
use crate::runner::coderabbit::tests::{hold_a_finding, now, summoned};
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted};

// Rewrites the runner's state file as a version 3 file, changed by `old`,
// and starts a runner on it.
fn as_version_3(rig: &Rig, runner: Mutex<Runner>, old: impl FnOnce(&mut Value)) -> Mutex<Runner> {
    drop(runner);
    let path = rig.paths().state;
    let mut saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    saved["version"] = json!(3);
    old(&mut saved);
    std::fs::write(&path, saved.to_string()).unwrap();
    rig.open().unwrap()
}

// The tally every version 3 work item kept, with no bot's read of its head.
fn unread_tally() -> Value {
    json!({ "rounds": 0, "cap_cleared": false, "satisfied": false })
}

#[test]
fn an_item_in_ci_that_no_bot_read_gets_a_pass_of_the_listed_bots_before_its_merge() {
    for auto in [false, true] {
        let (rig, runner, head) = Rig::with_pull_request("shep");
        rig.coderabbit_on();
        if auto {
            rig.merge_auto();
        }
        let runner = as_version_3(&rig, runner, |saved| {
            saved["work_items"][0]["coderabbit"] = unread_tally();
        });
        rig.forge.set_checks(&head, Checks::Passed);
        assert!(
            matches!(rig.verdict(&runner), Some(StepReport::MarkedReady { .. })),
            "auto: {auto}"
        );
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Summoned { .. })
        ));
        assert_eq!(rig.forge.merges(), [], "auto: {auto}");
        rig.forge.coderabbit.review(71, &head, now(&rig) + 60, &[]);
        rig.clock.advance(60);
        assert!(matches!(
            rig.threads_read(&runner),
            Some(StepReport::BotReviewed { round: 1, .. })
        ));
        let after = rig.verdict(&runner);
        if auto {
            assert!(
                matches!(after, Some(StepReport::Finished { merged: true, .. })),
                "{after:?}"
            );
        } else {
            assert!(
                matches!(after, Some(StepReport::Ruling { .. })),
                "{after:?}"
            );
        }
    }
}

#[test]
fn a_pending_merge_ruling_no_bot_read_for_is_withdrawn_for_the_bots_pass() {
    let (rig, runner, _) = Rig::parked("shep");
    rig.coderabbit_on();
    let runner = as_version_3(&rig, runner, |saved| {
        saved["work_items"][0]["coderabbit"] = unread_tally();
    });
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"], json!([]));
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    let said = "Merge ruling 1 on pull request #71 is withdrawn: no review bot shep lists \
                had read it, so they read it first, and the merge ruling is raised again \
                after they have. Nothing to answer.";
    assert!(
        runner
            .lock()
            .unwrap()
            .take_notes()
            .iter()
            .any(|n| n == said),
        "it is logged"
    );
    let posts = rig.alerts.posts();
    let (_, alert) = posts.last().expect("posted to the webhook");
    assert_eq!(alert.title, "kelpie: shep merge ruling 1 withdrawn");
    assert_eq!(alert.text, said);
    assert_eq!(alert.reply, None, "nothing to answer");
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
}

#[test]
fn an_item_a_bot_read_or_a_project_with_no_bot_goes_on_to_its_merge() {
    let (rig, runner, _) = Rig::parked("koji");
    let runner = as_version_3(&rig, runner, |saved| {
        saved["work_items"][0]["coderabbit"] = unread_tally();
    });
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["rulings"][0]["kind"]["kind"], "merge",
        "no bot listed"
    );
    assert_eq!(status["work_item"].get("bots_after_ci"), None);

    let (rig, runner, _) = Rig::parked("rotom");
    rig.coderabbit_on();
    let runner = as_version_3(&rig, runner, |saved| {
        saved["work_items"][0]["coderabbit"] =
            json!({ "rounds": 1, "cap_cleared": false, "satisfied": true });
    });
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["rulings"][0]["kind"]["kind"], "merge",
        "read clean already"
    );
}

// The cap held the threads it sent: a yes sends them for a fix that resolves them.
#[test]
fn a_pending_round_cap_ruling_sends_its_threads_on_a_yes_and_the_fix_resolves_them() {
    let (rig, runner, head) = summoned("shep");
    hold_a_finding(&rig, &runner, &head, "Name the flag.");
    let runner = as_version_3(&rig, runner, |saved| {
        let item = &mut saved["work_items"][0];
        item["coderabbit"] = unread_tally();
        item["phase"] = json!({ "state": "ruling", "id": 5 });
        saved["last_ruling"] = json!(5);
        saved["rulings"] = json!([{
            "id": 5, "issue": 7, "question": "q", "pull_request": 71, "alerted": true,
            "kind": {
                "kind": "coderabbit-cap", "rounds": 2, "held": 1,
                "prompt": "Fix the threads.", "head": head,
            },
        }]);
    });
    let status = rig.ask(&runner, "status", None);
    let question = status["rulings"][0]["question"].as_str().unwrap();
    assert!(
        question.contains("sends the worker those threads"),
        "{question}"
    );
    rig.ask(&runner, "rule", Some("5 yes"));
    rig.claude.script([Scripted::Push("flag.txt", "named\n")]);
    step(&runner).unwrap(); // the fix turn
    assert_eq!(rig.claude.calls().pop().unwrap().prompt, "Fix the threads.");
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    assert_eq!(rig.forge.coderabbit.resolved(), ["PRRT_71_0"]);
}

// An adoption's owed summon from an older file is owed by each listed bot.
#[test]
fn an_older_owed_summon_is_owed_by_the_listed_bot() {
    let (rig, runner, head) = Rig::with_pull_request("lugia");
    rig.coderabbit_on();
    rig.forge
        .coderabbit
        .review(71, &head, Rig::EPOCH - 3600, &[]);
    let runner = as_version_3(&rig, runner, |saved| {
        let item = &mut saved["work_items"][0];
        item["summon_owed"] = json!(true);
        item["phase"] = json!({ "state": "coderabbit", "stage": "lease", "head": head });
    });
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { .. })
    ));
    assert_eq!(
        rig.forge
            .comments()
            .into_iter()
            .filter(|(_, c)| c == FULL_REVIEW)
            .count(),
        1,
        "its review from before stands for no read, so it is asked in full"
    );
}
