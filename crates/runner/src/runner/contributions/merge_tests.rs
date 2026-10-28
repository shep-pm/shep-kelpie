//! The merge ruling of a contribution, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::{Value, json};

use super::pass_tests::{listing, read_through, write};
use super::post::{POST_TRIES, REACT};
use super::release_tests::{ASKS, TAKEN, held, later, waiting};
use super::ruling_tests::rule;
use super::security_tests::{commit_on, contribute, contribution};
use super::tests::{poll, poll_or_adopt};
use crate::github::ApiError;
use crate::ports::{AgentError, CLEAN, Checks, Finding, PullRequestState, Role, Severity};
use crate::runner::coderabbit::tests::now;
use crate::runner::review_bot::contribution_turns::{contributed, until_summoned};
use crate::runner::words::NO_NOTE;
use crate::runner::{Runner, StepReport, step};
use crate::settings::Harness;
use crate::test::{Rig, Scripted, ScriptedRound, ticket};

const MAINTAINER: &str = "Boss";
const SAID: &str = "The local reviewer found nothing. The maintainer has it now.";
const THANKS: &str = "Thank you for the pull request. It is merged now.";
const DECLINE: &str = "The maintainer would like a test first, please add one.";

// The clean pass's note as it is posted: the representative's words, then the
// line that tells the maintainer how to answer
fn passed() -> String {
    format!("{SAID}\n\n{REACT}")
}

// Pull request 30, whose head passed the security review, whose CI is green
// and whose pass over it has ended, with the ruling not yet looked at
fn reviewed() -> (Rig, Mutex<Runner>, String) {
    let (rig, runner) = listing(&["mine"]);
    rig.claude.script([Scripted::Text(CLEAN)]);
    // The pass of a clean head says so to the contributor.
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(SAID)]);
    let head = contribute(&rig, 30, |h| write(h, "one.txt", "one\n"));
    read_through(&rig, &runner, &head);
    (rig, runner, head)
}

// `reviewed`, with the merge ruling raised and its alert sent
fn ruled_on() -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = reviewed();
    poll(&rig, &runner);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(30) })
    );
    (rig, runner, head)
}

// `ruled_on`, where the pull request's branch is on a fork an organization
// owns, though its author allows edits
fn ruled_on_an_organizations_fork() -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = reviewed();
    rig.forge
        .set_fork(30, "https://example.test/fork.git", true);
    rig.forge.set_fork_owned_by_organization(30);
    poll(&rig, &runner);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(30) })
    );
    (rig, runner, head)
}

fn rulings(rig: &Rig, runner: &Mutex<Runner>) -> Vec<Value> {
    let status = rig.ask(runner, "status", None);
    status["rulings"].as_array().cloned().unwrap_or_default()
}

// The id of the note the representative posted on the clean pass
fn note_of(rig: &Rig, runner: &Mutex<Runner>) -> u64 {
    let held = contribution(rig, runner);
    held["pass"]["note"].as_u64().expect("the pass has a note")
}

fn found() -> Finding {
    Finding::new(
        Severity::Medium,
        "one.txt",
        1,
        "leftover placeholder",
        "it ships to users",
    )
}

