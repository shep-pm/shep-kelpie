//! A bot that reviews every push on its own, which the pass ended before
//! reaching, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use crate::ports::Checks;
use crate::runner::Runner;
use crate::runner::coderabbit::tests::now;
use crate::runner::review_bot::two_bots::{listing, reviewed};
use crate::test::{Rig, Scripted};

const P1: &str = "P1: The build and the upload share a token, so a script can swap `gh`.";

// qwen and Claude read, kelpie's own rule reads twice, and cubic, which nobody
// summoned, reviewed the head on its own with a P1 thread open.
fn cubic_read_alone() -> (Rig, Mutex<Runner>, String) {
    let rig = listing("shep", &["cubic"]);
    rig.reviews_need(Some(2));
    let (runner, head) = reviewed(&rig);
    rig.forge
        .coderabbit
        .cubic_review(71, &head, now(&rig) + 60, &[P1]);
    rig.clock.advance(60);
    (rig, runner, head)
}

#[test]
fn a_bots_open_thread_reaches_the_worker_when_the_pass_ended_before_its_round() {
    let (rig, runner, head) = cubic_read_alone();
    rig.forge.set_checks(&head, Checks::Passed);
    rig.claude.script([Scripted::PushSay(
        "other.txt",
        "other\n",
        "thread 1: declined: the job is split already",
    )]);

    let mut seen = Vec::new();
    for _ in 0..10 {
        seen.push(format!("{:?}", rig.verdict(&runner)));
        let pushed = rig.forge.head_of("kelpie/7").unwrap();
        rig.forge.set_checks(&pushed, Checks::Passed);
    }

    let file = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(file.contains("swap `gh`"), "{file}\n{seen:#?}");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["bot_reads"], json!({ "cubic": 1 }));
    let ruled = seen.iter().filter(|s| s.contains("Ruling")).count();
    assert_eq!(ruled, 1, "one merge ruling, after the worker: {seen:#?}");
    // The worker declined it, so it is still open: the ruling lists it with
    // its level and place.
    let ruling = seen.iter().find(|s| s.contains("Ruling")).unwrap();
    assert!(ruling.contains("cubic left 1 P1 thread open."), "{ruling}");
    assert!(ruling.contains("- P1 `"), "{ruling}");
}
