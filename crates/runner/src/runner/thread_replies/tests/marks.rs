//! A ruling answered by a reaction on its comment or a box ticked in it

use super::*;
use crate::runner::words::NO_NOTE;

// The App numbers its comments from 100, and the ruling's is the last it posted.
fn ruling_comment(rig: &Rig) -> u64 {
    99 + rig.github.comments().len() as u64
}

// The ruling's comment with the box for `key` ticked
fn with_ticked(rig: &Rig, key: char) -> String {
    let comments = rig.github.comments();
    let (_, body) = comments.last().unwrap();
    let ticked = body.replace(&format!("- [ ] {key}"), &format!("- [x] {key}"));
    assert_ne!(&ticked, body, "no box for {key} in {body}");
    ticked
}

// A merge comment carrying boxes to tick, which kelpie still reads for a tick
const MERGE_WITH_BOXES: &str = "### Merge this pull request at 6b0606e into main?\n\n\
     | | Option | If you pick it |\n\
     |---|---|---|\n\
     | **A** | Merge | Merges it into main |\n\
     | **B** | Send back | Sends the worker back for a fix that goes to CI and back to you |\n\
     | **C** | Rework | Sends the worker back for a change the whole review reads again |\n\n\
     @octocat\n\
     - 👍 or 👎 answers, or tick one:\n\
     - [ ] A\n\
     - [ ] B\n\
     - [ ] C\n\
     - Or reply `71 no <note>` or `71 rework <note>`";

// `MERGE_WITH_BOXES` with the box for `key` ticked
fn with_merge_box_ticked(key: char) -> String {
    MERGE_WITH_BOXES.replace(&format!("- [ ] {key}"), &format!("- [x] {key}"))
}

// Whether the board's events, which the project manager reads at its next
// wake, say ruling 1 on issue 7 was answered
fn pm_is_told_of_the_answer(runner: &Mutex<Runner>) -> bool {
    let runner = runner.lock().unwrap();
    (runner.state.events.iter()).any(|event| event.what == "ruling 71 on #7 answered")
}

// Reads the thread, then lets a merge the answer started land.
fn read_and_merge(rig: &Rig, runner: &Mutex<Runner>) {
    rig.next_look();
    step(runner).unwrap(); // takes the answer, and marks the pull request ready
    rig.clock.advance(CHECKS_SETTLE);
    step(runner).unwrap(); // merges
}

#[test]
fn a_thumbs_up_from_the_maintainer_on_the_ruling_merges_the_pull_request() {
    let (rig, runner, head) = a_merge_ruling_on_71();
    let comment = ruling_comment(&rig);
    rig.forge.threads.thumbs(comment, "octocat", true);

    read_and_merge(&rig, &runner);

    assert_eq!(rig.forge.merges(), [(71, head)]);
    assert_eq!(pending(&rig, &runner), none());
    assert!(pm_is_told_of_the_answer(&runner));
}

#[test]
fn a_thumbs_down_from_the_maintainer_sends_the_worker_back_with_a_note_that_says_it_has_none() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    let comment = ruling_comment(&rig);
    rig.forge.threads.thumbs(comment, "octocat", false);
    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);

    rig.next_look();
    step(&runner).unwrap(); // takes the no; the worker's fix turn pushes

    assert_eq!(pending(&rig, &runner), none());
    assert_eq!(rig.forge.merges(), []);
    let calls = rig.claude.calls();
    assert_eq!(
        calls.last().unwrap().prompt,
        format!(
            "The maintainer answered no on pull request #71, with this note:\n\n{NO_NOTE}\n\n\
             Update the pull request's body so it matches the head you push: what changed, \
             the files touched, the test evidence, and the decisions you made.\n"
        )
    );
}

#[test]
fn a_thumbs_from_someone_else_changes_nothing_whatever_it_may_write() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    rig.forge.threads.grant_write("stranger");
    let comment = ruling_comment(&rig);
    rig.forge.threads.thumbs(comment, "stranger", true);

    read_and_merge(&rig, &runner);

    assert_eq!(rig.forge.merges(), []);
    assert_eq!(pending(&rig, &runner), [ticket(71)]);
}

