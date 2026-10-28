use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;

use serde_json::{Value, json};

use super::*;
use crate::home::shepherd_address;
use crate::ports::{Cost, Usage};
use crate::profile;
use crate::runner::step;
use crate::runner::turn::paused::PAUSED;
use crate::settings::Effort;
use crate::test::{Hold, LEFT_BEHIND, Rig, Scripted, assert_reads, git, ticket, write_in};

pub(super) fn usage(n: u64) -> Usage {
    Usage {
        input: n,
        cache_write: 10 * n,
        cache_write_5m: 0,
        cache_read: 100 * n,
        output: 1000 * n,
    }
}

// A running project with issue 7 in flight
pub(super) fn with_issue_7(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    let runner = rig.open().unwrap();
    assert_eq!(
        rig.ask(&runner, "add", Some("7"))["work_item"]["issue"],
        "7"
    );
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
    assert_eq!(call.settings, worker.join("settings-7.json"));
    assert_eq!(call.instructions, Some(worker.join("instructions-7.md")));
    let instructions = fs::read_to_string(worker.join("instructions-7.md")).unwrap();
    assert!(
        instructions.starts_with(&profile::instructions(
            std::path::Path::new(Rig::KELPIE),
            None,
        )),
        "{instructions}"
    );
    assert!(seen.build_existed, "the build folder came after the worker");

    assert_eq!(git(&worktree, &["branch", "--show-current"]), "kelpie/7");
    let origin_main = git(&rig.repo(), &["rev-parse", "origin/main"]);
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), origin_main);
}

// The prompt a worker's first turn gets, for a ticket of each source. A
// GitHub ticket's is worded as it always was: its id is its issue number.
pub(super) fn issue_of() -> Issue {
    Issue {
        title: "Title of #7".to_owned(),
        body: "Body of #7.\n\n".to_owned(),
        labels: Vec::new(),
        open: true,
        parent: None,
        blocked_by: Vec::new(),
    }
}

#[test]
fn a_github_ticket_s_first_prompt_is_unchanged() {
    assert_eq!(
        first_prompt(ticket(7), &issue_of(), &[]),
        "Your work item is issue #7: Title of #7\n\nBody of #7.\n"
    );
}

#[test]
fn a_ticket_from_another_source_names_its_first_prompt_by_its_id() {
    let local: TicketId = "k12".parse().unwrap();
    assert_eq!(
        first_prompt(local, &issue_of(), &[]),
        "Your work item is ticket k12: Title of #7\n\nBody of #7.\n"
    );
}

#[test]
fn a_turn_logs_its_work_item_role_agent_and_session_as_its_call_starts() {
    let (rig, runner) = with_issue_7("shep");
    rig.claude
        .script([Scripted::Reply(usage(1), Cost(20_085_300))]);
    step(&runner).unwrap();

    let [seen] = rig.claude.seen().try_into().unwrap();
    let started = runner.lock().unwrap().take_started();
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(
        serde_json::to_value(&started[0]).unwrap(),
        json!({
            "step": "session-started",
            "issue": "7",
            "role": "worker",
            "agent": "sonnet-high",
            "harness": "claude-code",
            "session": seen.call.session.id().0,
            "transcript": {"folder": seen.call.settings.with_extension("transcripts")},
        })
    );
}

