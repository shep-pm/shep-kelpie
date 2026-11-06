//! A ruling's comment once someone other than the maintainer, replying in its
//! thread, has settled it

use super::*;
use crate::github::ApiError;
use crate::runner::StepReport;

const NOTHING_WAITS: &str = "Nothing waits on the maintainer.";

// The App numbers its comments from 100, and the ruling's is the last it posted.
fn ruling_comment(rig: &Rig) -> u64 {
    99 + rig.github.comments().len() as u64
}

#[test]
fn a_ruling_answered_at_the_terminal_has_its_comment_edited_to_say_so() {
    let (rig, runner, _) = a_merge_ruling_on_71();

    rig.ask(&runner, "rule", Some("71 no not yet"));

    let [(comment, body)] = rig.github.edits().try_into().unwrap();
    assert_eq!(comment, ruling_comment(&rig));
    let first = format!(
        "**Settled.** Answered at the terminal with `shep kelpie rule`: no, not yet. {NOTHING_WAITS}"
    );
    assert_eq!(body.lines().next(), Some(first.as_str()));
    // The question stays.
    assert!(body.contains("### Merge this pull request at"), "{body}");
    // Nothing invites a reply.
    for gone in [
        "@octocat",
        "- [ ]",
        "👍",
        "Reply",
        "| Option |",
        "| Answer |",
    ] {
        assert!(!body.contains(gone), "{gone} is still in:\n{body}");
    }
}

#[test]
fn a_ruling_answered_in_its_own_thread_is_left_as_it_is() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    rig.writes_on(71, "octocat", "71 no not yet");
    rig.claude.script([Scripted::Say("Fixed.")]);

    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(pending(&rig, &runner), none());
    assert_eq!(rig.github.edits(), []);

    // A later reply is told where it was answered without saying "this thread",
    // which it may not be.
    rig.writes_on(71, "octocat", "71 yes");
    rig.next_look();
    step(&runner).unwrap();
    assert_eq!(
        replies_on_71(&rig).last().map(String::as_str),
        Some("@octocat Ruling #71 is already answered in its own thread: no, not yet.")
    );
}

#[test]
fn a_ruling_answered_by_a_reaction_on_its_comment_is_left_as_it_is() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    rig.forge
        .threads
        .thumbs(ruling_comment(&rig), "octocat", false);
    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);

    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(pending(&rig, &runner), none());
    assert_eq!(rig.github.edits(), []);
}

#[test]
fn an_edit_that_failed_is_tried_again_after_a_wait_that_doubles() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    let comment = ruling_comment(&rig);
    rig.github.fail_next(ApiError::Refused(502));

    rig.ask(&runner, "rule", Some("71 no not yet"));
    assert_eq!(rig.github.edits().len(), 1, "the first try");

    // No poll in the first 15 seconds sends it again.
    rig.clock.advance(14);
    step(&runner).unwrap();
    assert_eq!(rig.github.edits().len(), 1, "still waiting");

    rig.clock.advance(1);
    rig.github.fail_next(ApiError::Refused(502));
    step(&runner).unwrap();
    assert_eq!(
        rig.github.edits().len(),
        2,
        "the second try, which fails too"
    );

    // The next wait is twice as long.
    rig.clock.advance(29);
    step(&runner).unwrap();
    assert_eq!(rig.github.edits().len(), 2, "still waiting");

    rig.clock.advance(1);
    step(&runner).unwrap();
    let [(first, said), (second, _), (again, body)] = rig.github.edits().try_into().unwrap();
    assert_eq!((first, second, again), (comment, comment, comment));
    assert_eq!(said, body);
    assert!(
        body.starts_with("**Settled.** Answered at the terminal"),
        "{body}"
    );

    // Once it went through, it is not sent again.
    rig.next_look();
    step(&runner).unwrap();
    assert_eq!(rig.github.edits().len(), 3);
}

#[test]
fn a_terminal_answer_to_a_head_the_branch_moved_past_edits_the_old_comment_to_say_withdrawn() {
    let (rig, runner, _) = Rig::with_pull_request_set("shep", |rig| {
        rig.with_app();
        rig.maintainer("octocat");
    });
    rig.push_by_hand("kelpie/7", "first.txt");
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. })
     if id == ticket(71)));
    let comment = ruling_comment(&rig);
    rig.push_by_hand("kelpie/7", "second.txt");
    // The withdrawn ruling is raised again under the same number, later.
    rig.clock.advance(60);

    rig.ask(&runner, "rule", Some("71 yes"));

    assert_eq!(pending(&rig, &runner), [ticket(71)]);
    let [(edited, body)] = rig.github.edits().try_into().unwrap();
    assert_eq!(edited, comment);
    assert!(
        body.starts_with("**Withdrawn:** the branch moved"),
        "{body}"
    );
}

#[test]
fn an_answer_in_the_thread_to_a_head_the_branch_moved_past_leaves_the_comment_as_it_is() {
    let (rig, runner, _) = Rig::with_pull_request_set("shep", |rig| {
        rig.with_app();
        rig.maintainer("octocat");
    });
    rig.forge.threads.grant_write("octocat");
    rig.push_by_hand("kelpie/7", "first.txt");
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. })
     if id == ticket(71)));
    rig.push_by_hand("kelpie/7", "second.txt");
    rig.clock.advance(5);
    rig.writes_on(71, "octocat", "71 yes");

    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(pending(&rig, &runner), [ticket(71)]);
    assert_eq!(rig.github.edits(), []);
}
