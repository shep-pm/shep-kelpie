//! The maintainer's free text under a ruling, which a project with a
//! project manager reads as words to it, and what it leaves unchanged

use std::sync::Mutex;
use std::time::Instant;

use super::asked::{
    advanced_until, looked, made_stuck, merge_put, pending, refusals, turns, woken_prompt,
    worker_sent,
};
use super::{NOTHING, name_pm, pm_seen};
use crate::ports::{Checks, Role};
use crate::runner::flight::advance;
use crate::runner::{Runner, step};
use crate::state::{Resume, RulingKind};
use crate::test::{CALL_PATIENCE, Rig, Scripted};

#[test]
fn a_plain_comment_under_a_merge_ruling_waits_for_the_project_manager() {
    let (rig, runner) = merge_put("asked-held-merge", true);
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);
    let before = turns(&rig);

    rig.writes_on(71, "octocat", "Why is this one so large?");
    rig.next_look();
    let prompt = woken_prompt(&rig, &runner);

    assert!(prompt.contains("Why is this one so large?"), "{prompt}");
    assert!(pending(&rig), "the ruling waits for an answer");
    looked(&rig, &runner);
    assert_eq!(turns(&rig), before, "no worker turn started");
    assert_eq!(rig.github.reactions().len(), 1, "a thumbs up");
    assert_eq!(refusals(&rig), Vec::<String>::new());
}

#[test]
fn a_plain_comment_under_a_stuck_ruling_waits_and_is_not_told_how_to_reply() {
    let (rig, runner) = merge_put("asked-held-stuck", true);
    made_stuck(&runner);
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    rig.writes_on(71, "octocat", "go ahead and retry");
    rig.next_look();
    let prompt = woken_prompt(&rig, &runner);

    assert!(prompt.contains("go ahead and retry"), "{prompt}");
    assert!(pending(&rig));
    assert_eq!(rig.github.reactions().len(), 1);
    assert_eq!(refusals(&rig), Vec::<String>::new());
}

#[test]
fn free_text_after_a_rulings_number_waits_for_the_project_manager() {
    let (rig, runner) = merge_put("asked-held-numbered", true);
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);
    let before = turns(&rig);

    rig.writes_on(71, "octocat", "71 is this the smaller option?");
    rig.next_look();
    let prompt = woken_prompt(&rig, &runner);

    assert!(prompt.contains("is this the smaller option?"), "{prompt}");
    assert!(pending(&rig));
    looked(&rig, &runner);
    assert_eq!(turns(&rig), before);
    assert_eq!(rig.github.reactions().len(), 1);
    assert_eq!(refusals(&rig), Vec::<String>::new());
}

#[test]
fn a_no_note_and_a_rework_still_answer_a_merge_ruling_with_a_project_manager_named() {
    for (reply, note) in [
        ("71 no fix the nit", "with this note:\n\nfix the nit"),
        ("71 rework split the parser", "split the parser"),
    ] {
        let (rig, runner) = merge_put("asked-held-answers", true);
        rig.claude.script([Scripted::Push("flag.txt", "flag\n")]);

        rig.writes_on(71, "octocat", reply);
        rig.next_look();
        worker_sent(&rig, &runner, note);

        assert!(!pending(&rig), "{reply}");
        assert_eq!(pm_seen(&rig).len(), 0, "{reply}: nothing was held");
    }
}

#[test]
fn plain_text_still_answers_a_workers_question_with_a_project_manager_named() {
    let (rig, runner) = merge_put("asked-held-question", true);
    {
        let mut runner = runner.lock().unwrap();
        let mut next = runner.state.clone();
        next.rulings[0].kind = RulingKind::Question {
            asked: "Which flag?".into(),
            resume: Resume::Nothing,
        };
        runner.save(next).unwrap();
    }

    rig.writes_on(71, "octocat", "use the blue one");
    rig.next_look();
    advanced_until(&runner, || !pending(&rig));

    assert_eq!(rig.github.reactions().len(), 1, "a thumbs up");
    assert_eq!(pm_seen(&rig).len(), 0, "the answer was the worker's");
}

