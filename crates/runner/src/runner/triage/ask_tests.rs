//! The author's round, through the runner's stand-ins: the ask, the questions,
//! the answers, and how the wait ends

use std::sync::Mutex;

use serde_json::{Value, json};

use super::ask::REPLY_HINT;
use super::marker::MARKER;
use super::pick_tests::{MAINTAINER, issue, list, rig_with};
use super::research_tests::{
    APP, calls, crashed_with, facts, handed, moves, reply, restart, triage, under,
};
use crate::github::ApiError;
use crate::ports::{Clock, Role};
use crate::runner::{Runner, step};
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted};

// What the representative says for the triage comment that asks, and for the one that reads the answers
pub(super) const ASK_WORDS: &str =
    "Thanks for the report. Answer the questions below in one comment.";
pub(super) const A_WORDS: &str = "That settles most of it.";

// The labels a repo may hold for the pass, named bare
const REPO_LABELS: [&str; 8] = [
    "bug",
    "kind:fix",
    "enhancement",
    "P1",
    "P2",
    "needs-triage",
    "needs-info",
    "ready-for-agent",
];

// A rig whose App covers the repo, with `[triage]` set to `lines`, open issue
// 30 by `author`, whose representative words the triage comment ASK_WORDS.
// `found` is what its first triager reply gives.
pub(super) fn filed(lines: &str, author: &str, found: Value) -> (Rig, Mutex<Runner>) {
    filed_before(lines, author, found, |_| {})
}

// [`filed`], with the rig changed before the runner opens, for a project's own
// hook, which a running runner does not read again
pub(super) fn filed_before(
    lines: &str,
    author: &str,
    found: Value,
    before: impl FnOnce(&Rig),
) -> (Rig, Mutex<Runner>) {
    let rig = rig_with(lines);
    rig.forge.set_repo_labels(&REPO_LABELS);
    let mut crash = issue(&rig, 30, author, 5);
    crash.title = "Crash on start".to_owned();
    crash.body = "It crashes when I run it.".to_owned();
    list(&rig, crash);
    before(&rig);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(found)]);
    let runner = rig.open().unwrap();
    (rig, runner)
}

// What the first pass found: a type, a priority, and one question only the
// author can answer
pub(super) fn asked() -> Value {
    json!({
        "type": "bug",
        "priority": "P2",
        "open": [{ "question": "Which OS crashes?", "recommended": "Linux" }],
    })
}

// The same with `n` open questions
pub(super) fn asked_as(most: usize) -> Value {
    let open: Vec<Value> = (1..=most)
        .map(|n| json!({ "question": format!("Which one, {n}?"), "recommended": format!("the {n}th") }))
        .collect();
    json!({ "type": "bug", "priority": "P2", "open": open })
}

// What the triager found with one named open question
pub(super) fn asked_one(question: &str, recommended: &str) -> Value {
    json!({
        "type": "bug",
        "priority": "P2",
        "open": [{ "question": question, "recommended": recommended }],
    })
}

// What the triager found with `question` first and a plain question second
pub(super) fn asked_second(question: &str, recommended: &str) -> Value {
    json!({
        "type": "bug",
        "priority": "P2",
        "open": [
            { "question": question, "recommended": recommended },
            { "question": "Which one, 2?", "recommended": "the 2nd" },
        ],
    })
}

// What the second pass found: everything settled, with what is settled as its body
pub(super) fn settled() -> Value {
    json!({
        "type": "bug",
        "priority": "P2",
        "body": "## Settled\n\n- Linux, on start.\n",
    })
}

// The same, written as an agent-ready body, which is what makes a maintainer's
// own issue ready
pub(super) fn settled_ready() -> Value {
    json!({
        "type": "bug",
        "kind": "fix",
        "priority": "P2",
        "body": "Touches:\n- `src/start.rs`\n\n## What to build\n\n- Fix the crash on start, \
            on Linux.\n\n## Acceptance criteria\n\n- [ ] `runner::start::it_starts`\n",
    })
}

