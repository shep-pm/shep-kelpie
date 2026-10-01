//! Codex's profile as a review bot
//!
//! Codex has no label: a comment summons it. It answers at once with a
//! comment, "To use Codex here, create an environment for this repo", which
//! is the sign it heard and not a refusal, and a few minutes on it posts its
//! review. A review that finds nothing is a conversation comment that names
//! the commit it read, `**Reviewed commit:**` and its first ten characters.
//! Each finding is a thread that opens with a priority badge, `P0` to `P3`,
//! and a bold title. Its refusal is a comment saying the plan's usage limits
//! for code reviews are reached, which parks it until the weekly allowance
//! resets. A thumbs up it leaves on the pull request reads as a review that
//! found nothing. Recorded on shep-pm/shep-kelpie#234 are its notice, its
//! clean review comment and its thumbs up; a review with findings and its
//! refusal are written from its documentation.

use crate::ports::{Finding, Severity, Timestamp};
use crate::review_bot::{Activity, Bot, CLOCK_SLACK, Login, Profile, Reading, Thread, one_line};

/// The comment that summons Codex
pub const SUMMON: &str = "@codex review";

/// Codex's login on both of the forge's APIs
pub const LOGIN: Login<'static> = Login {
    rest: "chatgpt-codex-connector[bot]",
    graphql: "chatgpt-codex-connector",
};

// What its review's body says it is, beside the commit it read.
const REVIEW: &str = "codex review";

// Where its comment on a review that found nothing names the commit, which
// it cuts to ten characters.
const READ: &str = "**Reviewed commit:** `";
const TEN: usize = 10;

// The reaction it leaves on the pull request when it has nothing to say.
const THUMBS_UP: &str = "THUMBS_UP";

/// Codex, the third review bot
#[derive(Debug, Clone, Copy, Default)]
pub struct Codex;

impl Profile for Codex {
    fn bot(&self) -> Bot {
        Bot::Codex
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
        let from = since.0.saturating_sub(CLOCK_SLACK);
        let mut since_summon = activity.comments.iter().filter(|c| c.at.0 >= from);
        // A refusal stands, whatever it posted after.
        if since_summon.clone().any(|c| refused(&c.body)) {
            return Reading::Refused { opens: None };
        }
        // A thumbs up is its review with nothing in it, and it may leave no
        // comment, only the notice it answers every summon with. It names no
        // commit, so one that came with a comment on another is that review's.
        let late = since_summon
            .clone()
            .filter_map(|c| commit_of(&c.body))
            .any(|commit| !names(head, commit));
        let thumbs = activity
            .reactions
            .iter()
            .find(|r| r.at.0 >= from && r.content == THUMBS_UP && !late);
        match (thumbs, since_summon.next()) {
            (Some(thumbs), _) => Reading::Completed { at: thumbs.at },
            (None, Some(_)) => Reading::Processing,
            (None, None) => Reading::Silent,
        }
    }

    fn heard(&self, activity: &Activity, _head: &str, since: Timestamp) -> bool {
        let from = since.0.saturating_sub(CLOCK_SLACK);
        activity.comments.iter().any(|c| c.at.0 >= from)
            || activity.reviews.iter().any(|r| r.at.0 >= from)
            || activity.reactions.iter().any(|r| r.at.0 >= from)
    }

    fn covers(&self, activity: &Activity, head: &str) -> bool {
        reviewed(activity).any(|commit| names(head, commit))
    }

