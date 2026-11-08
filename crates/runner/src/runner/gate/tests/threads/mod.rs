//! An open review thread holds the merge until the project manager rules on it,
//! through the runner's stand-ins

use std::time::Instant;

use serde_json::json;

use crate::board::READY;
use crate::ports::{Cost, PullRequestState, Role, Standing};
use crate::runner::flight::tests::Driven;
use crate::runner::retro::journal::{self, Entry};
use crate::runner::review_bot::is_reply;
use crate::runner::review_bot::two_bots::listing;
use crate::runner::{StepReport, step};
use crate::test::{CALL_PATIENCE, Hold, Scripted, ticket};

use helpers::*;

mod helpers;
mod more;

#[test]
fn an_open_nit_holds_the_merge_and_asks_the_project_manager() {
    let (rig, runner, head) = at_the_gate(pm_project("threads-nit"), &[NIT]);
    pm_says(&rig, [NOTHING]);

    waits_with(&rig, &runner, THREADS_HELD);
    let [wake] = woken(&rig, 1).try_into().unwrap();
    assert!(
        wake.call
            .prompt
            .contains("1. work.txt:1 (cubic-dev-ai[bot], LOW): The name could be shorter."),
        "{}",
        wake.call.prompt
    );
    assert!(
        wake.call.prompt.contains(&head[..7]),
        "{}",
        wake.call.prompt
    );

    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["phase"]["state"], "ci",
        "it waits in CI"
    );
    assert_eq!(status["rulings"], json!([]), "no merge ruling yet");

    step(&runner).unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"],
        json!([]),
        "a wake that rules on nothing raises no merge ruling either"
    );
    assert_eq!(
        pm_seen(&rig).len(),
        1,
        "the next ask waits out the item's back-off"
    );
}

#[test]
fn an_open_writers_thread_holds_the_merge_and_asks_the_project_manager() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-writer"), &[]);
    declined_thread(&rig, &runner);
    pm_says(&rig, [NOTHING]);

    waits_with(&rig, &runner, THREADS_HELD);
    let [wake] = woken(&rig, 1).try_into().unwrap();
    assert!(
        wake.call.prompt.contains("1. work.txt:3 (octocat,"),
        "{}",
        wake.call.prompt
    );
    assert!(
        wake.call.prompt.contains("Rename `bleats`."),
        "{}",
        wake.call.prompt
    );
    let line = wake
        .call
        .prompt
        .lines()
        .find(|l| l.contains("1. work.txt:3"));
    assert!(
        !line.expect("the thread's line").contains("public API"),
        "the worker's answer is not the thread: {}",
        wake.call.prompt
    );
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn the_project_manager_cannot_close_the_maintainers_thread() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-close-refused"), &[]);
    declined_thread(&rig, &runner);

    let (_, dropped) = ruled(&ruled_by(&rig, &runner, CLOSE));

    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert!(
        dropped[0].starts_with("close #7 thread 1: the maintainer wrote in it"),
        "{dropped:?}"
    );
    let said = rig.forge.coderabbit.replies();
    assert!(
        !said
            .iter()
            .any(|(_, body)| body.starts_with("Closed as wrong")),
        "{said:?}"
    );
    assert_eq!(rig.forge.coderabbit.resolved(), Vec::<String>::new());
}

#[test]
fn a_push_back_replies_and_holds_the_merge_until_the_maintainer_answers() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-push"), &[]);
    let id = declined_thread(&rig, &runner);

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, PUSH_BACK));
    assert_eq!(
        acted,
        ["ruled thread 1 of #7 as push-back: the race is real"],
        "{acted:?}"
    );
    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(
        last_reply(&rig),
        (id.clone(), "Pushed back: the race is real".to_owned())
    );
    assert!(
        !rig.forge.coderabbit.resolved().contains(&id),
        "a pushed-back thread stays open"
    );

    waits_with(&rig, &runner, PUSHED_BACK);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    assert_eq!(pm_seen(&rig).len(), 1, "the push-back is its whole answer");
}

