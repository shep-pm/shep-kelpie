use crate::adapters::gh::review_bot::{
    parse_comments, parse_reviews, parse_statuses, parse_threads,
};
use crate::coderabbit::*;
use crate::ports::{Severity, Timestamp};

// Recorded from shep with the gh adapter's own calls: its sticky holds
// a walkthrough up to ce143d9 and a limit block quoting the head.
const COMMENTS_598: &str = include_str!("../../../../../fixtures/coderabbit-comments-598.jsonl");
const REVIEWS_598: &str = include_str!("../../../../../fixtures/coderabbit-reviews-598.jsonl");
const HEAD_598: &str = "f0f94c8f2b74a03bec6ac085af8a3643b41e574a";

// A clean review of the head, which posted no review at all.
const COMMENTS_615: &str = include_str!("../../../../../fixtures/coderabbit-comments-615.jsonl");
const HEAD_615: &str = "a11b010bb5f36890e745a740874708c28e4bcbf9";

// A review of the head with three threads still open.
const REVIEWS_617: &str = include_str!("../../../../../fixtures/coderabbit-reviews-617.jsonl");
const THREADS_617: &str = include_str!("../../../../../fixtures/coderabbit-threads-617.json");
const HEAD_617: &str = "f642b8aca0a37c3044166a8f384458b7ecc381be";

// shep#614 after kelpie's owed summon by label at 05:53:21: its head only
// merged `main`, which changed nothing CodeRabbit reads. It marked the
// head done 24 seconds on and posted nothing but a skip notice.
const COMMENTS_614: &str = include_str!("../../../../../fixtures/coderabbit-comments-614.jsonl");
const STATUSES_614: &str = include_str!("../../../../../fixtures/coderabbit-statuses-614.jsonl");
const HEAD_614: &str = "7d30d0f6f8d03314fe9f060b4627643bb32db8cc";

// A repo whose `.coderabbit.yaml` has automatic reviews off: the status
// it set 12 seconds after a summon at 10:00:00. The text is as the
// maintainer saw it on the pull request; the head and times are made up.
const STATUSES_DISABLED: &str =
    include_str!("../../../../../fixtures/coderabbit-statuses-399.jsonl");
const HEAD_DISABLED: &str = "5e1c0de5a7b3c9d2f4a6b8c0d2e4f6a8b0c2d4e6";

// Two inline comments CodeRabbit left on #1602, on `stray.rs`, as recorded:
// the Major, with a collapsed analysis above its title, and the Minor.
const INLINE_MAJOR: &str = include_str!("../../../../../fixtures/coderabbit-inline-major.md");
const INLINE_MINOR: &str = include_str!("../../../../../fixtures/coderabbit-inline-minor.md");

fn thread_of(body: &str) -> Thread {
    Thread {
        id: "t".into(),
        resolved: false,
        outdated: false,
        path: "crates/process/src/worktree/stray.rs".into(),
        line: Some(102),
        body: body.into(),
    }
}

fn activity(comments: &str, reviews: &str, threads: &str) -> Activity {
    Activity {
        comments: parse_comments(comments.as_bytes()).unwrap(),
        reviews: parse_reviews(reviews.as_bytes()).unwrap(),
        threads: parse_threads(threads.as_bytes(), LOGIN.graphql).unwrap(),
        statuses: Vec::new(),
        reactions: Vec::new(),
    }
}

const NO_THREADS: &str =
    r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[]}}}}}"#;

// The skip notice a push set on the head at 04:57:57
fn skips_of_614() -> Vec<Status> {
    let all = parse_statuses(STATUSES_614.as_bytes()).unwrap();
    all.into_iter()
        .filter(|s| s.description.starts_with("Review skipped"))
        .collect()
}

fn iso(text: &str) -> Timestamp {
    let at: jiff::Timestamp = text.parse().unwrap();
    Timestamp(at.as_second().try_into().unwrap())
}

