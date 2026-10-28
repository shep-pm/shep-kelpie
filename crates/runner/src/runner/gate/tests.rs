use crate::test::ticket;
use crate::ticket::TicketId;
use serde_json::json;

use std::sync::{Arc, Mutex};

use super::*;
use crate::ports::{CLEAN, Cost, Finding, Session, Severity, Usage};
use crate::runner::answer;
use crate::runner::flight::{Pass, advance};
use crate::runner::step;
use crate::state::Ruling;
use crate::test::{CALL_PATIENCE, Rig, Scripted, git};

pub(super) fn ruling_report(report: Option<StepReport>) -> (TicketId, String) {
    match report {
        Some(StepReport::Ruling { id, question, .. }) => (id, question),
        other => panic!("no ruling was raised: {other:?}"),
    }
}

#[test]
fn a_pending_run_waits_and_a_green_one_raises_the_merge_ruling() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));

    rig.forge.set_checks(&head, Checks::Passed);
    let (id, question) = ruling_report(rig.verdict(&runner));
    assert_eq!(id, ticket(71));
    assert_eq!(
        question,
        format!(
            "Merge pull request #71 at {} into main? `shep kelpie rule 71 yes` \
             merges it, `shep kelpie rule 71 no <note>` sends the worker your note for \
             a fix that goes to CI and back to you, and `shep kelpie rule 71 rework \
             <note>` sends it your note for a change the whole review reads again.",
            &head[..7]
        )
    );
    assert_eq!(rig.forge.comments(), [], "a merge ruling is not posted");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["rulings"],
        json!([{
            "id": "71",
            "issue": "7",
            "question": question,
            "pull_request": 71,
            "kind": { "kind": "merge", "head": head },
            "alerted": false,
            "raised": Rig::EPOCH + 120,
            "effects": [
                "Merges it into main and closes #7: Title of #7. CI passed. \
                 2 review rounds read it.",
                "Sends the worker back for a fix that goes to CI and back to you, \
                 with no new review round.",
                "Sends the worker back for a change the whole review reads again.",
            ],
        }])
    );
    assert_eq!(
        status["work_item"]["phase"],
        json!({ "state": "ruling", "id": "71" })
    );
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(71) })
    );
    assert_eq!(step(&runner).unwrap(), None, "a parked worker waits");
}

// The worker pushed a head past the red one, and green CI on it raises
// the first merge ruling, which names it.
fn assert_fix_landed(rig: &Rig, runner: &std::sync::Mutex<Runner>, red: &str) {
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_ne!(fixed, red);
    rig.forge.set_checks(&fixed, Checks::Passed);
    let (id, question) = ruling_report(rig.verdict(runner));
    assert_eq!(id, ticket(71));
    assert!(question.contains(&fixed[..7]), "{question}");
}

#[test]
fn a_red_run_is_the_workers_next_turn_naming_the_failed_checks() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    let failed = vec!["lint".to_owned(), "test".to_owned()];
    rig.forge.set_checks(&head, Checks::Failed(failed.clone()));
    assert_eq!(
        rig.verdict(&runner),
        Some(StepReport::CiFailed {
            issue: ticket(7),
            pull_request: 71,
            head: head.clone(),
            checks: failed,
        })
    );

    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
    step(&runner).unwrap();
    let [first, fix] = rig.claude.calls().try_into().unwrap();
    assert_eq!(fix.session, Session::Resume(first.session.id().clone()));
    let named = format!(
        "\nCI failed on your pull request #71 at {}: lint, test. ",
        &head[..7]
    );
    assert!(fix.prompt.starts_with("/mattpocock:diagnosing-bugs "));
    assert!(fix.prompt.contains(&named), "{}", fix.prompt);

    assert_fix_landed(&rig, &runner, &head);
}

#[test]
fn a_ci_fix_prompt_asks_for_the_body_to_be_kept_true() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    rig.verdict(&runner);
    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
    step(&runner).unwrap();
    let fix = rig.claude.calls().pop().unwrap();
    assert!(
        fix.prompt
            .contains("Update the pull request's body so it matches the head you push"),
        "{}",
        fix.prompt
    );
}