// The author's word on the issue, a little after the ask, as the forge lists it
pub(super) fn wrote(rig: &Rig, author: &str, body: &str) {
    rig.clock.advance(10);
    rig.forge.threads.write(30, author, body, rig.clock.now());
}

// The triage record `status` holds for issue 30
pub(super) fn record(rig: &Rig, runner: &Mutex<Runner>) -> Value {
    let held = triage(rig, runner);
    let issues = match held["issues"].as_array() {
        Some(a) => a.clone(),
        None => panic!("no records: {held}"),
    };
    issues
        .into_iter()
        .find(|t| t["issue"] == 30)
        .expect("issue 30")
}

// How many comments GitHub took on issue 30, the App's own included
fn taken(rig: &Rig) -> usize {
    rig.github.posted_on(30).len()
}

// What the App posted on issue 30, in order
pub(super) fn posted(rig: &Rig) -> Vec<String> {
    rig.github
        .comments()
        .into_iter()
        .map(|(_, body)| body)
        .collect()
}

// The questions as the first comment lists them, the grilling skill's shape
const ONE_QUESTION: &str = "❓ Q1 - Which OS crashes?\n\n➡️ Linux";

// The part of the comment inside its collapsed section
fn collapsed(comment: &str) -> &str {
    let (_, rest) = comment
        .split_once("<details>")
        .expect("a <details> section");
    rest.split_once("</details>").expect("a closed section").0
}

// How many of the representative's calls have run
fn worded(rig: &Rig) -> usize {
    calls(rig, Role::Representative).len()
}

// What the second triager call was asked, which holds the answers fenced
fn answers_prompt(rig: &Rig) -> String {
    let mut seen = calls(rig, Role::Triager);
    assert_eq!(seen.len(), 2, "the first pass and the second");
    seen.remove(1).call.prompt
}

#[test]
fn the_first_comment_asks_the_questions_and_holds_the_local_prompt_collapsed() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());

    assert_eq!(step(&runner).unwrap(), None);

    let [comment] = posted(&rig).try_into().expect("one comment");
    assert!(
        comment.starts_with(&format!(
            "{ASK_WORDS}\n\n{ONE_QUESTION}\n\n{REPLY_HINT}\n\n<details>"
        )),
        "{comment}"
    );
    assert!(comment.ends_with(MARKER), "{comment}");
    let prompt = collapsed(&comment);
    assert!(prompt.contains("/grill-with-docs Issue "), "{prompt}");
    assert!(prompt.contains("#30: Crash on start."), "{prompt}");
    assert!(prompt.contains("(recommended: Linux)"), "{prompt}");
    assert!(
        !comment.replace(prompt, "").contains("/grill-with-docs"),
        "the prompt is only inside the section"
    );
    assert_eq!(moves(&rig), ["comment"], "no label goes on while it waits");
    let held = record(&rig, &runner);
    assert_eq!(held["step"], "commented");
    // The labels the pass owes wait in the record until the wait ends.
    assert_eq!(held["labels"], json!(["bug", "P2"]), "{held}");
    assert!(held["questions"]["comment"].is_number(), "{held}");
    assert!(held["questions"]["at"].is_number(), "{held}");
    assert!(held["local"]["body"].is_number(), "{held}");
    assert!(held.get("asked").is_none(), "{held}");
    // The facts the comment was worded from no longer ask for a word.
    let facts = calls(&rig, Role::Representative).remove(0).call.prompt;
    assert!(!facts.contains("`github`"), "{facts}");
    assert!(!facts.contains("`none`"), "{facts}");
}

