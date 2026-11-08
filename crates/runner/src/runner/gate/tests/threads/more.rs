//! The thread wait where a ruling meets a deferred finding, a stale answer or a late
//! reply, through the runner's stand-ins

use super::*;
use crate::test::Rig;

const FOLLOW_UP_BOTH: &str = "{\"threads\": [\
    {\"item\": 7, \"thread\": 1, \"decision\": \"follow-up\", \"why\": \"too big\"}, \
    {\"item\": 7, \"thread\": 2, \"decision\": \"follow-up\", \"why\": \"too big\"}]}";

#[test]
fn a_follow_up_refused_then_a_close_does_not_file_the_refused_finding_later() {
    let rig = pm_project("threads-refused-close");
    rig.merge_auto();
    let (rig, runner, head) = at_the_gate_found(rig, &[racy()]);
    defers(&rig, &[racy()]);
    open_bots(&rig, &head, &[NIT]);
    rig.forge.set_issues_down(true);
    ruled_by(&rig, &runner, FOLLOW_UP);
    assert_eq!(rig.forge.coderabbit.resolved(), Vec::<String>::new());

    rig.clock.advance(BACK_OFF);
    rig.forge.set_issues_down(false);
    ruled_by(&rig, &runner, CLOSE);
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);

    rig.clock.advance(24 * 60 * 60);
    ruled_by(&rig, &runner, NOTHING);
    let asked = pm_seen(&rig);
    let third = &asked.last().expect("a third wake").call.prompt;
    assert!(
        third.contains("looks racy"),
        "the deferred finding is still unruled: {third}"
    );
    assert!(
        !third.contains("could be shorter"),
        "the refused follow-up is not asked again once closed: {third}"
    );
    assert_eq!(rig.forge.created(), [], "nothing was filed");
}

#[test]
fn an_answer_that_lands_after_the_item_left_the_gate_acts_on_nothing() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-stale"), &[NIT]);
    let hold = Hold::default();
    rig.claude
        .script_for(Role::Pm, [Scripted::HoldThenSay(hold.clone(), FIX)]);
    Driven::new(&runner).started();
    assert!(hold.entered(CALL_PATIENCE), "the wake's call never began");

    rig.forge.set_state(71, PullRequestState::Closed);
    let deadline = Instant::now() + CALL_PATIENCE;
    while Instant::now() < deadline {
        step(&runner).unwrap();
        let status = rig.ask(&runner, "status", None);
        if status["work_item"]["phase"]["state"] == "ruling" {
            break;
        }
    }
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ruling",
        "the closed pull request parks the item"
    );
    hold.release();
    answers(&runner, 1);

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ruling", "{status}");
    assert_eq!(
        worker(&rig).len(),
        1,
        "no fix turn went out: {:?}",
        worker(&rig)
    );
}

#[test]
fn a_maintainer_reply_while_the_push_back_is_decided_asks_again() {
    let (rig, runner, _) = at_the_gate(pm_project("threads-push-race"), &[]);
    let id = declined_thread(&rig, &runner);
    let hold = Hold::default();
    rig.claude.script_for(
        Role::Pm,
        [
            Scripted::HoldThenSay(hold.clone(), PUSH_BACK),
            Scripted::Say(NOTHING),
        ],
    );
    Driven::new(&runner).started();
    assert!(hold.entered(CALL_PATIENCE), "the wake's call never began");

    rig.forge
        .coderabbit
        .person_replies(&id, "octocat", Standing::Writer, "Wait, one more thing.");
    worker_says(&rig, [Scripted::Push("theirs.txt", "theirs\n")]);
    hold.release();
    answers(&runner, 2);
    assert_eq!(
        pm_seen(&rig).len(),
        2,
        "a reply written after the ask was read is not what kelpie pushed back on"
    );
}