#[test]
fn the_merge_ruling_is_raised_when_the_pass_has_ended_and_ci_is_green_on_the_head() {
    let (rig, runner) = listing(&["mine"]);
    rig.claude.script([Scripted::Text(CLEAN)]);
    rig.reviewer
        .script([ScriptedRound::Findings(vec![found()])]);
    let head = contribute(&rig, 30, |h| write(h, "one.txt", "one\n"));
    read_through(&rig, &runner, &head);
    assert_eq!(
        rulings(&rig, &runner),
        Vec::<Value>::new(),
        "not before the poll"
    );

    // The poll after the pass raises it, and the step after that alerts.
    poll(&rig, &runner);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(30) })
    );

    let waiting = rulings(&rig, &runner);
    assert_eq!(waiting.len(), 1);
    let ruling = &waiting[0];
    assert_eq!(ruling["id"], "30");
    assert_eq!(ruling["issue"], Value::Null);
    assert_eq!(ruling["pull_request"], 30);
    assert_eq!(ruling["kind"]["kind"], "contribution");
    assert_eq!(ruling["kind"]["reason"], "merge");
    assert_eq!(ruling["kind"]["head"], head);
    let question = ruling["question"].as_str().unwrap();
    for named in [
        "leftover placeholder",
        "one.txt:1",
        "shep kelpie rule 30 yes",
    ] {
        assert!(question.contains(named), "{named}: {question}");
    }

    // Once per head: later polls raise no other, alert nothing more, merge nothing.
    poll(&rig, &runner);
    poll(&rig, &runner);
    assert_eq!(rulings(&rig, &runner).len(), 1);
    assert_eq!(rig.alerts.posts().len(), 1);
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn it_is_raised_under_git_merging_auto_too_and_sends_no_notice() {
    let (rig, runner) = listing(&["mine"]);
    drop(runner);
    rig.merge_auto();
    let runner = rig.open().unwrap();
    rig.claude.script([Scripted::Text(CLEAN)]);
    let head = contribute(&rig, 30, |h| write(h, "one.txt", "one\n"));
    read_through(&rig, &runner, &head);

    poll(&rig, &runner);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(30) })
    );
    poll(&rig, &runner);

    let waiting = rulings(&rig, &runner);
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0]["kind"]["reason"], "merge");
    assert_eq!(
        rig.forge.merges(),
        [],
        "nothing merges until the maintainer says"
    );
    // The one alert is the ruling's: no notice of a merge.
    let alerted = rig.alerts.posts();
    assert_eq!(alerted.len(), 1, "{alerted:?}");
    assert!(
        alerted[0].1.title.contains("ruling #30"),
        "{:?}",
        alerted[0].1
    );
}

#[test]
fn a_merge_ruling_on_a_fork_an_organization_owns_offers_no_rework() {
    let (rig, runner, head) = ruled_on_an_organizations_fork();

    let waiting = rulings(&rig, &runner);
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0]["kind"]["reason"], "merge");
    assert_eq!(waiting[0]["kind"]["head"], head);
    assert_eq!(waiting[0]["kind"]["rework_off"], true);
    let question = waiting[0]["question"].as_str().unwrap();
    for named in [
        "shep kelpie rule 30 yes",
        "shep kelpie rule 30 no <note>",
        "an organization owns it",
        "merge `main` into their branch",
        "push the change themselves",
    ] {
        assert!(question.contains(named), "{named}: {question}");
    }
    assert!(!question.contains("rework <note>"), "{question}");
    assert_eq!(comments(&rig), [passed()], "nothing more is posted");
}

#[test]
fn a_rework_answer_to_a_merge_ruling_on_a_fork_kelpie_cannot_push_to_is_refused() {
    let (rig, runner, _) = ruled_on_an_organizations_fork();

    let ruled = rule(&rig, &runner, "30 rework Add the test.");
    poll_or_adopt(&rig, &runner);
    poll_or_adopt(&rig, &runner);

    let error = ruled["error"].as_str().unwrap_or_default();
    assert!(error.contains("cannot be pushed to"), "{ruled}");
    assert_eq!(rulings(&rig, &runner).len(), 1, "the ruling stays");
    assert_eq!(rig.forge.merges(), [], "nothing was merged");
    let record = contribution(&rig, &runner);
    assert_eq!(record.get("released"), None, "{record}");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"], Value::Null, "nothing was queued");
    assert_eq!(rig.github.issues_created(), 0);
}

#[test]
fn a_yes_merges_the_head_it_named() {
    let (rig, runner, head) = ruled_on();

    rule(&rig, &runner, "30 yes");
    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), [(30, head)]);
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());

    // It merges once, however many polls follow.
    poll(&rig, &runner);
    assert_eq!(rig.forge.merges().len(), 1);
}