#[test]
fn a_fix_left_uncommitted_is_sent_back_naming_its_files_before_the_worker_is_parked() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    rig.verdict(&runner);
    rig.claude.script([
        Scripted::Plant("fix.txt", "fixed\n"),
        Scripted::Push("fix.txt", "fixed\n"),
    ]);
    step(&runner).unwrap();
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Ended { issue, .. }) if issue == ticket(7)
    ));
    let [first, _, again] = rig.claude.calls().try_into().unwrap();
    assert_eq!(again.session, Session::Resume(first.session.id().clone()));
    assert!(
        again
            .prompt
            .starts_with("Your last turn ended with uncommitted changes in your worktree"),
        "{}",
        again.prompt
    );
    assert!(again.prompt.contains("fix.txt"), "{}", again.prompt);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    assert_fix_landed(&rig, &runner, &head);
}

#[test]
fn a_second_red_run_on_a_head_the_worker_left_alone_parks_it() {
    let (rig, runner, head) = Rig::with_pull_request("rotom");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["lint".into()]));
    rig.verdict(&runner);
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(&runner).unwrap();

    let (_, question) = ruling_report(rig.verdict(&runner));
    let parked = format!(
        "CI failed again on pull request #71 at {}, and the worker pushed no fix: lint.",
        &head[..7]
    );
    assert!(question.starts_with(&parked), "{question}");
    assert_eq!(rig.claude.calls().len(), 2, "the red run went out once");
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { .. })
    ));
    assert_eq!(step(&runner).unwrap(), None);

    // The maintainer's fix and yes vouch for the head: no foreign-change ruling.
    let fixed = rig.push_by_hand("kelpie/7", "lint.txt");
    rig.ask(&runner, "rule", Some("71 yes"));
    rig.forge.set_checks(&fixed, Checks::Passed);
    let (_, question) = ruling_report(rig.verdict(&runner));
    assert!(question.starts_with("Merge pull request #71"), "{question}");
}

#[test]
fn a_branch_behind_main_is_rebased_and_asked_about_only_after_ci_on_the_new_head() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    let landed = rig.land_on_origin("landed.txt");
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Rebased { head: rebased, .. }) = step(&runner).unwrap() else {
        panic!("the branch was not rebased");
    };
    assert_eq!(rig.forge.head_of("kelpie/7").as_ref(), Some(&rebased));
    let worktree = rig.worktree_7();
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), rebased);
    assert_eq!(git(&worktree, &["rev-parse", "HEAD~1"]), landed);
    assert_eq!(git(&worktree, &["show", "HEAD:work.txt"]), "work");

    assert_eq!(
        step(&runner).unwrap(),
        None,
        "CI on the rebased head is pending"
    );
    assert_eq!(rig.forge.comments(), []);
    rig.forge.set_checks(&rebased, Checks::Passed);
    ruling_report(rig.verdict(&runner));
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"][0]["kind"],
        json!({ "kind": "merge", "head": rebased })
    );
}

// Seen live on the playground: the step after a rebase read the head
// from before the push, and parked the worker on a false ruling.
#[test]
fn a_forge_still_showing_the_head_from_before_a_push_is_waited_out() {
    let (rig, runner, head) = Rig::with_pull_request("webapp");
    rig.land_on_origin("landed.txt");
    let Some(StepReport::Rebased { head: rebased, .. }) = step(&runner).unwrap() else {
        panic!("the branch was not rebased");
    };
    rig.forge.set_lagging(71, Some(&head));
    rig.forge.set_checks(&head, Checks::Passed);
    rig.clock.advance(CHECKS_SETTLE);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    assert_eq!(rig.forge.head_of("kelpie/7"), Some(rebased.clone()));

    rig.forge.set_lagging(71, None);
    rig.forge.set_checks(&rebased, Checks::Passed);
    ruling_report(rig.verdict(&runner));
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"][0]["kind"],
        json!({ "kind": "merge", "head": rebased })
    );
}

