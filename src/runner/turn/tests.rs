use std::panic::{AssertUnwindSafe, catch_unwind};

use serde_json::json;

use super::*;
use crate::ports::{Cost, Usage};
use crate::profile;
use crate::settings::Effort;
use crate::test::{Hold, LEFT_BEHIND, Rig, Scripted, git, write_in};

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
    let worktree = rig.home.path().join("shep/kelpie/shep/worktrees/7");
    assert_eq!(call.role, Role::Worker);
    assert_eq!(
        (call.model.as_str(), call.effort),
        ("claude-sonnet-5-5", Effort::High)
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
    assert!(
        instructions.starts_with(&profile::instructions(std::path::Path::new(Rig::KELPIE))),
        "{instructions}"
    );
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
    let worktree = rig.home.path().join("shep/kelpie/reactmap/worktrees/3");
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), landed);
}

#[test]
fn the_settings_file_fences_writes_to_this_worktree_and_its_git_paths() {
    let (rig, runner) = with_issue_7("koji");
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    step(&runner).unwrap();
    let [seen] = rig.claude.seen().try_into().unwrap();
    let kelpie = rig.home.path().join("shep/kelpie");
    let git_dir = fs::canonicalize(rig.repo().join(".git")).unwrap();
    let allow = &seen.sandbox["filesystem"]["allowWrite"];
    assert_eq!(allow[0], json!(kelpie.join("koji/worktrees/7")));
    assert_eq!(allow[1], json!(kelpie.join("koji/builds/7")));
    assert_eq!(allow[2], json!(git_dir.join("objects")));
    assert_eq!(allow[3], json!(git_dir.join("worktrees/7")));
    assert_eq!(
        seen.sandbox["filesystem"]["denyWrite"][0],
        json!(git_dir.join("config"))
    );
    assert_eq!(
        seen.settings["hooks"]["PreToolUse"][1]["hooks"][0]["command"],
        format!(
            "'/opt/kelpie/bin/kelpie' 'guard' '{}' '{}' '--folder={}' '--folder={}'",
            git_dir.display(),
            kelpie.join("koji/worktrees/7").display(),
            kelpie.display(),
            rig.repo().display()
        ),
        "kelpie's own guard, with no project hooks in the settings"
    );
}