#[test]
fn a_maintainers_thumbs_up_on_the_note_merges_the_head_it_names() {
    let (rig, runner, head) = ruled_on();
    let note = note_of(&rig, &runner);

    rig.forge.threads.thumbs(note, MAINTAINER, true);
    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), [(30, head)]);
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());

    // It merges once, however many polls follow.
    poll(&rig, &runner);
    assert_eq!(rig.forge.merges().len(), 1);
}

#[test]
fn a_thumbs_up_from_the_contributor_or_a_stranger_answers_nothing() {
    let (rig, runner, _) = ruled_on();
    let note = note_of(&rig, &runner);

    rig.forge.threads.thumbs(note, "newcomer", true);
    rig.forge.threads.thumbs(note, "a-stranger", true);
    rig.forge.threads.thumbs(note, "a-stranger", false);
    poll(&rig, &runner);
    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), []);
    assert_eq!(rulings(&rig, &runner).len(), 1, "still waiting");
}

#[test]
fn the_maintainers_login_is_matched_ignoring_case() {
    let (rig, runner, head) = ruled_on();
    let note = note_of(&rig, &runner);

    rig.forge.threads.thumbs(note, "BOSS", true);
    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), [(30, head)]);
}

#[test]
fn a_maintainers_thumbs_down_answers_no_and_posts_nothing() {
    let (rig, runner, head) = ruled_on();
    let note = note_of(&rig, &runner);

    rig.forge.threads.thumbs(note, MAINTAINER, false);
    poll(&rig, &runner);
    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), []);
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
    assert_eq!(comments(&rig), [passed()], "nothing is posted for a no");
    assert_eq!(prompts(&rig).len(), 1, "no note was worded");
    assert_eq!(contribution(&rig, &runner)["head"], head);
}

const DECISION: &str =
    r#"{"kind": "decision", "question": "Will you merge this once it is rebased?"}"#;

// `reviewed`, where the contributor then asks for a decision: the question
// ruling holds the pull request's id, and the clean pass's note is up
fn asked_after_the_pass() -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = reviewed();
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(DECISION)]);
    rig.writes_on(30, "newcomer", "Will you merge this?");
    for _ in 0..3 {
        poll(&rig, &runner);
    }
    let waiting = rulings(&rig, &runner);
    assert_eq!(waiting.len(), 1, "{waiting:?}");
    assert_eq!(waiting[0]["kind"]["reason"], "question");
    assert!(!contribution(&rig, &runner)["deciding"].is_null());
    (rig, runner, head)
}

#[test]
fn a_maintainers_thumbs_up_on_the_note_merges_the_head_while_a_question_waits() {
    let (rig, runner, head) = asked_after_the_pass();
    let note = note_of(&rig, &runner);

    rig.forge.threads.thumbs(note, MAINTAINER, true);
    poll(&rig, &runner);
    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), [(30, head)]);
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
    assert!(contribution(&rig, &runner)["deciding"].is_null());
}

#[test]
fn a_maintainers_thumbs_down_on_the_note_answers_no_while_a_question_waits() {
    let (rig, runner, _) = asked_after_the_pass();
    let note = note_of(&rig, &runner);
    let posted = comments(&rig).len();

    rig.forge.threads.thumbs(note, MAINTAINER, false);
    poll(&rig, &runner);
    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), []);
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
    assert!(contribution(&rig, &runner)["deciding"].is_null());
    assert_eq!(comments(&rig).len(), posted, "nothing is posted for a no");
}

#[test]
fn a_thumbs_up_from_the_contributor_while_a_question_waits_answers_nothing() {
    let (rig, runner, _) = asked_after_the_pass();
    let note = note_of(&rig, &runner);

    rig.forge.threads.thumbs(note, "newcomer", true);
    poll(&rig, &runner);
    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), []);
    let waiting = rulings(&rig, &runner);
    assert_eq!(waiting.len(), 1, "{waiting:?}");
    assert_eq!(waiting[0]["kind"]["reason"], "question");
}