#[test]
fn a_verdict_waits_for_the_check_set_to_settle() {
    let (rig, runner, head) = Rig::with_pull_request("acme");
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(CHECKS_SETTLE - 1);
    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(1);
    ruling_report(step(&runner).unwrap());
}

#[test]
fn with_ci_off_the_checks_are_never_read() {
    let rig = Rig::new("webapp");
    rig.edit_settings(|s| s.replace("block = true", "block = false"));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
    step(&runner).unwrap(); // the worker's first turn: opens the pull request
    step(&runner).unwrap(); // review round 1, qwen: clean by default
    step(&runner).unwrap(); // review round 2, claude: scripted clean above
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["lint".into()]));

    let (id, question) = ruling_report(step(&runner).unwrap());
    assert!(question.starts_with("Merge pull request #71"), "{question}");
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished {
            issue,
            pull_request: Some(71),
            merged: true,
            ..
        }) if issue == ticket(7)
    ));
    assert_eq!(rig.forge.merges(), [(71, head)]);
}

#[test]
fn a_ruling_whose_comment_fails_still_stands_in_status_and_the_log() {
    let (rig, runner, _) = Rig::with_pull_request("golbat");
    rig.forge.set_state(71, PullRequestState::Closed);
    rig.forge.set_comments_down(true);
    let Some(StepReport::Ruling { comment_failed, .. }) = rig.verdict(&runner) else {
        panic!("no ruling was raised");
    };
    assert_eq!(
        comment_failed.as_deref(),
        Some("gh failed: comments are down")
    );
    assert_eq!(rig.ask(&runner, "status", None)["rulings"][0]["id"], "71");
}

#[test]
fn a_pull_request_merged_by_hand_ends_the_work_item_with_no_merge_by_kelpie() {
    let (rig, runner, _) = Rig::with_pull_request("acme");
    rig.forge.set_state(71, PullRequestState::Merged);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished {
            issue,
            pull_request: Some(71),
            merged: true,
            ..
        }) if issue == ticket(7)
    ));
    assert_eq!(rig.forge.merges(), []);
    assert!(!rig.worktree_7().exists());
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
}

#[test]
fn a_closed_pull_request_parks_the_worker_and_a_yes_keeps_its_branch() {
    let (rig, runner, head) = Rig::with_pull_request("reactmap");
    rig.forge.set_state(71, PullRequestState::Closed);
    let (id, question) = ruling_report(step(&runner).unwrap());
    assert!(
        question.starts_with("Pull request #71 was closed without merging."),
        "{question}"
    );
    assert_eq!(
        rig.forge.comments(),
        [(
            71,
            "This pull request was closed without merging.\n\nWaiting on the maintainer."
                .to_owned()
        )]
    );
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished {
            issue,
            pull_request: Some(71),
            merged: false,
            ..
        }) if issue == ticket(7)
    ));
    assert_eq!(rig.forge.head_of("kelpie/7"), Some(head));
    assert!(!rig.worktree_7().exists());
    assert_eq!(git(&rig.repo(), &["branch", "--list", "kelpie/7"]), "");
}

#[test]
fn a_label_added_outside_kelpie_parks_the_worker_naming_it() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge.label_pull_request(71, "bug");
    let (id, question) = ruling_report(step(&runner).unwrap());
    assert_eq!(id, ticket(71));
    assert_eq!(
        question,
        "Pull request #71 changed outside kelpie: the `bug` label was added. \
         `shep kelpie rule 71 yes` accepts it and kelpie carries on, and \
         `shep kelpie rule 71 no <note>` sends the worker your note."
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"][0]["kind"],
        json!({
            "kind": "foreign-change",
            "description": "the `bug` label was added",
            "known": { "labels": ["bug"], "ready": false, "head": head },
        })
    );

    // A yes accepts it, and kelpie carries on watching the same pull request.
    rig.ask(&runner, "rule", Some("71 yes"));
    rig.forge.set_checks(&head, Checks::Passed);
    let (next, merge_question) = ruling_report(rig.verdict(&runner));
    assert_eq!(next, ticket(71));
    assert!(merge_question.starts_with("Merge pull request #71"));
    // Kelpie's own record now matches, so the accepted label is not asked again.
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn marking_a_pull_request_ready_outside_kelpie_parks_the_worker() {
    let (rig, runner, _) = Rig::with_pull_request("koji");
    rig.forge.ready_pull_request(71);
    let (_, question) = ruling_report(step(&runner).unwrap());
    assert!(
        question.starts_with(
            "Pull request #71 changed outside kelpie: it was marked ready for review."
        ),
        "{question}"
    );
}

