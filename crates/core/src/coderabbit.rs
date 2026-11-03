//! CodeRabbit's profile as a review bot
//!
//! Two traps shape its reading. `gh pr view --json reviews` is empty for a
//! review CodeRabbit liked, so a clean review is read from its walkthrough
//! comment. And the rate-limit notice quotes the commit range it would have
//! read, so the reviewed commit comes from the walkthrough's own markers,
//! never from that range. A third: a summon CodeRabbit finds nothing new in
//! is marked done on the head's commit status, and posts nothing at all.

use crate::ports::{Finding, Severity, Timestamp};
pub use crate::review_bot::{Activity, Comment, Reading, Review, Status, Thread};
use crate::review_bot::{Bot, CLOCK_SLACK, Login, Profile, one_line};

/// The label shep's `.coderabbit.yaml` gates auto review on: the summon
pub const LABEL: &str = "review please";

/// The comment that asks CodeRabbit to read the whole pull request again
pub const FULL_REVIEW: &str = "@coderabbitai full review";

/// CodeRabbit's login on both of the forge's APIs
pub const LOGIN: Login<'static> = Login {
    rest: "coderabbitai[bot]",
    graphql: "coderabbitai",
};

/// CodeRabbit, the first review bot
#[derive(Debug, Clone, Copy, Default)]
pub struct CodeRabbit;

impl Profile for CodeRabbit {
    fn bot(&self) -> Bot {
        Bot::Coderabbit
    }

    fn login(&self) -> Login<'_> {
        LOGIN
    }

    fn label(&self) -> Option<&str> {
        Some(LABEL)
    }

    fn full_review(&self) -> Option<&str> {
        Some(FULL_REVIEW)
    }

    fn read(&self, activity: &Activity, head: &str, since: Timestamp) -> Reading {
        activity.read(head, since)
    }

    fn heard(&self, activity: &Activity, head: &str, since: Timestamp) -> bool {
        activity.heard(head, since)
    }

    fn covers(&self, activity: &Activity, head: &str) -> bool {
        activity.covers(head)
    }

    fn reviewed_besides(&self, activity: &Activity, head: &str) -> u32 {
        activity.reviewed_besides(head)
    }

    fn quota(&self, activity: &Activity) -> Option<(u32, Timestamp)> {
        activity.quota()
    }

    fn finding(&self, thread: &Thread) -> Finding {
        finding(thread)
    }
}

// The comment CodeRabbit edits in place, found by what it says.
const STICKY: [&str; 3] = [
    "## Walkthrough",
    "Review limit reached",
    "Files selected for processing",
];
const LIMIT: &str = "Review limit reached";
// A rate limit and a skip are marked done too, under other words.
const COMPLETED: &str = "Review completed";
const RUNNING: &str = "Review in progress";
// The statuses of a repo whose settings do not let the summon start a review.
const SKIPS: [&str; 2] = [
    "Review skipped: automatic reviews are disabled",
    "Review skipped: excluded by label configuration",
];

const WAITS: [&str; 2] = [
    "Next included review available in ",
    "next included review will be available in ",
];

/// CodeRabbit's reading of its activity, which only its profile calls
pub trait Reads {
    /// Whether it reviewed `head`
    fn covers(&self, head: &str) -> bool;
    /// What it said of `head` since `since`
    fn read(&self, head: &str, since: Timestamp) -> Reading;
    /// Whether it answered at all since `since`
    fn heard(&self, head: &str, since: Timestamp) -> bool;
    /// The reviews it has left and when the quota refills, if it said
    fn quota(&self) -> Option<(u32, Timestamp)>;
    /// How many reviews of `head` came from others than it
    fn reviewed_besides(&self, head: &str) -> u32;
    /// Its sticky comment, if it left one
    fn sticky(&self) -> Option<&Comment>;
}

impl Reads for Activity {
    fn covers(&self, head: &str) -> bool {
        self.reviews.iter().any(|r| r.commit == head)
            || self.sticky().and_then(|c| covered(&c.body)) == Some(head)
    }

    fn read(&self, head: &str, since: Timestamp) -> Reading {
        if self.covers(head) {
            return Reading::Reviewed;
        }
        if self.sticky().is_some_and(|c| processing(&c.body)) {
            return Reading::Processing;
        }
        // Listed by creation, and the walkthrough is edited in place, so
        // the newest refusal is found by when it was last edited.
        let from = since.0.saturating_sub(CLOCK_SLACK);
        let refusal = self
            .comments
            .iter()
            .filter(|c| c.at.0 >= from)
            .filter_map(|c| Some((c.at, wait(&c.body)?)))
            .max_by_key(|(at, _)| *at);
        if let Some((at, wait)) = refusal {
            return Reading::Refused {
                opens: Some(Timestamp(at.0.saturating_add(wait))),
            };
        }
        let ours = self
            .statuses
            .iter()
            .filter(|s| s.commit == head && s.at.0 >= from);
        let done = ours.clone().filter(|s| s.description == COMPLETED);
        if let Some(at) = done.map(|s| s.at).max() {
            return Reading::Completed { at };
        }
        // Only the newest status says where it stands: a running review
        // that ended in a status this does not parse is not still running.
        let newest = ours.max_by_key(|s| s.at);
        let Some(newest) = newest else {
            return Reading::Silent;
        };
        if newest.description == RUNNING {
            return Reading::Processing;
        }
        // A skip a push set just before the summon is not its answer, so this is
        // read from the summon itself, with no slack (#399). A clock ahead of
        // GitHub's reads the answer as Silent, which the two hours pass over: a
        // missed skip costs that wait, a mistaken one a review.
        let declined = SKIPS.into_iter().find(|skip| *skip == newest.description);
        match declined {
            Some(status) if newest.at.0 >= since.0 => Reading::Declined { status },
            _ => Reading::Silent,
        }
    }

