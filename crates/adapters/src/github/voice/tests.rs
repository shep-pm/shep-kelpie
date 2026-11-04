use std::sync::Arc;

use super::*;
use crate::github::Verb;
use crate::ports::{
    CheckRunId, CheckState, Conclusion, Cooldown, GateCheck, LineComment, PostedComment, Timestamp,
};
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

// A worker holds a token for its repo where the App covers it, and none
// where it does not: it then runs as the maintainer's login, as before.
#[test]
fn a_worker_gets_a_token_only_where_the_app_covers_the_repo() {
    let (voice, github, _home) = voiced();

    let covered = voice.worker_token(&slug("shep-pm/koji")).unwrap();
    let not_installed = voice.worker_token(&slug("shep-pm/golbat")).unwrap();
    let no_app = voice.worker_token(&slug("someone/else")).unwrap();

    assert_eq!(
        covered.map(|t| t.expose().to_owned()).as_deref(),
        Some("ghs_test1")
    );
    assert_eq!((not_installed, no_app), (None, None));
    assert!(writes(&github).is_empty(), "minting a token posts nothing");
}

// An App that is set but cannot mint a token for a worker still covers the
// repo: the call fails and says so, rather than run as the maintainer.
#[test]
fn a_worker_is_not_handed_the_maintainers_login_when_the_app_cannot_mint() {
    let (voice, github, _home) = voiced();
    github.fail_next(ApiError::Refused(502));

    let err = voice.worker_token(&slug("shep-pm/koji")).unwrap_err();

    assert!(matches!(err, ForgeError::AppBusy(_)), "{err:?}");
}

// A refusal that asking again will not change is told apart from GitHub
// being busy, which waits.
#[test]
fn a_permission_the_app_lacks_is_no_reason_to_wait() {
    let (voice, github, _home) = voiced();
    github.refuse_workers(ApiError::Refused(422));

    let err = voice.worker_token(&slug("shep-pm/koji")).unwrap_err();

    assert!(matches!(err, ForgeError::App(_)), "{err:?}");
}