#[test]
fn a_no_on_a_foreign_change_sends_the_worker_a_note_and_asks_again_once_it_reaches_ci() {
    let (rig, runner, head) = Rig::with_pull_request("rotom");
    rig.forge.label_pull_request(71, "bug");
    ruling_report(step(&runner).unwrap());
    rig.ask(&runner, "rule", Some("71 no  remove that label yourself "));
    rig.claude.script([
        Scripted::Push("rename.txt", "renamed\n"),
        Scripted::Text(CLEAN),
    ]);
    step(&runner).unwrap(); // the noted turn: pushes, enters round 1
    let [first, noted] = rig.claude.calls().try_into().unwrap();
    assert_eq!(noted.session, Session::Resume(first.session.id().clone()));
    assert_eq!(
        noted.prompt,
        "The maintainer answered no on pull request #71, with this note:\n\n\
         remove that label yourself\n\nUpdate the pull request's body so it matches \
         the head you push: what changed, the files touched, the test evidence, and \
         the decisions you made.\n"
    );
    let fixed = rig.forge.head_of("kelpie/7");
    assert_ne!(fixed, Some(head));

    rig.forge.set_checks(&fixed.unwrap(), Checks::Passed);
    rig.verdict(&runner); // review round 1, qwen: clean by default, once its head is green
    step(&runner).unwrap(); // review round 2, claude: scripted clean above
    // The label is still there, and unaccepted, so the next look parks again.
    let (id, _) = ruling_report(rig.verdict(&runner));
    assert_eq!(id, ticket(71));
}

#[test]
fn a_label_removed_after_a_yes_accepted_it_parks_the_worker_again() {
    let (rig, runner, _) = Rig::with_pull_request("chelone");
    rig.forge.label_pull_request(71, "bug");
    ruling_report(step(&runner).unwrap());
    rig.ask(&runner, "rule", Some("71 yes"));
    rig.forge.unlabel_pull_request(71, "bug");
    let (id, question) = ruling_report(step(&runner).unwrap());
    assert_eq!(id, ticket(71));
    assert!(
        question
            .starts_with("Pull request #71 changed outside kelpie: the `bug` label was removed."),
        "{question}"
    );
}

#[test]
fn a_normal_gate_run_with_nothing_changed_outside_kelpie_asks_nothing_about_it() {
    let (rig, runner, head) = Rig::with_pull_request("acme");
    rig.forge.set_checks(&head, Checks::Passed);
    let (_, question) = ruling_report(rig.verdict(&runner));
    assert!(question.starts_with("Merge pull request #71"), "{question}");
}

#[test]
fn a_work_item_saved_before_heads_were_known_parks_nothing_and_learns_its_head() {
    let (rig, runner, head) = Rig::with_pull_request("chelone");
    drop(runner);
    let state = rig.paths().state;
    let text = std::fs::read_to_string(&state).unwrap();
    let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    let known = saved["work_items"][0]["known"].as_object_mut().unwrap();
    assert!(known.remove("head").is_some());
    std::fs::write(&state, saved.to_string()).unwrap();

    let runner = rig.open().unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let (_, question) = ruling_report(rig.verdict(&runner));
    assert!(question.starts_with("Merge pull request #71"), "{question}");
    let text = std::fs::read_to_string(&state).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["work_items"][0]["known"]["head"], json!(head));
}