#[test]
fn a_clean_review_with_no_review_object_still_covers_its_head() {
    let seen = activity(COMMENTS_615, "", NO_THREADS);
    assert!(seen.reviews.is_empty(), "the trap: no review to read");
    assert!(seen.covers(HEAD_615));
    assert_eq!(
        seen.read(HEAD_615, iso("2026-09-24T13:00:00Z")),
        Reading::Reviewed
    );
    assert_eq!(seen.open_threads().count(), 0);
}

#[test]
fn a_summon_marked_done_with_nothing_posted_is_completed_not_reviewed() {
    let seen = Activity {
        statuses: parse_statuses(STATUSES_614.as_bytes()).unwrap(),
        ..activity(COMMENTS_614, "", NO_THREADS)
    };
    let summoned = iso("2026-09-29T05:53:21Z");
    assert!(
        !seen.covers(HEAD_614),
        "the walkthrough is of an older head"
    );
    assert!(
        seen.reviewed_besides(HEAD_614) > 0,
        "it read the pull request before"
    );
    let done = Reading::Completed {
        at: iso("2026-09-29T05:53:45Z"),
    };
    assert_eq!(seen.read(HEAD_614, summoned), done);
    assert_eq!(seen.since(summoned).read(HEAD_614, summoned), done);
    assert_eq!(
        seen.read(HEAD_614, iso("2026-09-29T06:10:00Z")),
        Reading::Silent,
        "done before a later summon is no answer to it"
    );
    assert_eq!(seen.read("0ther", summoned), Reading::Silent);
}

#[test]
fn a_running_review_that_ended_in_another_status_is_not_still_running() {
    let status = |description: &str, at: u64| Status {
        commit: HEAD_614.into(),
        description: description.into(),
        at: Timestamp(at),
    };
    let seen = Activity {
        statuses: vec![
            status("Review rate limited", 130),
            status("Review in progress", 110),
        ],
        ..Activity::default()
    };
    assert_eq!(seen.read(HEAD_614, Timestamp(100)), Reading::Silent);
    assert!(seen.heard(HEAD_614, Timestamp(100)));
}

#[test]
fn an_in_progress_status_on_the_head_is_a_review_running_and_a_sign() {
    let all = parse_statuses(STATUSES_614.as_bytes()).unwrap();
    let running: Vec<Status> = all
        .into_iter()
        .filter(|s| s.description == "Review in progress")
        .collect();
    let seen = Activity {
        statuses: running,
        ..activity(COMMENTS_614, "", NO_THREADS)
    };
    let summoned = iso("2026-09-29T05:53:21Z");
    assert_eq!(seen.read(HEAD_614, summoned), Reading::Processing);
    assert!(seen.heard(HEAD_614, summoned));
    assert!(
        !seen.heard(HEAD_614, iso("2026-09-29T06:10:00Z")),
        "a later summon"
    );
}

#[test]
fn a_rate_limit_or_a_skip_marked_on_the_head_is_not_completed() {
    let status = |description: &str| Status {
        commit: "c0ffee".into(),
        description: description.into(),
        at: Timestamp(100),
    };
    let limited = Activity {
        statuses: vec![status("Review rate limited")],
        ..Activity::default()
    };
    assert_eq!(limited.read("c0ffee", Timestamp(0)), Reading::Silent);
    let skipped = Activity {
        statuses: vec![status("Review skipped: excluded by label configuration")],
        ..Activity::default()
    };
    assert_ne!(
        skipped.read("c0ffee", Timestamp(0)),
        Reading::Completed { at: Timestamp(100) }
    );
}

#[test]
fn a_summon_answered_with_automatic_reviews_disabled_skips_coderabbit_at_once() {
    let seen = Activity {
        statuses: parse_statuses(STATUSES_DISABLED.as_bytes()).unwrap(),
        ..Activity::default()
    };
    assert_eq!(
        seen.read(HEAD_DISABLED, iso("2026-10-09T10:00:00Z")),
        Reading::Declined {
            status: "Review skipped: automatic reviews are disabled"
        }
    );
}