#[test]
fn the_comment_lists_the_questions_up_to_max_questions_with_their_recommended_answers() {
    let (rig, runner) = filed(
        "pick = \"maintainer\"\nmax_questions = 2",
        MAINTAINER,
        asked_as(4),
    );

    assert_eq!(step(&runner).unwrap(), None);

    let [comment] = posted(&rig).try_into().expect("one comment");
    assert!(
        comment.starts_with(&format!(
            "{ASK_WORDS}\n\n❓ Q1 - Which one, 1?\n\n➡️ the 1th\n\n---\n\n❓ Q2 - Which one, 2?\n\n\
             ➡️ the 2th\n\n{REPLY_HINT}\n\n<details>"
        )),
        "{comment}"
    );
    assert!(!comment.contains("Which one, 3?"), "{comment}");
}

#[test]
fn an_outside_issue_is_asked_under_pick_all_and_ends_needs_triage_under_pick_maintainer() {
    let (rig, runner) = filed("pick = \"all\"", "newcomer", asked());
    assert_eq!(step(&runner).unwrap(), None);
    let held = record(&rig, &runner);
    assert!(held["questions"]["at"].is_number(), "{held}");
    assert_eq!(moves(&rig), ["comment"], "asked, so no label yet");

    // Asked by hand under `pick = "maintainer"`, it is triaged and not asked.
    let (rig, runner) = filed("pick = \"maintainer\"", "newcomer", asked());
    rig.ask(&runner, "triage", Some("30"));
    assert_eq!(step(&runner).unwrap(), None);
    let held = record(&rig, &runner);
    assert!(held.get("questions").is_none(), "{held}");
    assert!(held.get("local").is_none(), "{held}");
    assert_eq!(held["step"], "done");
    assert_eq!(
        moves(&rig),
        ["comment", "label bug", "label P2", "label needs-triage"],
        "an outside author is not asked"
    );
    assert!(!posted(&rig)[0].contains("<details>"), "{:?}", posted(&rig));
}

#[test]
fn a_comment_with_no_keyword_starts_the_second_pass_with_it_as_the_answers() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);

    wrote(&rig, MAINTAINER, "Linux, it dies right on start.");
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    let prompt = answers_prompt(&rig);
    assert!(
        prompt.contains("Linux, it dies right on start."),
        "{prompt}"
    );
    assert!(prompt.contains(ONE_QUESTION), "{prompt}");
    assert_eq!(posted(&rig).len(), 2, "the answers comment, and no other");
    assert_eq!(worded(&rig), 2, "no reply is read for a word");
    assert_eq!(record(&rig, &runner)["step"], "done");
}

#[test]
fn a_github_followed_by_answers_is_read_as_answers_and_no_question_is_posted_again() {
    let (rig, runner) = filed("pick = \"all\"", "newcomer", asked());
    assert_eq!(step(&runner).unwrap(), None);

    wrote(&rig, "newcomer", "github\n\nQ1: Linux, on start.");
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    let prompt = answers_prompt(&rig);
    assert!(
        prompt.contains("github\n\nQ1: Linux, on start."),
        "{prompt}"
    );
    let posted = posted(&rig);
    assert_eq!(posted.len(), 2, "{posted:?}");
    assert_eq!(posted[1], handed(A_WORDS));
    assert!(!posted[1].contains("❓"), "no question is posted again");
    assert_eq!(
        rig.github.issue_edits(),
        [(
            30,
            under(
                "It crashes when I run it.",
                "## Settled\n\n- Linux, on start."
            )
        )]
    );
    assert_eq!(record(&rig, &runner)["step"], "done");
}

#[test]
fn silence_for_wait_days_hands_the_issue_to_the_maintainer_and_asks_nothing_more() {
    let (rig, runner) = filed("pick = \"maintainer\"\nwait_days = 2", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);

    rig.clock.advance(Rig::DAY);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig), ["comment"], "one day is not `wait_days`");

    rig.clock.advance(Rig::DAY);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(posted(&rig).len(), 1, "no second ask");
    assert_eq!(
        moves(&rig),
        ["comment", "label bug", "label P2", "label needs-triage"]
    );
    assert_eq!(worded(&rig), 1, "nothing was there to read");
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    let held = record(&rig, &runner);
    assert_eq!(held["step"], "done");
    assert!(held.get("questions").is_none(), "{held}");
    assert!(held.get("local").is_none(), "{held}");
}

