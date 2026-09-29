//! Reading what Gemini Code Assist has done on a pull request
//!
//! Gemini answers a `/gemini review` comment with a review of the head,
//! even a clean one, whose inline comments are its findings. Out of quota,
//! it answers with a comment instead. A review not posted in answer to one
//! of kelpie's summons is never read. Pure: the forge fetches, this reads.

use crate::lease::window::DAY;
use crate::outside::{CLOCK_SLACK, one_line};
pub use crate::outside::{Comment, Reading, Thread};
use crate::ports::{Finding, Severity, Timestamp};

/// The comment that summons a review
pub const SUMMON: &str = "/gemini review";

/// Everything Gemini, or a summon of it, has posted on one pull request
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    /// Its own comments on the conversation, oldest first
    pub comments: Vec<Comment>,
    /// When each `/gemini review` comment was posted, by anyone
    pub summons: Vec<Timestamp>,
    /// Its reviews, oldest first
    pub reviews: Vec<Review>,
    /// The review threads it opened
    pub threads: Vec<Thread>,
}

/// One review Gemini posted
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    /// The forge's id for it
    pub id: u64,
    /// The commit it reviewed
    pub commit: String,
    /// When it was posted
    pub at: Timestamp,
}

const REFUSAL: &str = "You have reached your daily quota limit";

impl Activity {
    /// Whether a summon was posted at `since` or later
    pub fn summoned(&self, since: Timestamp) -> bool {
        let from = since.0.saturating_sub(CLOCK_SLACK);
        self.summons.iter().any(|at| at.0 >= from)
    }

    /// What became of a summon made at `since`, for commit `head`
    pub fn read(&self, head: &str, since: Timestamp) -> Reading {
        if self.answer(head, since).is_some() {
            return Reading::Reviewed;
        }
        // The refusal asks to "wait up to 24 hours", so the window opens a
        // day after it.
        let from = since.0.saturating_sub(CLOCK_SLACK);
        let refusal = self
            .comments
            .iter()
            .filter(|c| c.at.0 >= from && c.body.contains(REFUSAL))
            .map(|c| c.at)
            .max();
        refusal.map_or(Reading::Silent, |at| Reading::Refused {
            opens: Timestamp(at.0.saturating_add(DAY)),
        })
    }

    /// The open threads of the review that answered a summon made at
    /// `since` for `head`. Threads from any other review are not read.
    pub fn open_threads(&self, head: &str, since: Timestamp) -> Vec<&Thread> {
        let Some(review) = self.answer(head, since) else {
            return Vec::new();
        };
        let ours = |t: &&Thread| t.review == Some(review.id) && !t.resolved;
        self.threads.iter().filter(ours).collect()
    }

    // The first review of `head` posted since the summon.
    fn answer(&self, head: &str, since: Timestamp) -> Option<&Review> {
        let from = since.0.saturating_sub(CLOCK_SLACK);
        self.reviews
            .iter()
            .filter(|r| r.commit == head && r.at.0 >= from)
            .min_by_key(|r| r.at)
    }
}

/// A thread as a finding the judge can rule on, in qwen's shape
///
/// The severity comes from the badge Gemini opens each comment with:
/// critical and high are high, medium is medium, low is a nit. The judge
/// regrades it anyway.
pub fn finding(thread: &Thread) -> Finding {
    let badge = thread.body.lines().next().unwrap_or_default();
    let severity = if badge.starts_with("![critical]") || badge.starts_with("![high]") {
        Severity::High
    } else if badge.starts_with("![low]") {
        Severity::Low
    } else {
        Severity::Medium
    };
    let prose = outside_fences(&thread.body);
    let mut paragraphs = prose
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty() && !p.starts_with("!["));
    let what = paragraphs.next().map(one_line).unwrap_or_default();
    let why = one_line(&paragraphs.collect::<Vec<_>>().join(" "));
    Finding {
        severity,
        file: thread.path.clone(),
        line: thread.line.unwrap_or(0),
        what,
        why,
    }
}

