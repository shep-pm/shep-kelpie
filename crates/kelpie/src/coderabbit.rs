//! Reading what CodeRabbit has done on a pull request
//!
//! Two traps shape this. `gh pr view --json reviews` is empty for a review
//! CodeRabbit liked, so a clean review is read from its walkthrough comment.
//! And the rate-limit notice quotes the commit range it would have read, so
//! the reviewed commit comes from the walkthrough's own markers, never from
//! that range. Pure: the forge fetches, this reads.

use crate::ports::{Finding, Severity, Timestamp};

/// Everything CodeRabbit has posted on one pull request
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    /// Its comments on the conversation, oldest first
    pub comments: Vec<Comment>,
    /// Its reviews, oldest first. A clean review posts none.
    pub reviews: Vec<Review>,
    /// The review threads it opened
    pub threads: Vec<Thread>,
}

/// One of CodeRabbit's conversation comments, as last edited
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// Its text
    pub body: String,
    /// When it was last edited: the walkthrough is edited in place
    pub at: Timestamp,
}

/// One review CodeRabbit posted
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    /// The commit it reviewed
    pub commit: String,
    /// Its text, whose footer states the quota
    pub body: String,
    /// When it was posted
    pub at: Timestamp,
}

/// A review thread CodeRabbit opened
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// The forge's id for it, which resolving it takes
    pub id: String,
    /// Whether it is resolved
    pub resolved: bool,
    /// The file it is on
    pub path: String,
    /// The line it is on, if it still maps to one
    pub line: Option<u32>,
    /// Its first comment: the finding
    pub body: String,
}

/// What became of a summon, read at one moment
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    /// A review covers the head
    Reviewed,
    /// A review is running
    Processing,
    /// CodeRabbit refused, quoting when its window opens
    Refused {
        /// When the window opens
        opens: Timestamp,
    },
    /// Nothing yet
    Silent,
}

// The comment CodeRabbit edits in place, found by what it says.
const STICKY: [&str; 3] = [
    "## Walkthrough",
    "Review limit reached",
    "Files selected for processing",
];
const LIMIT: &str = "Review limit reached";

// A refusal stamped this long before the summon is still its answer. One a
// minute older quotes the same window, so misreading it costs nothing.
const CLOCK_SLACK: u64 = 60;
const WAITS: [&str; 2] = [
    "Next included review available in ",
    "next included review will be available in ",
];

impl Activity {
    /// Whether a review covers commit `head`
    pub fn covers(&self, head: &str) -> bool {
        self.reviews.iter().any(|r| r.commit == head)
            || self.sticky().and_then(|c| covered(&c.body)) == Some(head)
    }

    /// What became of a summon made at `since`, for commit `head`
    pub fn read(&self, head: &str, since: Timestamp) -> Reading {
        if self.covers(head) {
            return Reading::Reviewed;
        }
        if self.sticky().is_some_and(|c| processing(&c.body)) {
            return Reading::Processing;
        }
        // Listed by creation, and the walkthrough is edited in place, so
        // the newest refusal is found by when it was last edited. GitHub's
        // clock is not this machine's, hence the slack.
        let from = since.0.saturating_sub(CLOCK_SLACK);
        let refusal = self
            .comments
            .iter()
            .filter(|c| c.at.0 >= from)
            .filter_map(|c| Some((c.at, wait(&c.body)?)))
            .max_by_key(|(at, _)| *at);
        refusal.map_or(Reading::Silent, |(at, wait)| Reading::Refused {
            opens: Timestamp(at.0.saturating_add(wait)),
        })
    }

    /// The quota the latest footer states: included reviews an hour
    pub fn quota(&self) -> Option<u32> {
        let reviews = self.reviews.iter().map(|r| (r.at, r.body.as_str()));
        let comments = self.comments.iter().map(|c| (c.at, c.body.as_str()));
        let mut footers: Vec<_> = reviews
            .chain(comments)
            .filter_map(|(at, body)| Some((at, quota(body)?)))
            .collect();
        footers.sort_by_key(|(at, _)| *at);
        footers.last().map(|(_, quota)| *quota)
    }

    /// Its threads not yet resolved
    pub fn open_threads(&self) -> impl Iterator<Item = &Thread> {
        self.threads.iter().filter(|t| !t.resolved)
    }

    fn sticky(&self) -> Option<&Comment> {
        let sticky = |c: &&Comment| STICKY.iter().any(|m| c.body.contains(m));
        self.comments.iter().rfind(sticky)
    }
}

// The commit the walkthrough says it read: its coverage marker, then its
// change assessment, then its own range when no limit block shares it.
fn covered(body: &str) -> Option<&str> {
    if let Some(id) = after(body, "\"coveredCommitId\":\"").and_then(|s| s.split('"').next()) {
        return Some(id);
    }
    if let Some(id) = after(body, "<!-- change_assessment_commit:\"") {
        return id.split('"').next();
    }
    if body.contains(LIMIT) || !body.contains("## Walkthrough") {
        return None;
    }
    let range = after(
        body,
        "Reviewing files that changed from the base of the PR and between ",
    )?;
    let (_, to) = range.split_once(" and ")?;
    to.split('.').next()
}

fn processing(body: &str) -> bool {
    body.contains("Currently processing new changes")
        || (body.contains("Files selected for processing")
            && !body.contains("## Walkthrough")
            && !body.contains(LIMIT))
}