// The ask pass owes the fallback with its other labels, so an author who never
// answers still leaves the issue ranked
#[test]
fn silence_after_a_pass_that_named_no_priority_ends_with_p3_on_the_issue() {
    let found = json!({
        "type": "bug",
        "open": [{ "question": "Which OS crashes?", "recommended": "Linux" }],
    });
    let (rig, runner) = filed("pick = \"maintainer\"\nwait_days = 1", MAINTAINER, found);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        moves(&rig),
        ["comment"],
        "no label while the author is asked"
    );
    let said = facts(&rig);
    assert!(!said.contains("kelpie set priority"), "{said}");

    rig.clock.advance(Rig::DAY);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        moves(&rig),
        [
            "comment",
            "label bug",
            "label priority: P3",
            "label needs-triage"
        ]
    );
    assert_eq!(record(&rig, &runner)["step"], "done");
}

#[test]
fn a_reply_by_anyone_but_the_author_is_not_the_answers() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);

    wrote(&rig, "someone-else", "Linux");
    wrote(&rig, "kelpie-shep-pm[bot]", "Linux");
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(posted(&rig).len(), 1);
    assert_eq!(moves(&rig), ["comment"], "the wait still holds");
    assert!(record(&rig, &runner)["questions"]["at"].is_number());
    assert_eq!(calls(&rig, Role::Triager).len(), 1, "no second pass");
}

#[test]
fn a_triage_label_put_on_by_hand_or_a_closed_issue_ends_the_wait_with_no_post() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);
    rig.forge.label(30, "needs-info");
    let mut labelled = issue(&rig, 30, MAINTAINER, 5);
    labelled.title = "Crash on start".to_owned();
    labelled.body = "It crashes when I run it.".to_owned();
    labelled.labels = vec!["needs-info".to_owned()];
    list(&rig, labelled);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(taken(&rig), 1);
    assert_eq!(moves(&rig), ["comment"], "kelpie labels it no further");
    let held = record(&rig, &runner);
    assert_eq!(held["step"], "done");
    assert!(held.get("questions").is_none(), "{held}");
    assert!(held.get("local").is_none(), "{held}");

    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);
    rig.forge.unlist_open_issue(30);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(taken(&rig), 1);
    assert_eq!(moves(&rig), ["comment"]);
    let held = record(&rig, &runner);
    assert_eq!(held["step"], "done");
    assert!(held.get("questions").is_none(), "{held}");
}

#[test]
fn a_restart_while_waiting_keeps_the_questions_and_their_clock() {
    let (rig, runner) = filed("pick = \"maintainer\"\nwait_days = 2", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);
    let at = record(&rig, &runner)["questions"]["at"].clone();

    let runner = restart(&rig, runner);
    assert_eq!(step(&runner).unwrap(), None);
    let held = record(&rig, &runner);
    assert_eq!(
        held["questions"]["at"], at,
        "its clock is the one it began with"
    );
    assert_eq!(held["step"], "commented");
    assert_eq!(moves(&rig), ["comment"], "a restart labels nothing early");

    rig.clock.advance(2 * Rig::DAY);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(record(&rig, &runner)["step"], "done");
}

// A file saved while kelpie waited on the word `github`, `local` or `none`
// holds an `asked` and no questions: what the author writes after it is the
// answers to the triage comment it was saved with.
#[test]
fn a_wait_saved_on_the_old_ask_takes_the_authors_comment_as_the_answers() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);
    let held = record(&rig, &runner);
    drop(runner);
    crashed_with(
        &rig,
        json!({
            "issue": 30, "comment": held["questions"]["comment"], "step": "commented",
            "labels": ["bug", "P2"], "seen": [],
            "asked": { "at": held["questions"]["at"] },
        }),
    );
    let runner = rig.open().unwrap();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig), ["comment"], "a restart labels nothing early");

    wrote(&rig, MAINTAINER, "Linux, on start.");
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    let prompt = calls(&rig, Role::Triager).remove(1).call.prompt;
    assert!(prompt.contains("Linux, on start."), "{prompt}");
    assert!(prompt.contains(ONE_QUESTION), "{prompt}");
}