#[test]
fn a_follow_up_files_an_issue_settles_the_thread_and_the_head_merges() {
    let rig = pm_project("threads-follow-up");
    rig.merge_auto();
    let (rig, runner, _) = at_the_gate(rig, &[NIT]);

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, FOLLOW_UP));
    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(
        acted,
        ["ruled thread 1 of #7 as follow-up: too big for this pull request"],
        "{acted:?}"
    );
    assert_eq!(
        last_reply(&rig),
        (
            THREAD.to_owned(),
            "Filed as follow-up #900: too big for this pull request".to_owned()
        )
    );
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(issue.labels, [READY], "the follow-up is ready for a worker");

    steps(&runner, |r| {
        matches!(
            r,
            StepReport::Finished {
                merged: true,
                pull_request: Some(71),
                ..
            }
        )
    });
}

#[test]
fn a_fix_runs_one_worker_turn_and_comes_back_to_the_gate() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-fix"), &[NIT]);
    worker_says(
        &rig,
        [Scripted::PushSay(
            "shorter.txt",
            "shorter\n",
            "thread 1: fixed",
        )],
    );

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, FIX));
    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(
        acted,
        ["ruled thread 1 of #7 as fix: the guard is missing"],
        "{acted:?}"
    );

    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert_eq!(
        item.threads_sent,
        [THREAD],
        "the ruled thread goes to the worker"
    );
    assert_eq!(item.counts.fix_turns, 1);

    steps(&runner, |r| matches!(r, StepReport::FixPushed { .. }));

    let turns = worker(&rig);
    let [_, fix] = turns.as_slice() else {
        panic!("{turns:?}");
    };
    assert!(
        fix.contains(
            "The project manager ruled on 1 open review thread(s) on your pull \
                      request #71"
        ),
        "{fix}"
    );
    assert!(fix.contains("review-findings.md"), "{fix}");
    let file = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(file.contains("the guard is missing"), "{file}");

    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci",
        "the fix ends at the gate, with no new pass"
    );
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
    assert_eq!(
        rig.forge.coderabbit.replies(),
        [(THREAD.to_owned(), "Fixed.".to_owned())],
        "the worker's own answer is what kelpie posted"
    );
    assert_eq!(
        pm_seen(&rig).len(),
        1,
        "a fixed thread is not put to the project manager again"
    );
}

#[test]
fn a_close_replies_with_the_reason_settles_the_thread_and_the_head_merges() {
    let rig = pm_project("threads-close");
    rig.merge_auto();
    let (rig, runner, _) = at_the_gate(rig, &[NIT]);

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, CLOSE));
    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(
        acted,
        ["ruled thread 1 of #7 as close: the caller checks it"],
        "{acted:?}"
    );
    assert_eq!(
        rig.forge.coderabbit.replies(),
        [(
            THREAD.to_owned(),
            "Closed as wrong: the caller checks it".to_owned()
        )]
    );
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);

    steps(&runner, |r| {
        matches!(
            r,
            StepReport::Finished {
                merged: true,
                pull_request: Some(71),
                ..
            }
        )
    });
}

#[test]
fn a_close_files_nothing() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-close-nothing"), &[NIT]);
    ruled_by(&rig, &runner, CLOSE);

    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert_eq!(item.follow_ups, None, "a close files no follow-up");
    assert_eq!(item.threads_accepted, [THREAD]);
    assert_eq!(rig.forge.coderabbit.labels(900), Vec::<String>::new());

    let question = merge_ruling(&runner);
    assert!(question.contains("into main?"), "{question}");
}

