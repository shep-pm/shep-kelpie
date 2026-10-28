//! A released contribution, and a quiet one kelpie cannot push to
//!
//! A contribution on a fork whose owner allows no edits is the maintainer's to
//! decide on. One kelpie can push to is released and taken over: kelpie files
//! the issue its work item hangs on when the pull request closes none, names
//! it on the pull request, and the record becomes a work item
//! ([`adopt`](super::adopt)).

use std::sync::Mutex;

use serde_json::{Value, json};

use super::release_tests::{
    ASKS, TAKEN, THANKED, YES, comments, held, later, polls, released, rulings, waiting,
};
use super::ruling_tests::rule;
use crate::github::ApiError;
use crate::ports::Tickets;
use crate::runner::Runner;
use crate::test::{Rig, ticket};

// Pull request 30, asked once and quiet through the whole window, so it is
// released and kelpie takes it over
fn taken_over() -> (Rig, Mutex<Runner>) {
    let (rig, runner) = waiting(&[ASKS, TAKEN]);
    later(&rig, &runner, 3);
    later(&rig, &runner, 3);
    later(&rig, &runner, 1);
    (rig, runner)
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

// A contribution asked once and quiet since, on a fork whose owner allows
// edits only if `edits`, one day short of `quiet_days`. The fork is the rig's
// own and holds the very commit the pull request's head is, so a released one
// can really be taken over.
fn nearly_quiet(edits: bool) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = waiting(&[ASKS, if edits { TAKEN } else { THANKED }]);
    let url = rig.fork_at_origin("topic/30");
    rig.forge.set_fork(30, &url, edits);
    later(&rig, &runner, 3);
    later(&rig, &runner, 3);
    (rig, runner)
}

// A contribution on a fork without edits, quiet for `quiet_days`: its ruling
// waits for the maintainer
fn without_edits() -> (Rig, Mutex<Runner>) {
    let (rig, runner) = nearly_quiet(false);
    later(&rig, &runner, 1);
    (rig, runner)
}

fn followed(runner: &Mutex<Runner>) -> Vec<u64> {
    let state = runner.lock().unwrap().state.clone();
    state.contributions.iter().map(|c| c.pull_request).collect()
}

#[test]
fn a_fork_without_maintainer_edits_raises_the_leave_open_or_close_ruling_at_quiet_days() {
    let (rig, runner) = nearly_quiet(false);
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new(), "not yet quiet");

    later(&rig, &runner, 1);

    let waiting = rulings(&rig, &runner);
    assert_eq!(waiting.len(), 1, "{waiting:?}");
    assert_eq!(waiting[0]["kind"]["kind"], "contribution");
    assert_eq!(waiting[0]["kind"]["reason"], "no-edits");
    let question = waiting[0]["question"].as_str().unwrap();
    for option in ["leave it open", "close with thanks"] {
        assert!(question.contains(option), "{option}: {question}");
    }
    assert!(question.contains("deleted"), "{question}");
    assert!(!question.contains("  "), "no run of spaces: {question}");
    assert_eq!(comments(&rig), [ASKS], "nothing is posted for it");
    assert_eq!(followed(&runner), [30], "still followed until it is ruled");
    polls(&rig, &runner);
    assert_eq!(rulings(&rig, &runner).len(), 1, "raised once");
}

#[test]
fn a_fork_an_organization_owns_raises_the_no_edits_ruling_at_quiet_days() {
    let (rig, runner) = nearly_quiet(true);
    rig.forge.set_fork_owned_by_organization(30);
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new(), "not yet quiet");

    later(&rig, &runner, 1);

    let waiting = rulings(&rig, &runner);
    assert_eq!(waiting.len(), 1, "{waiting:?}");
    assert_eq!(waiting[0]["kind"]["reason"], "no-edits");
    let question = waiting[0]["question"].as_str().unwrap();
    assert!(
        question.contains("belongs to an organization"),
        "{question}"
    );
    assert_eq!(comments(&rig), [ASKS], "nothing is posted for it");
    assert_eq!(followed(&runner), [30], "still followed until it is ruled");
}

