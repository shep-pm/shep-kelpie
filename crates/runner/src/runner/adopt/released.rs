//! Adopting a contribution its contributor released (ADR 0010)

use std::sync::Mutex;

use serde_json::{Value, json};

use super::*;
use crate::ports::{CLEAN, Checks};
use crate::runner::contributions::tests::picking;
use crate::runner::{StepReport, step};
use crate::state::{How, Released};
use crate::test::{Rig, Scripted, git, ticket};

// The fork holds the pull request's head under the name the forge reports
pub(in crate::runner) const FORK_BRANCH: &str = "topic/80";

// What a case does to the forge before pull request 80 is adopted
type Setup = fn(&Rig);

// Pull request 80 by `newcomer`, closing issue 5, followed by a runner whose
// project picks every contribution. Its branch on origin is `topic/80`.
pub(in crate::runner) fn contributed() -> (Rig, Mutex<Runner>) {
    let (rig, runner) = picking("all");
    rig.push_by_hand("topic/80", "work.txt");
    rig.forge.open_pull_request(80, "topic/80", &[5]);
    rig.forge.set_author(80, "newcomer");
    rig.forge.ready_pull_request(80);
    assert_eq!(step(&runner).unwrap(), None);
    (rig, runner)
}

// Records pull request 80's release, as the release stage does
pub(in crate::runner) fn release(runner: &Mutex<Runner>) {
    let mut runner = runner.lock().unwrap();
    let mut next = runner.state.clone();
    let held = (next.contributions.iter_mut()).find(|c| c.pull_request == 80);
    held.expect("a followed contribution").released = Some(Released {
        at: Timestamp(1),
        how: How::Silence,
    });
    runner.save(next).unwrap();
}

pub(in crate::runner) fn adopted() -> StepReport {
    StepReport::Adopted {
        issue: ticket(5),
        pull_request: 80,
        agent: "sonnet-high".to_owned().try_into().unwrap(),
    }
}

fn work_item(rig: &Rig, runner: &Mutex<Runner>) -> Value {
    rig.ask(runner, "status", None)["work_item"].clone()
}

#[test]
fn a_released_fork_contribution_is_adopted_and_its_worktree_is_at_the_forks_head() {
    let (rig, runner) = contributed();
    let (url, fork_head) = rig.push_to_fork(FORK_BRANCH, "fork.txt");
    rig.forge.set_fork(80, &url, true);
    let origin_head = rig.forge.head_of("topic/80").unwrap();
    assert_ne!(fork_head, origin_head);
    release(&runner);

    assert_eq!(rig.ask(&runner, "adopt", Some("80")).get("error"), None);
    assert_eq!(step(&runner).unwrap(), Some(adopted()));

    let state = runner.lock().unwrap().state.clone();
    let item = state.item(ticket(5)).expect("the adopted work item");
    assert_eq!(
        git(&rig.paths().worktree(ticket(5)), &["rev-parse", "HEAD"]),
        fork_head
    );
    assert_eq!(item.arrived.as_deref(), Some(fork_head.as_str()));
    assert_eq!(item.branch, "kelpie/5", "the fork's own name is not local");
    assert_eq!(
        item.remote,
        Remote::Fork {
            url,
            branch: FORK_BRANCH.to_owned()
        }
    );
    assert_eq!(
        rig.forge.head_of(FORK_BRANCH).unwrap(),
        origin_head,
        "nothing was fetched into or pushed to origin"
    );
}

#[test]
fn an_unreleased_contribution_is_still_refused_as_a_fork_or_another_author() {
    let cases: [(&str, Setup, &str); 2] = [
        (
            "fork",
            |rig| rig.forge.set_fork(80, "unused", true),
            "pull request #80 comes from a fork",
        ),
        (
            "author",
            |_| (),
            "pull request #80 was opened by newcomer, not by kelpie's account",
        ),
    ];
    for (name, setup, reason) in cases {
        let (rig, runner) = contributed();
        setup(&rig);

        assert_eq!(
            rig.ask(&runner, "adopt", Some("80")),
            json!({ "error": reason }),
            "{name}"
        );
        assert_eq!(work_item(&rig, &runner), Value::Null, "{name}");
    }
}