#[test]
fn a_close_whose_reply_is_refused_still_settles_the_thread() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-close-guard"), &[NIT]);
    let say: &'static str = Box::leak(
        String::from(
            "{\"threads\": [{\"item\": 7, \"thread\": 1, \"decision\": \"close\", \
             \"why\": \"the guard sits in /Users/me/work.txt\"}]}",
        )
        .into_boxed_str(),
    );

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, say));

    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(acted.len(), 1, "{acted:?}");
    assert_eq!(
        rig.forge.coderabbit.replies(),
        [(
            THREAD.to_owned(),
            "Closed as wrong: The project manager's reason names something on the machine \
             kelpie runs on, so it is not posted."
                .to_owned()
        )]
    );
    assert!(
        is_reply(&last_reply(&rig).1),
        "the replacement is still known as kelpie's own"
    );
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert_eq!(item.threads_accepted, [THREAD]);
}

#[test]
fn a_close_whose_reply_fails_is_tried_again_with_the_thread_still_open() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-reply-down"), &[NIT]);
    rig.forge.coderabbit.set_reply_down(true);

    let (_, dropped) = ruled(&ruled_by(&rig, &runner, CLOSE));

    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert!(dropped[0].contains("cannot reply"), "{dropped:?}");
    assert_eq!(
        rig.forge.coderabbit.resolved(),
        Vec::<String>::new(),
        "a thread whose reply failed stays open to be tried again"
    );

    rig.clock.advance(BACK_OFF);
    rig.forge.coderabbit.set_reply_down(false);
    ruled_by(&rig, &runner, CLOSE);

    assert_eq!(
        last_reply(&rig),
        (
            THREAD.to_owned(),
            "Closed as wrong: the caller checks it".to_owned()
        )
    );
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
}

#[test]
fn a_declined_thread_closed_as_wrong_is_not_asked_again() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-declined"), &[NIT]);
    worker_says(
        &rig,
        [Scripted::PushSay(
            "shorter.txt",
            "shorter\n",
            "thread 1: declined: the name is public API",
        )],
    );
    pm_says(&rig, [FIX, CLOSE]);

    answers(&runner, 2);

    assert_eq!(
        rig.forge.coderabbit.replies(),
        [
            (
                THREAD.to_owned(),
                "Declined: the name is public API".to_owned()
            ),
            (
                THREAD.to_owned(),
                "Closed as wrong: the caller checks it".to_owned()
            ),
        ]
    );
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert_eq!(item.threads_accepted, [THREAD], "the close settles it");
    assert_eq!(
        pm_seen(&rig).len(),
        2,
        "the settled thread is asked for no more"
    );
}

#[test]
fn a_thread_with_no_answer_stays_open_and_is_asked_again() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-no-answer"), &[NIT]);
    worker_says(&rig, [Scripted::Push("shorter.txt", "shorter\n")]);
    pm_says(&rig, [FIX, NOTHING]);

    answers(&runner, 2);

    let replies = rig.forge.coderabbit.replies();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert!(
        replies[0].1.starts_with("Not addressed:"),
        "kelpie says the worker left it: {replies:?}"
    );
    assert_eq!(
        rig.forge.coderabbit.resolved(),
        Vec::<String>::new(),
        "the thread stays open"
    );
    assert_eq!(pm_seen(&rig).len(), 2, "the open thread is put to it again");
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn the_item_waits_with_the_project_manager_down_and_raises_no_ruling() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-down"), &[NIT]);
    rig.claude.script_for(
        Role::Pm,
        [Scripted::Fail(crate::ports::AgentError::Failed(
            crate::settings::Harness::ClaudeCode,
            "overloaded".into(),
        ))],
    );

    steps(&runner, |r| matches!(r, StepReport::PmFailed { .. }));

    waits_with(&rig, &runner, PM_DOWN);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    assert_eq!(
        pm_seen(&rig).len(),
        1,
        "a project manager that is down is not woken again"
    );
}

