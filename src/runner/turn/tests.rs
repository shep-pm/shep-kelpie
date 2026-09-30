use std::panic::{AssertUnwindSafe, catch_unwind};

use serde_json::json;

use super::*;
use crate::ports::{Cost, Usage};
use crate::profile::INSTRUCTIONS;
use crate::settings::Effort;
use crate::test::{LEFT_BEHIND, Rig, Scripted, git};

fn usage(n: u64) -> Usage {
    Usage {
        input: n,
        cache_write: 10 * n,
        cache_read: 100 * n,
        output: 1000 * n,
    }
}

// A running project with issue 7 in flight
fn with_issue_7(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    assert_eq!(rig.ask(&runner, "add", Some("7"))["work_item"]["issue"], 7);
    (rig, runner)
}

#[test]
fn the_first_turn_starts_the_workers_session_in_its_own_worktree() {
    let (rig, runner) = with_issue_7("shep");
    rig.claude
        .script([Scripted::Reply(usage(1), Cost(20_085_300))]);
    step(&runner).unwrap();

    let [seen] = rig.claude.seen().try_into().unwrap();
    let call = seen.call;
    let worktree = rig.home.path().join("kelpie/wt/shep/7");
    assert_eq!(call.role, Role::Worker);
    assert_eq!(
        (call.model.as_str(), call.effort),
        ("claude-sonnet-5", Effort::Medium)
    );
    assert!(
        matches!(call.session, Session::New(_)),
        "{:?}",
        call.session
    );
    assert_eq!(call.cwd, worktree);
    assert!(call.prompt.starts_with("/mattpocock:implement "));
    assert!(
        call.prompt
            .ends_with("\nYour work item is issue #7: Title of #7\n\nBody of #7.\n"),
        "{}",
        call.prompt
    );
    let worker = rig.paths().worker;
    assert_eq!(call.settings, worker.join("settings.json"));
    assert_eq!(call.instructions, Some(worker.join("instructions.md")));
    let instructions = fs::read_to_string(worker.join("instructions.md")).unwrap();
    assert!(instructions.starts_with(INSTRUCTIONS), "{instructions}");
    assert!(seen.build_existed, "the build folder came after the worker");

    assert_eq!(git(&worktree, &["branch", "--show-current"]), "kelpie/7");
    let origin_main = git(&rig.repo(), &["rev-parse", "origin/main"]);
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), origin_main);
}

#[test]
fn the_branch_is_cut_from_the_latest_origin_main() {
    let rig = Rig::new("reactmap");
    let landed = rig.land_on_origin("landed.txt");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("3"));
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    step(&runner).unwrap();
    let worktree = rig.home.path().join("kelpie/wt/reactmap/3");
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), landed);
}

#[test]
fn the_settings_file_fences_writes_to_this_worktree_and_its_git_paths() {
    let (rig, runner) = with_issue_7("koji");
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    step(&runner).unwrap();
    let [seen] = rig.claude.seen().try_into().unwrap();
    let kelpie = rig.home.path().join("kelpie");
    let git_dir = fs::canonicalize(rig.repo().join(".git")).unwrap();
    let allow = &seen.settings["sandbox"]["filesystem"]["allowWrite"];
    assert_eq!(allow[0], json!(kelpie.join("wt/koji/7")));
    assert_eq!(allow[1], json!(kelpie.join("targets/koji/7")));
    assert_eq!(allow[2], json!(git_dir.join("objects")));
    assert_eq!(allow[3], json!(git_dir.join("worktrees/7")));
    assert_eq!(
        seen.settings["sandbox"]["filesystem"]["denyWrite"][0],
        json!(git_dir.join("config"))
    );
    assert_eq!(
        seen.settings["hooks"]["PreToolUse"][1]["hooks"][0]["command"],
        format!(
            "'/opt/kelpie/bin/kelpie' 'guard' '{}' '{}' '--folder={}' '--folder={}'",
            git_dir.display(),
            kelpie.join("wt/koji/7").display(),
            kelpie.display(),
            rig.repo().display()
        ),
        "kelpie's own guard, with no project hooks in the settings"
    );
}

#[test]
fn a_worker_cannot_read_the_shepherd_s_home_wherever_it_is() {
    let (rig, runner) = with_issue_7("koji");
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    step(&runner).unwrap();
    let [seen] = rig.claude.seen().try_into().unwrap();
    let rule = format!("Read(/{}/**)", rig.home.path().join("shep").display());
    let deny = seen.settings["permissions"]["deny"].as_array().unwrap();
    assert!(deny.contains(&json!(rule)), "{rule} not in {deny:?}");
    assert!(rule.starts_with("Read(//"), "{rule} is not rooted at /");
}