#[test]
fn a_thumbs_down_wins_over_a_thumbs_up() {
    let (rig, runner, _) = ruled_on();
    let note = note_of(&rig, &runner);

    rig.forge.threads.thumbs(note, MAINTAINER, true);
    rig.forge.threads.thumbs(note, MAINTAINER, false);
    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), []);
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
}

#[test]
fn a_reaction_on_the_note_of_an_older_head_is_ignored() {
    let (rig, runner, _) = ruled_on();
    let old = note_of(&rig, &runner);
    rig.claude.script([Scripted::Text(CLEAN)]);
    let moved = commit_on(&rig, 30, "topic/30", |h| write(h, "two.txt", "two\n"));
    poll(&rig, &runner);
    read_through(&rig, &runner, &moved);

    rig.forge.threads.thumbs(old, MAINTAINER, true);
    poll(&rig, &runner);
    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), [], "the old head is never merged");
    let waiting = rulings(&rig, &runner);
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0]["kind"]["head"], moved);
}

#[test]
fn a_reaction_after_the_terminal_answered_changes_nothing() {
    let (rig, runner, _) = ruled_on();
    let note = note_of(&rig, &runner);

    rule(&rig, &runner, &format!("30 no {NO_NOTE}"));
    rig.forge.threads.thumbs(note, MAINTAINER, true);
    poll(&rig, &runner);
    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), []);
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
}

#[test]
fn a_reaction_that_cannot_be_read_is_told_once_and_read_again() {
    let (rig, runner, head) = ruled_on();
    let note = note_of(&rig, &runner);
    rig.forge.threads.thumbs(note, MAINTAINER, true);
    rig.forge.threads.cannot_read_marks_of(note);
    runner.lock().unwrap().take_notes();

    poll(&rig, &runner);
    poll(&rig, &runner);

    let told = runner.lock().unwrap().take_notes();
    let told: Vec<_> = told.iter().filter(|n| n.contains("reaction")).collect();
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(rig.forge.merges(), []);

    // The comment can be read at a later poll, and the reaction answers.
    rig.forge.threads.can_read_marks_of(note);
    poll(&rig, &runner);
    assert_eq!(rig.forge.merges(), [(30, head)]);
}

#[test]
fn a_reaction_that_already_answered_does_not_answer_the_ruling_raised_again() {
    let (rig, runner, _) = ruled_on();
    let note = note_of(&rig, &runner);
    rig.forge.threads.thumbs(note, MAINTAINER, true);
    rig.forge.set_merges_down(true);

    // The merge is refused until it is given up and the maintainer is asked again.
    for _ in 0..POST_TRIES + 3 {
        poll(&rig, &runner);
    }
    rig.forge.set_merges_down(false);
    poll(&rig, &runner);
    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), [], "the old reaction answers nothing");
    assert_eq!(
        rulings(&rig, &runner).len(),
        1,
        "waiting for the maintainer"
    );
}

#[test]
fn a_pass_a_bot_answered_has_no_note_and_stays_waiting() {
    let (rig, runner, head) = contributed(&["cubic", "mine"]);
    until_summoned(&rig, &runner);
    rig.forge
        .coderabbit
        .cubic_review(30, &head, now(&rig) + 30, &[]);
    let held = read_through(&rig, &runner, &head);
    assert_eq!(held["pass"]["answered"], "cubic");
    assert_eq!(held["pass"]["note"], Value::Null);
    poll(&rig, &runner);

    for comment in 0..200 {
        rig.forge.threads.thumbs(comment, MAINTAINER, true);
    }
    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), []);
    assert_eq!(rulings(&rig, &runner).len(), 1, "still waiting");
}

fn down() -> AgentError {
    AgentError::Failed(Harness::ClaudeCode, "overloaded".into())
}