#[test]
fn a_fork_with_maintainer_edits_is_released_at_quiet_days_as_before() {
    let (rig, runner) = nearly_quiet(true);
    later(&rig, &runner, 1);

    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
    let record = held(&rig, &runner);
    assert_eq!(record["released"]["how"], "silence");
}
#[test]
fn leave_open_stops_watching_it_and_posts_nothing() {
    let (rig, runner) = without_edits();

    let ruled = rule(&rig, &runner, "30 answer leave it open");
    assert_eq!(ruled.get("error"), None, "{ruled}");
    polls(&rig, &runner);

    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
    assert_eq!(comments(&rig), [ASKS], "nothing was posted");
    assert_eq!(rig.forge.pull_request_closings(), Vec::<u64>::new());
    assert_eq!(followed(&runner), Vec::<u64>::new(), "no longer followed");
    later(&rig, &runner, 30);
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
    assert_eq!(comments(&rig), [ASKS], "still nothing");
    assert_eq!(followed(&runner), Vec::<u64>::new(), "not followed afresh");
}

#[test]
fn close_with_thanks_posts_thanks_and_closes_it() {
    let (rig, runner) = without_edits();

    let ruled = rule(&rig, &runner, "30 answer close with thanks");
    assert_eq!(ruled.get("error"), None, "{ruled}");
    polls(&rig, &runner);

    assert_eq!(comments(&rig), [ASKS, THANKED]);
    assert_eq!(rig.forge.pull_request_closings(), [30]);
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
    polls(&rig, &runner);
    assert_eq!(comments(&rig).len(), 2, "thanked once");
    assert_eq!(rig.forge.pull_request_closings(), [30], "closed once");
}

#[test]
fn a_reply_met_at_quiet_days_is_read_before_a_fork_without_edits_is_ruled_on() {
    let (rig, runner) = waiting(&[ASKS, YES, TAKEN]);
    let url = rig.fork_at_origin("topic/30");
    rig.forge.set_fork(30, &url, false);
    later(&rig, &runner, 3);
    later(&rig, &runner, 3);
    rig.writes_on(30, "newcomer", "Yes, please go ahead.");

    let record = released(&rig, &runner);
    polls(&rig, &runner);

    assert_eq!(record["released"]["how"], "yes", "the reply was read");
    let waiting = rulings(&rig, &runner);
    assert_eq!(waiting.len(), 1, "{waiting:?}");
    assert_eq!(
        waiting[0]["kind"]["reason"], "no-edits",
        "kelpie cannot push to it, so the maintainer decides"
    );
    assert_eq!(comments(&rig), [ASKS, TAKEN]);
}

#[test]
fn a_contribution_first_asked_at_quiet_days_is_asked_and_not_ruled_on() {
    let (rig, runner) = waiting(&[ASKS]);
    rig.forge
        .set_fork(30, "https://example.test/fork.git", false);

    later(&rig, &runner, 7);

    assert_eq!(
        comments(&rig),
        [ASKS],
        "asked, with the whole window to answer"
    );
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
}

