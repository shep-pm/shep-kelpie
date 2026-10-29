//! The issue check the board runs on every open issue, through the runner's
//! stand-ins: a passing issue is ready with no triage call, and only a failing
//! one reaches the triager

use std::sync::Mutex;

use serde_json::{Value, json};

use super::checks::CHECK_TRIES;
use super::pick_tests::{MAINTAINER, issue, list, rig_with};
use super::ready_tests::said;
use super::research_tests::{APP, WORDS, calls, moves, pump, reply, under};
use crate::github::ApiError;
use crate::ports::Timestamp;
use crate::ports::{OpenIssue, Role};
use crate::runner::slots_tests::dispatched;
use crate::runner::{Pass, Runner, advance, step};
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted};

// A body the issue check passes, with the labels it needs beside it
pub(super) const PASSING: &str = "Touches: `src/start.rs`\n\n## What to build\n\n- Fix the \
    crash on start.\n\n## Acceptance criteria\n\n- [ ] `runner::start::it_starts`\n";
pub(super) const PASSING_LABELS: [&str; 2] = ["kind:fix", "priority: P2"];

// A body the check refuses on every part
pub(super) const FAILING: &str = "It crashes when I run it.";

// The labels a repo holds, so none is made on the way
pub(super) const REPO_LABELS: [&str; 5] =
    ["bug", "P2", "needs-triage", "ready-for-agent", "kind:fix"];

// Issue 30 by `author`, with `body` and `labels`, opened after triage was on
pub(super) fn filed(rig: &Rig, author: &str, body: &str, labels: &[&str]) -> OpenIssue {
    let mut filed = issue(rig, 30, author, 5);
    filed.title = "Crash on start".to_owned();
    filed.body = body.to_owned();
    filed.labels = labels.iter().map(|&l| l.to_owned()).collect();
    filed
}

// A rig with `[triage]` set to `lines`, the repo holding `REPO_LABELS`, issue
// 30 listed as `filed` gives it, and a representative that says `WORDS`
pub(super) fn checking(
    lines: &str,
    author: &str,
    body: &str,
    labels: &[&str],
) -> (Rig, Mutex<Runner>) {
    let rig = rig_with(lines);
    rig.forge.set_repo_labels(&REPO_LABELS);
    list(&rig, filed(&rig, author, body, labels));
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    let runner = rig.open().unwrap();
    (rig, runner)
}

pub(super) fn checks(rig: &Rig) -> Value {
    let state = crate::state::StateStore::new(rig.paths().state);
    serde_json::to_value(state.load().unwrap().unwrap()).unwrap()["checks"].clone()
}

#[test]
fn a_maintainers_issue_that_passes_goes_ready_with_no_triage_call() {
    for pick in ["maintainer", "all", "off"] {
        let (rig, runner) = checking(
            &format!("pick = \"{pick}\""),
            MAINTAINER,
            PASSING,
            &PASSING_LABELS,
        );

        assert_eq!(step(&runner).unwrap(), None, "{pick}");

        assert_eq!(calls(&rig, Role::Triager).len(), 0, "{pick}");
        assert_eq!(moves(&rig), ["label ready-for-agent"], "{pick}");
        assert_eq!(checks(&rig)[0]["issue"], 30, "{pick}");
        assert!(checks(&rig)[0].get("failures").is_none(), "{pick}");

        // Unchanged, it is not checked again.
        rig.next_look();
        assert_eq!(step(&runner).unwrap(), None, "{pick}");
        assert_eq!(moves(&rig), ["label ready-for-agent"], "{pick}");
    }
}

#[test]
fn a_first_check_of_many_passing_issues_writes_a_poll_s_share_and_carries_the_rest() {
    let (rig, runner) = checking("pick = \"off\"", MAINTAINER, PASSING, &PASSING_LABELS);
    for number in 31..80 {
        let mut more = filed(&rig, MAINTAINER, PASSING, &PASSING_LABELS);
        more.number = number;
        list(&rig, more);
    }

    for (poll, written) in [(1, 20), (2, 40), (3, 50)] {
        if poll > 1 {
            rig.next_look();
        }
        assert_eq!(step(&runner).unwrap(), None, "poll {poll}");
        assert_eq!(moves(&rig).len(), written, "poll {poll}");
    }
    assert!(moves(&rig).iter().all(|m| m == "label ready-for-agent"));
    let labelled: std::collections::BTreeSet<u64> = (rig.github.writes().into_iter())
        .filter_map(|(_, call)| call.path.rsplit('/').nth(1)?.parse().ok())
        .collect();
    assert_eq!(labelled, (30..80).collect(), "each issue once");
    assert_eq!(checks(&rig).as_array().unwrap().len(), 50);

    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig).len(), 50, "nothing is left to write");
}

