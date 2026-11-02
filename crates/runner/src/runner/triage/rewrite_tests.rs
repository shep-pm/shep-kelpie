//! The maintainer's issue whose rewrite the issue check refuses, given one repair
//! round in the triager's own session, through the runner's stand-ins

use serde_json::json;

use super::marker::MARKER;
use super::pick_tests::MAINTAINER;
use super::ready_tests::filed;
use super::research_tests::{WORDS, calls, moves, reply, triage, under};
use crate::ports::{Role, Session};
use crate::runner::step;
use crate::test::Scripted;

// What the drafts of shep#1398, #1399 and #1418 held: the files and the work,
// under headings the check does not read
const WRONG_HEADINGS: &str = "## Files\n\n- `src/start.rs`\n\n## What\n\n- Fix the crash on \
    start.\n\n## Acceptance criteria\n\n- [ ] `runner::start::it_starts`\n";

// The same draft in the shape the check reads
const RIGHT_HEADINGS: &str = "Touches: `src/start.rs`\n\n## What to build\n\n- Fix the crash on \
    start.\n\n## Acceptance criteria\n\n- [ ] `runner::start::it_starts`\n";

fn drafted(body: &str) -> Scripted {
    reply(json!({ "type": "bug", "kind": "fix", "priority": "P2", "body": body }))
}

#[test]
fn a_rewrite_under_the_wrong_headings_is_repaired_in_its_session_and_goes_ready() {
    let (rig, runner) = filed("maintainer", MAINTAINER);
    rig.claude.script_for(
        Role::Triager,
        [drafted(WRONG_HEADINGS), drafted(RIGHT_HEADINGS)],
    );

    assert_eq!(step(&runner).unwrap(), None);

    let asked = calls(&rig, Role::Triager);
    assert_eq!(asked.len(), 2, "the repair is the only second call");
    assert_eq!(
        asked[1].call.session,
        Session::Resume(asked[0].call.session.id().clone())
    );
    // The repair says what the check refused, in the check's own words.
    let prompt = &asked[1].call.prompt;
    assert!(prompt.contains("`Touches:` line"), "{prompt}");
    assert!(prompt.contains("## What to build"), "{prompt}");
    assert_eq!(
        rig.github.issue_edits(),
        [(30, RIGHT_HEADINGS.trim_end().to_owned())],
        "the repaired draft goes over the issue's body"
    );
    assert_eq!(
        rig.github.comments(),
        [(30, format!("{WORDS}\n\n{MARKER}"))]
    );
    assert_eq!(
        moves(&rig),
        [
            "comment",
            "label bug",
            "label P2",
            "label kind:fix",
            "label ready-for-agent"
        ]
    );
    assert_eq!(triage(&rig, &runner)["issues"][0]["step"], "ready");
}

#[test]
fn a_rewrite_that_fails_the_check_twice_goes_under_the_text_and_waits_at_needs_triage() {
    let (rig, runner) = filed("maintainer", MAINTAINER);
    rig.claude.script_for(
        Role::Triager,
        [drafted(WRONG_HEADINGS), drafted(WRONG_HEADINGS)],
    );

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        2,
        "one repair and no more"
    );
    assert_eq!(
        rig.github.issue_edits(),
        [(
            30,
            under("It crashes when I run it.", WRONG_HEADINGS.trim_end())
        )]
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
    assert_eq!(triage(&rig, &runner)["issues"][0]["step"], "done");
}

#[test]
fn a_rewrite_the_check_passes_is_not_repaired() {
    let (rig, runner) = filed("maintainer", MAINTAINER);
    rig.claude
        .script_for(Role::Triager, [drafted(RIGHT_HEADINGS)]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 1);
}

#[test]
fn an_outside_issue_is_not_repaired_since_kelpie_never_rewrites_it() {
    let (rig, runner) = filed("all", "newcomer");
    rig.claude
        .script_for(Role::Triager, [drafted(WRONG_HEADINGS)]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 1);
}