#[test]
fn a_drain_holds_an_item_with_an_open_thread_and_starts_no_wake() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-drain"), &[NIT]);
    rig.ask(&runner, "drain", None);

    let deadline = Instant::now() + CALL_PATIENCE;
    let mut because = json!(null);
    while Instant::now() < deadline && because.as_str().is_none_or(|b| !b.contains("draining")) {
        step(&runner).unwrap();
        because = rig.ask(&runner, "status", None)["work_item"]["held_up"]["because"].clone();
    }
    assert!(because.as_str().unwrap().contains("draining"), "{because}");
    assert!(pm_seen(&rig).is_empty(), "a drain starts no wake");
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn a_restart_with_the_ask_out_asks_again() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-restart"), &[NIT]);
    let hold = Hold::default();
    rig.claude.script_for(
        Role::Pm,
        [Scripted::Hold(hold.clone()), Scripted::Say(NOTHING)],
    );
    let driven = Driven::new(&runner);
    driven.started(); // the gate holds the merge and wakes the project manager

    assert!(hold.entered(CALL_PATIENCE), "the wake's call never began");
    assert!(
        runner.lock().unwrap().pm_threads_out(ticket(7)),
        "the ask is out"
    );

    drop(driven);
    drop(runner);
    rig.claude.stop();
    let runner = rig.open().unwrap();
    answers(&runner, 1);

    assert_eq!(
        pm_seen(&rig).len(),
        2,
        "a restart asks about the thread again"
    );
    step(&runner).unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["held_up"]["because"].as_str(),
        Some(THREADS_HELD),
        "the merge still waits on the project manager"
    );
}

#[test]
fn a_refused_filing_retries_and_raises_no_ruling() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-refused"), &[NIT]);
    rig.forge.set_issues_down(true);

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, FOLLOW_UP));

    assert_eq!(acted.len(), 1, "{acted:?}");
    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert!(dropped[0].contains("issues are down"), "{dropped:?}");
    let logged = runner.lock().unwrap().take_started();
    let Some(StepReport::GateFailed { issue, reason }) = logged.last() else {
        panic!("the refusal was not logged as a step: {logged:?}");
    };
    assert_eq!(*issue, ticket(7));
    assert!(reason.contains("issues are down"), "{reason}");
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"],
        json!([]),
        "a refusal before the merge raises no follow-up ruling"
    );
    assert_eq!(
        rig.forge.coderabbit.resolved(),
        Vec::<String>::new(),
        "the thread stays open"
    );

    rig.clock.advance(BACK_OFF);
    rig.forge.set_issues_down(false);
    ruled_by(&rig, &runner, FOLLOW_UP);

    assert_eq!(rig.forge.created().len(), 1, "the filing is tried again");
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
}

#[test]
fn a_talk_that_cannot_be_read_holds_the_merge_with_the_threads_unruled() {
    let rig = pm_project("threads-talk-down");
    rig.merge_auto();
    let (rig, runner, _) = at_the_gate(rig, &[NIT]);
    rig.forge.set_talk_down(true);

    waits_with(&rig, &runner, TALK_UNREAD);
    assert!(pm_seen(&rig).is_empty(), "nothing was read to ask about");
    assert!(
        rig.forge.merges().is_empty(),
        "the head does not merge past it"
    );

    rig.forge.set_talk_down(false);
    pm_says(&rig, [NOTHING]);
    waits_with(&rig, &runner, THREADS_HELD);
    assert_eq!(
        woken(&rig, 1).len(),
        1,
        "the thread is put to the project manager"
    );
}