// A released contribution whose pull request closes no issue: kelpie files one
// from its title and body, with no `agent:` label so the agent board picks at
// dispatch, and names it on the pull request so the merge closes it.
#[test]
fn a_pull_request_that_closes_no_issue_gets_one_filed_and_resolves_is_added_to_its_body() {
    let (rig, runner) = taken_over();

    let status = rig.ask(&runner, "status", None);
    let issue = status["work_item"]["issue"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let filed = filed(&rig);
    assert_eq!(filed.len(), 1, "{filed:?}");
    assert_eq!(filed[0]["title"], "Title of pull request #30");
    let body = filed[0]["body"].as_str().unwrap();
    assert!(body.contains("pull request #30"), "{body}");
    assert!(
        body.contains("Body of pull request #30."),
        "the contributor's own body: {body}"
    );
    assert_eq!(filed[0]["labels"], json!([]));
    let edited = rig.forge.pull_request_body(30);
    assert_eq!(
        edited,
        format!("Body of pull request #30.\n\nResolves #{issue}"),
        "the body keeps what its contributor wrote"
    );
    assert_eq!(status["work_item"]["pull_request"], 30);
}

// A filing or a body edit the forge refuses is told in the log and tried at
// the next poll, and the pull request is queued only once both went through.
#[test]
fn a_failed_issue_or_body_edit_is_retried_at_the_next_poll() {
    let (rig, runner) = waiting(&[ASKS, TAKEN]);
    rig.github.refuse_issue_creates(ApiError::Refused(502));
    later(&rig, &runner, 3);
    later(&rig, &runner, 3);
    later(&rig, &runner, 1);

    let told = runner.lock().unwrap().take_notes();
    assert!(
        told.iter()
            .any(|n| n.contains("contribution #30: cannot file its issue")),
        "{told:?}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(rig.github.issues_created(), 0, "nothing was filed");
    assert_eq!(status["adopted"], json!([]), "nothing is queued");
    assert_eq!(status["work_item"], Value::Null, "nothing was adopted");
    assert_eq!(
        held(&rig, &runner)["released"]["how"],
        "silence",
        "the release stands"
    );

    // The issue goes out, and the body the forge will not let kelpie edit.
    rig.github.accept_issue_creates();
    rig.forge.set_pull_request_edit_down(true);
    later(&rig, &runner, 1);

    let asked = filed(&rig);
    assert_eq!(rig.github.issues_created(), 1, "filed once");
    assert_eq!(asked.len(), 2, "the refused try and the one that went out");
    let told = runner.lock().unwrap().take_notes();
    assert!(
        told.iter()
            .any(|n| n.contains("contribution #30: cannot add `Resolves #")),
        "{told:?}"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["adopted"],
        json!([]),
        "not queued until the body names the issue"
    );

    rig.forge.set_pull_request_edit_down(false);
    later(&rig, &runner, 1);

    let status = rig.ask(&runner, "status", None);
    let issue = status["work_item"]["issue"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert_eq!(rig.github.issues_created(), 1, "no second issue");
    assert_eq!(
        rig.forge.pull_request_body(30),
        format!("Body of pull request #30.\n\nResolves #{issue}")
    );
    assert_eq!(status["work_item"]["pull_request"], 30);
}

// The record goes the way a contribution leaves: at the next look, with its
// rulings withdrawn and nothing posted for it.
#[test]
fn the_contributions_record_is_replaced_by_the_work_item() {
    let (rig, runner) = taken_over();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["pull_request"], 30);
    assert_eq!(
        status["contributions"][0]["pull_request"], 30,
        "the record stands until the next look"
    );

    later(&rig, &runner, 1);

    let status = rig.ask(&runner, "status", None);
    assert_eq!(followed(&runner), Vec::<u64>::new(), "the record is gone");
    assert_eq!(status["work_items"].as_array().unwrap().len(), 1);
    assert_eq!(status["work_item"]["pull_request"], 30);
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
    assert_eq!(
        (comments(&rig).into_iter()).filter(|c| c == TAKEN).count(),
        1,
        "released once, said once, and nothing for the record leaving"
    );
    later(&rig, &runner, 7);
    assert_eq!(
        followed(&runner),
        Vec::<u64>::new(),
        "the pull request is not a contribution afresh"
    );
}

// The release queues the adoption and the board's step starts it in the same
// poll, in the free slot, ahead of the ready issue it would otherwise take.
#[test]
fn a_release_starts_an_adoption_in_the_next_free_slot_ahead_of_new_issues() {
    let (rig, runner) = waiting(&[ASKS, TAKEN]);
    later(&rig, &runner, 3);
    later(&rig, &runner, 3);
    rig.forge.list_ready(7, false);

    later(&rig, &runner, 1);

    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_items"].as_array().unwrap().len(),
        1,
        "the one slot went to the released pull request, not to #7"
    );
    assert_eq!(status["work_item"]["pull_request"], 30);
    assert_eq!(status["work_item"]["adopted"], json!(true));
}

// The issue kelpie filed for a pull request that closed none stays the one its
// work item hangs on, even where its contributor names an issue before it
// starts. The filed one is the one the pull request's body carries, so it
// closes with the merge and none is left open beside the work.
#[test]
fn a_pull_request_that_closes_an_issue_after_kelpie_filed_one_is_adopted_against_the_filed_one() {
    let (rig, runner) = waiting(&[ASKS, TAKEN]);
    rig.forge.set_pull_request_edit_down(true);
    later(&rig, &runner, 3);
    later(&rig, &runner, 3);
    later(&rig, &runner, 1);

    let record = held(&rig, &runner);
    assert_eq!(record["released"]["how"], "silence", "{record}");
    let filed = record["adopting"]["issue"].as_u64().unwrap();
    assert_eq!(
        record["adopting"].get("resolves"),
        None,
        "the body edit was refused: {record}"
    );
    assert_eq!(rig.ask(&runner, "status", None)["adopted"], json!([]));

    // Its contributor names an issue of their own before the work item starts.
    rig.forge.set_closes(30, &[5]);
    rig.forge.set_pull_request_edit_down(false);
    later(&rig, &runner, 1);

    let status = rig.ask(&runner, "status", None);
    let issue = status["work_item"]["issue"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert_eq!(status["work_item"]["pull_request"], 30);
    assert_eq!(
        issue, filed,
        "the issue kelpie filed, not the one named later"
    );
    let body = rig.forge.pull_request_body(30);
    assert!(body.ends_with(&format!("Resolves #{issue}")), "{body}");
}

// A released contribution whose pull request merges into another branch is no
// work item kelpie can start. Queueing it would have the board refuse it and
// post that refusal on the contributor's pull request, poll after poll: the
// maintainer is asked once, and nothing is filed or queued while that stands.
#[test]
fn a_released_pull_request_that_merges_into_another_branch_is_ruled_on_and_not_commented_on() {
    let (rig, runner) = waiting(&[ASKS, TAKEN]);
    rig.forge.set_base(30, "dev");

    later(&rig, &runner, 3);
    later(&rig, &runner, 3);
    later(&rig, &runner, 1);

    let asked = rulings(&rig, &runner);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0]["kind"]["reason"], "cannot-adopt");
    assert_eq!(asked[0]["issue"], Value::Null, "no work item");
    let why = asked[0]["kind"]["why"].as_str().unwrap();
    assert!(why.contains("`dev`"), "{why}");
    let question = asked[0]["question"].as_str().unwrap();
    for option in ["leave it open", "close with thanks"] {
        assert!(question.contains(option), "{option}: {question}");
    }
    assert_eq!(rig.github.issues_created(), 0, "no issue is filed for it");
    assert_eq!(rig.ask(&runner, "status", None)["adopted"], json!([]));
    assert_eq!(comments(&rig), [ASKS, TAKEN], "nothing on the pull request");

    polls(&rig, &runner);
    polls(&rig, &runner);
    assert_eq!(rulings(&rig, &runner).len(), 1, "asked once");
    assert_eq!(
        comments(&rig).len(),
        2,
        "no refusal comment on the contributor's pull request"
    );

    // Retargeting it at `main` is taken up at the next poll, and the ruling goes.
    rig.forge.set_base(30, "main");
    later(&rig, &runner, 1);

    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["pull_request"], 30);
    assert_eq!(status["work_item"]["adopted"], json!(true));
}