#[test]
fn a_thread_and_a_deferred_finding_followed_up_together_are_both_filed_before_the_merge() {
    let rig = pm_project("threads-deferred-both");
    rig.merge_auto();
    let (rig, runner, head) = at_the_gate_found(rig, &[racy()]);
    defers(&rig, &[racy()]);
    open_bots(&rig, &head, &[NIT]);

    ruled_by(&rig, &runner, FOLLOW_UP_BOTH);
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
    assert_eq!(rig.forge.created().len(), 2, "both are filed");

    let seen = steps(&runner, |r| matches!(r, StepReport::Finished { .. }));
    assert!(
        seen.iter().all(|r| !matches!(r, StepReport::Ruling { .. })),
        "nothing is asked at the merge: {seen:?}"
    );
    assert_eq!(rig.forge.merges(), [(71, head)], "the head merged");
    assert_eq!(rig.forge.created().len(), 2, "and nothing more was filed");
}

#[test]
fn a_push_back_on_a_deferred_finding_is_dropped_and_the_finding_stays_unruled() {
    let rig = pm_project("threads-deferred-push-back");
    let (rig, runner, _) = at_the_gate_found(rig, &[racy()]);
    defers(&rig, &[racy()]);

    let (acted, dropped) = ruled(&ruled_by(&rig, &runner, PUSH_BACK));
    assert_eq!(acted, Vec::<String>::new());
    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert!(dropped[0].contains("no thread to reply on"), "{dropped:?}");
    let left = std::fs::read_to_string(rig.build_7().join("deferred-findings.md")).unwrap();
    assert!(left.contains("looks racy"), "it is still deferred: {left}");
    assert_eq!(rig.forge.created(), []);
}

fn left_deferred(rig: &Rig) -> String {
    std::fs::read_to_string(rig.build_7().join("deferred-findings.md")).unwrap()
}

#[test]
fn a_deferred_finding_the_worker_declines_to_fix_stays_deferred() {
    let rig = pm_project("threads-deferred-declined");
    let (rig, runner, _) = at_the_gate_found(rig, &[racy()]);
    defers(&rig, &[racy()]);
    worker_says(
        &rig,
        [Scripted::PushSay(
            "other.txt",
            "other\n",
            "thread 1: declined: the caller checks it",
        )],
    );

    ruled_by(&rig, &runner, FIX);
    steps(&runner, |r| matches!(r, StepReport::FixPushed { .. }));
    let left = left_deferred(&rig);
    assert!(left.contains("looks racy"), "it is not lost: {left}");
    assert_eq!(rig.forge.created(), []);
}

#[test]
fn a_deferred_finding_fixed_by_the_worker_leaves_the_deferred_file() {
    let rig = pm_project("threads-deferred-fixed");
    let (rig, runner, _) = at_the_gate_found(rig, &[racy()]);
    defers(&rig, &[racy()]);
    worker_says(
        &rig,
        [Scripted::PushSay(
            "shorter.txt",
            "shorter\n",
            "thread 1: fixed",
        )],
    );

    ruled_by(&rig, &runner, FIX);
    steps(&runner, |r| matches!(r, StepReport::FixPushed { .. }));
    let left = left_deferred(&rig);
    assert!(!left.contains("looks racy"), "it is fixed: {left}");
}

#[test]
fn a_deferred_finding_written_twice_leaves_the_file_once_it_is_closed() {
    let rig = pm_project("threads-deferred-twice");
    let (rig, runner, _) = at_the_gate_found(rig, &[racy()]);
    defers(&rig, &[racy(), racy()]);

    ruled_by(&rig, &runner, CLOSE);
    let left = left_deferred(&rig);
    assert!(!left.contains("looks racy"), "no copy is left: {left}");
}

#[test]
fn a_deferred_finding_with_a_test_has_it_run_again_in_the_threads_turn() {
    let rig = pm_project("threads-deferred-test");
    let (rig, runner, _) = at_the_gate_found(rig, &[racy()]);
    defers(&rig, &[racy()]);
    runner.lock().unwrap().state.work_items[0].held[0].test = Some("tests/race.rs".to_owned());
    worker_says(
        &rig,
        [Scripted::PushSay(
            "shorter.txt",
            "shorter\n",
            "thread 1: fixed",
        )],
    );

    ruled_by(&rig, &runner, FIX);
    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert_eq!(item.test_checks.len(), 1, "{:?}", item.test_checks);
    assert!(item.test_checks[0].after.is_none(), "not run after yet");

    steps(&runner, |r| matches!(r, StepReport::FixPushed { .. }));
    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert!(
        item.test_checks[0].after.is_some(),
        "the test ran after the fix: {:?}",
        item.test_checks
    );
}