// Shaped like shep#614's statuses, whose skip came from a push.
#[test]
fn a_summon_answered_with_excluded_by_label_configuration_skips_coderabbit_at_once() {
    let seen = Activity {
        statuses: skips_of_614(),
        ..Activity::default()
    };
    assert_eq!(
        seen.read(HEAD_614, iso("2026-09-29T04:57:50Z")),
        Reading::Declined {
            status: "Review skipped: excluded by label configuration"
        }
    );
}

#[test]
fn a_skip_status_set_before_the_summon_is_not_its_answer() {
    let seen = Activity {
        statuses: skips_of_614(),
        ..Activity::default()
    };
    // 04:57:57 is inside CLOCK_SLACK of this summon, and still before it
    let summoned = iso("2026-09-29T04:58:20Z");
    assert_eq!(seen.read(HEAD_614, summoned), Reading::Silent);
    assert!(seen.heard(HEAD_614, summoned), "a sign, if no answer");
}

#[test]
fn a_later_review_in_progress_status_beats_an_earlier_skip() {
    let status = |description: &str, at: u64| Status {
        commit: "c0ffee".into(),
        description: description.into(),
        at: Timestamp(at),
    };
    let seen = Activity {
        statuses: vec![
            status("Review in progress", 110),
            status("Review skipped: automatic reviews are disabled", 100),
        ],
        ..Activity::default()
    };
    assert_eq!(seen.read("c0ffee", Timestamp(90)), Reading::Processing);
}

#[test]
fn each_reviewed_commit_counts_once_and_a_reply_is_no_review() {
    assert_eq!(
        activity(COMMENTS_615, "", NO_THREADS).reviewed_besides(""),
        1
    );
    let seen = activity(COMMENTS_598, REVIEWS_598, NO_THREADS);
    assert!(seen.reviews[1].body.is_empty(), "the second is a reply");
    assert_eq!(seen.reviewed_besides(""), 1, "all of it is ce143d9");
    let later_clean = activity(COMMENTS_615, REVIEWS_617, NO_THREADS);
    assert_eq!(later_clean.reviewed_besides(""), 2, "one posted, one clean");
    assert_eq!(later_clean.reviewed_besides(HEAD_615), 1);
    assert_eq!(activity("", "", NO_THREADS).reviewed_besides(""), 0);
}

#[test]
fn a_limit_block_quoting_the_head_is_not_a_review_of_it() {
    let seen = activity(COMMENTS_598, REVIEWS_598, NO_THREADS);
    let sticky = seen.sticky().unwrap();
    assert!(
        sticky.body.contains(&format!("and {HEAD_598}.")),
        "the trap"
    );
    assert!(!seen.covers(HEAD_598));
    assert!(seen.covers("ce143d98b68b2773c5bd5640d97a6b1c65786a3b"));
}

// The sticky was last edited at 22:29:37 with "available in 3 minutes".
#[test]
fn a_refusal_is_read_from_its_quoted_wait() {
    let seen = activity(COMMENTS_598, REVIEWS_598, NO_THREADS);
    assert_eq!(
        seen.read(HEAD_598, iso("2026-09-22T22:29:00Z")),
        Reading::Refused {
            opens: Some(iso("2026-09-22T22:32:37Z"))
        }
    );
}

#[test]
fn a_refusal_from_before_the_summon_is_not_its_answer() {
    let seen = activity(COMMENTS_598, REVIEWS_598, NO_THREADS);
    assert_eq!(
        seen.read(HEAD_598, iso("2026-09-22T23:00:00Z")),
        Reading::Silent
    );
}

#[test]
fn a_refusal_stamped_just_before_the_summon_by_githubs_clock_still_answers_it() {
    let seen = activity(COMMENTS_598, REVIEWS_598, NO_THREADS);
    assert!(matches!(
        seen.read(HEAD_598, iso("2026-09-22T22:30:30Z")),
        Reading::Refused { .. }
    ));
}

#[test]
fn a_reply_to_a_summon_by_comment_quotes_its_wait_too() {
    let seen = activity(COMMENTS_615, "", NO_THREADS);
    let summoned = iso("2026-09-24T12:20:00Z");
    assert_eq!(
        seen.read("0000000", summoned),
        Reading::Refused {
            opens: Some(iso("2026-09-24T13:00:41Z"))
        },
        "12:22:41 plus 38 minutes"
    );
}

