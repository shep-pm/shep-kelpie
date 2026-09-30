//! The shots comment at the merge ruling, its reworks and retries, and
//! what a finished work item takes with it

use serde_json::Value;

use super::{shots_comments, started, with_preview};
use crate::ports::{Checks, Role};
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted, ScriptedShots, git};

// Up to green CI on the reviewed head, which the Claude round's shots cover
fn green(project: &str) -> (Rig, std::sync::Mutex<Runner>, String) {
    let rig = with_preview(project);
    let runner = started(&rig);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    for _ in 0..4 {
        step(&runner).unwrap(); // the turn, qwen, the shots, claude
    }
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    (rig, runner, head)
}

fn tree(rig: &Rig, commit: &str) -> Vec<String> {
    let origin = rig.home.path().join("origin.git");
    let names = git(&origin, &["ls-tree", "--name-only", commit]);
    names.lines().map(str::to_owned).collect()
}

#[test]
fn the_merge_ruling_puts_the_shots_on_one_comment_off_the_branch() {
    let (rig, runner, head) = green("lab");
    rig.shots.script([]);
    let Some(StepReport::ShotsPosted {
        comment, head: of, ..
    }) = rig.verdict(&runner)
    else {
        panic!("no shots were posted before the ruling");
    };
    assert_eq!(of, head);
    assert_eq!(
        rig.shots.jobs().len(),
        1,
        "the round's run of this head is reused"
    );
    let Some(StepReport::Ruling { id: 1, .. }) = step(&runner).unwrap() else {
        panic!("the merge ruling did not follow");
    };

    let [body] = shots_comments(&rig).try_into().unwrap();
    let shots = rig
        .forge
        .head_of("kelpie-shots/71")
        .expect("the shots branch");
    assert!(
        body.contains(&format!("Kelpie's shots of {}.", &head[..7])),
        "{body}"
    );
    assert!(body.contains("the `kelpie-shots/71` branch"), "{body}");
    let url =
        format!("https://github.com/shep-pm/shep/blob/{shots}/events-mobile-dark.png?raw=true");
    assert!(body.contains(&url), "{body}");
    assert_eq!(tree(&rig, &shots).len(), 8);
    assert!(tree(&rig, "kelpie/7").iter().all(|f| !f.ends_with(".png")));
    assert_eq!(
        rig.forge.head_of("kelpie/7").unwrap(),
        head,
        "the branch never moved"
    );
    assert_eq!(
        rig.forge.edits(),
        Vec::<u64>::new(),
        "posted once, {comment}, and not yet edited"
    );
}

#[test]
fn under_auto_the_shots_comment_lands_before_the_merge() {
    let rig = with_preview("lab");
    rig.merge_auto();
    let runner = started(&rig);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    for _ in 0..4 {
        step(&runner).unwrap(); // the turn, qwen, the shots, claude
    }
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::ShotsPosted { .. }) = rig.verdict(&runner) else {
        panic!("auto went to merge without posting the shots");
    };
    assert_eq!(rig.forge.merges(), [], "not merged before the comment");
    for _ in 0..6 {
        rig.clock.advance(crate::runner::CHECKS_SETTLE);
        step(&runner).unwrap();
    }
    assert_eq!(rig.forge.merges(), [(71, head)]);
    assert_eq!(shots_comments(&rig).len(), 1);
}

#[test]
fn a_rework_edits_the_shots_comment_in_place() {
    let (rig, runner, first) = green("lab");
    let Some(StepReport::ShotsPosted { comment, .. }) = rig.verdict(&runner) else {
        panic!("no shots were posted");
    };
    step(&runner).unwrap(); // merge ruling 1
    let shots_1 = rig.forge.head_of("kelpie-shots/71").unwrap();

    rig.ask(&runner, "rule", Some("1 no make the header darker"));
    rig.claude.script([
        Scripted::Push("header.css", "dark\n"),
        Scripted::Text("CLEAN"),
    ]);
    for _ in 0..4 {
        step(&runner).unwrap(); // the fix, qwen, the shots, claude
    }
    let second = rig.forge.head_of("kelpie/7").unwrap();
    assert_ne!(second, first);
    rig.forge.set_checks(&second, Checks::Passed);
    let Some(StepReport::ShotsPosted {
        comment: again,
        head,
        ..
    }) = rig.verdict(&runner)
    else {
        panic!("the rework's shots were not posted");
    };
    assert_eq!((again, head.as_str()), (comment, second.as_str()));
    assert_eq!(rig.forge.edits(), [comment]);
    let [body] = shots_comments(&rig).try_into().unwrap();
    assert!(
        body.contains(&format!("Kelpie's shots of {}.", &second[..7])),
        "{body}"
    );
    let shots_2 = rig.forge.head_of("kelpie-shots/71").unwrap();
    let origin = rig.home.path().join("origin.git");
    assert_eq!(
        git(&origin, &["rev-parse", &format!("{shots_2}^")]),
        shots_1,
        "no force"
    );
}

