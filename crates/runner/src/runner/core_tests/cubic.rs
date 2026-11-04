use crate::adapters::gh::review_bot::{parse_checks, parse_comments, parse_reviews, parse_threads};
use crate::cubic::*;
use crate::ports::{Severity, Timestamp};
use crate::review_bot::Comment;
use crate::review_bot::{Activity, Profile, Reading, Thread};

// Recorded from shep-pm/shep#637 with the gh adapter's own calls: the
// maintainer's summon at 12:15:54, cubic's start at 12:16:00, and its
// review of d51bb92 at 12:23:15 with 11 threads. CodeRabbit's one
// thread there is not cubic's.
const COMMENTS_637: &str = include_str!("../../../../../fixtures/cubic-comments-637.jsonl");
const REVIEWS_637: &str = include_str!("../../../../../fixtures/cubic-reviews-637.jsonl");
const THREADS_637: &str = include_str!("../../../../../fixtures/cubic-threads-637.json");
const HEAD_637: &str = "d51bb92830c5c60afba8ecbf8f51940551ce2a18";

// shep-pm/shep#645: a clean review, which cubic posts all the same.
const REVIEWS_645: &str = include_str!("../../../../../fixtures/cubic-reviews-645.jsonl");
const HEAD_645: &str = "2eca0ecb90f7188f691b23aa959b4ca4dc0078b2";

// shep-pm/shep#1544: the plan out of reviewed lines, where cubic posted no
// comment and only this check run, completed 2026-11-04T12:47:24Z.
const CHECK_RUNS_1544: &str =
    include_str!("../../../../../fixtures/gh-cubic-check-runs-limit.json");
const HEAD_1544: &str = "014889df33ac47608d2056ef3eddd317758bdeac";
const RESUMES: &str = "2026-11-29T00:00:00Z";

fn iso(text: &str) -> Timestamp {
    let at: jiff::Timestamp = text.parse().unwrap();
    Timestamp(at.as_second().try_into().unwrap())
}

fn activity(comments: &str, reviews: &str) -> Activity {
    Activity {
        comments: parse_comments(comments.as_bytes()).unwrap(),
        reviews: parse_reviews(reviews.as_bytes()).unwrap(),
        threads: parse_threads(THREADS_637.as_bytes(), LOGIN.graphql).unwrap(),
        statuses: Vec::new(),
        checks: Vec::new(),
        reactions: Vec::new(),
    }
}

// A head with one cubic check run and nothing else it posted
fn checks(page: &str) -> Activity {
    Activity {
        checks: parse_checks(&[page.as_bytes().to_vec()], LOGIN.graphql).unwrap(),
        ..Activity::default()
    }
}

// A check-runs page with one cubic run titled `title` and summarised as `summary`
fn page(title: &str, summary: &str, at: &str) -> String {
    serde_json::json!({
        "total_count": 1,
        "check_runs": [{
            "app": {"slug": "cubic-dev-ai"},
            "status": "completed",
            "conclusion": "neutral",
            "completed_at": at,
            "output": {"title": title, "summary": summary}
        }]
    })
    .to_string()
}

#[test]
fn its_start_is_the_sign_it_heard_and_a_review_running() {
    let summoned = iso("2026-09-29T12:15:54Z");
    let started = activity(COMMENTS_637, "");
    assert!(started.comments[0].body.contains(STARTED));
    assert!(Cubic.heard(&started, HEAD_637, summoned));
    assert_eq!(
        Cubic.read(&started, HEAD_637, summoned),
        Reading::Processing
    );
    let later = iso("2026-09-29T13:00:00Z");
    assert!(!Cubic.heard(&started, HEAD_637, later), "a later summon");
    assert_eq!(Cubic.read(&started, HEAD_637, later), Reading::Silent);
}

#[test]
fn its_review_covers_the_commit_it_read_and_opens_eleven_threads() {
    let seen = activity(COMMENTS_637, REVIEWS_637);
    let summoned = iso("2026-09-29T12:15:54Z");
    assert_eq!(Cubic.read(&seen, HEAD_637, summoned), Reading::Reviewed);
    assert!(!Cubic.covers(&seen, "0ther"));
    assert_eq!(Cubic.reviewed_besides(&seen, HEAD_637), 0);
    assert_eq!(Cubic.reviewed_besides(&seen, ""), 1);
    assert_eq!(
        seen.open_threads().count(),
        11,
        "CodeRabbit's is not cubic's"
    );
    assert_eq!(Cubic.quota(&seen), None);
}

#[test]
fn a_clean_review_is_still_posted_and_covers_its_head() {
    let seen = activity("", REVIEWS_645);
    assert!(seen.reviews[0].body.contains("**No issues found**"));
    assert!(Cubic.covers(&seen, HEAD_645));
    assert_eq!(Cubic.reviewed_besides(&seen, ""), 1);
}

#[test]
fn a_reply_with_no_summary_is_no_review() {
    let mut seen = activity("", REVIEWS_637);
    seen.reviews[0].body = "Thanks, fixed.".into();
    assert!(!Cubic.covers(&seen, HEAD_637));
    assert_eq!(Cubic.reviewed_besides(&seen, ""), 0);
}