// The two ways #1142 and #1144 lost the maintainer's answers, and the answer
// that came while a post was read back (#1194, #1144), each held by a test.

// The questions comment went up and the forge's answer to the post was lost;
// a restart comes before the marker is read again. The record made from the
// comment found on the issue owes the wait its questions opened, measured from
// when the comment went up.
#[test]
fn a_questions_comment_found_on_the_issue_after_a_restart_saves_the_wait() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    // The post is refused here, but the forge really took it.
    rig.github.refuse_comments(ApiError::Refused(502));
    assert_eq!(step(&runner).unwrap(), None);
    let went_up = rig.clock.now();
    let posted = format!(
        "{ASK_WORDS}\n\n{ONE_QUESTION}\n\n{REPLY_HINT}\n\n<details>...\n</details>\n\n{MARKER}"
    );
    rig.forge.threads.write(30, APP, &posted, went_up);
    rig.github.accept_comments();
    // A restart before the next poll.
    let runner = restart(&rig, runner);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let held = record(&rig, &runner);
    assert_eq!(held["step"], "commented", "{held}");
    assert_eq!(
        held["questions"]["at"],
        serde_json::to_value(went_up).unwrap(),
        "the wait is saved, from when the comment went up: {held}"
    );
}

// The same, on an issue that carried a `needs-triage` the maintainer put on by
// hand before the pass: the label that was there before the questions went up
// does not end the wait at the next poll.
#[test]
fn a_questions_comment_found_on_the_issue_waits_under_a_hand_needs_triage() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    let mut by_hand = issue(&rig, 30, MAINTAINER, 5);
    by_hand.title = "Crash on start".to_owned();
    by_hand.body = "It crashes when I run it.".to_owned();
    by_hand.labels = vec!["needs-triage".to_owned()];
    list(&rig, by_hand);
    rig.forge.label(30, "needs-triage");
    rig.github.refuse_comments(ApiError::Refused(502));
    assert_eq!(step(&runner).unwrap(), None);
    let posted = format!(
        "{ASK_WORDS}\n\n{ONE_QUESTION}\n\n{REPLY_HINT}\n\n<details>...\n</details>\n\n{MARKER}"
    );
    rig.forge.threads.write(30, APP, &posted, rig.clock.now());
    rig.github.accept_comments();
    let runner = restart(&rig, runner);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let held = record(&rig, &runner);
    assert!(held["questions"]["at"].is_number(), "{held}");
    // The next poll reads the label that was already there and lets the wait
    // stand on.
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let held = record(&rig, &runner);
    assert!(
        held["questions"]["at"].is_number(),
        "the wait survived: {held}"
    );
    assert_eq!(held["step"], "commented", "{held}");
}

// The comment was worded while the listing still lacked the `needs-triage` the
// maintainer put on, and the forge was busy, so it is posted at the next poll.
// The label was on the issue before the comment went up, so it does not end
// the wait.
#[test]
fn a_hand_needs_triage_that_landed_before_the_post_does_not_end_the_wait() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    // The forge is busy: the comment is kept for the next poll.
    rig.github.refuse_comments(ApiError::Refused(502));
    assert_eq!(step(&runner).unwrap(), None);
    rig.github.accept_comments();
    // The maintainer puts `needs-triage` on before the comment goes up.
    let mut by_hand = issue(&rig, 30, MAINTAINER, 5);
    by_hand.title = "Crash on start".to_owned();
    by_hand.body = "It crashes when I run it.".to_owned();
    by_hand.labels = vec!["needs-triage".to_owned()];
    list(&rig, by_hand);
    rig.forge.label(30, "needs-triage");
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let held = record(&rig, &runner);
    assert!(held["questions"]["at"].is_number(), "{held}");
    // The next poll reads the label as one that was there before the questions
    // went up, and the wait stands on.
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let held = record(&rig, &runner);
    assert!(
        held["questions"]["at"].is_number(),
        "the wait survived: {held}"
    );
    assert_eq!(held["step"], "commented", "{held}");
}

