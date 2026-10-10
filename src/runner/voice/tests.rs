use std::sync::Mutex;

use crate::github::ApiError;
use crate::ports::{Checks, Finding, Severity};
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::test::{Rig, Scripted, ScriptedRound};

const ASKS: &str = "I added the flag.\n\n<kelpie-question>\nShould it be `--dry-run` or \
                    `--check`?\n</kelpie-question>\n";
const QUESTION: &str = "Should it be `--dry-run` or `--check`?";

// The reply a question ruling is answered with, as its comment ends
const HOW_TO_ANSWER: &str = "Reply in this thread to answer ruling 1: your reply is the answer.\n\n\
     If other rulings wait on this thread, start your reply with `1`. \
     At the terminal: `shep kelpie rule 1 <your answer>`.";

// A running project with issue 7 in flight, whose worker's first turn asks
// before any pull request is open
fn asking_before_a_pull_request(rig: &Rig) -> (Mutex<Runner>, StepReport) {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say(ASKS)]);
    let asked = step(&runner)
        .unwrap()
        .expect("the question raised a ruling");
    (runner, asked)
}

// Pull request 71 is open and a qwen finding went to the worker, whose fix
// turn asks a question: ruling 1 is on the pull request.
fn asking_on_a_pull_request(rig: &Rig) -> (Mutex<Runner>, StepReport) {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
    rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
        severity: Severity::Medium,
        file: "src/lib.rs".into(),
        line: 3,
        what: "unused variable".into(),
        why: "dead code".into(),
    }])]);
    step(&runner).unwrap(); // round 1's qwen call
    step(&runner).unwrap(); // the finding goes to the worker
    rig.claude.script([Scripted::Say(ASKS)]);
    let asked = step(&runner)
        .unwrap()
        .expect("the fix turn's question raised a ruling");
    (runner, asked)
}

#[test]
fn a_merge_ruling_is_posted_through_the_app_on_its_pull_request_and_mentions_the_maintainer() {
    let (rig, _runner, head) = Rig::parked_set("shep", |rig| {
        rig.with_app();
        rig.maintainer("octocat");
    });

    let short = &head[..7];
    let body = format!(
        "@octocat Merge this pull request at {short} into main?\n\n\
         Reply in this thread to answer ruling 1:\n\
         - `yes` merges it\n\
         - `no <note>` sends the worker your note for a fix that goes to CI and back to you\n\
         - `rework <note>` sends it your note for a change the whole review reads again\n\n\
         If other rulings wait on this thread, start your reply with `1`. \
         At the terminal: `shep kelpie rule 1 yes`."
    );
    // The two review rounds that came first are on the thread too.
    let comments = rig.github.comments();
    let [qwen, claude, ruling] = comments.as_slice() else {
        panic!("two rounds and the ruling: {comments:?}");
    };
    assert_eq!((qwen.0, claude.0), (71, 71));
    assert_eq!(ruling, &(71, body));
    let tokens: Vec<_> = rig.github.writes().into_iter().map(|(t, _)| t).collect();
    assert!(
        tokens.iter().all(|t| t == "ghs_test1"),
        "through the App's token: {tokens:?}"
    );
    assert_eq!(
        rig.forge.comments(),
        [],
        "nothing posted as the maintainer's login"
    );
}

#[test]
fn a_ruling_before_any_pull_request_is_posted_on_the_issue() {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.maintainer("octocat");

    let (_runner, asked) = asking_before_a_pull_request(&rig);

    assert!(matches!(asked, StepReport::Asked { .. }), "{asked:?}");
    assert_eq!(
        rig.github.comments(),
        [(7, format!("@octocat {QUESTION}\n\n{HOW_TO_ANSWER}"))]
    );
    assert_eq!(rig.forge.comments(), []);
}