#[test]
fn a_worker_reads_its_worktree_inside_the_shepherds_home_and_not_shep_s_socket() {
    let (rig, runner) = with_issue_7("koji");
    let shep = rig.home.path().join("shep");
    write_in(&shep, "run/shep.sock", "");
    write_in(&shep, "dogs.toml", "");
    write_in(&shep, "kelpie/rotom/state.json", "{}");
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    step(&runner).unwrap();
    let [seen] = rig.claude.seen().try_into().unwrap();
    let kelpie = shep.join("kelpie");
    let (worktree, build) = (
        kelpie.join("koji/worktrees/7"),
        kelpie.join("koji/builds/7"),
    );
    assert_eq!(seen.call.cwd, worktree);

    // The sandbox denies the whole home and lets the worker back into its own folders.
    let files = &seen.sandbox["filesystem"];
    let shep_rule = format!("{}/**", shep.display());
    assert!(
        files["denyRead"]
            .as_array()
            .unwrap()
            .contains(&json!(shep_rule))
    );
    let allowed = files["allowRead"].as_array().unwrap();
    for own in [&worktree, &build] {
        assert!(allowed.contains(&json!(own)), "{own:?} not in {allowed:?}");
    }
    assert!(
        files["allowWrite"]
            .as_array()
            .unwrap()
            .contains(&json!(worktree))
    );

    // Claude Code's own rules take no exceptions, so they name each entry around them.
    let deny: Vec<&str> = seen.settings["permissions"]["deny"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    let rule = |path: &Path| format!("Read(/{})", path.display());
    for denied in [
        rule(&shep.join("run/**")),
        rule(&shep.join("dogs.toml")),
        rule(&kelpie.join("rotom/**")),
        rule(&kelpie.join("koji/state.json")),
    ] {
        assert!(deny.contains(&denied.as_str()), "{denied} not in {deny:?}");
    }
    for read in [&worktree, &build] {
        let hidden = deny.iter().find(|rule| {
            let glob = rule.trim_start_matches("Read(/").trim_end_matches(')');
            let folder = glob.trim_end_matches("**").trim_end_matches('/');
            read.starts_with(folder)
        });
        assert_eq!(hidden, None, "a rule hides {read:?}");
    }
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
            cost_usd: Some(0.0200853),
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
    let worktree = rig.home.path().join("shep/kelpie/golbat/worktrees/7");
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
    let allow = &seen.sandbox["filesystem"]["allowWrite"];
    assert_eq!(allow[2], json!(git_dir.join("objects")));
    assert_eq!(allow[3], json!(git_dir.join("worktrees/7")));
    assert!(!allow.to_string().contains("wanted"), "{allow}");
}

#[test]
fn a_turn_stopped_with_the_runner_resumes_when_it_starts_again() {
    let (rig, runner) = with_issue_7("reactmap");
    rig.claude.script([Scripted::Fail(AgentError::Stopped)]);
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
        Scripted::Fail(AgentError::NoSession(
            crate::settings::Harness::ClaudeCode,
            first.clone(),
        )),
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
    assert!(
        !rig.home
            .path()
            .join("shep/kelpie/chelone/worktrees/5")
            .exists()
    );

    rig.ask(&runner, "start", None);
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    assert!(step(&runner).unwrap().is_some());
    assert_eq!(rig.claude.calls().len(), 1);
}

#[test]
fn a_failed_turn_raises_a_ruling_carrying_why_and_alerts_like_the_rest() {
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([Scripted::Fail(AgentError::Failed(
        crate::settings::Harness::ClaudeCode,
        "overloaded".into(),
    ))]);
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
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([Scripted::Fail(AgentError::Failed(
        crate::settings::Harness::ClaudeCode,
        "overloaded".into(),
    ))]);
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
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([Scripted::Fail(AgentError::Failed(
        crate::settings::Harness::ClaudeCode,
        "overloaded".into(),
    ))]);
    step(&runner).unwrap();
    let first = rig.claude.calls()[0].session.id().clone();
    rig.ask(&runner, "rule", Some("1 yes"));
    rig.claude.script([
        Scripted::Fail(AgentError::NoSession(
            crate::settings::Harness::ClaudeCode,
            first.clone(),
        )),
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
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([Scripted::Fail(AgentError::Failed(
        crate::settings::Harness::ClaudeCode,
        "overloaded".into(),
    ))]);
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
    rig.claude.script([Scripted::Fail(AgentError::Failed(
        crate::settings::Harness::ClaudeCode,
        "overloaded".into(),
    ))]);
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
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([Scripted::Fail(AgentError::TimedOut(
        crate::settings::Harness::ClaudeCode,
    ))]);
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
    rig.claude.script([Scripted::Fail(AgentError::TimedOut(
        crate::settings::Harness::ClaudeCode,
    ))]);
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
    let (rig, runner) = with_issue_7("acme");
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
    let folder = rig.home.path().join("shep/kelpie/koji/worktrees/7");
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

const SENT_BACK: &str = "Your last turn ended with no pull request for this work item \
                         and no question. If a tool failed, try it again or find another \
                         way, and open the draft pull request once the work is done. If \
                         only the maintainer can unblock you, end your reply with a \
                         <kelpie-question> block.";

#[test]
fn a_turn_that_ends_with_no_pull_request_and_no_question_sends_the_worker_back_once() {
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([
        Scripted::Say("cargo test failed in the sandbox, so I stopped."),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    step(&runner).unwrap();
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ended { issue: 7, .. })
    ));
    let [first, again] = rig.claude.calls().try_into().unwrap();
    assert_eq!(again.session, Session::Resume(first.session.id().clone()));
    assert_eq!(again.prompt, SENT_BACK);
}

#[test]
fn a_turn_that_ends_with_uncommitted_files_and_no_push_is_sent_back_naming_them() {
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([
        Scripted::Plant("fix.txt", "fixed\n"),
        Scripted::Push("fix.txt", "fixed\n"),
    ]);
    step(&runner).unwrap();
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ended { issue: 7, .. })
    ));
    let [first, again] = rig.claude.calls().try_into().unwrap();
    assert_eq!(again.session, Session::Resume(first.session.id().clone()));
    assert!(
        again
            .prompt
            .starts_with("Your last turn ended with uncommitted changes in your worktree"),
        "{}",
        again.prompt
    );
    assert!(again.prompt.contains("fix.txt"), "{}", again.prompt);
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"],
        json!([]),
        "the worker was sent back before any ruling"
    );
}