// The author answers while the questions post is still being read back: the
// reply is written after the comment went up and before kelpie recorded it.
// The wait is measured from the comment, so the answer is heard.
#[test]
fn an_answer_written_while_the_questions_post_is_read_back_is_heard() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    // The forge takes the post, but its answer is lost: the marker is read at
    // the next poll.
    rig.github.refuse_comments(ApiError::Refused(502));
    assert_eq!(step(&runner).unwrap(), None);
    let went_up = rig.clock.now();
    let posted = format!(
        "{ASK_WORDS}\n\n{ONE_QUESTION}\n\n{REPLY_HINT}\n\n<details>...\n</details>\n\n{MARKER}"
    );
    rig.forge.threads.write(30, APP, &posted, went_up);
    rig.github.accept_comments();
    // The author answers a minute after the comment went up.
    rig.clock.advance(60);
    rig.forge
        .threads
        .write(30, MAINTAINER, "ok", rig.clock.now());
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    for _ in 0..3 {
        rig.next_look();
        step(&runner).unwrap();
    }
    assert_eq!(calls(&rig, Role::Triager).len(), 2, "the answers pass ran");
    let held = record(&rig, &runner);
    assert!(
        held["step"] == "done" || held["step"] == "ready",
        "the round closed: {held}"
    );
}

