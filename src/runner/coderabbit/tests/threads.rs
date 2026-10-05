//! Resolving the threads a round sent the worker, once its fix moves the head

use super::{hold_a_finding, now, phase, summoned};
use crate::ports::Checks;
use crate::runner::{StepReport, step};
use crate::test::Scripted;

#[test]
fn a_forge_that_keeps_refusing_to_resolve_lets_the_fix_go_on_after_three_steps() {
    let (rig, runner, head) = summoned("shep");
    hold_a_finding(&rig, &runner, &head, "Name the flag.");
    rig.forge.coderabbit.set_resolve_down(true);
    rig.claude.script([Scripted::Push("flag.txt", "named\n")]);
    step(&runner).unwrap(); // the fix turn
    for _ in 0..2 {
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::GateFailed { .. })
        ));
        assert_eq!(phase(&rig, &runner)["stage"], "fixing", "tried again");
    }
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ThreadsLeftOpen {
            issue: 7,
            pull_request: 71,
            threads: vec!["PRRT_71_0".into()],
            reason: "cannot resolve a thread on #71: gh failed: resolving is down".into(),
        })
    );
    assert_eq!(phase(&rig, &runner)["state"], "ci");
    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert_eq!((item.threads_sent.len(), item.resolve_failures), (0, 0));

    // Still open, the thread goes to the worker again with the next review.
    rig.forge.coderabbit.set_resolve_down(false);
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned { .. })
    ));
    rig.forge.coderabbit.review(71, &fixed, now(&rig) + 60, &[]);
    rig.clock.advance(60);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitReviewed {
            round: 2,
            open_threads: 1,
            ..
        })
    ));
}

#[test]
fn one_refusal_then_a_resolve_goes_on_to_ci_as_usual() {
    let (rig, runner, head) = summoned("rotom");
    hold_a_finding(&rig, &runner, &head, "Name the flag.");
    rig.forge.coderabbit.set_resolve_down(true);
    rig.claude.script([Scripted::Push("flag.txt", "named\n")]);
    step(&runner).unwrap(); // the fix turn
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::GateFailed { .. })
    ));
    rig.forge.coderabbit.set_resolve_down(false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    assert_eq!(rig.forge.coderabbit.resolved(), ["PRRT_71_0"]);
    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert_eq!(item.resolve_failures, 0);
}

// A file saved by a build before `threads_sent` holds a bot fix with no
// thread ids, and nothing on the work item names them, so none is resolved.
#[test]
fn a_bot_fix_saved_before_threads_were_kept_resolves_none_and_the_next_review_sends_them_again() {
    let (rig, runner, head) = summoned("koji");
    hold_a_finding(&rig, &runner, &head, "Name the flag.");
    drop(runner);
    let state = rig.paths().state;
    let text = std::fs::read_to_string(&state).unwrap();
    let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    let item = saved["work_items"][0].as_object_mut().unwrap();
    assert!(item.remove("threads_sent").is_some());
    std::fs::write(&state, saved.to_string()).unwrap();

    let runner = rig.open().unwrap();
    assert_eq!(phase(&rig, &runner)["stage"], "fixing");
    rig.claude.script([Scripted::Push("flag.txt", "named\n")]);
    step(&runner).unwrap(); // the fix turn
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    assert!(rig.forge.coderabbit.resolved().is_empty());
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned { .. })
    ));
    rig.forge.coderabbit.review(71, &fixed, now(&rig) + 60, &[]);
    rig.clock.advance(60);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitReviewed {
            open_threads: 1,
            ..
        })
    ));
}

#[test]
fn a_new_pass_of_the_review_forgets_the_threads_sent() {
    let (rig, runner, head) = summoned("mew");
    hold_a_finding(&rig, &runner, &head, "Name the flag.");
    rig.claude.script([Scripted::Say("Nothing to change.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    let Some(StepReport::Ruling { id, .. }) = step(&runner).unwrap() else {
        panic!("no ruling on a fix that pushed nothing");
    };
    rig.ask(&runner, "rule", Some(&format!("{id} no rename it instead")));
    rig.claude
        .script([Scripted::Push("rename.txt", "renamed\n")]);
    step(&runner).unwrap(); // the noted turn: pushes, and a pass begins
    assert_eq!(phase(&rig, &runner)["state"], "review");
    step(&runner).unwrap(); // round 1, qwen
    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert!(item.threads_sent.is_empty(), "{:?}", item.threads_sent);
}