#[test]
fn a_thread_reaches_the_worker_with_its_level_and_confidence() {
    let seen = activity("", REVIEWS_637);
    let first = Cubic.finding(&seen.threads[0]);
    assert_eq!(first.severity, Severity::Medium, "P2");
    assert_eq!(
        (first.file.as_str(), first.line),
        ("web/src/pages/docs/first-flockfile.astro", 141)
    );
    assert!(
        first
            .what
            .starts_with("`shep bleats` does not always show the same lines"),
        "{}",
        first.what
    );
    assert!(
        first.what.ends_with("and add a regression test."),
        "{}",
        first.what
    );
    assert!(!first.what.contains("Prompt for AI agents"));
    assert_eq!(first.why, "cubic rates it P2, with confidence 9 of 10.");
    let levels: Vec<Severity> = seen
        .threads
        .iter()
        .map(|t| Cubic.finding(t).severity)
        .collect();
    assert_eq!(
        levels.iter().filter(|s| **s == Severity::Low).count(),
        7,
        "P3"
    );
    assert_eq!(
        levels.iter().filter(|s| **s == Severity::Medium).count(),
        4,
        "P2"
    );
}

#[test]
fn a_p1_is_high_and_a_thread_with_no_level_is_medium() {
    let thread = |body: &str| Thread {
        id: "t".into(),
        resolved: false,
        outdated: false,
        path: "a.rs".into(),
        line: None,
        body: body.into(),
    };
    let p1 = Cubic.finding(&thread(
        "<!-- metadata:{\"confidence\":8} -->\nP1: It drops the error.",
    ));
    assert_eq!((p1.severity, p1.line), (Severity::High, 0));
    assert_eq!(p1.what, "It drops the error.");
    assert_eq!(
        Cubic.finding(&thread("P0: It leaks.")).severity,
        Severity::High
    );
    let bare = Cubic.finding(&thread("It drops the error."));
    assert_eq!(bare.severity, Severity::Medium);
    assert_eq!(
        (bare.what.as_str(), bare.why.as_str()),
        ("It drops the error.", "")
    );
}

#[test]
fn a_line_limit_check_run_is_a_refusal_that_opens_on_the_day_it_names() {
    let seen = checks(CHECK_RUNS_1544);
    assert_eq!(seen.checks.len(), 1);
    assert_eq!(seen.checks[0].title, LIMIT);
    assert_eq!(seen.checks[0].conclusion, "neutral");
    let summoned = iso("2026-11-04T12:47:00Z");
    assert!(
        Cubic.heard(&seen, HEAD_1544, summoned),
        "it posted no comment"
    );
    assert_eq!(
        Cubic.read(&seen, HEAD_1544, summoned),
        Reading::Refused {
            opens: Some(iso(RESUMES))
        }
    );
}

#[test]
fn a_line_limit_check_run_from_before_the_summon_still_refuses_it() {
    let seen = checks(CHECK_RUNS_1544);
    let summoned = iso("2026-11-05T09:00:00Z");
    let owed = seen.since(summoned);
    assert_eq!(
        owed.checks.len(),
        1,
        "a check run is kept whatever its time"
    );
    assert_eq!(
        Cubic.read(&owed, HEAD_1544, summoned),
        Reading::Refused {
            opens: Some(iso(RESUMES))
        }
    );
}

#[test]
fn a_line_limit_summary_with_no_date_refuses_with_no_opening() {
    let said = "You've reached your plan's monthly review limit.";
    let seen = checks(&page(LIMIT, said, "2026-11-04T12:47:24Z"));
    assert_eq!(
        Cubic.read(&seen, HEAD_1544, Timestamp(100)),
        Reading::Refused { opens: None }
    );
    assert!(Cubic.heard(&seen, HEAD_1544, Timestamp(100)));
}

#[test]
fn a_review_skipped_or_completed_check_run_is_no_refusal() {
    let done = checks(&page(
        "AI review completed",
        "**No issues found** across 8 files",
        "2026-11-04T12:47:24Z",
    ));
    assert_eq!(
        Cubic.read(&done, HEAD_1544, Timestamp(100)),
        Reading::Silent
    );
    let skipped = checks(&page(
        "AI review skipped by configuration",
        "",
        "2026-11-04T12:47:24Z",
    ));
    assert_eq!(
        Cubic.read(&skipped, HEAD_1544, Timestamp(100)),
        Reading::Silent
    );
    let rewritten = checks(&page(
        "Automatic AI review not started after branch rewrite",
        "",
        "2026-11-04T12:47:24Z",
    ));
    assert_eq!(
        Cubic.read(&rewritten, HEAD_1544, Timestamp(100)),
        Reading::Silent
    );
    assert!(
        !Cubic.heard(&skipped, HEAD_1544, Timestamp(100)),
        "a run that is no refusal is no sign"
    );
}

#[test]
fn a_comment_saying_a_limit_was_reached_is_a_refusal_with_no_opening() {
    let refusal = Activity {
        comments: vec![Comment {
            body: "You have reached your monthly reviewed-line limit.".into(),
            at: Timestamp(130),
        }],
        ..Activity::default()
    };
    assert_eq!(
        Cubic.read(&refusal, "c0ffee", Timestamp(100)),
        Reading::Refused { opens: None }
    );
    assert!(Cubic.heard(&refusal, "c0ffee", Timestamp(100)));
    assert!(refused("The reviewed-line limit was exceeded."));
    let mut then_more = refusal.clone();
    then_more.comments.push(Comment {
        body: "Upgrade to keep reviewing.".into(),
        at: Timestamp(140),
    });
    assert_eq!(
        Cubic.read(&then_more, "c0ffee", Timestamp(100)),
        Reading::Refused { opens: None },
        "a later comment does not hide it"
    );
    let warning = Activity {
        comments: vec![Comment {
            body: "You're at about 91% of the monthly reviewed-line limit.".into(),
            at: Timestamp(130),
        }],
        ..Activity::default()
    };
    assert_eq!(
        Cubic.read(&warning, "c0ffee", Timestamp(100)),
        Reading::Processing
    );
}