#[test]
fn the_maintainer_defaults_to_the_owner_of_a_user_s_repo_and_to_nobody_for_an_organization_s() {
    let user = Rig::new("shep");
    user.with_app();
    user.forge.set_owner_is_user(true);
    asking_before_a_pull_request(&user);
    let org = Rig::new("koji");
    org.with_app();
    asking_before_a_pull_request(&org);

    assert_eq!(
        user.github.comments(),
        [(7, format!("@shep-pm {QUESTION}\n\n{HOW_TO_ANSWER}"))]
    );
    assert_eq!(
        org.github.comments(),
        [(7, format!("{QUESTION}\n\n{HOW_TO_ANSWER}"))]
    );
}

#[test]
fn the_repo_s_owner_is_asked_about_once_however_many_rulings_are_posted() {
    let (rig, runner, _) = Rig::parked_set("shep", |rig| {
        rig.with_app();
        rig.forge.set_owner_is_user(true);
    });
    rig.ask(&runner, "rule", Some("1 no not yet"));
    rig.claude.script([Scripted::Say(ASKS)]);
    step(&runner).unwrap(); // the fix turn asks a question: a second ruling

    let mentions = (rig.github.comments().into_iter())
        .filter(|(_, body)| body.starts_with("@shep-pm "))
        .count();
    assert_eq!(mentions, 2, "the merge ruling and the question");
    assert_eq!(rig.forge.owner_reads(), 1);
}

#[test]
fn with_no_app_a_ruling_posts_as_it_always_did() {
    let rig = Rig::new("shep");
    let (_runner, asked) = asking_on_a_pull_request(&rig);
    assert!(matches!(asked, StepReport::Asked { .. }), "{asked:?}");

    assert_eq!(
        rig.forge.comments(),
        [(71, format!("{QUESTION}\n\nWaiting on the maintainer."))]
    );
    assert_eq!(rig.github.writes(), []);
    let (rig, _runner, _) = Rig::parked("koji");
    assert_eq!(rig.forge.comments(), [], "a merge ruling has no comment");
    assert_eq!(rig.github.writes(), []);
}

#[test]
fn an_app_that_is_registered_for_another_owner_leaves_a_ruling_as_it_was() {
    let rig = Rig::new("shep");
    let elsewhere = crate::test::FakeGithub::new(rig.clock.clone(), "someone-else");
    elsewhere.registered(&rig.paths().kelpie_home, "someone-else/koji");

    asking_on_a_pull_request(&rig);

    assert_eq!(rig.forge.comments().len(), 1);
    assert_eq!(rig.github.writes(), []);
}

#[test]
fn a_ruling_whose_post_fails_is_still_raised_and_says_why() {
    let rig = Rig::new("shep");
    rig.with_app();
    // GitHub takes no comment: the ruling is raised and saved, the step says
    // why the comment failed, and nothing is posted as the maintainer instead.
    rig.github.refuse_comments(ApiError::Refused(403));

    let (runner, asked) = asking_before_a_pull_request(&rig);

    let StepReport::Asked {
        id, comment_failed, ..
    } = asked
    else {
        panic!("not a question: {asked:?}");
    };
    let why = "the GitHub App could not post: GitHub answered HTTP 403";
    assert_eq!(comment_failed.as_deref(), Some(why));
    assert_eq!(rig.forge.comments(), []);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"][0]["id"], id);
}

#[test]
fn an_automatic_merge_s_notice_is_a_comment_on_its_pull_request_as_well_as_the_webhook_post() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    drop(runner);
    rig.with_app();
    rig.merge_auto();
    let runner = rig.open().unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    rig.verdict(&runner);
    rig.clock.advance(CHECKS_SETTLE);

    let merged = step(&runner).unwrap();

    assert!(
        matches!(merged, Some(StepReport::Finished { merged: true, .. })),
        "{merged:?}"
    );
    let notice = format!(
        "Pull request #71 for issue #7 merged into main at {}, every gate passed. \
         Nothing to answer.",
        &head[..7]
    );
    assert_eq!(rig.github.comments(), [(71, notice)]);
    assert_eq!(rig.forge.comments(), []);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Noticed { .. })
    ));
    assert_eq!(
        rig.alerts.posts().len(),
        1,
        "the webhook still gets its notice"
    );
}