#[test]
fn a_work_item_saved_before_the_gate_existed_is_left_alone() {
    let (rig, runner, head) = Rig::with_pull_request("xilriws");
    drop(runner);
    let state = rig.paths().state;
    let text = std::fs::read_to_string(&state).unwrap();
    let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    let item = saved["work_items"][0].as_object_mut().unwrap();
    item.remove("phase");
    item.remove("red_head");
    std::fs::write(&state, saved.to_string()).unwrap();

    let runner = rig.open().unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.forge.comments(), []);
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({ "state": "implement" })
    );
}

// The gate's reads are made with the runner's lock let go: a trigger is answered while each is
// held, and a change that lands in the gap reaches the step made again as it would have inline.
// A project whose pull request 71 is red and has been for as long as the gate settles, so
// the next pass reads its failed runs' logs
fn red(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner, head) = Rig::with_pull_request_set(project, |_| {});
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".to_owned()]));
    assert_eq!(step(&runner).unwrap(), None, "CI settles");
    rig.clock.advance(CHECKS_SETTLE);
    (rig, runner)
}

fn ci_failed(pass: &Result<Pass, StateError>) -> bool {
    matches!(pass, Ok(Pass::Report(StepReport::CiFailed { .. })))
}

#[test]
fn a_trigger_is_answered_while_the_failed_logs_are_read() {
    let (rig, runner) = red("koji");
    let read = rig.forge.hold_next_failed_logs_read();

    let (reply, pass) = rig.asked_while_held(&runner, read, "status", None);

    let reply = reply.expect("the trigger waited for the lock the read held");
    assert_eq!(reply["work_items"].as_array().unwrap().len(), 1);
    assert!(ci_failed(&pass), "{pass:?}");
}

#[test]
fn a_trigger_is_answered_while_the_pull_request_is_read_for_the_ci_gate() {
    let (rig, runner, _) = Rig::with_pull_request("koji");
    // The board's read would take the listing the pass makes first, and a finishing runner picks
    // nothing.
    rig.ask(&runner, "finish", None);
    let read = rig.forge.hold_next_open_pull_requests_read();

    let (reply, pass) = rig.asked_while_held(&runner, read, "status", None);

    let reply = reply.expect("the trigger waited for the lock the read held");
    assert_eq!(reply["work_items"].as_array().unwrap().len(), 1);
    pass.unwrap();
}

#[test]
fn a_log_read_that_fails_is_handled_as_the_inline_read_was() {
    let (rig, runner, head) = Rig::with_pull_request_set("koji", Rig::with_app);
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".to_owned()]));
    assert_eq!(step(&runner).unwrap(), None, "CI settles");
    rig.clock.advance(CHECKS_SETTLE);
    rig.forge.set_ci_log_error("the logs are gone");
    let read = rig.forge.hold_next_failed_logs_read();

    let (_, pass) = rig.asked_while_held(&runner, read, "status", None);

    // The worker is sent back for the failure, told that kelpie could not read its logs.
    assert!(ci_failed(&pass), "{pass:?}");
    rig.claude.script([Scripted::Say("fixed")]);
    step(&runner).unwrap();
    let prompt = rig.claude.calls().pop().unwrap().prompt;
    assert!(prompt.contains("could not read it for you"), "{prompt}");
    assert!(prompt.contains("the logs are gone"), "{prompt}");
}

#[test]
fn a_pull_request_that_closed_during_the_read_is_not_gated() {
    let (rig, runner) = red("koji");
    rig.ask(&runner, "finish", None);
    let read = rig.forge.hold_next_open_pull_requests_read();
    let started = rig.claude.all_calls().len();

    let (_, pass) = rig.during_read(
        read,
        || advance(&runner),
        || rig.forge.set_state(71, PullRequestState::Closed),
    );

    let Ok(Pass::Report(StepReport::Ruling { question, .. })) = pass else {
        panic!("a closed pull request was gated: {pass:?}");
    };
    assert!(
        question.starts_with("Pull request #71 was closed without merging."),
        "{question}"
    );
    assert_eq!(rig.claude.all_calls().len(), started);
}