#[test]
fn a_released_branch_in_the_repo_itself_by_another_author_is_adopted() {
    let (rig, runner) = contributed();
    let head = rig.forge.head_of("topic/80").unwrap();
    release(&runner);

    assert_eq!(rig.ask(&runner, "adopt", Some("80")).get("error"), None);
    assert_eq!(step(&runner).unwrap(), Some(adopted()));

    let state = runner.lock().unwrap().state.clone();
    let item = state.item(ticket(5)).expect("the adopted work item");
    assert_eq!(item.branch, "topic/80");
    assert_eq!(
        item.remote,
        Remote::Origin,
        "a branch of origin's is origin's"
    );
    assert_eq!(
        git(&rig.paths().worktree(ticket(5)), &["rev-parse", "HEAD"]),
        head
    );
}

#[test]
fn a_released_fork_without_edits_cannot_be_adopted() {
    let (rig, runner) = contributed();
    let (url, _) = rig.push_to_fork(FORK_BRANCH, "fork.txt");
    rig.forge.set_fork(80, &url, false);
    release(&runner);

    assert_eq!(
        rig.ask(&runner, "adopt", Some("80")),
        json!({ "error": "pull request #80 comes from a fork kelpie cannot push to: it does not let maintainers edit it, it has been deleted, or an organization owns it" })
    );
    assert_eq!(work_item(&rig, &runner), Value::Null);
}

#[test]
fn a_catch_up_with_main_on_an_adopted_fork_merges_into_the_fork_and_not_origin() {
    let (rig, runner) = contributed();
    let (url, fork_head) = rig.push_to_fork(FORK_BRANCH, "fork.txt");
    rig.forge.set_fork(80, &url, true);
    release(&runner);
    assert_eq!(rig.ask(&runner, "adopt", Some("80")).get("error"), None);
    assert_eq!(step(&runner).unwrap(), Some(adopted()));
    let landed = rig.land_on_origin("landed.txt");
    rig.forge.set_checks(&fork_head, Checks::Passed);

    // The adopted pull request is read by the project's reviewers first, and
    // the catch-up with `main` comes at the CI gate once that pass ends.
    let mut merged = None;
    for _ in 0..12 {
        if let Some(StepReport::Rebased { head, .. }) = step(&runner).unwrap() {
            merged = Some(head);
            break;
        }
    }
    let Some(merged) = merged else {
        panic!("the fork's branch was not caught up");
    };

    let fork = std::path::Path::new(&url);
    assert_eq!(
        git(fork, &["rev-parse", &format!("refs/heads/{FORK_BRANCH}")]),
        merged,
        "the merge went to the fork"
    );
    assert_eq!(
        git(
            fork,
            &["rev-parse", &format!("{merged}^1"), &format!("{merged}^2")]
        ),
        format!("{fork_head}\n{landed}")
    );
    assert_eq!(
        rig.forge.head_of("kelpie/5"),
        None,
        "nothing reached origin"
    );
}

// The issue the pull request closes is the work item's own, and kelpie files
// nothing.
#[test]
fn a_pull_request_that_closes_an_issue_is_adopted_against_that_one() {
    let (rig, runner) = contributed();
    release(&runner);
    assert_eq!(rig.ask(&runner, "adopt", Some("80")).get("error"), None);

    assert_eq!(step(&runner).unwrap(), Some(adopted()));

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["issue"], "5");
    assert!(rig.forge.created().is_empty(), "no issue was filed");
    assert_eq!(
        rig.github.issues_created(),
        0,
        "none through the App either"
    );
    assert_eq!(
        rig.forge.pull_request_body(80),
        "Body of pull request #80.\n",
        "its body is left as its contributor wrote it"
    );
}

// A released contribution is kelpie's own work item now: the project's whole
// review pass reads it, where an adopted pull request of
// kelpie's own owes only the review bots the read they are owed.
#[test]
fn the_adopted_work_item_runs_the_projects_whole_review_pass() {
    let (rig, runner) = contributed();
    let head = rig.forge.head_of("topic/80").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    rig.claude.script([Scripted::Text(CLEAN)]);
    release(&runner);
    assert_eq!(rig.ask(&runner, "adopt", Some("80")).get("error"), None);

    assert_eq!(step(&runner).unwrap(), Some(adopted()));
    for _ in 0..8 {
        step(&runner).unwrap();
    }

    let told: Vec<String> = (rig.github.comments().into_iter())
        .map(|(_, body)| body)
        .collect();
    assert_eq!(rig.reviewer.seen().len(), 1, "the qwen round ran: {told:?}");
    assert!(
        told.iter()
            .any(|c| c.contains("qwen read this pull request")),
        "{told:?}"
    );
    assert!(
        told.iter()
            .any(|c| c.contains("claude read this pull request")),
        "{told:?}"
    );
}