#[test]
fn fix_after_fix_parks_on_over_budget_and_raises_no_merge_ruling() {
    let rig = pm_project("threads-budget");
    rig.edit_settings(|s| s.replace("per_item_usd = 10", "per_item_usd = 5"));
    let (rig, runner, _) = at_the_gate(rig, &[NIT]);
    worker_says(
        &rig,
        [Scripted::Billed(
            "thread 1: declined: the name is public API",
            Cost(6 * DOLLAR),
        )],
    );
    pm_says(&rig, [FIX, FIX]);

    answers(&runner, 2);
    let seen = steps(&runner, |r| matches!(r, StepReport::Failed { .. }));

    let StepReport::Failed {
        issue,
        id,
        question,
        ..
    } = seen.last().unwrap()
    else {
        panic!("{seen:?}");
    };
    assert_eq!(*issue, ticket(7));
    assert_eq!(*id, ticket(71), "the item parks on its own ruling");
    assert!(
        question.contains("has cost $6.00 against its cap of $5.00"),
        "{question}"
    );
    let status = rig.ask(&runner, "status", None);
    let rulings = status["rulings"].as_array().unwrap();
    assert_eq!(rulings.len(), 1, "{status}");
    assert_eq!(rulings[0]["kind"]["kind"], "stuck");
    assert_eq!(rulings[0]["kind"]["reason"], "over-budget");
}

#[test]
fn a_push_back_asks_nothing_more_until_the_maintainer_replies() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-push-wait"), &[]);
    let id = declined_thread(&rig, &runner);
    ruled_by(&rig, &runner, PUSH_BACK);

    waits_with(&rig, &runner, PUSHED_BACK);
    assert_eq!(pm_seen(&rig).len(), 1, "the push-back holds the ask back");
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));

    rig.forge.coderabbit.person_replies(
        &id,
        "octocat",
        Standing::Writer,
        "Fair. I will fix it myself.",
    );
    worker_says(&rig, [Scripted::Push("theirs.txt", "theirs\n")]);
    pm_says(&rig, [NOTHING]);
    answers(&runner, 1);

    let seen = woken(&rig, 2);
    assert!(
        seen[1].call.prompt.contains("Rename `bleats`."),
        "the maintainer's reply puts the thread back to it: {}",
        seen[1].call.prompt
    );
    assert_eq!(
        rig.forge.coderabbit.resolved(),
        Vec::<String>::new(),
        "the thread stays open throughout"
    );
}

#[test]
fn each_wait_says_why_in_status() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-why-asked"), &[NIT]);
    pm_says(&rig, [NOTHING]);
    waits_with(&rig, &runner, THREADS_HELD);
    drop(runner);

    let (rig, runner, _) = at_the_gate(pm_project("threads-why-down"), &[NIT]);
    rig.claude.script_for(
        Role::Pm,
        [Scripted::Fail(crate::ports::AgentError::Failed(
            crate::settings::Harness::ClaudeCode,
            "overloaded".into(),
        ))],
    );
    steps(&runner, |r| matches!(r, StepReport::PmFailed { .. }));
    waits_with(&rig, &runner, PM_DOWN);
    drop(runner);

    let (rig, runner, _) = at_the_gate(pm_project("threads-why-back"), &[]);
    declined_thread(&rig, &runner);
    ruled_by(&rig, &runner, PUSH_BACK);
    waits_with(&rig, &runner, PUSHED_BACK);
}

#[test]
fn a_strangers_thread_holds_nothing_and_reaches_no_prompt() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-stranger"), &[]);
    rig.forge.coderabbit.person_opens(
        71,
        "work.txt",
        "passerby",
        Standing::Other,
        "This looks odd to me.",
    );

    let question = merge_ruling(&runner);
    assert!(
        !question.contains("looks odd"),
        "a stranger's thread is not the ruling's: {question}"
    );
    assert!(pm_seen(&rig).is_empty(), "no model is asked about it");
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("1 open thread on #71 held nothing")),
        "{notes:?}"
    );
}