// What the App posted on the pull requests
fn comments(rig: &Rig) -> Vec<String> {
    (rig.github.comments().into_iter())
        .map(|(_, body)| body)
        .collect()
}

// The prompts of the representative's calls
fn prompts(rig: &Rig) -> Vec<String> {
    let all = rig.claude.all_seen();
    (all.into_iter())
        .filter(|s| s.call.role == Role::Representative)
        .map(|s| s.call.prompt)
        .collect()
}

#[test]
fn a_merge_posts_thanks_in_the_representatives_words() {
    let (rig, runner, _) = ruled_on();
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(THANKS)]);

    rule(&rig, &runner, "30 yes");
    poll(&rig, &runner);
    poll(&rig, &runner);

    assert_eq!(
        comments(&rig),
        [passed(), THANKS.to_owned()],
        "one thanks, however many polls"
    );
    let asked = prompts(&rig);
    assert_eq!(asked.len(), 2);
    assert!(asked[1].contains("merged"), "{}", asked[1]);
    assert!(asked[1].contains("@newcomer"), "{}", asked[1]);
}

#[test]
fn a_failed_thanks_call_does_not_hold_the_merge() {
    let (rig, runner, head) = ruled_on();
    rig.claude
        .script_for(Role::Representative, [Scripted::Fail(down())]);

    rule(&rig, &runner, "30 yes");
    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), [(30, head)], "merged all the same");
    assert_eq!(comments(&rig), [passed()], "no thanks is posted");
}

#[test]
fn a_head_that_moved_withdraws_the_ruling_and_the_next_pass_raises_another() {
    let (rig, runner, head) = ruled_on();
    rig.claude.script([Scripted::Text(CLEAN)]);
    let moved = commit_on(&rig, 30, "topic/30", |h| write(h, "two.txt", "two\n"));

    poll(&rig, &runner);
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new(), "withdrawn");
    rule(&rig, &runner, "30 yes");
    assert_eq!(rig.forge.merges(), [], "the old head is never merged");

    read_through(&rig, &runner, &moved);
    poll(&rig, &runner);
    let waiting = rulings(&rig, &runner);
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0]["kind"]["head"], moved);
    assert_ne!(moved, head);
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn a_no_with_a_note_posts_the_note_in_the_representatives_words() {
    let (rig, runner, head) = ruled_on();
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(DECLINE)]);

    rule(&rig, &runner, "30 no Ask for a test before this lands.");
    poll(&rig, &runner);
    poll(&rig, &runner);

    assert_eq!(
        comments(&rig),
        [passed(), DECLINE.to_owned()],
        "one post, however many polls"
    );
    let asked = prompts(&rig);
    assert_eq!(asked.len(), 2);
    assert!(
        asked[1].contains("Ask for a test before this lands."),
        "{}",
        asked[1]
    );
    assert_eq!(rig.forge.merges(), []);
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
    // The head is not asked about again, but a new one is.
    assert_eq!(rig.alerts.posts().len(), 1);
    let record = contribution(&rig, &runner);
    assert_eq!(record["head"], head);
    assert_eq!(record["ruled"]["verdict"]["no"], serde_json::json!({}));
}

#[test]
fn a_no_with_no_note_posts_nothing() {
    let (rig, runner, _) = ruled_on();
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(DECLINE)]);

    // A reaction or a tick gives the placeholder note.
    rule(&rig, &runner, &format!("30 no {NO_NOTE}"));
    poll(&rig, &runner);
    poll(&rig, &runner);

    assert_eq!(comments(&rig), [passed()]);
    assert_eq!(prompts(&rig).len(), 1, "only the pass spoke");
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn a_no_starts_no_fix_turn() {
    let (rig, runner, _) = ruled_on();
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(DECLINE)]);

    rule(&rig, &runner, "30 no Ask for a test before this lands.");
    for _ in 0..4 {
        poll(&rig, &runner);
    }

    let calls = rig.claude.all_seen();
    let roles: Vec<Role> = calls.iter().map(|s| s.call.role).collect();
    assert!(
        roles.iter().all(|r| *r != Role::Worker),
        "no worker turn: {roles:?}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_items"], serde_json::json!([]), "{status}");
    assert_eq!(status["work_item"], Value::Null);
}