#[test]
fn the_branch_is_cut_from_the_latest_origin_main() {
    let rig = Rig::new("reactmap");
    let landed = rig.land_on_origin("landed.txt");
    let runner = rig.open().unwrap();
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
    let home = rig.home.path();
    let shep = home.join("shep");
    // A named pipe is no file, so only a Unix shepherd leaves one to read.
    if cfg!(unix) {
        let socket = shepherd_address(&shep);
        write_in(
            &shep,
            socket.strip_prefix(&shep).unwrap().to_str().unwrap(),
            "",
        );
    }
    write_in(&shep, "dogs.toml", "");
    write_in(&shep, "kelpie/rotom/state.json", "{}");
    // Another repo of the same home, private and holding its own secret.
    write_in(home, "GitHub/rotom/.env", "SECRET=1\n");
    write_in(home, "GitHub/rotom/src/lib.rs", "work\n");
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
    let denied = files["denyRead"].as_array().unwrap();
    assert!(
        denied.contains(&json!(format!("{}/**", home.display()))),
        "{denied:?}"
    );
    assert!(
        !denied.contains(&json!(format!("{}/**", shep.display()))),
        "the home already covers it: {denied:?}"
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
        rule(&home.join("GitHub/**")),
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
    // Its shell is held to the same set: the other repo's file is denied by the
    // sandbox, and nothing allows it back.
    let other = home.join("GitHub/rotom/.env");
    let allowed = files["allowRead"].as_array().unwrap();
    assert!(
        allowed
            .iter()
            .filter_map(Value::as_str)
            .all(|folder| !other.starts_with(folder.trim_end_matches("/**"))),
        "{other:?} is allowed back: {allowed:?}"
    );
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
            issue: ticket(7),
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
    assert_eq!((issue, pull_request, id), (ticket(7), None, ticket(7)));
    assert!(
        question.starts_with("The worker's turn on issue #7 failed: claude failed: overloaded."),
        "{question}"
    );
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(
        item["turn"],
        json!({ "state": "failed", "at": Rig::EPOCH, "reason": "claude failed: overloaded" })
    );
    assert_eq!(item["phase"], json!({ "state": "ruling", "id": "7" }));

    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(7) })
    );
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
    rig.ask(&runner, "rule", Some("7 yes"));
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
    rig.ask(&runner, "rule", Some("7 yes"));
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
    assert!(
        again.prompt.starts_with(&original.prompt),
        "{}",
        again.prompt
    );
    assert!(
        again.prompt.ends_with(
            "\n\nYour last turn failed before it finished. \
             Carry on with the work item from where you left off."
        ),
        "{}",
        again.prompt
    );
}

#[test]
fn a_no_on_a_failed_turn_stops_the_work_item_the_way_a_timed_out_one_does() {
    let (rig, runner) = with_issue_7("rotom");
    rig.claude.script([Scripted::Fail(AgentError::Failed(
        crate::settings::Harness::ClaudeCode,
        "overloaded".into(),
    ))]);
    step(&runner).unwrap();
    rig.ask(&runner, "rule", Some("7 no not worth another go"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished {
            issue,
            pull_request: None,
            merged: false,
            ..
        })
     if issue == ticket(7)));
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
    assert_eq!((issue, pull_request, id), (ticket(7), None, ticket(7)));
    assert!(
        question.starts_with(
            "The worker on issue #7 has been running past its turn's ceiling, \
             and kelpie stopped it."
        ),
        "{question}"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({ "state": "ruling", "id": "7" })
    );

    rig.ask(&runner, "rule", Some("7 yes"));
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
         Commit any work that builds now, then carry on from where you left off."
    );
}

#[test]
fn a_no_on_a_timed_out_turn_stops_the_work_item_keeping_nothing_of_its_own() {
    let (rig, runner) = with_issue_7("rotom");
    rig.claude.script([Scripted::Fail(AgentError::TimedOut(
        crate::settings::Harness::ClaudeCode,
    ))]);
    step(&runner).unwrap();
    rig.ask(&runner, "rule", Some("7 no not worth waiting for"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished {
            issue,
            pull_request: None,
            merged: false,
            ..
        })
     if issue == ticket(7)));
    assert!(!rig.worktree_7().exists());
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
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
    assert_eq!(id, ticket(7));
    assert_eq!(
        rig.claude.calls().len(),
        1,
        "the killed call, and no second one"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({ "state": "ruling", "id": "7" })
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
    rig.ask(&runner, "rule", Some("7 yes"));
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

// Steps issue 7's turn with a tool call that a `drain pause` lands in, as
// `shep kelpie pause` sends it while the worker is mid-turn, and the hook
// then ends the turn at that call's result. `then` is how the turn would
// have gone on.
pub(in crate::runner) fn step_paused_in_a_tool(
    rig: &Rig,
    runner: &Mutex<Runner>,
    then: Scripted,
) -> Option<StepReport> {
    let tool = Hold::default();
    rig.claude
        .script([Scripted::AtTool(tool.clone(), Box::new(then))]);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            assert!(
                tool.entered(crate::test::CALL_PATIENCE),
                "the tool call never began"
            );
            // A `drain pause` that lands while the step holds the runner's
            // lock is answered from the posted copy, and its pause waits for a
            // pass the step will not run until the call ends. So it asks
            // again, as `shep kelpie pause` polls, until one reaches the runner.
            let deadline = std::time::Instant::now() + crate::test::CALL_PATIENCE;
            loop {
                let drained = rig.ask(runner, "drain", Some("pause"));
                if drained["draining"]["at_tool"] == json!(["7"]) {
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "{drained}");
                std::thread::yield_now();
            }
            tool.release();
        });
        step(runner).unwrap()
    })
}