#[test]
fn status_shows_the_session_and_what_the_work_item_has_cost() {
    let (rig, runner) = with_issue_7("golbat");
    rig.claude
        .script([Scripted::Reply(usage(2), Cost(20_085_300))]);
    let report = step(&runner).unwrap().unwrap();
    let session = rig.claude.calls()[0].session.id().clone();
    assert_eq!(
        report,
        StepReport::Ended {
            issue: 7,
            session: session.clone(),
            usage: usage(2),
            cost_usd: 0.0200853,
            work_item_cost_usd: 0.0200853,
            pull_request: None,
        }
    );
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(item["session"], json!(session));
    assert_eq!(item["cost_usd"], 0.0200853);
    assert_eq!(item["calls"], 1);
    assert_eq!(item["turn"], json!({ "state": "ended", "at": Rig::EPOCH }));
}

#[test]
fn a_runner_killed_mid_turn_resumes_the_same_session_in_the_same_worktree() {
    let (rig, runner) = with_issue_7("rotom");
    rig.claude.script([Scripted::Kill]);
    let killed = catch_unwind(AssertUnwindSafe(|| step(&runner)));
    assert!(killed.is_err(), "the scripted kill did not happen");
    drop(runner);

    let runner = rig.open().unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["turn"]["state"],
        "running"
    );
    // The resumed call reports the whole session so far, the killed call
    // included, and all of it lands on the work item.
    rig.claude
        .script([Scripted::Reply(usage(1), Cost(30_000_000))]);
    step(&runner).unwrap();

    let [first, second] = rig.claude.calls().try_into().unwrap();
    assert_eq!(second.session, Session::Resume(first.session.id().clone()));
    assert_eq!(second.cwd, first.cwd);
    assert!(
        second.cwd.join(LEFT_BEHIND).exists(),
        "the worktree was replaced"
    );
    assert_eq!(second.prompt, CONTINUE);
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["cost_usd"],
        0.03
    );
}

#[test]
fn a_worker_that_repoints_its_git_file_cannot_move_its_fence() {
    let (rig, runner) = with_issue_7("golbat");
    rig.claude.script([Scripted::Kill]);
    let _ = catch_unwind(AssertUnwindSafe(|| step(&runner)));
    drop(runner);

    // What a worker could do from inside its worktree: a fake git dir
    // whose common dir is a folder it wants to write.
    let worktree = rig.home.path().join("kelpie/wt/golbat/7");
    let wanted = rig.home.path().join("wanted");
    let fake = worktree.join("fake");
    for dir in ["refs", "objects"] {
        fs::create_dir_all(fake.join(dir)).unwrap();
        fs::create_dir_all(wanted.join(dir)).unwrap();
    }
    fs::write(fake.join("HEAD"), "ref: refs/heads/kelpie/7\n").unwrap();
    fs::write(fake.join("commondir"), format!("{}\n", wanted.display())).unwrap();
    fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", fake.display()),
    )
    .unwrap();

    let runner = rig.open().unwrap();
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    step(&runner).unwrap();
    let [_, seen] = rig.claude.seen().try_into().unwrap();
    let git_dir = fs::canonicalize(rig.repo().join(".git")).unwrap();
    let allow = &seen.settings["sandbox"]["filesystem"]["allowWrite"];
    assert_eq!(allow[2], json!(git_dir.join("objects")));
    assert_eq!(allow[3], json!(git_dir.join("worktrees/7")));
    assert!(!allow.to_string().contains("wanted"), "{allow}");
}

#[test]
fn a_turn_stopped_with_the_runner_resumes_when_it_starts_again() {
    let (rig, runner) = with_issue_7("reactmap");
    rig.claude.script([Scripted::Fail(ClaudeError::Stopped)]);
    assert_eq!(step(&runner).unwrap(), None);
    drop(runner);

    let runner = rig.open().unwrap();
    rig.claude.script([Scripted::Reply(usage(1), Cost(9))]);
    step(&runner).unwrap();
    let [first, second] = rig.claude.calls().try_into().unwrap();
    assert_eq!(second.session, Session::Resume(first.session.id().clone()));
    assert_eq!(second.prompt, CONTINUE);
}