#[test]
fn the_project_manager_is_not_asked_to_answer_the_merge_ruling() {
    let (rig, runner, _) = ruled_on();
    for _ in 0..3 {
        poll(&rig, &runner);
    }

    let calls = rig.claude.all_seen();
    let asked: Vec<Role> = (calls.iter().map(|s| s.call.role))
        .filter(|r| *r == Role::Pm)
        .collect();
    assert_eq!(asked, [], "the project manager was not called");
    let waiting = rulings(&rig, &runner);
    assert_eq!(waiting.len(), 1, "it waits for the maintainer");
}

#[test]
fn a_closed_pull_request_drops_the_record_and_the_ruling_with_no_post() {
    let (rig, runner, _) = ruled_on();
    let said = prompts(&rig).len();

    rig.forge.set_state(30, PullRequestState::Closed);
    poll(&rig, &runner);

    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status.get("contributions"), None, "{status}");
    assert_eq!(
        comments(&rig),
        [passed()],
        "nothing was posted for the close"
    );
    assert_eq!(
        prompts(&rig).len(),
        said,
        "the representative was not asked"
    );
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn the_maintainers_comment_does_not_reset_the_contributors_clock() {
    let (rig, runner) = listing(&["mine"]);
    rig.claude.script([Scripted::Text(CLEAN)]);
    rig.reviewer
        .script([ScriptedRound::Findings(vec![found()])]);
    let head = contribute(&rig, 30, |h| write(h, "one.txt", "one\n"));
    read_through(&rig, &runner, &head);
    poll(&rig, &runner);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(30) })
    );
    let started = contribution(&rig, &runner)["waiting_since"].clone();
    assert_ne!(started, Value::Null, "the findings wait on the contributor");
    assert_eq!(rulings(&rig, &runner).len(), 1, "the merge ruling waits");

    rig.clock.advance(Rig::DAY);
    rig.writes_on(30, "Boss", "Please add a test");
    poll(&rig, &runner);

    let held = contribution(&rig, &runner);
    assert_eq!(held["waiting_since"], started);
    assert_eq!(held["context"].as_array().map(Vec::len), Some(1), "{held}");
    assert_eq!(held.get("heard"), None, "not the contributor's words");
    assert_eq!(rulings(&rig, &runner).len(), 1, "the ruling still waits");
}