// Seconds until the window opens, from "... available in 12 minutes." or
// "... in 1 hour and 5 minutes".
fn wait(body: &str) -> Option<u64> {
    let text = WAITS.iter().find_map(|w| after(body, w))?;
    let mut words = text.split_whitespace();
    let mut total = None;
    while let (Some(n), Some(unit)) = (words.next(), words.next()) {
        let Ok(n) = n.parse::<u64>() else { break };
        let unit = unit.trim_end_matches(|c: char| !c.is_ascii_alphabetic());
        let scale = match unit.trim_end_matches('s') {
            "hour" => 3600,
            "minute" => 60,
            "second" => 1,
            _ => break,
        };
        let sum: &mut u64 = total.get_or_insert(0);
        *sum = sum.saturating_add(n.saturating_mul(scale));
        if words.next() != Some("and") {
            break;
        }
    }
    total
}

fn quota(body: &str) -> Option<u32> {
    let text = after(body, "Your plan provides up to ")?;
    text.split_whitespace().next()?.parse().ok()
}

fn after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    text.find(marker).map(|at| &text[at + marker.len()..])
}

/// A thread as a finding the judge can rule on, in qwen's shape
///
/// The severity comes from CodeRabbit's own label: Critical and Major are
/// high, Minor is medium, Trivial is a nit. The judge regrades it anyway.
pub fn finding(thread: &Thread) -> Finding {
    let label = thread.body.lines().next().unwrap_or_default();
    let severity = if label.contains("Critical") || label.contains("Major") {
        Severity::High
    } else if label.contains("Trivial") || label.contains("Nitpick") {
        Severity::Low
    } else {
        Severity::Medium
    };
    let prose = outside_details(&thread.body);
    let paragraphs: Vec<&str> = prose
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    // Without a bold title the judge still gets the text, past the label.
    let (what, why) = match paragraphs.iter().position(|p| p.starts_with("**")) {
        Some(at) => (
            one_line(paragraphs[at].trim_matches('*')),
            paragraphs
                .get(at + 1)
                .map(|p| one_line(p))
                .unwrap_or_default(),
        ),
        None => (
            one_line(&paragraphs[1.min(paragraphs.len())..].join(" ")),
            String::new(),
        ),
    };
    Finding {
        severity,
        file: thread.path.clone(),
        line: thread.line.unwrap_or(0),
        what,
        why,
    }
}

// The finding's prose, without the collapsed sections of scripts and fixes.
fn outside_details(body: &str) -> String {
    let mut depth = 0usize;
    let mut kept = String::new();
    for line in body.lines() {
        let opens = line.matches("<details").count();
        let closes = line.matches("</details>").count();
        if depth == 0 && opens == 0 {
            kept.push_str(line);
            kept.push('\n');
        }
        depth = (depth + opens).saturating_sub(closes);
    }
    kept
}

// A findings file holds one finding a line, fields split by `|`.
fn one_line(text: &str) -> String {
    const MOST: usize = 600;
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = flat.replace('|', "/");
    match flat.char_indices().nth(MOST) {
        Some((cut, _)) => format!("{}...", &flat[..cut]),
        None => flat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::gh::coderabbit::{parse_comments, parse_reviews, parse_threads};

    // Recorded from shep with the gh adapter's own calls: its sticky holds
    // a walkthrough up to ce143d9 and a limit block quoting the head.
    const COMMENTS_598: &str = include_str!("../fixtures/coderabbit-comments-598.jsonl");
    const REVIEWS_598: &str = include_str!("../fixtures/coderabbit-reviews-598.jsonl");
    const HEAD_598: &str = "f0f94c8f2b74a03bec6ac085af8a3643b41e574a";

    // A clean review of the head, which posted no review at all.
    const COMMENTS_615: &str = include_str!("../fixtures/coderabbit-comments-615.jsonl");
    const HEAD_615: &str = "a11b010bb5f36890e745a740874708c28e4bcbf9";

    // A review of the head with three threads still open.
    const REVIEWS_617: &str = include_str!("../fixtures/coderabbit-reviews-617.jsonl");
    const THREADS_617: &str = include_str!("../fixtures/coderabbit-threads-617.json");
    const HEAD_617: &str = "f642b8aca0a37c3044166a8f384458b7ecc381be";

    fn activity(comments: &str, reviews: &str, threads: &str) -> Activity {
        Activity {
            comments: parse_comments(comments.as_bytes()).unwrap(),
            reviews: parse_reviews(reviews.as_bytes()).unwrap(),
            threads: parse_threads(threads.as_bytes()).unwrap(),
        }
    }

    const NO_THREADS: &str =
        r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[]}}}}}"#;

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
                opens: iso("2026-09-22T22:32:37Z")
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
                opens: iso("2026-09-24T13:00:41Z")
            },
            "12:22:41 plus 38 minutes"
        );
    }

    #[test]
    fn the_quota_comes_from_the_latest_footer() {
        let seen = activity(COMMENTS_598, REVIEWS_598, NO_THREADS);
        assert_eq!(seen.quota(), Some(1));
        let mut seen = activity(COMMENTS_615, "", NO_THREADS);
        seen.reviews.push(Review {
            commit: "c0ffee".into(),
            body: "**Included review availability:** Your plan provides up to 10 \
                   included reviews per hour; 6 remain after this review."
                .into(),
            at: iso("2026-09-25T00:00:00Z"),
        });
        assert_eq!(seen.quota(), Some(10));
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
    fn a_thread_without_a_bold_title_still_gives_the_judge_its_text() {
        let thread = Thread {
            id: "t".into(),
            resolved: false,
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

    #[test]
    fn a_long_finding_is_cut_to_one_line() {
        let long = "word ".repeat(200);
        let cut = one_line(&long);
        assert_eq!(cut.chars().count(), 603);
        assert!(cut.ends_with("..."));
        assert_eq!(one_line("a |b\n c"), "a /b c");
    }
}