#[test]
fn the_quota_comes_from_the_latest_footer() {
    let seen = activity(COMMENTS_598, REVIEWS_598, NO_THREADS);
    assert_eq!(seen.quota(), Some((1, iso("2026-09-22T21:38:33Z"))));
    let mut seen = activity(COMMENTS_615, "", NO_THREADS);
    seen.reviews.push(Review {
        commit: "c0ffee".into(),
        body: "**Included review availability:** Your plan provides up to 10 \
               included reviews per hour; 6 remain after this review."
            .into(),
        at: iso("2026-09-25T00:00:00Z"),
    });
    assert_eq!(seen.quota(), Some((10, iso("2026-09-25T00:00:00Z"))));
    assert_eq!(Activity::default().quota(), None);
}

#[test]
fn a_review_with_findings_covers_its_commit_and_lists_its_open_threads() {
    let seen = activity("", REVIEWS_617, THREADS_617);
    assert!(seen.covers(HEAD_617));
    let open: Vec<_> = seen.open_threads().map(|t| t.line).collect();
    assert_eq!(open, [Some(500), Some(625), Some(266)]);
    assert_eq!(
        seen.threads.len(),
        3,
        "the maintainer's own threads are not CodeRabbit's"
    );
}

#[test]
fn a_thread_becomes_a_finding_in_one_line_each() {
    let seen = activity("", REVIEWS_617, THREADS_617);
    let finding = finding(&seen.threads[1]);
    assert_eq!(finding.severity, Severity::Medium, "Minor");
    assert_eq!(
        (finding.file.as_str(), finding.line),
        ("benches/versus-pm2/versus-pm2.sh", 625)
    );
    assert_eq!(
        finding.what,
        "Preserve the baseline and compare options in the re-judge command."
    );
    assert!(
        finding
            .why
            .starts_with("The `--check` path passes `--baseline \"$BASELINE\"`"),
        "{}",
        finding.why
    );
    assert!(!finding.why.contains('\n') && !finding.why.contains('|'));
}

#[test]
fn a_thread_without_a_bold_title_still_gives_the_worker_its_text() {
    let thread = Thread {
        id: "t".into(),
        resolved: false,
        outdated: false,
        path: "a.rs".into(),
        line: Some(3),
        body: "_🟡 Minor_\n\nThis drops the error.\n\nIt matters.".into(),
    };
    assert_eq!(finding(&thread).what, "This drops the error. It matters.");
}

// shep#550's walkthrough, older than the coverage marker, names its
// commit only in the change assessment and its own range.
#[test]
fn an_older_walkthrough_is_read_from_its_change_assessment() {
    let head = "3886dbd25007f9cfa45bcc31b27044dcd0e48137";
    let body = format!(
        "## Walkthrough\n\n<!-- change_assessment_commit:\"{head}\" -->\n\n\
         Reviewing files that changed from the base of the PR and between \
         dd9b89c and 0000000."
    );
    let seen = Activity {
        comments: vec![Comment {
            body,
            at: Timestamp(1),
        }],
        ..Activity::default()
    };
    assert!(seen.covers(head));
    let range_only = Activity {
        comments: vec![Comment {
            body: format!(
                "## Walkthrough\n\nReviewing files that changed from the base of \
                 the PR and between dd9b89c and {head}."
            ),
            at: Timestamp(1),
        }],
        ..Activity::default()
    };
    assert!(range_only.covers(head));
}

#[test]
fn severity_follows_coderabbits_label() {
    let thread = |label: &str| Thread {
        id: "t".into(),
        resolved: false,
        outdated: false,
        path: "a.rs".into(),
        line: None,
        body: format!("_⚠️ Potential issue_ | {label}\n\n**Title.**\n\nWhy."),
    };
    assert_eq!(finding(&thread("_🟠 Major_")).severity, Severity::High);
    assert_eq!(finding(&thread("_🔴 Critical_")).severity, Severity::High);
    assert_eq!(finding(&thread("_🔵 Trivial_")).severity, Severity::Low);
    let odd = finding(&thread("_❓ Unheard of_"));
    assert_eq!((odd.severity, odd.line), (Severity::Medium, 0));
    assert_eq!((odd.what.as_str(), odd.why.as_str()), ("Title.", "Why."));
}