// `rework` takes the pull request over at once: no release, no merge, and a
// worker changes the pull request before the project's own reviewers read it.
#[test]
fn a_rework_answer_to_the_merge_ruling_adopts_at_once_and_runs_the_full_pass() {
    let (rig, runner, head) = ruled_on();
    // The worker's turn, then its Claude reviewer. The project's qwen round is
    // a review bot and needs no script.
    rig.claude
        .script([Scripted::Push("two.txt", "two\n"), Scripted::Text(CLEAN)]);

    rule(
        &rig,
        &runner,
        "30 rework Add the test the review asked for.",
    );
    poll_or_adopt(&rig, &runner);
    poll_or_adopt(&rig, &runner);

    let status = rig.ask(&runner, "status", None);
    assert_eq!(rig.forge.merges(), [], "nothing was merged");
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new(), "answered");
    assert_eq!(status["work_item"]["pull_request"], 30);
    assert_eq!(status["work_item"]["adopted"], Value::Bool(true));
    assert_eq!(rig.github.issues_created(), 1, "an issue for the work");

    let told = runner.lock().unwrap().take_notes();
    assert!(
        told.iter()
            .any(|n| n.contains("Add the test the review asked for.")),
        "{told:?}"
    );

    // A worker went before any reviewer read it, and its prompt named the file
    // with the pull request, its issue and the maintainer's note.
    let [worker] = rig.claude.calls().try_into().unwrap();
    let issue = status["work_item"]["issue"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let named = rig
        .paths()
        .build(ticket(issue))
        .join("adopted-pull-request.md");
    assert!(
        worker.prompt.contains(&named.display().to_string()),
        "{}",
        worker.prompt
    );
    let text = std::fs::read_to_string(named).unwrap();
    assert!(
        text.contains(
            "# The maintainer's note on pull request #30\n\nAdd the test the review asked for."
        ),
        "{text}"
    );

    // The project's own reviewers over what the worker pushed: its qwen round,
    // then its Claude reviewer, where `contributions.reviewers` listed only
    // "mine".
    for _ in 0..10 {
        step(&runner).unwrap();
    }

    let said = comments(&rig);
    assert!(
        said.iter()
            .any(|c| c.contains("qwen read this pull request")),
        "{said:?}"
    );
    assert!(
        said.iter()
            .any(|c| c.contains("claude read this pull request")),
        "{said:?}"
    );
    let read = rig.reviewer.seen();
    assert!(!read.is_empty(), "{read:?}");
    assert_ne!(
        read[0].base, head,
        "the reviewers read the head the worker pushed"
    );
}

