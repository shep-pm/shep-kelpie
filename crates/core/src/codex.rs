//! Codex's profile as a review bot
//!
//! Codex has no label: a comment summons it. It answers at once with "To use
//! Codex here, create an environment for this repo", the sign it heard and not
//! a refusal, and posts its review a few minutes on. A review that finds
//! nothing is a comment naming the commit it read (`**Reviewed commit:**` and
//! ten characters) or a thumbs up on the pull request. A finding is a thread
//! opening with a priority badge, `P0` to `P3`, and a bold title. Its refusal
//! is a comment that the plan's usage limits are reached, which parks it until
//! the weekly allowance resets. Recorded: shep-pm/shep-kelpie#234.

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
        // A failure counts from the summon itself, with no slack: a summon
        // sent again seconds after a failure must not read that one as its
        // answer.
        let failed = activity
            .comments
            .iter()
            .any(|c| c.at >= since && went_wrong(&c.body));
        match (thumbs, since_summon.next()) {
            (Some(thumbs), _) => Reading::Completed { at: thumbs.at },
            (None, Some(_)) if failed => Reading::Failed,
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

    fn level(&self, thread: &Thread) -> Option<String> {
        let head = thread.body.lines().next().unwrap_or_default();
        badge(head).map(str::to_owned)
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

/// Its refusal is meant to name the plan's usage limits for code reviews, but
/// was never recorded, so a comment naming a limit reached or exceeded, as a
/// phrase, is read as one. A comment that names the commit it read is a review,
/// whatever else it says.
pub fn refused(body: &str) -> bool {
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

// Its reply when a review broke off: "Something went wrong. Try again later
// by commenting "@codex review"." A comment that names the commit it read is
// a review, whatever else it says.
fn went_wrong(body: &str) -> bool {
    commit_of(body).is_none() && says(body, "something went wrong")
}

/// A thread as a finding the worker is sent
///
/// The severity comes from Codex's badge: P0 and P1 are high, P2 is medium,
/// P3 is a nit. The badge goes to the worker as the why.
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
    Finding::new(
        severity,
        thread.path.clone(),
        thread.line.unwrap_or(0),
        one_line(&what),
        level.map_or_else(String::new, |l| format!("Codex rates it {l}.")),
    )
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