#[test]
fn a_worker_sent_back_for_uncommitted_files_is_not_sent_back_for_them_again() {
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([
        Scripted::Plant("fix.txt", "fixed\n"),
        Scripted::Plant("fix.txt", "fixed\n"),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    for _ in 0..3 {
        step(&runner).unwrap();
    }
    let [_, dirty, stopped] = rig.claude.calls().try_into().unwrap();
    assert!(dirty.prompt.contains("fix.txt"), "{}", dirty.prompt);
    assert_eq!(stopped.prompt, SENT_BACK);
}

// Turn 1 pushes the branch and opens no pull request. Turn 2 opens one, pushes
// nothing and leaves a file uncommitted, so it is sent back to commit. The
// pull request was found at turn 2's end, so the review is still owed.
#[test]
fn a_pull_request_found_on_a_turn_sent_back_to_commit_still_goes_to_review_first() {
    let (rig, runner) = with_issue_7("acme");
    let hold = Hold::default();
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Hold(hold.clone()),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    step(&runner).unwrap();
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| step(&runner));
        assert!(hold.entered(Duration::from_secs(30)), "turn 2 never ran");
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        write_in(&rig.worktree_7(), "left.txt", "left\n");
        hold.release();
        turn.join().unwrap().unwrap();
    });
    step(&runner).unwrap();
    let [_, _, commit] = rig.claude.calls().try_into().unwrap();
    assert!(commit.prompt.contains("left.txt"), "{}", commit.prompt);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["pull_request"], 71);
    assert_eq!(status["work_item"]["phase"]["state"], "review");
}

// A worktree whose git kelpie will not trust cannot be listed, and the turn
// ends as it did before, with no follow-up and no hold-up.
#[test]
fn a_worktree_git_cannot_be_read_in_ends_the_turn_without_a_follow_up() {
    let (rig, runner) = with_issue_7("acme");
    let hold = Hold::default();
    rig.claude.script([
        Scripted::Hold(hold.clone()),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    let worktree = rig.worktree_7();
    let ended = std::thread::scope(|scope| {
        let turn = scope.spawn(|| step(&runner));
        assert!(hold.entered(Duration::from_secs(30)), "the turn never ran");
        write_in(&worktree, "left.txt", "left\n");
        // The repo no longer says this worktree is its own.
        fs::remove_file(rig.repo().join(".git/worktrees/7/gitdir")).unwrap();
        hold.release();
        turn.join().unwrap().unwrap()
    });
    assert!(matches!(ended, Some(StepReport::Ended { issue: 7, .. })));
    let _ = step(&runner);
    for call in rig.claude.calls() {
        assert!(
            !call
                .prompt
                .starts_with("Your last turn ended with uncommitted"),
            "{}",
            call.prompt
        );
    }
}

#[test]
fn a_change_only_staged_counts_as_uncommitted() {
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([
        Scripted::Stage("staged.txt", "staged\n"),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    let [_, again] = rig.claude.calls().try_into().unwrap();
    assert!(again.prompt.contains("staged.txt"), "{}", again.prompt);
}

#[test]
fn a_turn_that_ends_with_a_clean_worktree_is_not_sent_back_for_uncommitted_files() {
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([
        Scripted::Reply(usage(1), Cost(1)),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    let [_, again] = rig.claude.calls().try_into().unwrap();
    assert_eq!(again.prompt, SENT_BACK);
}

#[test]
fn a_second_turn_that_stops_short_parks_the_worker_on_a_ruling() {
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([
        Scripted::Reply(usage(1), Cost(1)),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    let Some(StepReport::Failed {
        issue: 7,
        pull_request: None,
        id: 1,
        question,
        ..
    }) = step(&runner).unwrap()
    else {
        panic!("the worker was not parked");
    };
    assert!(
        question.starts_with(
            "The worker's turn on issue #7 failed: \
             it ended twice with no pull request and no question."
        ),
        "{question}"
    );
    assert!(question.ends_with("stops the work item."), "{question}");
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.claude.calls().len(), 2);

    // A yes sends it back again, into the same session.
    rig.ask(&runner, "rule", Some("1 yes"));
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    step(&runner).unwrap();
    let [first, _, third] = rig.claude.calls().try_into().unwrap();
    assert_eq!(third.session, Session::Resume(first.session.id().clone()));
    assert_eq!(third.prompt, SENT_BACK);
}

#[test]
fn a_no_on_a_worker_that_stopped_short_stops_the_work_item() {
    let (rig, runner) = with_issue_7("rotom");
    rig.claude.script([
        Scripted::Reply(usage(1), Cost(1)),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    for _ in 0..3 {
        step(&runner).unwrap();
    }
    rig.ask(&runner, "rule", Some("1 no the issue is already fixed"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished {
            issue: 7,
            merged: false,
            ..
        })
    ));
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
}

#[test]
fn a_pull_request_the_turns_end_missed_goes_to_review_instead() {
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    rig.forge.set_board_down(true);
    step(&runner).unwrap();
    // The forge cannot be asked, so nothing is sent back yet.
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::GateFailed { issue: 7, .. })
    ));
    rig.forge.set_board_down(false);
    rig.forge.open_pull_request(70, "kelpie/7", &[7]);
    step(&runner).unwrap();
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(item["pull_request"], 70);
    assert_eq!(item["phase"]["state"], "review");
    assert_eq!(rig.claude.calls().len(), 1, "the worker was not sent back");
}