#[test]
fn a_current_major_comment_reads_as_a_high_finding_with_its_title_and_reasoning() {
    let found = finding(&thread_of(INLINE_MAJOR));
    assert_eq!(found.severity, Severity::High);
    assert_eq!(
        found.what,
        "Use the OS-resolved executable identity for containment."
    );
    assert!(
        found
            .why
            .starts_with("`inside` matches the raw `args` text from `ps`.")
            && found
                .why
                .ends_with("can be set independently of the executable."),
        "{}",
        found.why
    );
    assert!(!found.what.contains('<') && !found.what.contains("**"));
    // The reasoning names `/proc/<pid>/exe`, so only markup is ruled out.
    for markup in ["<!--", "<picture", "<img", "<details", "**", "\n"] {
        assert!(!found.why.contains(markup), "{markup}: {}", found.why);
    }
}

#[test]
fn a_current_minor_comment_reads_as_medium() {
    let found = finding(&thread_of(INLINE_MINOR));
    assert_eq!(found.severity, Severity::Medium);
    assert_eq!(
        found.what,
        "Log a process-listing failure during the grace period."
    );
    assert!(
        found.why.starts_with("If a later `ps` call fails"),
        "{}",
        found.why
    );
    assert!(!found.why.contains('<'));
}

#[test]
fn a_trivial_or_nitpick_label_is_a_nit_in_the_current_shape() {
    let comment = |label: &str| {
        let body = format!(
            "<!-- coderabbit-ai-generated:v1 -->\n\n**<picture><img src=\"x/{0}-16.svg\" \
             alt=\"\"></picture> {0}** · **Style** · **Quick win**\n\n**Rename it.**\n\nIt reads badly.",
            label
        );
        finding(&thread_of(&body))
    };
    for label in ["Trivial", "Nitpick"] {
        let found = comment(label);
        assert_eq!(found.severity, Severity::Low, "{label}");
        assert_eq!(
            (found.what.as_str(), found.why.as_str()),
            ("Rename it.", "It reads badly.")
        );
    }
}

#[test]
fn the_october_label_line_still_reads() {
    let body = "**🎯 Functional Correctness** | **🟡 Minor** | **⚡ Quick win**\n\n\
                **Keep the last line.**\n\nIt is dropped today.";
    let found = finding(&thread_of(body));
    assert_eq!(found.severity, Severity::Medium);
    assert_eq!(
        (found.what.as_str(), found.why.as_str()),
        ("Keep the last line.", "It is dropped today.")
    );
}

#[test]
fn a_running_review_is_processing_not_done() {
    // No run in progress was ever recorded, since the comment is edited
    // over; the shape is the one the maintainer's review driver reads.
    let running = Activity {
        comments: vec![Comment {
            body: "<details>\n<summary>📒 Files selected for processing (4)</summary>".into(),
            at: Timestamp(1),
        }],
        ..Activity::default()
    };
    assert_eq!(running.read("c0ffee", Timestamp(0)), Reading::Processing);
    let rereading = Activity {
        comments: vec![Comment {
            body: "## Walkthrough\n\nCurrently processing new changes in this PR.".into(),
            at: Timestamp(1),
        }],
        ..Activity::default()
    };
    assert_eq!(rereading.read("c0ffee", Timestamp(0)), Reading::Processing);
}

#[test]
fn waits_are_read_in_every_unit_seen() {
    let read = |text: &str| wait(&format!("Next included review available in {text}"));
    assert_eq!(read("12 minutes.**"), Some(720));
    assert_eq!(read("1 minute."), Some(60));
    assert_eq!(read("59 seconds."), Some(59));
    assert_eq!(read("1 hour and 5 minutes."), Some(3900));
    assert_eq!(read("a moment."), None);
    assert_eq!(read("18446744073709551615 hours."), Some(u64::MAX));
}