#[test]
fn a_strangers_reply_in_a_writers_thread_reaches_no_prompt() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-stranger-reply"), &[NIT, NIT]);
    rig.forge.coderabbit.person_replies(
        THREAD,
        "passerby",
        Standing::Other,
        "I agree, this is wrong.",
    );
    pm_says(&rig, [CLOSE]);

    answers(&runner, 1);

    let [wake] = woken(&rig, 1).try_into().unwrap();
    assert!(
        wake.call.prompt.contains("The name could be shorter."),
        "{}",
        wake.call.prompt
    );
    assert!(
        !wake.call.prompt.contains("I agree"),
        "the stranger's words reach no prompt: {}",
        wake.call.prompt
    );
    let case = std::fs::read_to_string(rig.paths().pm.join("cases/7.md")).expect("a case file");
    assert!(!case.contains("I agree, this is wrong."), "{case}");
}

#[test]
fn a_project_with_no_project_manager_keeps_its_ruling_and_its_nits_hold_nothing() {
    let rig = {
        let rig = listing("threads-no-pm", &["cubic"]);
        rig.reviews_need(Some(2));
        rig
    };
    let (rig, runner, _) = at_the_gate(rig, &[NIT]);

    let question = merge_ruling(&runner);
    assert!(
        question.contains("1 nit left open (cubic)"),
        "the ruling names the nit and merges anyway: {question}"
    );
    assert!(pm_seen(&rig).is_empty(), "no project manager is woken");
}

#[test]
fn an_adopted_contribution_keeps_its_ruling_and_asks_nothing() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-adopted"), &[NIT]);
    runner
        .lock()
        .unwrap()
        .update(|item| item.contributor = Some("octocat".to_owned()))
        .unwrap();

    let question = merge_ruling(&runner);
    assert!(
        question.contains("1 nit left open (cubic)"),
        "its threads stay the maintainer's to read: {question}"
    );
    assert!(
        pm_seen(&rig).is_empty(),
        "nothing is asked of the project manager"
    );
}

#[test]
fn a_deferred_finding_asks_before_the_merge() {
    let rig = pm_project("threads-deferred-asks");
    rig.merge_auto();
    let (rig, runner, _) = at_the_gate_found(rig, &[racy()]);
    defers(&rig, &[racy()]);
    pm_says(&rig, [NOTHING]);

    waits_with(&rig, &runner, THREADS_HELD);
    let [wake] = woken(&rig, 1).try_into().unwrap();
    assert!(
        wake.call
            .prompt
            .contains("1. src/lib.rs:9 (deferred by the worker, HIGH): looks racy"),
        "{}",
        wake.call.prompt
    );
    assert_eq!(rig.forge.merges(), [], "the merge waits for the ruling");
    assert_eq!(rig.forge.created(), [], "nothing is filed before it");
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn a_deferred_finding_followed_up_is_filed_before_the_merge_and_not_again() {
    let rig = pm_project("threads-deferred-follow-up");
    rig.merge_auto();
    let (rig, runner, head) = at_the_gate_found(rig, &[racy()]);
    defers(&rig, &[racy()]);

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, FOLLOW_UP));
    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(
        acted,
        ["ruled thread 1 of #7 as follow-up: too big for this pull request"],
        "{acted:?}"
    );
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(issue.labels, [READY], "the follow-up is ready for a worker");
    assert!(issue.body.contains("looks racy"), "{}", issue.body);
    assert!(
        issue.body.contains("two threads write the same field"),
        "the issue carries kelpie's own words: {}",
        issue.body
    );
    assert_eq!(rig.forge.coderabbit.replies(), [], "it has no thread to reply on");

    let seen = steps(&runner, |r| matches!(r, StepReport::Finished { .. }));
    assert!(
        seen.iter().all(|r| !matches!(
            r,
            StepReport::Ruling { .. } | StepReport::FollowUpsFiled { .. }
        )),
        "nothing is asked or filed at the merge: {seen:?}"
    );
    assert_eq!(rig.forge.merges(), [(71, head)]);
    assert_eq!(rig.forge.created().len(), 1, "filed once");
    assert_eq!(pm_seen(&rig).len(), 1, "it is not put to the project manager again");
}