#[test]
fn a_passing_maintainers_issue_loses_its_needs_triage_and_a_hand_decision_stands() {
    let mut labels = PASSING_LABELS.to_vec();
    labels.push("needs-triage");
    let (rig, runner) = checking("pick = \"maintainer\"", MAINTAINER, PASSING, &labels);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig), ["label ready-for-agent"]);
    assert_eq!(rig.github.label_takes(), [(30, "needs-triage".to_owned())]);

    let mut labels = PASSING_LABELS.to_vec();
    labels.push("needs-info");
    let (rig, runner) = checking("pick = \"maintainer\"", MAINTAINER, PASSING, &labels);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig), Vec::<String>::new(), "`needs-info` stands");
}

#[test]
fn a_passing_maintainers_issue_an_open_pull_request_closes_is_not_made_ready() {
    let (rig, runner) = checking("pick = \"off\"", MAINTAINER, PASSING, &PASSING_LABELS);
    rig.forge.list_open_pull(77, "Fix the crash", &[30]);

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig), Vec::<String>::new());
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig), Vec::<String>::new());
}

#[test]
fn another_persons_issue_that_passes_waits_at_needs_triage_with_no_triage_call() {
    let (rig, runner) = checking("pick = \"all\"", "newcomer", PASSING, &PASSING_LABELS);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 0);
    assert_eq!(
        moves(&rig),
        ["label needs-triage"],
        "it waits for the maintainer to label it ready"
    );
    assert_eq!(rig.github.comments(), []);

    let (rig, runner) = checking("pick = \"off\"", "newcomer", PASSING, &PASSING_LABELS);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        moves(&rig),
        Vec::<String>::new(),
        "no queue while triage is off"
    );
}

// A rewrite still short of the format: it names its files and no more
pub(super) const STILL: &str = "Touches: `src/start.rs`\n\nIt crashes when I run it.";

#[test]
fn a_failing_issue_is_triaged_and_a_rewrite_that_still_fails_gets_one_comment() {
    let (rig, runner) = checking("pick = \"maintainer\"", MAINTAINER, FAILING, &[]);
    let found = json!({ "type": "bug", "kind": "fix", "priority": "P2", "body": STILL });
    rig.claude.script_for(Role::Triager, [reply(found)]);

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(rig.github.comments().len(), 1, "the triage comment");
    assert_eq!(
        rig.github.issue_edits(),
        [(30, under(FAILING, STILL))],
        "a rewrite the check refuses goes under the maintainer's text"
    );
    assert_eq!(
        moves(&rig),
        [
            "comment",
            "label bug",
            "label P2",
            "label kind:fix",
            "label needs-triage"
        ]
    );

    // The forge as the pass left it: rewritten, labelled, and still short of the format
    let labels = ["bug", "P2", "kind:fix", "needs-triage"];
    list(&rig, filed(&rig, MAINTAINER, STILL, &labels));
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    let comments = rig.github.comments();
    assert_eq!(comments.len(), 2, "{comments:?}");
    let said = &comments[1].1;
    assert!(said.contains("the issue check still refuses it"), "{said}");
    assert!(said.contains("What to build"), "it names the part: {said}");
    assert!(!said.contains("Touches"), "only what it refused: {said}");

    // Not triaged again, and not told again, until it is edited.
    for _ in 0..2 {
        rig.next_look();
        assert_eq!(step(&runner).unwrap(), None);
    }
    assert_eq!(rig.github.comments().len(), 2);
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
}