#[test]
fn a_session_killed_before_it_began_starts_over_with_the_same_id() {
    let (rig, runner) = with_issue_7("xilriws");
    rig.claude.script([Scripted::Kill]);
    let _ = catch_unwind(AssertUnwindSafe(|| step(&runner)));
    drop(runner);

    let runner = rig.open().unwrap();
    let first = rig.claude.calls()[0].session.id().clone();
    rig.claude.script([
        Scripted::Fail(ClaudeError::NoSession(first.clone())),
        Scripted::Reply(usage(1), Cost(5)),
    ]);
    step(&runner).unwrap();
    let [original, resumed, again] = rig.claude.calls().try_into().unwrap();
    assert_eq!(resumed.session, Session::Resume(first.clone()));
    assert_eq!(again.session, Session::New(first));
    assert_eq!(again.prompt, original.prompt);
}

#[test]
fn a_paused_project_runs_no_turn_until_it_starts() {
    let rig = Rig::new("chelone");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("5"));
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.claude.calls(), []);
    assert!(!rig.home.path().join("kelpie/wt/chelone/5").exists());

    rig.ask(&runner, "start", None);
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    assert!(step(&runner).unwrap().is_some());
    assert_eq!(rig.claude.calls().len(), 1);
}

#[test]
fn a_failed_turn_raises_a_ruling_carrying_why_and_alerts_like_the_rest() {
    let (rig, runner) = with_issue_7("zeus");
    rig.claude
        .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
    let Some(StepReport::Failed {
        issue,
        pull_request,
        id,
        question,
        ..
    }) = step(&runner).unwrap()
    else {
        panic!("the failed turn raised no ruling");
    };
    assert_eq!((issue, pull_request, id), (7, None, 1));
    assert!(
        question.starts_with("The worker's turn on issue #7 failed: claude failed: overloaded."),
        "{question}"
    );
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(
        item["turn"],
        json!({ "state": "failed", "at": Rig::EPOCH, "reason": "claude failed: overloaded" })
    );
    assert_eq!(item["phase"], json!({ "state": "ruling", "id": 1 }));

    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    let [(_, alert)] = rig.alerts.posts().try_into().unwrap();
    assert_eq!(alert.text, question);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.claude.calls().len(), 1);
}

#[test]
fn a_yes_on_a_failed_turn_resumes_its_session() {
    let (rig, runner) = with_issue_7("zeus");
    rig.claude
        .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
    step(&runner).unwrap();
    rig.ask(&runner, "rule", Some("1 yes"));
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ended { .. })
    ));
    let [failed, retried] = rig.claude.calls().try_into().unwrap();
    assert_eq!(
        retried.session,
        Session::Resume(failed.session.id().clone())
    );
    assert_eq!(
        retried.prompt,
        "Your last turn failed before it finished. \
         Carry on with the work item from where you left off."
    );
}

#[test]
fn a_retry_whose_session_never_began_starts_it_over_from_the_issue() {
    let (rig, runner) = with_issue_7("zeus");
    rig.claude
        .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
    step(&runner).unwrap();
    let first = rig.claude.calls()[0].session.id().clone();
    rig.ask(&runner, "rule", Some("1 yes"));
    rig.claude.script([
        Scripted::Fail(ClaudeError::NoSession(first.clone())),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ended { .. })
    ));
    let [original, _, again] = rig.claude.calls().try_into().unwrap();
    assert_eq!(again.session, Session::New(first));
    assert_eq!(again.prompt, original.prompt);
}

#[test]
fn a_retried_turn_gets_a_whole_ceiling_however_long_the_ruling_waited() {
    let (rig, runner) = with_issue_7("zeus");
    rig.claude
        .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
    step(&runner).unwrap();
    rig.clock.advance(2000);
    rig.ask(&runner, "rule", Some("1 yes"));
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    step(&runner).unwrap();
    let [_, retried] = rig.claude.calls().try_into().unwrap();
    assert_eq!(retried.timeout, Some(Duration::from_secs(3600)));
}

#[test]
fn a_no_on_a_failed_turn_stops_the_work_item_the_way_a_timed_out_one_does() {
    let (rig, runner) = with_issue_7("rotom");
    rig.claude
        .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
    step(&runner).unwrap();
    rig.ask(&runner, "rule", Some("1 no not worth another go"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished {
            issue: 7,
            pull_request: None,
            merged: false,
            ..
        })
    ));
    assert!(!rig.worktree_7().exists());
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
}

#[test]
fn a_turn_past_its_ceiling_is_stopped_and_a_yes_resumes_its_session() {
    let (rig, runner) = with_issue_7("zeus");
    rig.claude.script([Scripted::Fail(ClaudeError::TimedOut)]);
    let Some(StepReport::TimedOut {
        issue,
        session,
        pull_request,
        id,
        question,
        ..
    }) = step(&runner).unwrap()
    else {
        panic!("the timed-out turn raised no ruling");
    };
    assert_eq!((issue, pull_request, id), (7, None, 1));
    assert!(
        question.starts_with(
            "The worker on issue #7 has been running past its turn's ceiling, \
             and kelpie stopped it."
        ),
        "{question}"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({ "state": "ruling", "id": 1 })
    );

    rig.ask(&runner, "rule", Some("1 yes"));
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ended { .. })
    ));
    let [_, resumed] = rig.claude.calls().try_into().unwrap();
    assert_eq!(resumed.session, Session::Resume(session));
    assert_eq!(
        resumed.prompt,
        "Kelpie stopped your last turn: it ran past its ceiling. \
         Carry on with the work item from where you left off."
    );
}