#[test]
fn a_deferred_finding_sent_back_rides_the_threads_turn() {
    let rig = pm_project("threads-deferred-fix");
    rig.merge_auto();
    let (rig, runner, head) = at_the_gate_found(rig, &[racy()]);
    defers(&rig, &[racy()]);
    open_bots(&rig, &head, &[NIT]);
    let turns_before = worker(&rig).len();
    worker_says(
        &rig,
        [Scripted::PushSay(
            "shorter.txt",
            "shorter\n",
            "thread 1: fixed\nthread 2: fixed",
        )],
    );

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, FIX_BOTH));
    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(acted.len(), 2, "{acted:?}");
    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert_eq!(item.threads_sent.len(), 2, "both ride one turn");
    assert_eq!(item.counts.fix_turns, 2, "the round's turn and this one");

    steps(&runner, |r| matches!(r, StepReport::FixPushed { .. }));
    let turns = worker(&rig);
    assert_eq!(turns.len(), turns_before + 1, "one worker turn: {turns:?}");
    let file = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(file.contains("thread 1|"), "{file}");
    assert!(file.contains("thread 2|HIGH|src/lib.rs:9|looks racy"), "{file}");
    let seen = steps(&runner, |r| matches!(r, StepReport::Finished { .. }));
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
    assert_eq!(rig.forge.created(), [], "a fixed finding is filed nowhere");
    assert!(
        seen.iter().all(|r| !matches!(r, StepReport::Ruling { .. })),
        "nothing is asked at the merge: {seen:?}"
    );
    let merges = rig.forge.merges();
    assert_eq!(merges.len(), 1, "{merges:?}");
    assert_ne!(merges[0].1, head, "the fix's push is what merged");
}

#[test]
fn a_deferred_finding_closed_files_nothing_and_journals_why() {
    let rig = pm_project("threads-deferred-close");
    rig.merge_auto();
    let (rig, runner, _) = at_the_gate_found(rig, &[racy()]);
    defers(&rig, &[racy()]);

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, CLOSE));
    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(
        acted,
        ["ruled thread 1 of #7 as close: the caller checks it"],
        "{acted:?}"
    );
    let journaled: Vec<String> = (journal::read(&rig.paths().journal).into_iter())
        .flat_map(|line| match line.entry {
            Entry::Wake { acted, .. } => acted,
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(journaled, acted, "the reason is in the journal");
    let left = std::fs::read_to_string(rig.build_7().join("deferred-findings.md")).unwrap();
    assert_eq!(left, "", "it left the deferred findings");

    let seen = steps(&runner, |r| matches!(r, StepReport::Finished { .. }));
    assert!(
        seen.iter().all(|r| !matches!(r, StepReport::Ruling { .. })),
        "nothing is asked at the merge: {seen:?}"
    );
    assert_eq!(rig.forge.created(), [], "a close files nothing");
    assert_eq!(rig.forge.coderabbit.replies(), [], "it has no thread to reply on");
}

#[test]
fn a_disputed_finding_asks_nothing() {
    let rig = pm_project("threads-deferred-disputed");
    rig.merge_auto();
    let (rig, runner, _) = at_the_gate_found(rig, &[racy()]);
    defers(&rig, &[racy()]);
    let disputed = format!("{}  the caller holds the lock\n", deferred(&[racy()]));
    std::fs::write(rig.build_7().join("disputed-findings.md"), disputed).unwrap();

    let seen = steps(&runner, |r| matches!(r, StepReport::Finished { .. }));
    assert!(
        seen.iter().all(|r| !matches!(r, StepReport::Ruling { .. })),
        "{seen:?}"
    );
    assert!(pm_seen(&rig).is_empty(), "nothing is put to the project manager");
    assert_eq!(rig.forge.created(), [], "a disputed finding is not filed");
    assert_eq!(rig.forge.merges().len(), 1);
}