// The finding's prose, without its suggested code.
fn outside_fences(body: &str) -> String {
    let mut fenced = false;
    let mut kept = String::new();
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::gh::gemini::{BOT_GRAPHQL, parse_comments, parse_reviews};
    use crate::adapters::gh::outside::parse_threads;

    // google-gemini/gemini-cli#29499: a review on opening, then three
    // `/gemini review` summons, the first answered with two findings and the
    // others clean.
    const COMMENTS_29499: &str = include_str!("../fixtures/gemini-comments-29499.jsonl");
    const REVIEWS_29499: &str = include_str!("../fixtures/gemini-reviews-29499.jsonl");
    const THREADS_29499: &str = include_str!("../fixtures/gemini-threads-29499.json");
    const HEAD_29499: &str = "ab355d1e0dc5d7c5fa7516c9651c23f656439d4e";

    // sednalabs/codex#626: a summon answered with a high and a medium finding.
    const COMMENTS_626: &str = include_str!("../fixtures/gemini-comments-626.jsonl");
    const REVIEWS_626: &str = include_str!("../fixtures/gemini-reviews-626.jsonl");
    const THREADS_626: &str = include_str!("../fixtures/gemini-threads-626.json");
    const HEAD_626: &str = "b165fb8d7ad44294ba790ca08068990757831250";

    // jmmaloney4/codex-proxy#6: every summon refused, out of quota.
    const COMMENTS_6: &str = include_str!("../fixtures/gemini-comments-6.jsonl");

    fn activity(comments: &str, reviews: &str, threads: &str) -> Activity {
        let (comments, summons) = parse_comments(comments.as_bytes()).unwrap();
        Activity {
            comments,
            summons,
            reviews: parse_reviews(reviews.as_bytes()).unwrap(),
            threads: parse_threads(threads.as_bytes(), BOT_GRAPHQL).unwrap(),
        }
    }

    fn iso(text: &str) -> Timestamp {
        let at: jiff::Timestamp = text.parse().unwrap();
        Timestamp(at.as_second().try_into().unwrap())
    }

    #[test]
    fn the_review_gemini_posts_on_its_own_is_not_the_answer_to_a_summon() {
        let seen = activity(COMMENTS_29499, REVIEWS_29499, THREADS_29499);
        let summon = iso("2026-09-25T07:51:29Z");
        assert!(seen.summoned(summon));
        assert_eq!(seen.read(HEAD_29499, summon), Reading::Reviewed);
        let open: Vec<_> = seen.open_threads(HEAD_29499, summon);
        assert!(open.is_empty(), "both threads were resolved by hand");
        let answer = seen.answer(HEAD_29499, summon).unwrap();
        assert_eq!(answer.id, 5_315_120_386, "not the review on opening");
        assert_eq!(
            seen.threads.iter().map(|t| t.review).collect::<Vec<_>>(),
            [Some(answer.id); 2]
        );
    }

    #[test]
    fn a_summon_for_a_head_no_review_covers_is_silent() {
        let seen = activity(COMMENTS_29499, REVIEWS_29499, THREADS_29499);
        let summon = iso("2026-09-25T20:27:58Z");
        assert_eq!(seen.read(HEAD_29499, summon), Reading::Silent);
        assert_eq!(seen.read("0000000", summon), Reading::Silent);
        assert!(!seen.summoned(iso("2026-09-25T21:00:00Z")));
    }

    #[test]
    fn a_clean_review_has_no_open_threads() {
        let seen = activity(COMMENTS_29499, REVIEWS_29499, THREADS_29499);
        let head = "abf19b0bca2f7d311ff2dd8375f5030d4a2e2f14";
        let summon = iso("2026-09-25T08:07:19Z");
        assert_eq!(seen.read(head, summon), Reading::Reviewed);
        assert!(seen.open_threads(head, summon).is_empty());
    }

    #[test]
    fn the_answering_reviews_open_threads_become_findings_in_one_line_each() {
        let seen = activity(COMMENTS_626, REVIEWS_626, THREADS_626);
        let summon = iso("2026-08-20T12:54:55Z");
        assert!(
            seen.summoned(summon),
            "a summon with a trailing space counts"
        );
        assert_eq!(seen.read(HEAD_626, summon), Reading::Reviewed);
        let findings: Vec<Finding> = seen
            .open_threads(HEAD_626, summon)
            .into_iter()
            .map(finding)
            .collect();
        let severities: Vec<_> = findings.iter().map(|f| f.severity).collect();
        assert_eq!(severities, [Severity::High, Severity::Medium]);
        let first = &findings[0];
        assert_eq!(
            (first.file.as_str(), first.line),
            ("codex-rs/ext/goal/tests/continuation_diagnostics.rs", 112)
        );
        assert!(
            first
                .what
                .starts_with("The test is named `record_only_error_observation_keeps_goal_active`"),
            "{}",
            first.what
        );
        assert!(!first.what.contains("```") && !first.why.contains("on_turn_error("));
        assert!(!first.what.contains('\n') && !first.what.contains('|'));
    }

    #[test]
    fn a_refusal_opens_the_window_a_day_later() {
        let seen = activity(
            COMMENTS_6,
            "",
            r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[]}}}}}"#,
        );
        let summon = iso("2026-06-08T05:41:22Z");
        assert!(seen.summoned(summon));
        assert_eq!(
            seen.read("c0ffee", summon),
            Reading::Refused {
                opens: Timestamp(iso("2026-06-08T05:43:07Z").0 + DAY)
            },
            "the latest refusal since the summon"
        );
        assert_eq!(
            seen.read("c0ffee", iso("2026-06-09T00:00:00Z")),
            Reading::Silent,
            "a refusal before the summon is not its answer"
        );
    }

    #[test]
    fn severity_follows_geminis_badge() {
        let thread = |badge: &str| Thread {
            id: "t".into(),
            resolved: false,
            path: "a.rs".into(),
            line: None,
            body: format!(
                "![{badge}](https://www.gstatic.com/codereviewagent/{badge}.svg)\n\n\
                 What.\n\nWhy.\n\n```suggestion\nfix\n```"
            ),
            review: Some(1),
        };
        assert_eq!(finding(&thread("critical")).severity, Severity::High);
        assert_eq!(finding(&thread("high")).severity, Severity::High);
        assert_eq!(finding(&thread("medium")).severity, Severity::Medium);
        assert_eq!(finding(&thread("low")).severity, Severity::Low);
        let odd = finding(&thread("unheard-of"));
        assert_eq!((odd.severity, odd.line), (Severity::Medium, 0));
        assert_eq!((odd.what.as_str(), odd.why.as_str()), ("What.", "Why."));
    }
}
