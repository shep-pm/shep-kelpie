//! Who a triage comment is for: the maintainer's own issue is told where it
//! stands, and a comment that hands an outside issue over mentions the
//! maintainer, through the runner's stand-ins

use serde_json::json;

use super::ask_tests::{A_WORDS, ASK_WORDS, asked, filed, posted, settled_ready, wrote};
use super::marker::{MARKER, duplicate_marker};
use super::pick_tests::{MAINTAINER, issue, list, rig_with};
use super::research_tests::{WORDS, facts, moves, reply, triage, untouched};
use crate::ports::{ForgeError, Role};
use crate::runner::step;
use crate::test::{Rig, Scripted};

// The one line kelpie's code writes under the representative's words
fn mention() -> String {
    format!("cc @{MAINTAINER}")
}

// How many lines of `comment` mention anyone through kelpie's `cc` line
fn mentions(comment: &str) -> usize {
    comment
        .lines()
        .filter(|line| line.starts_with("cc @"))
        .count()
}

#[test]
fn a_maintainer_pass_that_is_sure_says_the_issue_is_ready_for_an_agent_at_its_priority() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, settled_ready());

    assert_eq!(step(&runner).unwrap(), None);

    let said = facts(&rig);
    assert!(said.contains("ready for an agent"), "{said}");
    assert!(said.contains("priority P2"), "{said}");
    let comments = posted(&rig);
    assert_eq!(comments, [format!("{ASK_WORDS}\n\n{MARKER}")]);
    assert_eq!(moves(&rig).last().unwrap(), "label ready-for-agent");
}

#[test]
fn a_maintainer_pass_that_is_not_sure_speaks_to_them_and_recommends_a_priority() {
    let found = json!({ "type": "bug", "priority": "P2" });
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, found);

    assert_eq!(step(&runner).unwrap(), None);

    let said = facts(&rig);
    assert!(said.contains("as \"you\""), "{said}");
    assert!(said.contains("would recommend priority P2"), "{said}");
    assert!(said.contains("never call it ready"), "{said}");
    assert!(!said.contains("ready for an agent"), "{said}");
    assert!(!said.contains("The maintainer triages it next"), "{said}");
    assert_eq!(posted(&rig), [format!("{ASK_WORDS}\n\n{MARKER}")]);
    assert_eq!(moves(&rig).last().unwrap(), "label needs-triage");
}

#[test]
fn a_maintainer_pass_that_is_not_sure_lists_what_it_could_not_settle() {
    let found = json!({ "type": "bug" });
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, found);

    assert_eq!(step(&runner).unwrap(), None);

    let said = facts(&rig);
    assert!(said.contains("still theirs to settle"), "{said}");
    assert!(!said.contains("the priority"), "{said}");
    assert!(!said.contains("the type"), "{said}");
    assert!(!said.contains("would recommend"), "{said}");
    assert!(said.contains("kelpie set priority P3 itself"), "{said}");
    assert!(said.contains("the maintainer can change it"), "{said}");
}

#[test]
fn a_maintainer_pass_that_asks_does_not_say_where_the_issue_ends() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());

    assert_eq!(step(&runner).unwrap(), None);

    let said = facts(&rig);
    assert!(said.contains("as \"you\""), "{said}");
    assert!(said.contains("labels nothing yet"), "{said}");
    assert!(said.contains("their answers decide"), "{said}");
    assert!(!said.contains("The maintainer triages"), "{said}");
    assert!(
        !said.contains("what is still open goes to the maintainer"),
        "{said}"
    );
    assert_eq!(mentions(&posted(&rig)[0]), 0);
}

#[test]
fn a_first_pass_on_an_outside_issue_that_asks_nothing_mentions_the_maintainer() {
    let (rig, runner) = untouched();
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        rig.github.comments(),
        [(30, format!("{WORDS}\n\n{}\n\n{MARKER}", mention()))]
    );
    let said = facts(&rig);
    assert!(said.contains("mentions the maintainer"), "{said}");
    assert_eq!(triage(&rig, &runner)["issues"][0]["step"], "done");
}

#[test]
fn the_comment_that_asks_the_author_mentions_no_one() {
    let (rig, runner) = filed("pick = \"all\"", "newcomer", asked());

    assert_eq!(step(&runner).unwrap(), None);

    let comments = posted(&rig);
    assert_eq!(comments.len(), 1);
    assert_eq!(mentions(&comments[0]), 0, "{}", comments[0]);
    assert!(comments[0].ends_with(MARKER), "{}", comments[0]);
    let said = facts(&rig);
    assert!(!said.contains("mentions the maintainer"), "{said}");
}

#[test]
fn the_duplicate_close_mentions_no_one() {
    let (rig, runner) = untouched();
    rig.forge
        .list_open_pull(41, "Fix the crash on start", &[30]);
    let covers = json!({ "number": 41, "kind": "pull_request", "why": "The same crash." });
    rig.claude
        .script_for(Role::Triager, [reply(json!({ "addressed_by": covers }))]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        rig.github.comments(),
        [(30, format!("{WORDS}\n\n{}", duplicate_marker(41)))]
    );
}

#[test]
fn silence_posts_nothing_so_no_one_is_mentioned() {
    let (rig, runner) = filed("pick = \"all\"\nwait_days = 2", "newcomer", asked());
    assert_eq!(step(&runner).unwrap(), None);

    rig.clock.advance(2 * Rig::DAY);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    let comments = posted(&rig);
    assert_eq!(comments.len(), 1, "no post for the silence");
    assert_eq!(mentions(&comments[0]), 0);
}

#[test]
fn the_answers_pass_on_an_outside_issue_mentions_the_maintainer() {
    let (rig, runner) = filed("pick = \"all\"", "newcomer", asked());
    assert_eq!(step(&runner).unwrap(), None);

    wrote(&rig, "newcomer", "Q1: Linux, on start.");
    rig.claude.script_for(
        Role::Triager,
        [reply(json!({ "type": "bug", "priority": "P2" }))],
    );
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    let comments = posted(&rig);
    assert_eq!(comments.len(), 2);
    assert_eq!(mentions(&comments[0]), 0, "the ask mentions no one");
    assert_eq!(
        comments[1],
        format!("{A_WORDS}\n\n{}\n\n{MARKER}", mention())
    );
}

#[test]
fn no_login_posts_the_hand_over_without_the_line_and_says_so_once() {
    let rig = rig_with("pick = \"all\"");
    rig.edit_settings(|s| s.replace(&format!("\nmaintainer = \"{MAINTAINER}\""), ""));
    rig.forge
        .set_viewer_down(ForgeError::Failed("no login".into()));
    for number in [30, 31] {
        list(&rig, issue(&rig, number, "newcomer", 5));
    }
    let words = [Scripted::Say(WORDS), Scripted::Say(WORDS)];
    rig.claude.script_for(Role::Representative, words);
    rig.claude
        .script_for(Role::Triager, [reply(json!({})), reply(json!({}))]);
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    let comments = rig.github.comments();
    assert_eq!(comments.len(), 2, "{comments:?}");
    for (_, body) in &comments {
        assert_eq!(body, &format!("{WORDS}\n\n{MARKER}"));
    }
    let notes = runner.lock().unwrap().take_notes();
    let told = notes
        .iter()
        .filter(|note| note.contains("without a mention of the maintainer"))
        .count();
    assert_eq!(told, 1, "{notes:?}");
}