#[test]
fn the_maintainers_comment_with_no_ruling_waits_and_starts_no_worker_turn() {
    let (rig, runner, head) = Rig::with_pull_request_set("asked-held-bare", |rig| {
        rig.with_app();
        rig.maintainer("octocat");
        name_pm(rig);
    });
    rig.forge.threads.grant_write("octocat");
    // The comments are read as CI goes green, before any ruling is raised
    rig.forge.set_checks(&head, Checks::Passed);
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);
    let before = turns(&rig);

    rig.writes_on(71, "octocat", "Can you say why the cache is a map?");
    step(&runner).unwrap(); // sees the checks
    rig.clock.advance(crate::runner::CHECKS_SETTLE);
    let prompt = woken_prompt(&rig, &runner);

    assert!(
        prompt.contains("Can you say why the cache is a map?"),
        "{prompt}"
    );
    looked(&rig, &runner);
    assert_eq!(turns(&rig), before, "no worker turn started");
}

#[test]
fn another_writers_comment_is_still_a_request_of_the_worker() {
    let (rig, runner) = merge_put("asked-held-other", true);
    rig.forge.threads.grant_write("octo-reviewer");
    rig.claude
        .script([Scripted::Push("rename.txt", "renamed\n")]);

    rig.writes_on(71, "octo-reviewer", "Please rename the flag.");
    rig.next_look();
    worker_sent(&rig, &runner, "Please rename the flag.");

    assert_eq!(pm_seen(&rig).len(), 0, "nothing was held");
}

#[test]
fn the_maintainers_review_thread_still_reaches_the_worker() {
    use crate::ports::Standing;
    let (rig, runner) = merge_put("asked-held-thread", true);
    rig.claude
        .script([Scripted::Push("rename.txt", "renamed\n")]);

    rig.forge.coderabbit.person_opens(
        71,
        "src/lib.rs",
        "octocat",
        Standing::Writer,
        "This name is wrong.",
    );
    rig.next_look();
    worker_sent(&rig, &runner, "This name is wrong.");

    assert_eq!(pm_seen(&rig).len(), 0, "nothing was held");
}

#[test]
fn a_read_made_again_holds_a_comment_once() {
    let (rig, runner) = merge_put("asked-held-once", true);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(NOTHING), Scripted::Say(NOTHING)]);

    rig.writes_on(71, "octocat", "Why is this one so large?");
    rig.next_look();
    let prompt = woken_prompt(&rig, &runner);
    assert_eq!(prompt.matches("Why is this one so large?").count(), 1);
    for _ in 0..4 {
        looked(&rig, &runner);
        rig.clock.advance(crate::runner::review_bot::LATE_READ);
    }

    let wakes: Vec<_> = pm_seen(&rig)
        .into_iter()
        .filter(|s| s.call.prompt.contains("Why is this one so large?"))
        .collect();
    assert_eq!(wakes.len(), 1, "the words went to it once");
}

#[test]
fn held_words_outlast_a_restart_and_wake_the_project_manager() {
    let (rig, runner) = merge_put("asked-held-restart", true);
    rig.writes_on(71, "octocat", "Why is this one so large?");
    rig.next_look();
    // Read and held, with no wake started yet
    let held = |runner: &Mutex<Runner>| runner.lock().unwrap().state.pm_told.clone();
    let deadline = Instant::now() + CALL_PATIENCE;
    while held(&runner).is_empty() {
        assert!(Instant::now() < deadline, "the comment was never held");
        advance(&runner).unwrap();
        assert_eq!(pm_seen(&rig).len(), 0, "woken before the restart");
    }
    drop(runner);
    let runner = rig.open().unwrap();
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    rig.next_look();
    let prompt = woken_prompt(&rig, &runner);

    assert!(prompt.contains("Why is this one so large?"), "{prompt}");
}