#[test]
fn a_no_on_a_timed_out_turn_stops_the_work_item_keeping_nothing_of_its_own() {
    let (rig, runner) = with_issue_7("rotom");
    rig.claude.script([Scripted::Fail(ClaudeError::TimedOut)]);
    step(&runner).unwrap();
    rig.ask(&runner, "rule", Some("1 no not worth waiting for"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished {
            issue: 7,
            pull_request: None,
            merged: false,
            ..
        })
    ));
    assert!(!rig.worktree_7().exists());
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
}

#[test]
fn a_worker_turn_carries_the_projects_timeout() {
    let (rig, runner) = with_issue_7("golbat");
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    step(&runner).unwrap();
    let [seen] = rig.claude.calls().try_into().unwrap();
    assert_eq!(seen.timeout, Some(std::time::Duration::from_secs(3600)));
}

#[test]
fn a_restart_before_the_ceiling_passes_resumes_with_only_the_time_left() {
    let (rig, runner) = with_issue_7("zeus");
    rig.claude.script([Scripted::Kill]);
    let _ = catch_unwind(AssertUnwindSafe(|| step(&runner)));
    drop(runner);

    rig.clock.advance(2000);
    let runner = rig.open().unwrap();
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    step(&runner).unwrap();
    let [_, resumed] = rig.claude.calls().try_into().unwrap();
    assert_eq!(
        resumed.timeout,
        Some(std::time::Duration::from_secs(1600)),
        "the restart must not reset the ceiling to a fresh hour"
    );
}

#[test]
fn a_restart_after_the_ceiling_passed_parks_it_with_no_call_spent() {
    let (rig, runner) = with_issue_7("chelone");
    rig.claude.script([Scripted::Kill]);
    let _ = catch_unwind(AssertUnwindSafe(|| step(&runner)));
    drop(runner);

    // The whole ceiling, and then some, passes while kelpie is down.
    rig.clock.advance(3601);
    let runner = rig.open().unwrap();
    let Some(StepReport::TimedOut { id, .. }) = step(&runner).unwrap() else {
        panic!("a turn found past its ceiling on restart raised no ruling");
    };
    assert_eq!(id, 1);
    assert_eq!(
        rig.claude.calls().len(),
        1,
        "the killed call, and no second one"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({ "state": "ruling", "id": 1 })
    );
}

#[test]
fn a_foreign_folder_where_the_worktree_goes_fails_the_turn_before_any_call() {
    let (rig, runner) = with_issue_7("koji");
    let folder = rig.home.path().join("kelpie/wt/koji/7");
    fs::create_dir_all(&folder).unwrap();
    let Some(StepReport::Failed { question, .. }) = step(&runner).unwrap() else {
        panic!("the turn ran in a folder kelpie did not make");
    };
    assert!(
        question.contains("is not this work item's worktree"),
        "{question}"
    );
    assert_eq!(rig.claude.calls(), []);

    // A yes retries the step that failed: the first turn, from the issue.
    fs::remove_dir_all(&folder).unwrap();
    rig.ask(&runner, "rule", Some("1 yes"));
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    step(&runner).unwrap();
    let [call] = rig.claude.calls().try_into().unwrap();
    assert!(
        matches!(call.session, Session::New(_)),
        "{:?}",
        call.session
    );
    assert!(call.prompt.starts_with("/mattpocock:implement "));
    assert!(
        call.prompt
            .ends_with("\nYour work item is issue #7: Title of #7\n\nBody of #7.\n"),
        "{}",
        call.prompt
    );
}

#[test]
fn a_branch_left_behind_stays_a_refusal_for_a_new_work_item_even_when_it_matches_main() {
    let (rig, runner) = with_issue_7("golbat");
    git(&rig.repo(), &["branch", "kelpie/7", "origin/main"]);
    let Some(StepReport::Failed { question, .. }) = step(&runner).unwrap() else {
        panic!("a fresh work item took a branch that was not its own");
    };
    assert!(
        question.contains("branch kelpie/7 already exists without its worktree"),
        "{question}"
    );
    assert_eq!(rig.claude.calls(), []);
}