#[test]
fn a_thumbs_up_and_a_thumbs_down_from_the_maintainer_answer_nothing() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    let comment = ruling_comment(&rig);
    rig.forge.threads.thumbs(comment, "octocat", true);
    rig.forge.threads.thumbs(comment, "octocat", false);

    read_and_merge(&rig, &runner);

    assert_eq!(rig.forge.merges(), []);
    assert_eq!(pending(&rig, &runner), [ticket(71)]);
}

#[test]
fn a_box_ticked_by_the_maintainer_gives_that_options_answer() {
    let (rig, runner, head) = a_merge_ruling_on_71();
    let comment = ruling_comment(&rig);
    rig.forge
        .threads
        .edits(comment, "octocat", &with_merge_box_ticked('A'));

    read_and_merge(&rig, &runner);

    assert_eq!(rig.forge.merges(), [(71, head)]);
    assert_eq!(pending(&rig, &runner), none());
    assert!(pm_is_told_of_the_answer(&runner));
}

#[test]
fn a_box_ticked_by_someone_else_is_ignored_even_with_write_access() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    rig.forge.threads.grant_write("stranger");
    let comment = ruling_comment(&rig);
    rig.forge
        .threads
        .edits(comment, "stranger", &with_merge_box_ticked('A'));

    read_and_merge(&rig, &runner);

    assert_eq!(rig.forge.merges(), []);
    assert_eq!(pending(&rig, &runner), [ticket(71)]);
}

#[test]
fn a_box_the_maintainer_ticked_after_someone_else_edited_is_ignored() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    let comment = ruling_comment(&rig);
    let ticked = with_merge_box_ticked('A');
    rig.forge.threads.edits(comment, "stranger", &ticked);
    rig.forge.threads.edits(comment, "octocat", &ticked);

    read_and_merge(&rig, &runner);

    assert_eq!(rig.forge.merges(), []);
    assert_eq!(pending(&rig, &runner), [ticket(71)]);
}

#[test]
fn two_boxes_ticked_answer_nothing() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    let comment = ruling_comment(&rig);
    let both = with_merge_box_ticked('A').replace("- [ ] B", "- [x] B");
    rig.forge.threads.edits(comment, "octocat", &both);

    read_and_merge(&rig, &runner);

    assert_eq!(rig.forge.merges(), []);
    assert_eq!(pending(&rig, &runner), [ticket(71)]);
}

#[test]
fn every_ruling_comment_that_cannot_be_read_is_told_not_only_the_last() {
    let (rig, runner, _) = a_merge_ruling_on_71();
    with_a_second_ruling_on(&runner, &rig, 71);
    let first = ruling_comment(&rig);
    let second = first + 50;
    {
        let mut runner = runner.lock().unwrap();
        let mut next = runner.state.clone();
        next.heard.get_mut(&ticket(8)).unwrap().ruling_comment = second;
        runner.save(next).unwrap();
    }
    rig.forge.threads.cannot_read_marks_of(first);
    rig.forge.threads.cannot_read_marks_of(second);

    rig.next_look();
    step(&runner).unwrap();

    let notes = runner.lock().unwrap().take_notes().join("\n");
    assert!(notes.contains(&format!("comment {first}")), "{notes}");
    assert!(notes.contains(&format!("comment {second}")), "{notes}");
    assert_eq!(saved_ids(&runner), [ticket(71), ticket(8)]);
}

#[test]
fn a_box_ticked_on_a_workers_question_is_the_answer_its_option_says() {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.maintainer("octocat");
    rig.forge.threads.grant_write("octocat");
    rig.forge.list_ready(7, false);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say(
        "<kelpie-question>\nWhich flag?\n- `--dry-run`\n- `--check`\n</kelpie-question>\n",
    )]);
    step(&runner).unwrap();
    rig.clock.advance(5);
    let comment = ruling_comment(&rig);
    rig.forge
        .threads
        .edits(comment, "octocat", &with_ticked(&rig, 'B'));
    rig.claude.script([Scripted::Say("Done.")]);

    rig.next_look();
    step(&runner).unwrap(); // takes the box; the worker's turn with the answer

    assert_eq!(pending(&rig, &runner), none());
    let calls = rig.claude.calls();
    assert_eq!(
        calls.last().unwrap().prompt,
        "The maintainer answered your question:\n\n--check\n"
    );
}