#[test]
fn a_later_rewrite_the_check_refuses_replaces_kelpies_section_and_keeps_the_rest() {
    let (rig, runner) = checking("pick = \"maintainer\"", MAINTAINER, FAILING, &[]);
    let found = json!({ "type": "bug", "kind": "fix", "priority": "P2", "body": STILL });
    rig.claude.script_for(Role::Triager, [reply(found)]);
    assert_eq!(step(&runner).unwrap(), None);
    let first = under(FAILING, STILL);
    assert_eq!(rig.github.issue_edits(), [(30, first.clone())]);
    let labels = ["bug", "P2", "kind:fix", "needs-triage"];
    list(&rig, filed(&rig, MAINTAINER, &first, &labels));
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.github.comments().len(), 2, "the still-failing comment");

    // The maintainer adds a line under kelpie's section.
    let edited = format!("{first}\n\nOn Linux only.");
    list(&rig, filed(&rig, MAINTAINER, &edited, &labels));
    let again = "Touches: `src/start.rs`\n\nIt crashes on Linux.";
    rig.claude
        .script_for(Role::Triager, [reply(json!({ "body": again }))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 2);
    let whole = format!("{}\n\nOn Linux only.", under(FAILING, again));
    assert_eq!(rig.github.issue_edits().last(), Some(&(30, whole)));
}

// The issue as `a_failing_issue_is_triaged...` leaves it: triaged, its
// still-failing rewrite told
pub(super) fn told() -> (Rig, Mutex<Runner>) {
    let (rig, runner) = checking("pick = \"maintainer\"", MAINTAINER, FAILING, &[]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    assert_eq!(step(&runner).unwrap(), None);
    list(&rig, filed(&rig, MAINTAINER, STILL, &["needs-triage"]));
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.github.comments().len(), 2);
    (rig, runner)
}

#[test]
fn a_retry_after_a_lost_reply_posts_the_still_failing_comment_once() {
    let (rig, runner) = checking("pick = \"maintainer\"", MAINTAINER, FAILING, &[]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    assert_eq!(step(&runner).unwrap(), None);
    list(&rig, filed(&rig, MAINTAINER, STILL, &["needs-triage"]));
    rig.github.refuse_comments(ApiError::Refused(422));
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let tried = rig.github.comments();
    assert_eq!(tried.len(), 2);

    // The forge took the post whose answer was lost.
    rig.forge.threads.write(30, APP, &tried[1].1, Timestamp(1));
    rig.github.accept_comments();
    for _ in 0..2 {
        rig.next_look();
        assert_eq!(step(&runner).unwrap(), None);
    }
    assert_eq!(rig.github.comments().len(), 2, "it is not posted again");
}

#[test]
fn a_still_failing_comment_the_forge_keeps_refusing_is_given_up() {
    let (rig, runner) = checking("pick = \"maintainer\"", MAINTAINER, FAILING, &[]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    assert_eq!(step(&runner).unwrap(), None);
    list(&rig, filed(&rig, MAINTAINER, STILL, &["needs-triage"]));
    rig.github.refuse_comments(ApiError::Refused(422));
    for _ in 0..CHECK_TRIES + 2 {
        rig.next_look();
        assert_eq!(step(&runner).unwrap(), None);
    }
    let tries = usize::try_from(CHECK_TRIES).unwrap();
    assert_eq!(rig.github.comments().len(), 1 + tries);
}

// A body that passes, with what the triager had a good answer for stated in it
const ASSUMED: &str = "Touches: `src/start.rs`\n\n## What to build\n\n- Fix the crash on \
    start.\n- Assumed: it crashes on Linux only. Say otherwise.\n\n## Acceptance criteria\n\n\
    - [ ] `runner::start::it_starts`\n";

#[test]
fn a_rewrite_that_passes_states_its_assumptions_and_goes_ready_with_its_kind_made() {
    let (rig, runner) = checking("pick = \"maintainer\"", MAINTAINER, FAILING, &[]);
    let found = json!({ "type": "bug", "kind": "feat", "priority": "P2", "body": ASSUMED });
    rig.claude.script_for(Role::Triager, [reply(found)]);

    assert_eq!(step(&runner).unwrap(), None);

    let asked = &calls(&rig, Role::Triager)[0].call;
    assert!(
        asked
            .prompt
            .contains("`Assumed: <the answer>. Say otherwise.`"),
        "{}",
        asked.prompt
    );
    assert!(
        (asked.plugin_dirs.iter()).any(|d| d.ends_with(crate::skills::issue::PLUGIN)),
        "the issue skill is the format: {:?}",
        asked.plugin_dirs
    );
    assert_eq!(
        rig.github.issue_edits(),
        [(30, ASSUMED.trim_end().to_owned())]
    );
    assert_eq!(
        moves(&rig),
        [
            "make kind:feat",
            "comment",
            "label bug",
            "label P2",
            "label kind:feat",
            "label ready-for-agent"
        ],
        "no question is asked, and the kind the repo lacked is made"
    );

    // The pass's end, checked again, passes: nothing more is written.
    let labels = ["bug", "P2", "kind:feat", "ready-for-agent"];
    list(&rig, filed(&rig, MAINTAINER, ASSUMED, &labels));
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.github.comments().len(), 1);
    assert_eq!(moves(&rig).len(), 6);
}

#[test]
fn a_maintainers_edit_made_while_the_triager_ran_survives_its_rewrite() {
    let edited = "It crashes when I run it, on Linux.";
    for rewrite in [ASSUMED, STILL] {
        let (rig, runner) = checking("pick = \"maintainer\"", MAINTAINER, FAILING, &[]);
        let found = json!({ "type": "bug", "kind": "fix", "priority": "P2", "body": rewrite });
        let hold = Hold::default();
        rig.claude.script_for(
            Role::Triager,
            [Scripted::HoldThenSay(hold.clone(), said(found))],
        );

        assert!(matches!(advance(&runner).unwrap(), Pass::Started));
        assert!(hold.entered(CALL_PATIENCE), "the research never began");
        list(&rig, filed(&rig, MAINTAINER, edited, &[]));
        hold.release();
        pump(&runner, || rig.github.comments().len() == 2);

        assert_eq!(rig.github.issue_edits(), [], "{rewrite}");
        let notice = &rig.github.comments()[1].1;
        assert!(
            notice.contains("it was edited while kelpie read it"),
            "{notice}"
        );
        assert!(!moves(&rig).contains(&"label ready-for-agent".to_owned()));

        // The edit is checked at the next look, and triaged as any edit is.
        rig.claude.script_for(Role::Triager, [reply(json!({}))]);
        rig.claude
            .script_for(Role::Representative, [Scripted::Say(WORDS)]);
        let labels = ["bug", "P2", "kind:fix", "needs-triage"];
        list(&rig, filed(&rig, MAINTAINER, edited, &labels));
        rig.next_look();
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(calls(&rig, Role::Triager).len(), 2, "{rewrite}");
    }
}

#[test]
fn an_edit_that_passes_makes_the_issue_ready_with_no_second_call() {
    let (rig, runner) = told();
    let mut labels = PASSING_LABELS.to_vec();
    labels.push("needs-triage");
    list(&rig, filed(&rig, MAINTAINER, PASSING, &labels));
    rig.next_look();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(moves(&rig).last().unwrap(), "label ready-for-agent");
    assert_eq!(
        rig.github.label_takes().last().unwrap(),
        &(30, "needs-triage".to_owned())
    );
}

#[test]
fn an_edit_that_still_fails_is_triaged_again() {
    let (rig, runner) = told();
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    let edited = "It crashes when I run it, on Linux.";
    list(&rig, filed(&rig, MAINTAINER, edited, &["needs-triage"]));
    rig.next_look();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 2, "the edit is triaged");
    assert_eq!(rig.github.comments().len(), 3);
}