// A released contribution whose issue is labelled for an implementer the
// project does not list gives its work item no agent. The maintainer is asked,
// and listing the agent takes the adoption up.
#[test]
fn a_released_pull_request_whose_issue_names_an_unlisted_agent_is_ruled_on() {
    let (rig, runner) = waiting(&[ASKS, TAKEN]);
    rig.forge.set_closes(30, &[5]);
    rig.forge.label(5, "agent:nope");

    later(&rig, &runner, 3);
    later(&rig, &runner, 3);
    later(&rig, &runner, 1);

    let asked = rulings(&rig, &runner);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0]["kind"]["reason"], "cannot-adopt");
    let why = asked[0]["kind"]["why"].as_str().unwrap();
    assert!(why.contains("agent:nope"), "{why}");
    assert_eq!(comments(&rig), [ASKS, TAKEN], "nothing on the pull request");
    polls(&rig, &runner);
    assert_eq!(rulings(&rig, &runner).len(), 1, "asked once");

    let repo = rig.settings().git.remote.unwrap();
    rig.forge
        .set_issue_label(&repo, ticket(5), "agent:nope", false)
        .unwrap();
    later(&rig, &runner, 1);

    assert_eq!(
        rulings(&rig, &runner),
        Vec::<Value>::new(),
        "the reason is gone, and the ruling goes with it"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["pull_request"],
        30,
        "the issue its contributor's pull request closes"
    );
}