#[test]
fn a_drain_that_lands_while_the_failed_logs_are_read_starts_no_call() {
    let (rig, runner) = red("koji");
    let runner = Arc::new(runner);
    let postings = runner.lock().unwrap().postings();
    let read = rig.forge.hold_next_failed_logs_read();
    let started = rig.claude.all_calls().len();

    std::thread::scope(|scope| {
        let pass = scope.spawn(|| advance(&runner));
        assert!(
            read.waiting(CALL_PATIENCE),
            "the gate never read the failed logs"
        );
        let asking = {
            let (runner, postings) = (Arc::clone(&runner), Arc::clone(&postings));
            scope.spawn(move || answer(&runner, &postings, "drain", Some("pause")))
        };
        let reply: serde_json::Value =
            serde_json::from_str(&asking.join().unwrap()).expect("a JSON reply");
        assert!(
            postings.draining(),
            "the drain did not hold new calls back: {reply}"
        );
        drop(read);
        pass.join().expect("the pass thread").unwrap();
    });

    // The red run is the worker's next turn, which the drain holds back.
    assert!(matches!(advance(&runner), Ok(Pass::Idle)));
    assert_eq!(rig.claude.all_calls().len(), started, "the worker started");
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["held_up"]["because"],
        "the runner is draining, so no turn starts"
    );
}

#[test]
fn a_trigger_is_answered_while_the_base_is_read() {
    let (rig, runner, _) = Rig::with_pull_request("koji");
    let read = rig.hold_next_git_fetch();

    let (reply, pass) = rig.asked_while_held(&runner, read, "status", None);

    let reply = reply.expect("the trigger waited for the lock the fetch held");
    assert_eq!(reply["work_items"].as_array().unwrap().len(), 1);
    pass.unwrap();
}

fn rebased(pass: &Result<Pass, StateError>) -> bool {
    matches!(pass, Ok(Pass::Report(StepReport::Rebased { .. })))
}

#[test]
fn a_trigger_is_answered_while_a_branch_is_rebased() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    rig.land_on_origin("landed.txt");
    let read = rig.hold_next_git_push();

    let (reply, pass) = rig.asked_while_held(&runner, read, "status", None);

    let reply = reply.expect("the trigger waited for the lock the rebase held");
    assert_eq!(reply["work_items"].as_array().unwrap().len(), 1);
    assert!(rebased(&pass), "{pass:?}");
    assert_ne!(
        rig.forge.head_of("kelpie/7"),
        Some(head),
        "the push went out"
    );
}

#[test]
fn a_drop_sent_during_a_rebase_cleans_up_after_it() {
    let (rig, runner, _) = Rig::with_pull_request("koji");
    rig.land_on_origin("landed.txt");
    let read = rig.hold_next_git_push();

    let (reply, pass) = rig.asked_while_held(&runner, read, "drop", Some("7"));

    let reply = reply.expect("the drop waited for the lock the rebase held");
    assert_eq!(reply.get("error"), None, "{reply}");
    assert!(pass.is_ok(), "{pass:?}");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_items"],
        json!([]),
        "the drop ran after the job"
    );
    assert!(!rig.worktree_7().exists(), "its worktree was removed");
}

#[test]
fn a_drop_refused_after_a_rebase_leaves_the_rebase_saved() {
    use crate::runner::trigger::lock;
    use crate::work_item::ReviewCallState;

    let (rig, runner, _) = Rig::with_pull_request("koji");
    rig.land_on_origin("landed.txt");
    let read = rig.hold_next_git_push();

    let (reply, pass) = rig.during_read(
        read,
        || advance(&runner),
        || {
            let reply = rig.ask(&runner, "drop", Some("7"));
            let mut held = lock(&runner);
            held.focus = Some(ticket(7));
            let since = held.ports.clock.now();
            held.update(|item| item.review_call = ReviewCallState::Running { since })
                .unwrap();
            reply
        },
    );

    let reply = reply.expect("the drop waited for the lock the rebase held");
    assert_eq!(reply.get("error"), None, "{reply}");
    assert!(matches!(pass, Ok(Pass::Idle)), "{pass:?}");
    let pushed = rig.forge.head_of("kelpie/7").unwrap();
    let saved = lock(&runner)
        .state
        .item(ticket(7))
        .expect("the refused drop kept it")
        .known
        .head
        .clone();
    assert_eq!(saved, Some(pushed), "the pushed head was saved");
    let status = rig.ask(&runner, "status", None);
    assert!(
        status["standing"].to_string().contains("drop 7"),
        "the refusal stands: {status}"
    );
}