#[test]
fn another_persons_failing_issue_is_triaged_only_where_the_pick_reaches_it() {
    let (rig, runner) = checking("pick = \"maintainer\"", "newcomer", FAILING, &[]);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(calls(&rig, Role::Triager).len(), 0);
    assert_eq!(moves(&rig), Vec::<String>::new(), "nothing is written");
    assert_eq!(checks(&rig)[0]["triage"], "owed");

    let (rig, runner) = checking("pick = \"all\"", "newcomer", FAILING, &[]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
}

// The board reads `ready-for-agent` issues on a list of their own, which
// gives issue 30 this title and body and the labels the forge holds on it
fn ready_on_the_board(rig: &Rig, body: &str, labels: &[&str]) {
    rig.forge.list_ready(30, false);
    rig.forge.set_ready_body(30, body);
    for label in labels {
        rig.forge.label(30, label);
    }
    let mut all = vec!["ready-for-agent"];
    all.extend(labels);
    let mut listed = filed(rig, MAINTAINER, body, &all);
    listed.title = "Title of #30".to_owned();
    list(rig, listed);
}

#[test]
fn a_ready_issue_the_check_refuses_is_held_off_the_board_until_an_edit_passes() {
    let rig = rig_with("pick = \"off\"");
    ready_on_the_board(&rig, FAILING, &[]);
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None, "nothing starts");

    let skipped = rig.ask(&runner, "status", None)["skipped"].clone();
    assert_eq!(skipped[0]["reason"], "check", "{skipped}");
    assert_eq!(skipped[0]["issue"], "30", "{skipped}");
    let failures = skipped[0]["failures"].as_array().unwrap();
    assert!(
        failures
            .iter()
            .any(|f| f.as_str().unwrap().contains("Acceptance criteria")),
        "{skipped}"
    );

    rig.forge.unlist_ready(30);
    ready_on_the_board(&rig, PASSING, &PASSING_LABELS);
    rig.next_look();
    let (started, _) = dispatched(step(&runner).unwrap());
    assert_eq!(started.github(), Some(30));
}