// Posted once, then a rework whose head is green, ready for its shots comment
fn reworked(project: &str) -> (Rig, std::sync::Mutex<Runner>, u64) {
    let (rig, runner, _) = green(project);
    let Some(StepReport::ShotsPosted { comment, .. }) = rig.verdict(&runner) else {
        panic!("no shots were posted");
    };
    step(&runner).unwrap(); // merge ruling 1
    rig.ask(&runner, "rule", Some("1 no make the header darker"));
    rig.claude.script([
        Scripted::Push("header.css", "dark\n"),
        Scripted::Text("CLEAN"),
    ]);
    for _ in 0..4 {
        step(&runner).unwrap(); // the fix, qwen, the shots, claude
    }
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    (rig, runner, comment)
}

#[test]
fn a_shots_comment_someone_deleted_is_posted_again() {
    let (rig, runner, comment) = reworked("lab");
    rig.forge.delete_comment(comment);
    let Some(StepReport::ShotsPosted { comment: again, .. }) = rig.verdict(&runner) else {
        panic!("the shots were not posted again");
    };
    assert_ne!(again, comment);
}

#[test]
fn an_edit_that_fails_posts_no_second_comment() {
    let (rig, runner, _) = reworked("lab");
    rig.forge.set_comments_down(true);
    let report = rig.verdict(&runner);
    assert!(
        matches!(report, Some(StepReport::ShotsNotPosted { .. })),
        "{report:?}"
    );
    rig.forge.set_comments_down(false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ruling { id: 2, .. })
    ));
    assert_eq!(shots_comments(&rig).len(), 1);
}

#[test]
fn a_page_that_calls_a_domain_off_the_list_says_so_on_the_comment() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    let blocked = "blocked https://cdn.example/a.png: cdn.example is not a preview domain";
    rig.shots
        .script([ScriptedShots::Problems(vec![blocked.into()])]);
    for _ in 0..4 {
        step(&runner).unwrap();
    }
    let all = rig.claude.all_seen();
    let round = all.iter().find(|s| s.call.role == Role::Reviewer).unwrap();
    assert!(
        round
            .call
            .prompt
            .contains(&format!("/events at desktop, light: {blocked}"))
    );

    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    rig.verdict(&runner);
    let [body] = shots_comments(&rig).try_into().unwrap();
    assert!(
        body.contains(&format!(
            "What went wrong:\n\n- `/ at mobile, light: {blocked}`\n"
        )),
        "{body}"
    );
}

// What shep's pull requests got: npm's error, naming its log in the home folder
fn failing(rig: &Rig) -> &'static str {
    let reason = format!(
        "the dev server exited: npm error Missing script: \"dev\"\n\
         npm error A complete log of this run can be found in: {}/.npm/_logs/debug-0.log",
        rig.home.path().display()
    );
    Box::leak(reason.into_boxed_str())
}

#[test]
fn a_failed_run_posts_nothing_and_the_merge_ruling_says_so() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    let reason = failing(&rig);
    rig.shots.script([ScriptedShots::Fail(reason)]);
    for _ in 0..4 {
        step(&runner).unwrap(); // the turn, qwen, the shots, claude
    }
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Ruling {
        id: 1, question, ..
    }) = rig.verdict(&runner)
    else {
        panic!("the merge ruling did not come");
    };
    assert_eq!(
        question,
        format!(
            "Merge pull request #71 at {} into main? Kelpie's shots of it failed, so \
             none of this head's are on the pull request (an earlier head's may be); \
             the runner's log says why. `shep kelpie rule 1 yes` merges it, and `shep kelpie rule 1 no <note>` \
             sends the worker your note.",
            &head[..7]
        )
    );
    assert_eq!(rig.forge.comments(), [], "nothing on the pull request");
    assert_eq!(rig.forge.head_of("kelpie-shots/71"), None);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["shots_failed"], reason);
}

