//! cubic's profile as a review bot
//!
//! cubic has no label: a comment summons it, and a comment a few seconds on
//! says it started. Its review posts whether or not it found anything, on
//! the commit it read, with its summary between markers. Each finding is a
//! thread whose text opens with its level, `P1` to `P3`, below a marker
//! carrying its confidence out of 10. It sets no commit status and states
//! no quota, and its refusal was never seen, so one reads as any comment
//! of its since the summon that says a limit was reached.

use crate::ports::{Finding, Severity, Timestamp};
use crate::review_bot::{Activity, Bot, CLOCK_SLACK, Login, Profile, Reading, Thread, one_line};

/// The comment that summons cubic
pub const SUMMON: &str = "@cubic-dev-ai review";

/// cubic's login on both of the forge's APIs
pub const LOGIN: Login<'static> = Login {
    rest: "cubic-dev-ai[bot]",
    graphql: "cubic-dev-ai",
};

const STARTED: &str = "I have started the AI code review";
const SUMMARY: &str = "<!-- cubic:review-summary:start -->";
const CONFIDENCE: &str = "<!-- metadata:{\"confidence\":";

/// cubic, the second review bot
#[derive(Debug, Clone, Copy, Default)]
pub struct Cubic;

impl Profile for Cubic {
    fn bot(&self) -> Bot {
        Bot::Cubic
    }

    fn login(&self) -> Login<'_> {
        LOGIN
    }

    fn label(&self) -> Option<&str> {
        None
    }

    fn full_review(&self) -> Option<&str> {
        Some(SUMMON)
    }

    fn read(&self, activity: &Activity, head: &str, since: Timestamp) -> Reading {
        if self.covers(activity, head) {
            return Reading::Reviewed;
        }
        // A refusal among its comments since the summon stands, whatever it
        // posted after.
        let from = since.0.saturating_sub(CLOCK_SLACK);
        let mut since_summon = activity.comments.iter().filter(|c| c.at.0 >= from);
        match since_summon.clone().next() {
            Some(_) if since_summon.any(|c| refused(&c.body)) => Reading::Refused { opens: None },
            Some(_) => Reading::Processing,
            None => Reading::Silent,
        }
    }

    fn heard(&self, activity: &Activity, _head: &str, since: Timestamp) -> bool {
        let from = since.0.saturating_sub(CLOCK_SLACK);
        activity.comments.iter().any(|c| c.at.0 >= from)
            || activity.reviews.iter().any(|r| r.at.0 >= from)
    }

    fn covers(&self, activity: &Activity, head: &str) -> bool {
        reviewed(activity).any(|commit| commit == head)
    }

    fn reviewed_besides(&self, activity: &Activity, head: &str) -> u32 {
        let mut commits: Vec<&str> = reviewed(activity).filter(|c| *c != head).collect();
        commits.sort_unstable();
        commits.dedup();
        u32::try_from(commits.len()).unwrap_or(u32::MAX)
    }

    fn quota(&self, _activity: &Activity) -> Option<(u32, Timestamp)> {
        None
    }

    fn finding(&self, thread: &Thread) -> Finding {
        finding(thread)
    }
}

// The commits its reviews read. A reply in a thread posts a review with no
// summary, which is no review.
fn reviewed(activity: &Activity) -> impl Iterator<Item = &str> {
    let posted = activity.reviews.iter().filter(|r| r.body.contains(SUMMARY));
    posted.map(|r| r.commit.as_str())
}

// Any comment that is not its start and names a limit reached or exceeded.
fn refused(body: &str) -> bool {
    let body = body.to_lowercase();
    !body.contains(&STARTED.to_lowercase())
        && body.contains("limit")
        && (body.contains("reached") || body.contains("exceeded"))
}

/// A thread as a finding the judge can rule on
///
/// The severity comes from cubic's level: P0 and P1 are high, P2 is
/// medium, P3 is a nit. The level and confidence go to the judge as the why.
fn finding(thread: &Thread) -> Finding {
    let prose: Vec<&str> = thread
        .body
        .lines()
        .take_while(|line| !line.starts_with("<details"))
        .filter(|line| !line.trim_start().starts_with("<!--"))
        .collect();
    let prose = prose.join("\n");
    let prose = prose.trim();
    let (level, text) = match prose.split_once(": ") {
        Some((level @ ("P0" | "P1" | "P2" | "P3"), text)) => (Some(level), text),
        _ => (None, prose),
    };
    let severity = match level {
        Some("P0" | "P1") => Severity::High,
        Some("P3") => Severity::Low,
        _ => Severity::Medium,
    };
    let confidence = thread
        .body
        .find(CONFIDENCE)
        .map(|at| &thread.body[at + CONFIDENCE.len()..])
        .and_then(|rest| rest.split('}').next())
        .and_then(|n| n.trim().parse::<u8>().ok());
    let why = match (level, confidence) {
        (Some(level), Some(n)) => format!("cubic rates it {level}, with confidence {n} of 10."),
        (Some(level), None) => format!("cubic rates it {level}."),
        (None, Some(n)) => format!("cubic's confidence is {n} of 10."),
        (None, None) => String::new(),
    };
    Finding {
        severity,
        file: thread.path.clone(),
        line: thread.line.unwrap_or(0),
        what: one_line(text),
        why,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::gh::review_bot::{parse_comments, parse_reviews, parse_threads};
    use crate::review_bot::Comment;

    // Recorded from shep-pm/shep#637 with the gh adapter's own calls: the
    // maintainer's summon at 12:15:54, cubic's start at 12:16:00, and its
    // review of d51bb92 at 12:23:15 with 11 threads. CodeRabbit's one
    // thread there is not cubic's.
    const COMMENTS_637: &str = include_str!("../fixtures/cubic-comments-637.jsonl");
    const REVIEWS_637: &str = include_str!("../fixtures/cubic-reviews-637.jsonl");
    const THREADS_637: &str = include_str!("../fixtures/cubic-threads-637.json");
    const HEAD_637: &str = "d51bb92830c5c60afba8ecbf8f51940551ce2a18";

    // shep-pm/shep#645: a clean review, which cubic posts all the same.
    const REVIEWS_645: &str = include_str!("../fixtures/cubic-reviews-645.jsonl");
    const HEAD_645: &str = "2eca0ecb90f7188f691b23aa959b4ca4dc0078b2";

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
            reactions: Vec::new(),
        }
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
    fn a_thread_reaches_the_judge_with_its_level_and_confidence() {
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
}