#[test]
fn a_rebase_whose_item_changed_during_the_job_is_not_saved() {
    use crate::runner::trigger::lock;

    let (rig, runner, head) = Rig::with_pull_request("koji");
    rig.land_on_origin("landed.txt");
    let read = rig.hold_next_git_push();

    let (moved, pass) = rig.during_read(
        read,
        || advance(&runner),
        || {
            let mut held = lock(&runner);
            held.focus = Some(ticket(7));
            held.update(|item| item.pull_request = Some(72)).unwrap();
        },
    );

    moved.expect("the item moved while the rebase ran");
    assert!(matches!(pass, Ok(Pass::Idle)), "{pass:?}");
    let pushed = rig.forge.head_of("kelpie/7").unwrap();
    assert_ne!(pushed, head, "the push went out");
    let seen = lock(&runner)
        .state
        .item(ticket(7))
        .unwrap()
        .known
        .head
        .clone();
    assert_ne!(seen, Some(pushed), "the new head was not saved");
}

// The base is read for the head the step saw. A step that asks for another head's base reads it,
// whatever an earlier read found for the first.
#[test]
fn a_base_read_for_a_head_that_moved_during_the_gap_is_not_used() {
    use crate::runner::git_reads::Found;
    use crate::runner::trigger::lock;
    use crate::worktree::Base;

    let (_rig, runner, head) = Rig::with_pull_request("koji");
    let mut runner = lock(&runner);
    runner.focus = Some(ticket(7));
    let moved = "0".repeat(40);
    runner.reads.git.keep(Found::Base {
        head: moved.clone(),
        base: Ok(Base::Behind),
    });

    assert!(matches!(runner.base_standing(&moved), Ok(Base::Behind)));
    assert!(matches!(runner.base_standing(&head), Ok(Base::Current)));
}

// The worker's first turn merges `kelpie/9`, the branch of open pull request 72, into its own, as
// the worker for #1178 merged `kelpie/1174`. `setup` changes the rig's settings.
fn borrowing(project: &str, setup: impl FnOnce(&Rig)) -> (Rig, Mutex<Runner>, String) {
    let borrow = |rig: &Rig| {
        rig.push_by_hand("kelpie/9", "borrowed.txt");
        rig.forge.open_pull_request(72, "kelpie/9", &[9]);
        setup(rig);
    };
    let work = Scripted::PushMerging("kelpie/9", "work.txt", "work\n");
    Rig::with_pull_request_worked(project, borrow, work)
}

// Pull request 72's work item, as kelpie kept it: parked on a ruling, with one finding its worker
// was sent to fix
fn keeping_72(runner: &Mutex<Runner>) {
    let mut runner = runner.lock().unwrap();
    let mut other = runner.state.work_items[0].clone();
    other.issue = ticket(9);
    other.branch = "kelpie/9".to_owned();
    other.pull_request = Some(72);
    other.phase = Phase::Ruling { id: ticket(72) };
    other.held = vec![Finding::new(
        Severity::High,
        "src/lib.rs",
        12,
        "loses a write",
        "two callers race",
    )];
    runner.state.work_items.push(other);
    let parked = Ruling {
        issue: Some(ticket(9)),
        pull_request: Some(72),
        alerted: true,
        ..Ruling::new(
            ticket(72),
            "Is it closed?",
            RulingKind::Stuck(Stuck::Closed),
        )
    };
    runner.state.rulings.push(parked);
}