#[test]
fn under_auto_a_failed_run_posts_nothing_and_the_notice_says_so() {
    let rig = with_preview("lab");
    rig.merge_auto();
    let runner = started(&rig);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    rig.shots.script([ScriptedShots::Fail(failing(&rig))]);
    for _ in 0..4 {
        step(&runner).unwrap(); // the turn, qwen, the shots, claude
    }
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    for _ in 0..8 {
        rig.clock.advance(crate::runner::CHECKS_SETTLE);
        step(&runner).unwrap();
    }
    assert_eq!(rig.forge.merges(), [(71, head.clone())]);
    assert_eq!(rig.forge.comments(), [], "nothing on the pull request");
    let [(_, notice)] = rig.alerts.posts().try_into().unwrap();
    assert_eq!(
        notice.text,
        format!(
            "Pull request #71 for issue #7 merged into main at {} on lab, every gate passed. \
             Kelpie's shots of it failed, so none of this head's are on the pull request (an earlier head's may be); \
             the runner's log says why. Nothing to answer.",
            &head[..7]
        )
    );
}

// A page's own error can name a local folder too, and the forge port refuses it.
#[test]
fn a_shots_comment_naming_a_local_folder_is_refused_once_and_the_ruling_says_so() {
    let rig = with_preview("lab");
    let runner = started(&rig);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    let page = format!(
        "console: cannot read {}/app/.env",
        rig.home.path().display()
    );
    rig.shots.script([ScriptedShots::Problems(vec![page])]);
    for _ in 0..4 {
        step(&runner).unwrap(); // the turn, qwen, the shots, claude
    }
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::ShotsNotPosted { reason, .. }) = rig.verdict(&runner) else {
        panic!("the shots comment was not refused");
    };
    assert_eq!(
        reason,
        "not posted: the text names a folder on this machine"
    );
    let Some(StepReport::Ruling { question, .. }) = step(&runner).unwrap() else {
        panic!("the merge ruling did not follow");
    };
    assert!(
        question.contains("Kelpie's shots of it failed"),
        "{question}"
    );
    assert_eq!(rig.forge.comments(), [], "nothing on the pull request");
    step(&runner).unwrap(); // the ruling's alert
    rig.clock.advance(600);
    assert_eq!(step(&runner).unwrap(), None, "never tried again");
}

#[test]
fn a_post_that_failed_is_tried_again_while_the_ruling_waits() {
    let (rig, runner, head) = green("lab");
    rig.forge.set_comments_down(true);
    rig.verdict(&runner); // ShotsNotPosted
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    rig.forge.set_comments_down(false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: 1 })
    ));
    assert_eq!(step(&runner).unwrap(), None, "not before its retry is due");
    rig.clock.advance(60);
    let Some(StepReport::ShotsPosted { head: of, .. }) = step(&runner).unwrap() else {
        panic!("the failed post was not tried again");
    };
    assert_eq!(of, head);
    assert_eq!(step(&runner).unwrap(), None, "posted once");
    rig.clock.advance(600);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(shots_comments(&rig).len(), 1);
}

#[test]
fn a_comment_that_cannot_be_posted_does_not_hold_the_ruling() {
    let (rig, runner, _) = green("lab");
    rig.forge.set_comments_down(true);
    let report = rig.verdict(&runner);
    let Some(StepReport::ShotsNotPosted { reason, .. }) = report else {
        panic!("{report:?}");
    };
    assert_eq!(reason, "gh failed: comments are down");
    rig.forge.set_comments_down(false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ruling { .. })
    ));
}

#[test]
fn a_finished_work_item_takes_its_shots_with_it() {
    let (rig, runner, _) = green("lab");
    rig.verdict(&runner); // the shots comment
    step(&runner).unwrap(); // merge ruling 1
    assert!(rig.home.path().join("kelpie/shots/lab/7").is_dir());
    rig.ask(&runner, "rule", Some("1 yes"));
    for _ in 0..4 {
        rig.clock.advance(crate::runner::CHECKS_SETTLE);
        step(&runner).unwrap();
    }
    assert_eq!(rig.forge.merges().len(), 1);
    assert!(!rig.home.path().join("kelpie/shots/lab/7").exists());
    assert!(!rig.home.path().join("kelpie/playwright/lab/7").exists());
    assert_eq!(rig.forge.head_of("kelpie-shots/71"), None, "its branch too");
}

#[test]
fn a_dropped_work_item_deletes_its_shots_branch_too() {
    let (rig, runner, _) = green("lab");
    rig.verdict(&runner); // the shots comment
    assert!(rig.forge.head_of("kelpie-shots/71").is_some());
    step(&runner).unwrap(); // merge ruling 1
    let status = rig.ask(&runner, "drop", None);
    assert_eq!(status["work_item"], Value::Null, "{status}");
    assert_eq!(rig.forge.head_of("kelpie-shots/71"), None);
    assert!(
        rig.forge.head_of("kelpie/7").is_some(),
        "a drop keeps the work item's own branch"
    );
}