#[test]
fn a_ready_issue_the_check_has_not_read_is_checked_where_the_board_picks() {
    let rig = rig_with("pick = \"off\"");
    rig.forge.list_ready(30, false);
    rig.forge.set_ready_body(30, FAILING);
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None, "nothing starts");

    let skipped = rig.ask(&runner, "status", None)["skipped"].clone();
    assert_eq!(skipped[0]["reason"], "check", "{skipped}");
    assert_eq!(checks(&rig), Value::Null, "no result was kept for it");
}

// The maintainer's issue 30 titled `title`, with `labels`, passing the check but
// for what its labels and title give it; the repo holds `REPO_LABELS`
fn titled(title: &str, labels: &[&str]) -> (Rig, Mutex<Runner>) {
    let rig = rig_with("pick = \"off\"");
    rig.forge.set_repo_labels(&REPO_LABELS);
    let mut filed = filed(&rig, MAINTAINER, PASSING, labels);
    filed.title = title.to_owned();
    list(&rig, filed);
    let runner = rig.open().unwrap();
    (rig, runner)
}

const NO_KIND: [&str; 1] = ["priority: P2"];

#[test]
fn a_typed_title_with_no_kind_label_gains_it_and_passes() {
    for (title, kind) in [
        ("fix(board): crash", "fix"),
        ("feat!: a thing", "feat"),
        ("feat(scope)!: a thing", "feat"),
    ] {
        let (rig, runner) = titled(title, &NO_KIND);

        assert_eq!(step(&runner).unwrap(), None, "{title}");

        let mut want = vec![
            format!("label kind:{kind}"),
            "label ready-for-agent".to_owned(),
        ];
        if kind == "feat" {
            want.insert(0, "make kind:feat".to_owned());
        }
        assert_eq!(moves(&rig), want, "{title}");
        assert!(checks(&rig)[0].get("failures").is_none(), "{title}");

        // The next listing carries the label, and is the same edit.
        let mut relisted = filed(&rig, MAINTAINER, PASSING, &NO_KIND);
        relisted.title = title.to_owned();
        relisted.labels.push(format!("kind:{kind}"));
        list(&rig, relisted);
        rig.next_look();
        assert_eq!(step(&runner).unwrap(), None, "{title}");
        assert_eq!(moves(&rig), want, "{title}");
    }
}

#[test]
fn a_title_that_names_no_kind_gains_no_label() {
    for title in [
        "Crash on start",
        "build: bump a thing",
        "revert: a thing",
        "feat: ",
    ] {
        let (rig, runner) = titled(title, &NO_KIND);

        assert_eq!(step(&runner).unwrap(), None, "{title}");

        let labelled: Vec<String> = moves(&rig);
        assert!(
            labelled.iter().all(|m| !m.starts_with("label kind:")),
            "{title}: {labelled:?}"
        );
    }
}

#[test]
fn an_existing_kind_label_is_kept_even_if_the_title_disagrees() {
    let (rig, runner) = titled("feat: a thing", &["kind:fix", "priority: P2"]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(moves(&rig), ["label ready-for-agent"]);
}

#[test]
fn a_label_that_cannot_be_added_is_noted_once_and_the_issue_is_checked_as_it_stands() {
    let (rig, runner) = titled("fix: crash", &NO_KIND);
    rig.github.fail_next(ApiError::Refused(422));

    assert_eq!(step(&runner).unwrap(), None);
    let mut notes = runner.lock().unwrap().take_notes();
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    notes.extend(runner.lock().unwrap().take_notes());

    let said = notes.iter().filter(|n| n.contains("`kind:fix`")).count();
    assert_eq!(said, 1, "{notes:?}");
    // The title carries the kind, so the issue passes as it did before.
    assert_eq!(moves(&rig).last().unwrap(), "label ready-for-agent");
}
