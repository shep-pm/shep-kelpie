use std::sync::Arc;

use super::*;
use crate::github::Verb;
use crate::test::{Asked, FakeClock, FakeGithub};

fn slug(s: &str) -> ForgeSlug {
    ForgeSlug::try_from(s.to_owned()).unwrap()
}

// Kelpie's App registered for `shep-pm` and installed on `shep-pm/koji`.
fn voiced() -> (AppVoice, FakeGithub, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let github = FakeGithub::new(FakeClock::at(1_000_000), "shep-pm");
    github.registered(home.path(), "shep-pm/koji");
    let tokens = Arc::new(github.tokens(home.path()));
    (
        AppVoice::new(tokens, Box::new(github.clone())),
        github,
        home,
    )
}

fn writes(github: &FakeGithub) -> Vec<(String, Call)> {
    (github.asked().into_iter())
        .filter_map(|asked| match asked {
            Asked::Write(token, call) => Some((token, call)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_comment_is_posted_on_its_thread_with_the_apps_token() {
    let (voice, github, _home) = voiced();

    voice.comment(&slug("shep-pm/koji"), 12, "hello").unwrap();

    assert_eq!(
        writes(&github),
        [(
            "ghs_test1".to_owned(),
            Call {
                verb: Verb::Post,
                path: "/repos/shep-pm/koji/issues/12/comments".to_owned(),
                body: Some(r#"{"body":"hello"}"#.to_owned()),
            }
        )]
    );
}

#[test]
fn an_issue_is_opened_with_its_labels_and_its_number_read_back() {
    let (voice, github, _home) = voiced();

    let number = voice
        .create_issue(&slug("shep-pm/koji"), "t", "b", &["needs-triage"])
        .unwrap();

    assert_eq!(number, 900);
    let [(_, call)] = writes(&github).try_into().unwrap();
    assert_eq!(call.path, "/repos/shep-pm/koji/issues");
    assert_eq!(
        call.body.as_deref(),
        Some(r#"{"body":"b","labels":["needs-triage"],"title":"t"}"#)
    );
}

#[test]
fn a_label_is_made_and_put_on_and_taken_off_an_issue() {
    let (voice, github, _home) = voiced();
    let koji = slug("shep-pm/koji");

    let new = NewLabel {
        name: "needs-triage",
        color: "ededed",
        description: "Read first",
    };
    voice.create_label(&koji, &new).unwrap();
    voice
        .set_issue_label(&koji, 5, "agent:opus high", true)
        .unwrap();
    voice
        .set_issue_label(&koji, 5, "agent:opus high", false)
        .unwrap();

    let calls: Vec<_> = writes(&github).into_iter().map(|(_, c)| c).collect();
    assert_eq!(
        (calls[0].verb, calls[0].path.as_str()),
        (Verb::Post, "/repos/shep-pm/koji/labels")
    );
    assert_eq!(
        calls[0].body.as_deref(),
        Some(r#"{"color":"ededed","description":"Read first","name":"needs-triage"}"#)
    );
    assert_eq!(calls[1].path, "/repos/shep-pm/koji/issues/5/labels");
    assert_eq!(
        calls[1].body.as_deref(),
        Some(r#"{"labels":["agent:opus high"]}"#)
    );
    assert_eq!(
        (
            calls[2].verb,
            calls[2].path.as_str(),
            calls[2].body.as_deref()
        ),
        (
            Verb::Delete,
            "/repos/shep-pm/koji/issues/5/labels/agent%3Aopus%20high",
            None
        ),
        "a label's colon and space cannot end its path segment"
    );
}

#[test]
fn a_label_the_issue_lacks_is_as_good_as_taken_off() {
    let (voice, github, _home) = voiced();
    let koji = slug("shep-pm/koji");
    // The token's mint is the first two calls; the removal is refused as absent.
    voice
        .set_issue_label(&koji, 5, "in-progress", true)
        .unwrap();
    github.fail_next(ApiError::Refused(404));

    assert_eq!(
        voice.set_issue_label(&koji, 5, "in-progress", false),
        Ok(())
    );
}

#[test]
fn a_repo_is_covered_only_where_the_owner_has_an_app_installed() {
    let (voice, github, _home) = voiced();

    assert!(voice.covers(&slug("shep-pm/koji")));
    assert!(
        !voice.covers(&slug("shep-pm/golbat")),
        "not installed there"
    );
    assert!(!voice.covers(&slug("other/koji")), "no App for that owner");
    assert_eq!(writes(&github), []);
}

#[test]
fn an_app_that_cannot_mint_now_still_covers_its_repo_and_its_write_fails() {
    let (voice, github, _home) = voiced();
    github.fail_next(ApiError::Unreachable("down".to_owned()));
    github.fail_next(ApiError::Unreachable("down".to_owned()));

    assert!(
        voice.covers(&slug("shep-pm/koji")),
        "set, though GitHub is out of reach"
    );
    let error = voice.comment(&slug("shep-pm/koji"), 1, "x").unwrap_err();
    assert!(matches!(error, ForgeError::App(_)), "{error:?}");
    assert_eq!(writes(&github), [], "nothing is posted in its place");
}

#[test]
fn a_refused_write_says_what_github_answered() {
    let (voice, github, _home) = voiced();
    voice.comment(&slug("shep-pm/koji"), 1, "x").unwrap();
    github.fail_next(ApiError::Refused(403));

    let error = voice.comment(&slug("shep-pm/koji"), 1, "y").unwrap_err();

    assert_eq!(
        error.to_string(),
        "the GitHub App could not post: GitHub answered HTTP 403"
    );
}