// The card's `rework` option and a tick give no note of their own: the worker
// is sent to the pull request, and no placeholder is written for it.
#[test]
fn a_rework_answer_with_the_note_a_tick_gives_sends_the_worker_none() {
    let (rig, runner, _) = ruled_on();
    rig.claude.script([Scripted::Text(CLEAN)]);

    rule(&rig, &runner, &format!("30 rework {NO_NOTE}"));
    poll_or_adopt(&rig, &runner);

    let record = contribution(&rig, &runner);
    assert_eq!(record.get("rework"), None, "{record}");
    let told = runner.lock().unwrap().take_notes();
    assert!(
        told.iter()
            .any(|n| n.ends_with("kelpie takes the pull request over")),
        "{told:?}"
    );

    let issue = rig.ask(&runner, "status", None)["work_item"]["issue"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let text = std::fs::read_to_string(
        rig.paths()
            .build(ticket(issue))
            .join("adopted-pull-request.md"),
    )
    .unwrap();
    assert!(
        !text.contains("The maintainer's note"),
        "no note for the worker: {text}"
    );

    // The issue kelpie filed says who handed the pull request over, and does
    // not claim its contributor released it.
    let filed = filed(&rig);
    assert_eq!(filed.len(), 1, "{filed:?}");
    let body = filed[0]["body"].as_str().unwrap();
    assert!(
        body.contains("the maintainer's ruling had kelpie take over"),
        "{body}"
    );
}

// The issues the App filed, oldest first, as the forge was asked to open them
fn filed(rig: &Rig) -> Vec<Value> {
    (rig.github.writes().into_iter())
        .filter(|(_, call)| call.path.ends_with("/issues"))
        .filter_map(|(_, call)| {
            let text: Value = serde_json::from_str(call.body.as_deref()?).ok()?;
            Some(text)
        })
        .collect()
}

// A contributor who pushes after the maintainer answered `rework` does not take
// the note away: it sits on the record, and the worker still reads it. Nothing
// is asked about merging a pull request kelpie has taken over.
#[test]
fn a_new_head_after_a_rework_answer_keeps_the_maintainers_note_for_the_worker() {
    let (rig, runner, _) = ruled_on();
    rig.claude
        .script([Scripted::Text(CLEAN), Scripted::Text(CLEAN)]);
    // The issue is filed and its body names it, so the pull request is queued.
    rig.forge.set_pull_request_edit_down(true);

    rule(
        &rig,
        &runner,
        "30 rework Add the test the review asked for.",
    );
    poll_or_adopt(&rig, &runner);

    let record = contribution(&rig, &runner);
    assert_eq!(record["released"]["how"], "rework", "{record}");
    assert_eq!(
        record["rework"], "Add the test the review asked for.",
        "{record}"
    );
    assert_eq!(rig.ask(&runner, "status", None)["adopted"], json!([]));

    // Its contributor pushes before its work item starts.
    let moved = commit_on(&rig, 30, "origin/main", |h| write(h, "two.txt", "two\n"));
    rig.forge.set_checks(&moved, Checks::Passed);
    rig.forge.set_pull_request_edit_down(false);

    for _ in 0..4 {
        poll_or_adopt(&rig, &runner);
        if rig.ask(&runner, "status", None)["work_item"] != Value::Null {
            break;
        }
    }

    let status = rig.ask(&runner, "status", None);
    let issue = status["work_item"]["issue"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert_eq!(status["work_item"]["pull_request"], 30);
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
    let text = std::fs::read_to_string(
        rig.paths()
            .build(ticket(issue))
            .join("adopted-pull-request.md"),
    )
    .unwrap();
    assert!(
        text.contains(
            "# The maintainer's note on pull request #30\n\nAdd the test the review asked for."
        ),
        "{text}"
    );
}

// A release that landed while the merge ruling still waits is the maintainer's
// `rework` to overrule. The record keeps the note and says the maintainer took
// the pull request over, which is what starts its work item at a worker's turn
// rather than a review pass of the contributor's own head.
#[test]
fn a_rework_answer_after_the_release_landed_takes_the_pull_request_over_with_the_note() {
    let (rig, runner) = waiting(&[ASKS, TAKEN]);
    // The release lands at `quiet_days`, and its issue cannot be filed, so
    // nothing is queued and the record stands with its merge ruling open.
    rig.github.refuse_issue_creates(ApiError::Refused(502));
    later(&rig, &runner, 3);
    later(&rig, &runner, 3);
    later(&rig, &runner, 1);

    let record = held(&rig, &runner);
    assert_eq!(record["released"]["how"], "silence", "{record}");
    assert_eq!(rig.ask(&runner, "status", None)["adopted"], json!([]));

    rig.claude
        .script([Scripted::Push("two.txt", "two\n"), Scripted::Text(CLEAN)]);
    rule(
        &rig,
        &runner,
        "30 rework Add the test the review asked for.",
    );
    rig.github.accept_issue_creates();
    later(&rig, &runner, 1);

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["pull_request"], 30);
    assert_eq!(status["work_item"]["adopted"], json!(true));
    assert_eq!(
        status["work_item"]["phase"]["state"], "implement",
        "a worker goes first: {status}"
    );
    assert_eq!(status["work_item"]["turn"]["state"], "due", "{status}");
    assert_eq!(
        held(&rig, &runner)["released"]["how"],
        "rework",
        "the maintainer's answer, not the contributor's silence"
    );
    assert_eq!(
        held(&rig, &runner)["rework"],
        "Add the test the review asked for.",
        "the note is kept for the worker"
    );

    // The worker's turn went before any reviewer read the pull request, and it
    // was sent the file with the note in it.
    for _ in 0..4 {
        step(&runner).unwrap();
        if !rig.claude.seen().is_empty() {
            break;
        }
    }
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["qwen"]["rounds"], 0,
        "no review round had gone: {status}"
    );
    let seen = rig.claude.seen();
    let [worker] = seen.try_into().unwrap();
    let issue = status["work_item"]["issue"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let named = rig
        .paths()
        .build(ticket(issue))
        .join("adopted-pull-request.md");
    assert!(
        worker.call.prompt.contains(&named.display().to_string()),
        "{}",
        worker.call.prompt
    );
    let text = std::fs::read_to_string(named).unwrap();
    assert!(
        text.contains(
            "# The maintainer's note on pull request #30\n\nAdd the test the review asked for."
        ),
        "{text}"
    );
}