#[test]
fn each_local_round_is_posted_on_the_pull_request_as_it_ends() {
    let rig = Rig::new("shep");
    rig.with_app();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.reviewer.script([ScriptedRound::Findings(vec![
        Finding {
            severity: Severity::High,
            file: "src/lib.rs".into(),
            line: 3,
            what: "unused variable".into(),
            why: "dead code".into(),
        },
        Finding {
            severity: Severity::Low,
            file: "README.md".into(),
            line: 0,
            what: "typo".into(),
            why: "reads badly".into(),
        },
    ])]);

    step(&runner).unwrap(); // round 1's qwen call

    let short = &head[..7];
    let found = format!(
        "qwen read this pull request at {short} in review round 1 and found 2 things:\n\n\
         - HIGH `src/lib.rs:3`: unused variable. dead code\n\
         - LOW `README.md`: typo. reads badly"
    );
    assert_eq!(rig.github.comments(), [(71, found)]);

    step(&runner).unwrap(); // the findings go to the worker
    rig.claude.script([
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the fix turn pushes
    step(&runner).unwrap(); // the head moved, so round 1 ends
    step(&runner).unwrap(); // round 2, the Claude reviewer, reads the fix
    let comments = rig.github.comments();
    let [_, (thread, clean)] = comments.as_slice() else {
        panic!("one comment for each round: {comments:?}");
    };
    let head = rig.forge.head_of("kelpie/7").unwrap();
    let want = format!(
        "claude read this pull request at {} in review round 2 and found nothing.",
        &head[..7]
    );
    assert_eq!((*thread, clean.as_str()), (71, want.as_str()));
    assert_eq!(rig.forge.comments(), []);
}

#[test]
fn a_round_whose_post_fails_still_ends_and_the_failure_is_told_once() {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.github.refuse_comments(ApiError::Refused(403));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)

    let round = step(&runner).unwrap(); // round 1's qwen call: clean

    assert!(
        matches!(round, Some(StepReport::ReviewFindingsSent { held: 0, .. })),
        "{round:?}"
    );
    let notes = runner.lock().unwrap().take_notes();
    let told: Vec<_> = (notes.iter())
        .filter(|n| n.contains("cannot post qwen's round 1 on #71"))
        .collect();
    assert_eq!(told.len(), 1, "{notes:?}");
    assert!(told[0].contains("HTTP 403"), "{told:?}");
}

#[test]
fn with_no_app_a_round_posts_nothing() {
    let (rig, _runner, _) = Rig::with_pull_request("shep");

    assert_eq!(rig.github.writes(), []);
    assert_eq!(rig.forge.comments(), []);
}

#[test]
fn the_label_that_holds_an_issue_is_put_on_through_the_app() {
    let rig = Rig::new("shep");
    rig.with_app();
    let runner = rig.open().unwrap();

    rig.ask(&runner, "add", Some("7"));
    step(&runner).unwrap();

    let held = (rig.github.writes().into_iter())
        .find(|(_, call)| call.path == "/repos/shep-pm/shep/issues/7/labels")
        .expect("the issue was labelled through the App");
    assert_eq!(held.0, "ghs_test1");
    assert_eq!(
        held.1.body.as_deref(),
        Some(r#"{"labels":["in-progress"]}"#)
    );
}

#[test]
fn an_issue_closed_with_no_change_is_noticed_on_the_issue() {
    let rig = Rig::new("shep");
    rig.with_app();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say(ASKS)]);
    step(&runner).unwrap(); // asks, parked with no pull request
    rig.forge.close_issue(7);
    rig.clock.advance(3600);

    step(&runner).unwrap(); // reads the parked issue and finds it closed

    let comments = rig.github.comments();
    let (thread, notice) = comments.last().unwrap();
    assert_eq!(*thread, 7);
    assert!(
        notice.starts_with("Issue #7 was closed with no change"),
        "{notice}"
    );
}