#[test]
fn a_paused_turn_ends_at_its_next_tool_result_and_its_item_keeps_its_stage() {
    let (rig, runner) = with_issue_7("shep");
    let before = rig.ask(&runner, "status", None)["work_item"]["phase"].clone();

    let ended = step_paused_in_a_tool(&rig, &runner, Scripted::Push("a.txt", "a"));

    assert_eq!(
        ended, None,
        "a paused turn reports nothing: it did not finish"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"], before);
    assert_eq!(
        status["work_item"]["phase"],
        json!({ "state": "implement" })
    );
    assert_eq!(status["work_item"]["turn"]["state"], "next");
    assert_eq!(status["work_item"]["turn"]["prompt"], PAUSED);
    assert_eq!(status["rulings"], json!([]));
    // What the turn would have gone on to push was not pushed: it ended at the tool.
    let [paused] = rig.claude.calls().try_into().unwrap();
    assert!(
        git(&rig.repo(), &["ls-remote", "origin", "kelpie/7"]).is_empty(),
        "{paused:?}"
    );
}

#[test]
fn a_paused_turn_runs_no_unpushed_check_and_raises_no_ruling() {
    let (rig, runner) = with_issue_7("shep");
    let worktree = rig.home.path().join("shep/kelpie/shep/worktrees/7");
    assert_eq!(
        step_paused_in_a_tool(&rig, &runner, Scripted::Say("done")),
        None
    );
    // Work left uncommitted and nothing pushed or opened: a turn that finished
    // so would be sent back for it, and a second one parked on a ruling.
    write_in(&worktree, "scratch.txt", "in progress\n");

    for _ in 0..2 {
        rig.ask(&runner, "undrain", None);
        let ended = step_paused_in_a_tool(&rig, &runner, Scripted::Say("done"));
        assert_eq!(ended, None);
    }

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"], json!([]), "a pause raises no ruling");
    assert_eq!(
        status["work_item"]["phase"],
        json!({ "state": "implement" })
    );
    let calls = rig.claude.calls();
    assert_eq!(calls.len(), 3);
    assert!(
        calls[1..].iter().all(|c| c.prompt == PAUSED),
        "no unpushed check sent the worker back: {:?}",
        calls.iter().map(|c| &c.prompt).collect::<Vec<_>>()
    );
}

#[test]
fn the_turn_after_a_pause_resumes_the_session_with_a_fresh_ceiling() {
    let (rig, runner) = with_issue_7("shep");
    let ended = step_paused_in_a_tool(&rig, &runner, Scripted::Say("done"));
    assert_eq!(ended, None);
    // The runner stops, and the whole ceiling passes before it starts again.
    drop(runner);
    rig.clock.advance(3601);
    let runner = rig.open().unwrap();

    rig.claude.script([Scripted::Reply(usage(1), Cost(9))]);
    let resumed = step(&runner).unwrap();

    assert!(
        matches!(resumed, Some(StepReport::Ended { .. })),
        "a ceiling from before the pause parked it: {resumed:?}"
    );
    let [first, second] = rig.claude.calls().try_into().unwrap();
    assert_eq!(second.session, Session::Resume(first.session.id().clone()));
    assert_eq!(second.prompt, PAUSED);
}

// A worker's first turn fetches before it makes its worktree, and the fetch can wait on the
// network for seconds. A trigger sent in that time is answered, not queued behind the lock.
#[test]
fn a_trigger_is_answered_while_a_worktree_is_prepared() {
    let (rig, runner) = with_issue_7("shep");
    rig.claude
        .script([Scripted::Reply(usage(1), Cost(20_085_300))]);
    let read = rig.hold_next_git_fetch();

    let (reply, pass) = rig.asked_while_held(&runner, read, "status", None);

    let reply = reply.expect("the trigger waited for the lock the fetch held");
    assert_eq!(reply["work_items"].as_array().unwrap().len(), 1);
    assert!(
        matches!(pass, Ok(crate::runner::flight::Pass::Started)),
        "the turn did not start: {pass:?}"
    );
}

mod base;
mod browser;
mod budget;
mod ceiling;
mod dep_sources;
mod dev_server;
mod draining;
mod hand_over;
mod held;
mod install;
mod main_fetch;
mod maintainer_login;
mod moved;
mod off_lock;
mod pause_log;
mod question;
mod registries;
mod script_cli;
mod unfinished;