    // Any comment edited or posted, review posted, or status set on the
    // head counts. A refusal is a comment, so it counts.
    fn heard(&self, head: &str, since: Timestamp) -> bool {
        let from = since.0.saturating_sub(CLOCK_SLACK);
        self.comments.iter().any(|c| c.at.0 >= from)
            || self.reviews.iter().any(|r| r.at.0 >= from)
            || self
                .statuses
                .iter()
                .any(|s| s.commit == head && s.at.0 >= from)
    }

    // The quota the latest footer states, and when that footer was posted.
    fn quota(&self) -> Option<(u32, Timestamp)> {
        let reviews = self.reviews.iter().map(|r| (r.at, r.body.as_str()));
        let comments = self.comments.iter().map(|c| (c.at, c.body.as_str()));
        reviews
            .chain(comments)
            .filter_map(|(at, body)| Some((quota(body)?, at)))
            .max_by_key(|(_, at)| *at)
    }

    // Each commit a posted review is of, and the one a clean review's
    // walkthrough covers. A reply in a thread posts a review with no body.
    fn reviewed_besides(&self, head: &str) -> u32 {
        let posted = self.reviews.iter().filter(|r| !r.body.trim().is_empty());
        let mut commits: Vec<&str> = posted.map(|r| r.commit.as_str()).collect();
        commits.extend(self.sticky().and_then(|c| covered(&c.body)));
        commits.sort_unstable();
        commits.dedup();
        commits.retain(|c| *c != head);
        u32::try_from(commits.len()).unwrap_or(u32::MAX)
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

/// Seconds until the window opens, from "... available in 12 minutes." or
/// "... in 1 hour and 5 minutes".
pub fn wait(body: &str) -> Option<u64> {
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

/// A thread as a finding the worker is sent, in qwen's shape
///
/// The severity comes from CodeRabbit's own label, the first line past its
/// HTML comments: Critical and Major are high, Minor is medium, Trivial and
/// Nitpick are nits. The title is the bold paragraph after it.
pub fn finding(thread: &Thread) -> Finding {
    // The collapsed blocks go first, since a script shown in one may hold
    // a stray `<!--` that would swallow the title after it.
    let prose = outside_details(&thread.body);
    let shown = strip(&strip(&prose, "<!--", "-->"), "<picture>", "</picture>");
    let paragraphs: Vec<&str> = shown
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let label = paragraphs.first().and_then(|p| label(p));
    let severity = label.unwrap_or(Severity::Medium);
    // Past the label the title is the first bold paragraph. Without a bold
    // title the worker still gets the text, past the first paragraph.
    let body = &paragraphs[usize::from(label.is_some()).min(paragraphs.len())..];
    let (what, why) = match body.iter().position(|p| p.starts_with("**")) {
        Some(at) => (
            one_line(body[at].trim_matches('*')),
            body.get(at + 1).map(|p| one_line(p)).unwrap_or_default(),
        ),
        None => (
            one_line(&paragraphs[1.min(paragraphs.len())..].join(" ")),
            String::new(),
        ),
    };
    Finding::new(
        severity,
        thread.path.clone(),
        thread.line.unwrap_or(0),
        what,
        why,
    )
}

// The severity a paragraph's first line reads as when that line is a label.
// A label line is joined by `|` or `·`, whatever word it names, or is plain
// text naming a severity. A bold line is a title, even one naming a severity.
fn label(paragraph: &str) -> Option<Severity> {
    let line = paragraph.lines().next()?;
    let named = NAMES.iter().find(|(name, _)| line.contains(name));
    let severity = named.map(|(_, severity)| *severity);
    if line.contains(" | ") || line.contains(" \u{b7} ") {
        Some(severity.unwrap_or(Severity::Medium))
    } else if line.starts_with("**") {
        None
    } else {
        severity
    }
}

// The words of CodeRabbit's label and the severity each is read as.
const NAMES: [(&str, Severity); 5] = [
    ("Critical", Severity::High),
    ("Major", Severity::High),
    ("Minor", Severity::Medium),
    ("Trivial", Severity::Low),
    ("Nitpick", Severity::Low),
];

// The text without each `open` .. `close` span. An unclosed `open` stays.
fn strip(text: &str, open: &str, close: &str) -> String {
    let mut kept = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        let Some(end) = rest[start..].find(close) else {
            break;
        };
        kept.push_str(&rest[..start]);
        rest = &rest[start + end + close.len()..];
    }
    kept.push_str(rest);
    kept
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