// What GitHub's headers said of when to ask again reaches the hold, whether
// the call that was refused was a write or the mint of a worker's token.
#[test]
fn a_rate_limited_app_call_carries_the_cooldown_its_answer_gave() {
    let (voice, github, _home) = voiced();
    let cooldown = Cooldown {
        retry_after: Some(45),
        reset: None,
    };
    github.refuse_comments(ApiError::RateLimited(cooldown));

    let err = voice
        .comment(&slug("shep-pm/koji"), 12, "hello")
        .unwrap_err();
    assert!(
        matches!(&err, ForgeError::Limited { cooldown: said, .. } if *said == cooldown),
        "{err:?}"
    );

    github.fail_next(ApiError::RateLimited(cooldown));
    let err = voice.worker_token(&slug("shep-pm/koji")).unwrap_err();
    assert!(
        matches!(&err, ForgeError::Limited { cooldown: said, .. } if *said == cooldown),
        "{err:?}"
    );
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
fn a_comment_posted_is_given_back_with_its_id_and_the_time_github_says() {
    let (voice, _github, _home) = voiced();

    let posted = voice.post(&slug("shep-pm/koji"), 12, "hello").unwrap();

    assert_eq!(
        posted,
        PostedComment {
            id: 100,
            created: Timestamp(1_000_000)
        }
    );
}

#[test]
fn an_answer_with_no_id_or_time_is_unreadable() {
    for body in ["{}", r#"{"id":4}"#, r#"{"id":4,"created_at":"soon"}"#, "x"] {
        assert!(
            matches!(api::parse_posted(body), Err(ApiError::Unreadable(_))),
            "{body}"
        );
    }
    assert_eq!(
        api::parse_posted(r#"{"id":4,"created_at":"2026-10-10T05:31:54Z","body":"x"}"#),
        Ok((4, Timestamp(1_791_610_314)))
    );
}

#[test]
fn a_comment_is_reacted_to_with_a_thumbs_up_as_the_app() {
    let (voice, github, _home) = voiced();

    voice.thumbs_up(&slug("shep-pm/koji"), 5001).unwrap();

    assert_eq!(
        writes(&github),
        [(
            "ghs_test1".to_owned(),
            Call {
                verb: Verb::Post,
                path: "/repos/shep-pm/koji/issues/comments/5001/reactions".to_owned(),
                body: Some(r#"{"content":"+1"}"#.to_owned()),
            }
        )]
    );
}

#[test]
fn a_comment_is_edited_in_place_as_the_app() {
    let (voice, github, _home) = voiced();

    voice
        .edit_comment(&slug("shep-pm/koji"), 5001, "settled")
        .unwrap();

    assert_eq!(
        writes(&github),
        [(
            "ghs_test1".to_owned(),
            Call {
                verb: Verb::Patch,
                path: "/repos/shep-pm/koji/issues/comments/5001".to_owned(),
                body: Some(r#"{"body":"settled"}"#.to_owned()),
            }
        )]
    );
}

#[test]
fn an_issues_body_is_replaced_in_place_with_the_title_as_it_stands() {
    let (voice, github, _home) = voiced();

    voice
        .edit_issue_body(&slug("shep-pm/koji"), 12, "agent-ready")
        .unwrap();

    assert_eq!(
        writes(&github),
        [(
            "ghs_test1".to_owned(),
            Call {
                verb: Verb::Patch,
                path: "/repos/shep-pm/koji/issues/12".to_owned(),
                // The documented edit names only the fields to change, so a
                // call holding no `title` leaves the title as it stands.
                body: Some(r#"{"body":"agent-ready"}"#.to_owned()),
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

#[test]
fn a_thread_is_replied_to_and_resolved_with_the_apps_token() {
    let (voice, github, _home) = voiced();
    let repo = slug("shep-pm/koji");

    voice.reply_to_thread(&repo, "PRRT_1", "Fixed.").unwrap();
    voice.resolve_thread(&repo, "PRRT_1").unwrap();

    let made = writes(&github);
    assert_eq!(made.len(), 2, "{made:?}");
    for (_, call) in &made {
        assert_eq!(call.path, "/graphql");
    }
    let sent: serde_json::Value = serde_json::from_str(made[0].1.body.as_deref().unwrap()).unwrap();
    assert_eq!(
        sent["variables"],
        serde_json::json!({ "id": "PRRT_1", "body": "Fixed." })
    );
    assert!(
        sent["query"]
            .as_str()
            .unwrap()
            .contains("addPullRequestReviewThreadReply")
    );
}

// GitHub refuses a thread's resolve to a token without Contents written,
// which its documentation does not say. Only that call is minted the
// permission: kelpie's posts, a reply included, still cannot push.
#[test]
fn a_thread_is_resolved_with_a_token_that_holds_contents_and_posts_use_one_that_does_not() {
    let (voice, github, _home) = voiced();
    let repo = slug("shep-pm/koji");

    voice.reply_to_thread(&repo, "PRRT_1", "Fixed.").unwrap();
    voice.resolve_thread(&repo, "PRRT_1").unwrap();

    let requests: Vec<String> = (github.asked().into_iter())
        .filter_map(|asked| match asked {
            Asked::AccessToken(_, body) => Some(body),
            _ => None,
        })
        .collect();
    assert_eq!(
        requests,
        [
            r#"{"permissions":{"issues":"write","pull_requests":"write"},"repositories":["koji"]}"#,
            r#"{"permissions":{"contents":"write","pull_requests":"write"},"repositories":["koji"]}"#,
        ]
    );
    let made = writes(&github);
    assert_eq!(made[0].0, "ghs_test1", "the reply uses kelpie's token");
    assert_eq!(made[1].0, "ghs_test2", "the resolve uses its own");
}

fn check(state: CheckState) -> GateCheck {
    GateCheck {
        name: "kelpie".to_owned(),
        state,
        title: "Waiting on review".to_owned(),
        summary: "Round 1 is with qwen.".to_owned(),
    }
}

#[test]
fn a_check_run_is_created_and_completed_on_the_head() {
    let (voice, github, _home) = voiced();
    github.answer_check_runs_with(include_str!("../../../../../fixtures/gh-check-run.json"));
    let repo = slug("shep-pm/koji");

    let id = voice
        .check_run(&repo, "9fceb02", &check(CheckState::InProgress))
        .unwrap();
    voice
        .update_check_run(
            &repo,
            id,
            &check(CheckState::Completed(Conclusion::Success)),
        )
        .unwrap();

    assert_eq!(id, CheckRunId(4_399_123_455));
    let json = |call: &Call| -> serde_json::Value {
        serde_json::from_str(call.body.as_deref().unwrap()).unwrap()
    };
    let made = writes(&github);
    assert_eq!(made.len(), 2);
    assert_eq!(made[0].1.verb, Verb::Post);
    assert_eq!(made[0].1.path, "/repos/shep-pm/koji/check-runs");
    assert_eq!(
        json(&made[0].1),
        serde_json::json!({
            "name": "kelpie",
            "head_sha": "9fceb02",
            "status": "in_progress",
            "output": {"title": "Waiting on review", "summary": "Round 1 is with qwen."},
        })
    );
    assert_eq!(made[1].1.verb, Verb::Patch);
    assert_eq!(made[1].1.path, "/repos/shep-pm/koji/check-runs/4399123455");
    assert_eq!(json(&made[1].1)["status"], "completed");
    assert_eq!(json(&made[1].1)["conclusion"], "success");
}

// Posting comments must work on an installation that never accepted Checks,
// so the permission is asked for by the check run's token alone.
#[test]
fn a_check_run_asks_for_a_checks_only_token() {
    let (voice, github, _home) = voiced();
    let repo = slug("shep-pm/koji");

    voice.comment(&repo, 12, "hello").unwrap();
    voice
        .check_run(&repo, "9fceb02", &check(CheckState::InProgress))
        .unwrap();

    let requests: Vec<String> = (github.asked().into_iter())
        .filter_map(|asked| match asked {
            Asked::AccessToken(_, body) => Some(body),
            _ => None,
        })
        .collect();
    assert_eq!(
        requests,
        [
            r#"{"permissions":{"issues":"write","pull_requests":"write"},"repositories":["koji"]}"#,
            r#"{"permissions":{"checks":"write"},"repositories":["koji"]}"#,
        ]
    );
    let made = writes(&github);
    assert_eq!(made[0].0, "ghs_test1", "the comment uses kelpie's token");
    assert_eq!(made[1].0, "ghs_test2", "the check run uses its own");
}

// A permission the installation has not accepted is told apart from GitHub
// being busy: asking again changes nothing.
#[test]
fn a_check_run_without_the_checks_permission_says_so() {
    let (voice, github, _home) = voiced();
    github.never_given(&["checks"]);

    let err = voice
        .check_run(
            &slug("shep-pm/koji"),
            "9fceb02",
            &check(CheckState::InProgress),
        )
        .unwrap_err();

    assert!(matches!(err, ForgeError::NoPermission(_)), "{err:?}");
}

fn line(path: &str, line: u32, body: &str) -> LineComment {
    LineComment {
        path: path.to_owned(),
        line,
        body: body.to_owned(),
    }
}

#[test]
fn the_app_voice_parses_the_recorded_review_response() {
    let (voice, github, _home) = voiced();

    let id = voice
        .review(
            &slug("shep-pm/koji"),
            30,
            "abc123",
            "I read the change and left two notes.",
            &[line("src/lib.rs", 4, "This can panic.")],
        )
        .unwrap();

    assert_eq!(id, crate::test::REVIEW_ID);
    let (token, call) = writes(&github).remove(0);
    assert_eq!(token, "ghs_test1");
    assert_eq!(call.verb, Verb::Post);
    assert_eq!(call.path, "/repos/shep-pm/koji/pulls/30/reviews");
    let sent: serde_json::Value = serde_json::from_str(&call.body.unwrap()).unwrap();
    assert_eq!(sent["event"], "COMMENT");
    assert_eq!(sent["commit_id"], "abc123");
    assert_eq!(sent["comments"][0]["path"], "src/lib.rs");
    assert_eq!(sent["comments"][0]["line"], 4);
    assert_eq!(sent["comments"][0]["side"], "RIGHT");
}

#[test]
fn an_answer_with_no_review_id_is_unreadable() {
    for body in ["{}", r#"{"id":"x"}"#, "x"] {
        assert!(api::parse_review(body).is_err(), "{body}");
    }
}

#[test]
fn a_review_the_forge_took_with_an_answer_naming_no_id_is_not_a_failure() {
    let (voice, github, _home) = voiced();
    github.answer_reviews_with("{}");

    let id = voice
        .review(&slug("shep-pm/koji"), 30, "abc123", "Notes.", &[])
        .unwrap();

    assert_eq!(id, UNREAD_ID);
    assert_eq!(github.reviews().len(), 1, "sent once");
}

#[test]
fn a_review_the_forge_refuses_for_its_line_comments_is_posted_once_more_in_its_body() {
    let (voice, github, _home) = voiced();
    github.refuse_line_comments();

    let id = voice
        .review(
            &slug("shep-pm/koji"),
            30,
            "abc123",
            "Two notes.",
            &[line("src/lib.rs", 4, "This can panic.")],
        )
        .unwrap();

    assert_eq!(id, crate::test::REVIEW_ID);
    let reviews = github.reviews();
    assert_eq!(reviews.len(), 2, "tried once, then retried once");
    assert_eq!(reviews[0].1.comments.len(), 1);
    let (_, retry) = &reviews[1];
    assert_eq!(retry.comments, []);
    assert_eq!(retry.event, "COMMENT");
    assert_eq!(retry.head, "abc123");
    assert!(retry.body.starts_with("Two notes."), "{}", retry.body);
    assert!(retry.body.contains("src/lib.rs:4"), "{}", retry.body);
    assert!(retry.body.contains("This can panic."), "{}", retry.body);
}

// The token is minted by the first call, so a failure set after it lands on a review.
fn minted(voice: &AppVoice) {
    voice.comment(&slug("shep-pm/koji"), 1, "hello").unwrap();
}

#[test]
fn a_review_the_retry_does_not_save_fails_and_is_not_tried_a_third_time() {
    let (voice, github, _home) = voiced();
    minted(&voice);
    github.fail_next(ApiError::Refused(422));
    github.fail_next(ApiError::Refused(422));

    let err = voice
        .review(
            &slug("shep-pm/koji"),
            30,
            "abc123",
            "Notes.",
            &[line("a.rs", 1, "x")],
        )
        .unwrap_err();

    assert!(matches!(err, ForgeError::App(_)), "{err:?}");
    assert_eq!(github.reviews().len(), 2);
}

#[test]
fn a_review_with_no_line_comment_is_not_retried_on_a_422() {
    let (voice, github, _home) = voiced();
    minted(&voice);
    github.fail_next(ApiError::Refused(422));

    let err = voice
        .review(&slug("shep-pm/koji"), 30, "abc123", "Notes.", &[])
        .unwrap_err();

    assert!(matches!(err, ForgeError::App(_)), "{err:?}");
    assert_eq!(github.reviews().len(), 1);
}
