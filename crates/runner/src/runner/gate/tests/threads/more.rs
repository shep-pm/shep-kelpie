//! The thread wait where a ruling meets a deferred finding, a stale answer or a late
//! reply, through the runner's stand-ins

use super::*;

#[test]
fn a_follow_up_refused_then_a_close_leaves_the_deferred_findings_to_the_merge() {
    let found = Finding::new(
        Severity::High,
        "src/lib.rs",
        9,
        "looks racy",
        "two threads write the same field",
    );
    let rig = pm_project("threads-refused-close");
    rig.merge_auto();
    let (rig, runner, head) = at_the_gate_found(rig, std::slice::from_ref(&found));
    std::fs::write(
        rig.build_7().join("deferred-findings.md"),
        deferred(std::slice::from_ref(&found)),
    )
    .unwrap();
    open_bots(&rig, &head, &[NIT]);
    rig.forge.set_issues_down(true);
    ruled_by(&rig, &runner, FOLLOW_UP);
    assert_eq!(rig.forge.coderabbit.resolved(), Vec::<String>::new());

    rig.clock.advance(BACK_OFF);
    rig.forge.set_issues_down(false);
    ruled_by(&rig, &runner, CLOSE);
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);

    let question = merge_ruling(&runner);
    assert!(question.contains("looks racy"), "{question}");
    assert!(
        !question.contains("could be shorter"),
        "the refused follow-up is not filed at the merge: {question}"
    );
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
    // The head the declined comment's fix pushed goes back through a pass of the
    // review first, so the wake comes a few passes after the ask would have
    let driven = Driven::new(&runner);
    let began = (0..12)
        .map(|_| {
            driven.pass();
            hold.entered(std::time::Duration::from_secs(1))
        })
        .any(|entered| entered);
    assert!(began, "the wake's call never began");

    rig.forge
        .coderabbit
        .person_replies(&id, "octocat", Standing::Writer, "Wait, one more thing.");
    worker_says(&rig, [Scripted::Push("theirs.txt", "theirs\n")]);
    read_clean(&rig);
    hold.release();
    answers(&runner, 2);
    assert_eq!(
        pm_seen(&rig).len(),
        2,
        "a reply written after the ask was read is not what kelpie pushed back on"
    );
}

#[test]
fn deferred_findings_are_still_filed_at_the_merge_after_a_follow_up_before_it() {
    let found = Finding::new(
        Severity::High,
        "src/lib.rs",
        9,
        "looks racy",
        "two threads write the same field",
    );
    let rig = pm_project("threads-deferred");
    rig.merge_auto();
    let (rig, runner, head) = at_the_gate_found(rig, std::slice::from_ref(&found));
    std::fs::write(
        rig.build_7().join("deferred-findings.md"),
        deferred(std::slice::from_ref(&found)),
    )
    .unwrap();
    open_bots(&rig, &head, &[NIT]);

    ruled_by(&rig, &runner, FOLLOW_UP);
    assert_eq!(rig.forge.coderabbit.resolved(), [THREAD]);
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(issue.labels, [READY], "the follow-up is ready for a worker");

    let question = merge_ruling(&runner);
    assert_eq!(rig.forge.merges(), [(71, head)], "the head merged");
    assert!(
        question.contains("looks racy"),
        "the deferred finding is still there to file: {question}"
    );
}

// #7's pull request, whose qwen round found `found` and whose worker fixed it,
// then read clean by Claude. The item stands at the gate with `found` still held.
fn at_the_gate_found(rig: Rig, found: &[Finding]) -> (Rig, Mutex<Runner>, String) {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    runner.lock().unwrap().plan_two_reads_for_the_rig();
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.reviewer
        .script([ScriptedRound::Findings(found.to_vec())]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text(CLEAN),
    ]);
    for _ in 0..20 {
        step(&runner).unwrap();
        if rig.ask(&runner, "status", None)["work_item"]["phase"]["state"] == "ci" {
            break;
        }
    }
    let head = rig.forge.head_of("kelpie/7").expect("the worker pushed");
    (rig, runner, head)
}