    fn reviewed_besides(&self, activity: &Activity, head: &str) -> u32 {
        // A comment gives ten characters of a commit and a review all of it.
        let mut commits: Vec<&str> = reviewed(activity)
            .filter(|c| !names(head, c))
            .map(|c| c.get(..TEN).unwrap_or(c))
            .collect();
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

// The commits it read: a review's, or a comment's that says so. A reply in a
// thread posts a review with no such heading, which is no review. A comment
// names only the first ten characters, so a commit is read by its prefix.
fn reviewed(activity: &Activity) -> impl Iterator<Item = &str> {
    let posted = activity.reviews.iter().filter(|r| says(&r.body, REVIEW));
    let said = activity.comments.iter().filter_map(|c| commit_of(&c.body));
    posted.map(|r| r.commit.as_str()).chain(said)
}

// The commit a comment says it read, as far as it gives it.
fn commit_of(body: &str) -> Option<&str> {
    let (_, after) = body.split_once(READ)?;
    after.split('`').next().filter(|commit| !commit.is_empty())
}

// Whether `commit`, whole or cut short, is `head`.
fn names(head: &str, commit: &str) -> bool {
    !commit.is_empty() && head.starts_with(commit)
}

fn says(body: &str, phrase: &str) -> bool {
    body.to_lowercase().contains(phrase)
}

// Its refusal is meant to name the plan's usage limits for code reviews, but
// was never recorded, so a comment naming a limit reached or exceeded, as a
// phrase, is read as one. A comment that names the commit it read is a review,
// whatever else it says.
fn refused(body: &str) -> bool {
    if commit_of(body).is_some() {
        return false;
    }
    let body = body.to_lowercase();
    [
        "usage limit",
        "limits for code review",
        "limit reached",
        "limit exceeded",
    ]
    .iter()
    .any(|phrase| body.contains(phrase))
        || ["reached your", "exceeded your"]
            .iter()
            .any(|phrase| body.contains(phrase) && body.contains(" limit"))
}

/// A thread as a finding the judge can rule on
///
/// The severity comes from Codex's badge: P0 and P1 are high, P2 is medium,
/// P3 is a nit. The badge goes to the judge as the why.
fn finding(thread: &Thread) -> Finding {
    let mut lines = thread.body.lines();
    let head = lines.next().unwrap_or_default();
    let level = badge(head);
    let title = title(head);
    let body: Vec<&str> = lines
        .take_while(|line| !line.starts_with("Useful?") && !line.starts_with("<details"))
        .collect();
    let body = body.join("\n");
    let body = body.trim();
    let what = match (title.is_empty(), body.is_empty()) {
        (_, true) => title.to_owned(),
        (true, false) => body.to_owned(),
        (false, false) => format!("{title}: {body}"),
    };
    let severity = match level {
        Some("P0" | "P1") => Severity::High,
        Some("P3") => Severity::Low,
        _ => Severity::Medium,
    };
    Finding {
        severity,
        file: thread.path.clone(),
        line: thread.line.unwrap_or(0),
        what: one_line(&what),
        why: level.map_or_else(String::new, |l| format!("Codex rates it {l}.")),
    }
}

// The level its badge names: `![P1 Badge](...)`.
fn badge(head: &str) -> Option<&str> {
    let at = head.find("![P")? + 2;
    let level = head.get(at..at + 2)?;
    let digit = level.as_bytes()[1];
    (b'0'..=b'3').contains(&digit).then_some(level)
}

// The bold title after the badge's markup, which closes with `</sub></sub>`.
fn title(head: &str) -> &str {
    let after = head.rsplit_once("</sub>").map_or(head, |(_, title)| title);
    after.trim().trim_matches('*').trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::gh::review_bot::{parse_comments, parse_reactions};
    use crate::review_bot::{Comment, Reaction, Review};

    // Recorded from shep-pm/shep-kelpie#234 with the gh adapter's own call:
    // the maintainer's summon at 04:39:19Z drew "create an environment" at
    // 04:39:27Z, and its review of 8eecd21 at 04:43:07Z found nothing.
    const COMMENTS_234: &str = include_str!("../fixtures/codex-comments-234.jsonl");
    const REACTIONS_234: &str = include_str!("../fixtures/codex-reactions-234.json");
    const HEAD_234: &str = "8eecd218357d9d3a017baeff71c374f6865466e1";

    fn iso(text: &str) -> Timestamp {
        let at: jiff::Timestamp = text.parse().unwrap();
        Timestamp(at.as_second().try_into().unwrap())
    }

    fn activity(comments: &str) -> Activity {
        Activity {
            comments: parse_comments(comments.as_bytes()).unwrap(),
            ..Activity::default()
        }
    }

    fn thread(body: &str) -> Thread {
        Thread {
            id: "t".into(),
            resolved: false,
            path: "src/a.rs".into(),
            line: Some(12),
            body: body.into(),
        }
    }

    fn comment(body: &str, at: u64) -> Comment {
        Comment {
            body: body.into(),
            at: Timestamp(at),
        }
    }

    #[test]
    fn its_first_comment_is_the_sign_it_heard_and_no_refusal() {
        let summoned = iso("2026-10-01T04:39:19Z");
        let answered = activity(COMMENTS_234.lines().next().unwrap());
        assert!(answered.comments[0].body.contains("create an environment"));
        assert!(Codex.heard(&answered, HEAD_234, summoned));
        assert_eq!(
            Codex.read(&answered, HEAD_234, summoned),
            Reading::Processing
        );
        let later = iso("2026-10-01T05:00:00Z");
        assert!(!Codex.heard(&answered, HEAD_234, later), "a later summon");
        assert_eq!(Codex.read(&answered, HEAD_234, later), Reading::Silent);
    }

    #[test]
    fn its_comment_naming_the_commit_covers_it_by_the_ten_characters_it_gives() {
        let seen = activity(COMMENTS_234);
        let summoned = iso("2026-10-01T04:39:19Z");
        assert_eq!(Codex.read(&seen, HEAD_234, summoned), Reading::Reviewed);
        assert!(!Codex.covers(&seen, "8eecd21fffffff"));
        assert!(!Codex.covers(&seen, ""));
        assert_eq!(Codex.reviewed_besides(&seen, HEAD_234), 0);
        assert_eq!(Codex.reviewed_besides(&seen, "0ther"), 1);
        assert_eq!(Codex.quota(&seen), None);
    }

    #[test]
    fn a_review_covers_the_commit_it_read() {
        let seen = Activity {
            reviews: vec![Review {
                commit: "abc123".into(),
                body: "### 💡 Codex Review\n\nHere are some automated review suggestions.".into(),
                at: Timestamp(200),
            }],
            ..Activity::default()
        };
        assert_eq!(
            Codex.read(&seen, "abc123", Timestamp(100)),
            Reading::Reviewed
        );
        assert!(!Codex.covers(&seen, "other"));
        assert_eq!(Codex.reviewed_besides(&seen, "other"), 1);
    }

    #[test]
    fn a_reply_in_a_thread_is_no_review() {
        let seen = Activity {
            reviews: vec![Review {
                commit: "abc123".into(),
                body: "Thanks, fixed.".into(),
                at: Timestamp(200),
            }],
            ..Activity::default()
        };
        assert!(!Codex.covers(&seen, "abc123"));
    }

    #[test]
    fn a_usage_limit_reply_is_a_refusal_with_no_opening() {
        let refusal = Activity {
            comments: vec![comment(
                "You have reached your Codex usage limits for code reviews. You can see \
                 your limits in the [Codex usage dashboard](https://chatgpt.com/codex/settings/usage).",
                130,
            )],
            ..Activity::default()
        };
        assert_eq!(
            Codex.read(&refusal, "abc123", Timestamp(100)),
            Reading::Refused { opens: None }
        );
        assert!(Codex.heard(&refusal, "abc123", Timestamp(100)));
        let mut then_more = refusal.clone();
        then_more
            .comments
            .push(comment("To use Codex here, create an environment.", 140));
        assert_eq!(
            Codex.read(&then_more, "abc123", Timestamp(100)),
            Reading::Refused { opens: None },
            "a later comment does not hide it"
        );
    }

    #[test]
    fn a_thread_reaches_the_judge_with_its_badge_and_title() {
        let first = Codex.finding(&thread(
            "**<sub><sub>![P1 Badge](https://img.shields.io/badge/P1-orange?style=flat)</sub></sub>  \
             Guard against empty input**\n\n`parse()` panics when `input` is empty.\n\n\
             Useful? React with 👍 / 👎.",
        ));
        assert_eq!(first.severity, Severity::High);
        assert_eq!((first.file.as_str(), first.line), ("src/a.rs", 12));
        assert_eq!(
            first.what,
            "Guard against empty input: `parse()` panics when `input` is empty."
        );
        assert_eq!(first.why, "Codex rates it P1.");
    }

    #[test]
    fn a_p2_is_medium_a_p3_a_nit_and_a_thread_with_no_badge_is_medium() {
        let level = |badge: &str| {
            Codex
                .finding(&thread(&format!(
                    "**<sub><sub>![{badge} Badge](https://img.shields.io/badge/x)</sub></sub>  T**\n\nWhy."
                )))
                .severity
        };
        assert_eq!(level("P2"), Severity::Medium);
        assert_eq!(level("P3"), Severity::Low);
        assert_eq!(level("P0"), Severity::High);
        let bare = Codex.finding(&thread("It drops the error."));
        assert_eq!(bare.severity, Severity::Medium);
        assert_eq!(
            (bare.what.as_str(), bare.why.as_str()),
            ("It drops the error.", "")
        );
    }

    #[test]
    fn its_thumbs_up_recorded_on_the_pull_request_is_a_review_with_nothing_in_it() {
        let seen = Activity {
            reactions: parse_reactions(REACTIONS_234.as_bytes(), LOGIN.rest).unwrap(),
            ..Activity::default()
        };
        let summoned = iso("2026-10-01T04:39:19Z");
        assert!(Codex.heard(&seen, HEAD_234, summoned));
        assert_eq!(
            Codex.read(&seen, HEAD_234, summoned),
            Reading::Completed {
                at: iso("2026-10-01T04:43:06Z")
            }
        );
        let later = iso("2026-10-01T05:00:00Z");
        assert_eq!(Codex.read(&seen, HEAD_234, later), Reading::Silent);
    }

    #[test]
    fn a_thumbs_up_does_not_hide_a_refusal_and_another_reaction_is_no_review() {
        let eyes = Reaction {
            content: "EYES".into(),
            at: Timestamp(120),
        };
        let seen = Activity {
            reactions: vec![eyes.clone()],
            ..Activity::default()
        };
        assert!(Codex.heard(&seen, "abc123", Timestamp(100)));
        assert_eq!(Codex.read(&seen, "abc123", Timestamp(100)), Reading::Silent);
        let refused = Activity {
            comments: vec![comment("You've hit your usage limit for reviews.", 130)],
            reactions: vec![Reaction {
                content: "THUMBS_UP".into(),
                at: Timestamp(140),
            }],
            ..Activity::default()
        };
        assert_eq!(
            Codex.read(&refused, "abc123", Timestamp(100)),
            Reading::Refused { opens: None }
        );
    }

    #[test]
    fn a_refusal_in_other_words_is_still_one_and_the_review_footer_is_not() {
        assert!(refused("Usage limit reached for code reviews."));
        assert!(refused("You have exceeded your limit this week."));
        assert!(!refused(
            "Codex Review: Didn't find any major issues.\n\nCodex can also answer questions."
        ));
        assert!(!refused(
            "To use Codex here, create an environment for this repo."
        ));
    }

    #[test]
    fn one_commit_read_by_a_review_and_by_a_comment_counts_once() {
        let seen = Activity {
            reviews: vec![Review {
                commit: "8eecd218357d9d3a017baeff71c374f6865466e1".into(),
                body: "### 💡 Codex Review".into(),
                at: Timestamp(100),
            }],
            comments: vec![comment("**Reviewed commit:** `8eecd21835`", 200)],
            ..Activity::default()
        };
        assert_eq!(Codex.reviewed_besides(&seen, "0ther"), 1);
        assert_eq!(Codex.reviewed_besides(&seen, HEAD_234), 0);
    }

    #[test]
    fn a_thumbs_up_beside_a_review_of_another_commit_is_not_a_review_of_the_head() {
        let seen = Activity {
            comments: vec![comment(
                "Codex Review: Didn't find any major issues.\n\n**Reviewed commit:** `aaaaaaaaaa`",
                200,
            )],
            reactions: vec![Reaction {
                content: "THUMBS_UP".into(),
                at: Timestamp(201),
            }],
            ..Activity::default()
        };
        let head = "bbbbbbbbbbbbbbbbbbbb";
        assert_eq!(Codex.read(&seen, head, Timestamp(100)), Reading::Processing);
        assert_eq!(
            Codex.read(&seen, "aaaaaaaaaabbbb", Timestamp(100)),
            Reading::Reviewed
        );
    }

    #[test]
    fn a_stale_clean_comment_or_a_word_that_holds_limit_is_no_refusal() {
        let stale = Activity {
            comments: vec![comment(
                "Codex Review: the usage limit is reached in `rate.rs`.\n\n**Reviewed commit:** `aaaaaaaaaa`",
                200,
            )],
            ..Activity::default()
        };
        assert_eq!(
            Codex.read(&stale, "bbbbbbbbbb", Timestamp(100)),
            Reading::Processing
        );
        for text in [
            "Delimiters reached the parser.",
            "An unlimited plan exceeded nothing.",
            "The rate-limited path was reached.",
        ] {
            assert!(!refused(text), "{text}");
        }
        assert!(refused(
            "You have reached your Codex usage limits for code reviews."
        ));
        assert!(refused("Review limit exceeded."));
    }
}