#[test]
fn a_head_carrying_another_open_pull_requests_commits_gets_a_merge_ruling_naming_it() {
    let (rig, runner, head) = borrowing("shep", |_| {});
    keeping_72(&runner);
    rig.forge.set_checks(&head, Checks::Passed);

    let (id, question) = ruling_report(rig.verdict(&runner));

    assert_eq!(id, ticket(71));
    assert_eq!(
        question,
        format!(
            "Merge pull request #71 at {} into main? `shep kelpie rule 71 yes` merges it, \
             `shep kelpie rule 71 no <note>` sends the worker your note for a fix that goes to CI \
             and back to you, and `shep kelpie rule 71 rework <note>` sends it your note for a \
             change the whole review reads again.\n\
             This head carries commits of other open pull requests, so a yes merges those \
             into main too. #72 (a draft): kelpie sent its worker to fix HIGH src/lib.rs:12. \
             The review on the pull request may hold more.",
            &head[..7]
        )
    );
    let status = rig.ask(&runner, "status", None);
    let kept = status["rulings"].as_array().unwrap();
    let kept = kept.iter().find(|r| r["id"] == "71");
    assert_eq!(kept.unwrap()["question"], json!(question));
}

#[test]
fn a_borrowed_pull_request_no_work_item_holds_says_kelpie_has_no_record_of_it() {
    let (rig, runner, head) = borrowing("shep", |_| {});
    rig.forge.set_checks(&head, Checks::Passed);

    let (_, question) = ruling_report(rig.verdict(&runner));

    assert!(
        question.ends_with(
            "#72 (a draft): kelpie holds no record of it. Read its review on the pull request."
        ),
        "{question}"
    );
}

#[test]
fn a_borrowing_head_gets_the_merge_ruling_under_auto_too() {
    let (rig, runner, head) = borrowing("shep", Rig::merge_auto);
    rig.forge.set_checks(&head, Checks::Passed);

    let (id, question) = ruling_report(rig.verdict(&runner));

    assert_eq!(id, ticket(71));
    assert!(question.contains("#72 (a draft)"), "{question}");
}

#[test]
fn a_head_carrying_no_other_pull_requests_commits_gets_the_ruling_it_got_before() {
    // Pull request 72 is open, but the worker never merged its branch.
    let (rig, runner, head) = Rig::with_pull_request_set("shep", |rig| {
        rig.push_by_hand("kelpie/9", "borrowed.txt");
        rig.forge.open_pull_request(72, "kelpie/9", &[9]);
    });
    rig.forge.set_checks(&head, Checks::Passed);

    let (_, question) = ruling_report(rig.verdict(&runner));

    assert_eq!(
        question,
        format!(
            "Merge pull request #71 at {} into main? `shep kelpie rule 71 yes` merges it, \
             `shep kelpie rule 71 no <note>` sends the worker your note for a fix that goes to CI \
             and back to you, and `shep kelpie rule 71 rework <note>` sends it your note for a \
             change the whole review reads again.",
            &head[..7]
        )
    );
}

#[test]
fn a_head_carrying_no_other_pull_requests_commits_is_not_stopped_under_auto() {
    let (rig, runner, head) = Rig::with_pull_request_set("shep", |rig| {
        rig.merge_auto();
        rig.push_by_hand("kelpie/9", "borrowed.txt");
        rig.forge.open_pull_request(72, "kelpie/9", &[9]);
    });
    rig.forge.set_checks(&head, Checks::Passed);

    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
}

#[test]
fn a_borrowed_read_that_fails_parks_the_gate() {
    use crate::runner::git_reads::Found;
    use crate::runner::trigger::lock;
    use crate::worktree::WorktreeError;

    let (rig, runner, head) = borrowing("shep", |_| {});
    let open = rig.forge.list_open().unwrap();
    let mut runner = lock(&runner);
    runner.focus = Some(ticket(7));
    runner.reads.git.keep(Found::Borrowed {
        head: head.clone(),
        borrowed: Err(WorktreeError::Spawn("git is gone".to_owned())),
    });

    let Err(begin) = runner.borrowed(&open, &head) else {
        panic!("the failed read was passed over");
    };

    let Begin::Report(StepReport::GateFailed { issue, reason }) = *begin else {
        panic!("the gate was not parked");
    };
    assert_eq!(issue, ticket(7));
    assert!(
        reason.starts_with("cannot tell which pull requests this head carries"),
        "{reason}"
    );
    assert!(reason.contains("git is gone"), "{reason}");
}