// #1194: `shep kelpie triage` over an issue kelpie already triaged asked the
// settled questions again. The pass is now given the earlier triage comments
// and their author's replies, and a question the author answered is not asked
// again.
#[test]
fn a_pass_asked_again_over_a_triaged_issue_reads_the_rounds_it_ran() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);
    wrote(&rig, MAINTAINER, "ok");
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(record(&rig, &runner)["step"], "done");

    // The verb asks again, and the triager answers with the same open question
    // and a recommendation against what the author took.
    rig.ask(&runner, "triage", Some("30"));
    rig.claude.script_for(Role::Triager, [reply(asked())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let prompts: Vec<String> = (calls(&rig, Role::Triager))
        .into_iter()
        .map(|seen| seen.call.prompt)
        .collect();
    let last = prompts.last().unwrap();
    assert!(
        last.contains("Which OS crashes?"),
        "the earlier questions reach the triager: {last}"
    );
    assert!(
        last.contains(&format!("{MAINTAINER} wrote:")),
        "the author's reply reaches the triager: {last}"
    );
    assert!(
        last.contains("Do not ask again in `open` what those rounds settled"),
        "{last}"
    );
    let comments = posted(&rig);
    let again = comments.last().unwrap();
    assert!(
        !again.contains("Which OS crashes?"),
        "the settled question is not posted again: {again}"
    );
    assert!(!again.contains("Q1 - "), "no questions block: {again}");
}

// #1304: only what the author answered is settled. The first round asked two
// questions and the author answered one, so the re-ask puts the other back.
#[test]
fn a_reask_asks_again_the_question_its_author_left_unanswered() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked_as(2));
    assert_eq!(step(&runner).unwrap(), None);
    wrote(&rig, MAINTAINER, "Q2: the second one");
    rig.claude.script_for(Role::Triager, [reply(asked_as(1))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    rig.ask(&runner, "triage", Some("30"));
    rig.claude.script_for(Role::Triager, [reply(asked_as(2))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let again = posted(&rig).last().expect("a comment").clone();
    assert!(
        again.contains("Which one, 1?"),
        "the question its author left unanswered is asked again: {again}"
    );
    assert!(
        !again.contains("Which one, 2?"),
        "the one it answered is not: {again}"
    );
}

// #1304 round 2: the author's reply to a later round answers that round alone.
// The questions an earlier round asked and never heard an answer to are still
// open, and the next pass asks them.
#[test]
fn a_reask_does_not_take_a_later_round_s_answer_for_an_earlier_one() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked_as(2));
    assert_eq!(step(&runner).unwrap(), None);
    rig.ask(&runner, "triage", Some("30"));
    rig.claude.script_for(
        Role::Triager,
        [reply(asked_one("Which display server?", "X11"))],
    );
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    wrote(&rig, MAINTAINER, "Q1: X11, on Wayland.");
    rig.claude.script_for(Role::Triager, [reply(asked_as(1))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    rig.ask(&runner, "triage", Some("30"));
    rig.claude.script_for(Role::Triager, [reply(asked_as(2))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let again = posted(&rig).last().expect("a comment").clone();
    for n in 1..=2 {
        assert!(
            again.contains(&format!("Which one, {n}?")),
            "the question no round heard an answer to is asked again: {again}"
        );
    }
    assert!(
        !again.contains("Which display server?"),
        "the question its author answered to that round is not: {again}"
    );
}

// #1304 round 2: only the form the questions comment asked for settles a round.
// An author whose sentence starts like `ok` answered no question kelpie can
// name, and reading what they meant is the triager's job, not the filter's.
#[test]
fn an_author_s_words_that_are_not_an_ok_settle_nothing() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);
    wrote(&rig, MAINTAINER, "Okay, any update on this?");
    rig.claude.script_for(Role::Triager, [reply(asked_as(1))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    rig.ask(&runner, "triage", Some("30"));
    rig.claude.script_for(Role::Triager, [reply(asked())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let again = posted(&rig).last().expect("a comment").clone();
    assert!(
        again.contains("Which OS crashes?"),
        "the question its sentence answered nothing for is asked again: {again}"
    );
}

// #1304 round 2: the settled questions are compared in one case, so a question
// holding a letter outside ASCII is known for the one its author answered.
#[test]
fn a_settled_question_holding_a_letter_outside_ascii_is_not_asked_again() {
    let (rig, runner) = filed(
        "pick = \"maintainer\"",
        MAINTAINER,
        asked_second("\u{00c7}a plante sous Wayland ?", "Oui"),
    );
    assert_eq!(step(&runner).unwrap(), None);
    wrote(&rig, MAINTAINER, "Q1: Oui, sous Wayland.");
    rig.claude.script_for(Role::Triager, [reply(asked_as(1))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    rig.ask(&runner, "triage", Some("30"));
    rig.claude.script_for(
        Role::Triager,
        [reply(asked_second(
            "\u{00c7}a plante sous Wayland ?",
            "Oui",
        ))],
    );
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let again = posted(&rig).last().expect("a comment").clone();
    assert!(
        again.contains("Which one, 2?"),
        "the unanswered one is asked again: {again}"
    );
    assert!(
        !again.contains("plante sous Wayland"),
        "the question its author answered is not asked again: {again}"
    );
}

// #1304 round 2: a question is compared as the questions block wrote it, so the
// marks it took out and the line it broke at do not hide what was settled.
#[test]
fn a_settled_question_the_questions_block_rewrote_is_not_asked_again() {
    let (rig, runner) = filed(
        "pick = \"maintainer\"",
        MAINTAINER,
        asked_second("Does ```kelpie``` crash on start?", "No"),
    );
    assert_eq!(step(&runner).unwrap(), None);
    wrote(&rig, MAINTAINER, "Q1: No, only under Wayland.");
    rig.claude.script_for(Role::Triager, [reply(asked_as(1))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    rig.ask(&runner, "triage", Some("30"));
    rig.claude.script_for(
        Role::Triager,
        [reply(asked_second(
            "Does ```kelpie``` crash on start?",
            "No",
        ))],
    );
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let again = posted(&rig).last().expect("a comment").clone();
    assert!(
        again.contains("Which one, 2?"),
        "the unanswered one is asked again: {again}"
    );
    assert!(
        !again.contains("Does kelpie crash on start?"),
        "the question its author answered is not asked again: {again}"
    );
}

// #1304 round 2: a label that says what to do with the issue, put on while the
// questions comment was being worded, is the maintainer's own. It ends the wait
// and nothing is heard from the author after it.
#[test]
fn a_needs_info_put_on_while_the_questions_comment_is_worded_ends_the_wait() {
    let hold = Hold::default();
    let (rig, runner) = filed_before("pick = \"maintainer\"", MAINTAINER, asked(), |rig| {
        rig.claude.script_for(
            Role::Representative,
            [Scripted::HoldThenSay(hold.clone(), ASK_WORDS)],
        );
    });
    std::thread::scope(|scope| {
        let posting = scope.spawn(|| step(&runner).unwrap());
        assert!(hold.entered(CALL_PATIENCE), "the comment was never worded");
        rig.forge.label(30, "needs-info");
        hold.release();
        posting.join().unwrap();
    });
    assert_eq!(taken(&rig), 1, "the questions went up");
    let mut labelled = issue(&rig, 30, MAINTAINER, 5);
    labelled.title = "Crash on start".to_owned();
    labelled.body = "It crashes when I run it.".to_owned();
    labelled.labels = vec!["needs-info".to_owned()];
    list(&rig, labelled);
    wrote(&rig, MAINTAINER, "ok");
    for _ in 0..3 {
        rig.next_look();
        assert_eq!(step(&runner).unwrap(), None);
    }
    assert_eq!(taken(&rig), 1, "no answers comment");
    assert_eq!(calls(&rig, Role::Triager).len(), 1, "no second pass");
    let held = record(&rig, &runner);
    assert_eq!(held["step"], "done", "{held}");
    assert!(held["questions"].is_null(), "no wait left: {held}");
}

// #1194 and #1144: a `shep kelpie triage` pass asked again, the author answered
// within a minute, and no answers pass ran. The re-ask was recorded from the
// marker read at a later poll, and the wait was measured from that later time,
// so a reply written between the comment and the record fell outside for good.
#[test]
fn a_reask_whose_post_is_recorded_from_the_marker_hears_an_answer_written_early() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);
    wrote(&rig, MAINTAINER, "Q1: Linux, on start.");
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(record(&rig, &runner)["step"], "done");

    // The verb asks again, and the new questions post is taken by the forge
    // while its answer is lost.
    rig.ask(&runner, "triage", Some("30"));
    rig.claude.script_for(
        Role::Triager,
        [reply(json!({
            "type": "bug",
            "priority": "P2",
            "open": [{ "question": "Which display server?", "recommended": "X11" }],
        }))],
    );
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.github.refuse_comments(ApiError::Refused(502));
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let went_up = rig.clock.now();
    let landed = format!(
        "{ASK_WORDS}\n\n❓ Q1 - Which display server?\n\n➡️ X11\n\n{REPLY_HINT}\n\n<details>...\n</details>\n\n{MARKER}"
    );
    rig.forge.threads.write(30, APP, &landed, went_up);
    rig.github.accept_comments();

    // The author answers a minute after the comment went up, well before the
    // poll that reads the marker back.
    rig.clock.advance(60);
    rig.forge
        .threads
        .write(30, MAINTAINER, "ok", rig.clock.now());
    rig.claude
        .script_for(Role::Triager, [reply(settled_ready())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    for _ in 0..4 {
        rig.next_look();
        step(&runner).unwrap();
    }
    // The reply landed a minute after the comment and before the poll that read
    // the marker back. Measured from the comment, it is inside the wait; measured
    // from the record, it is outside it for good and no second call is made.
    let held = record(&rig, &runner);
    assert_eq!(
        held["step"], "ready",
        "the re-ask's round ran to the end: {held}"
    );
    assert_eq!(
        calls(&rig, Role::Triager).len(),
        4,
        "the first pass, its round, the re-ask, and the re-ask's round"
    );
    assert_eq!(
        posted(&rig).last().unwrap(),
        &format!("{A_WORDS}\n\n{MARKER}"),
        "the re-ask's answers comment went up"
    );
}
