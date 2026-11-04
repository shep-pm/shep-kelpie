//! cubic's profile as a review bot
//!
//! cubic has no label: a comment summons it, and a comment a few seconds on
//! says it started. Its review posts whether or not it found anything, on
//! the commit it read, with its summary between markers. Each finding is a
//! thread whose text opens with its level, `P1` to `P3`, below a marker
//! carrying its confidence out of 10. It sets no commit status and states no
//! quota. Its refusal is a check run on the head titled [`LIMIT`], which
//! names the day reviews resume; a comment saying a limit was reached is
//! read as one too, though none has been seen.

use crate::ports::{Finding, Severity, Timestamp};
use crate::review_bot::{
    Activity, Bot, CLOCK_SLACK, CheckRun, Login, Profile, Reading, Thread, one_line,
};

/// The comment that summons cubic
pub const SUMMON: &str = "@cubic-dev-ai review";

/// cubic's login on both of the forge's APIs
pub const LOGIN: Login<'static> = Login {
    rest: "cubic-dev-ai[bot]",
    graphql: "cubic-dev-ai",
};

/// What cubic says when it starts a review
pub const STARTED: &str = "I have started the AI code review";

/// What cubic's check run on the head is titled when its plan is out of
/// reviewed lines, which is the refusal seen
pub const LIMIT: &str = "AI review line limit reached";

const SUMMARY: &str = "<!-- cubic:review-summary:start -->";
const CONFIDENCE: &str = "<!-- metadata:{\"confidence\":";
const RESUME: &str = "Reviews resume on";
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

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
        // A head cubic ran out of reviewed lines on is refused whatever else
        // it posted there.
        if let Some(run) = limited(activity) {
            return Reading::Refused {
                opens: resumes(&run.summary),
            };
        }
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
            || limited(activity).is_some()
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

    fn level(&self, thread: &Thread) -> Option<String> {
        level(&prose(thread)).map(str::to_owned)
    }
}

// The check run on the head saying cubic's plan is out of reviewed lines
fn limited(activity: &Activity) -> Option<&CheckRun> {
    activity.checks.iter().find(|run| run.title == LIMIT)
}

// The day its summary names for reviews resuming, as 00:00 UTC that day.
// None when it names none kelpie can read, which leaves the window's own
// definition to decide.
fn resumes(summary: &str) -> Option<Timestamp> {
    let said: String = summary
        .split(RESUME)
        .nth(1)?
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == ' ')
        .collect();
    let mut words = said.split_whitespace();
    let day: u32 = words.next()?.parse().ok()?;
    let month_name = words.next()?;
    let month = MONTHS
        .iter()
        .position(|name| name.eq_ignore_ascii_case(month_name))?;
    let year: i32 = words.next()?.parse().ok()?;
    let at: jiff::Timestamp = format!("{year:04}-{:02}-{day:02}T00:00:00Z", month + 1)
        .parse()
        .ok()?;
    Some(Timestamp(u64::try_from(at.as_second()).ok()?))
}

// The commits its reviews read. A reply in a thread posts a review with no
// summary, which is no review.
fn reviewed(activity: &Activity) -> impl Iterator<Item = &str> {
    let posted = activity.reviews.iter().filter(|r| r.body.contains(SUMMARY));
    posted.map(|r| r.commit.as_str())
}

/// Any comment that is not its start and names a limit reached or exceeded.
pub fn refused(body: &str) -> bool {
    let body = body.to_lowercase();
    !body.contains(&STARTED.to_lowercase())
        && body.contains("limit")
        && (body.contains("reached") || body.contains("exceeded"))
}

/// A thread as a finding the worker is sent
///
/// The severity comes from cubic's level: P0 and P1 are high, P2 is
/// medium, P3 is a nit. The level and confidence go to the worker as the why.
fn finding(thread: &Thread) -> Finding {
    let prose = prose(thread);
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
    Finding::new(
        severity,
        thread.path.clone(),
        thread.line.unwrap_or(0),
        one_line(text),
        why,
    )
}

// The thread's text above its collapsed details and markers
fn prose(thread: &Thread) -> String {
    let prose: Vec<&str> = thread
        .body
        .lines()
        .take_while(|line| !line.starts_with("<details"))
        .filter(|line| !line.trim_start().starts_with("<!--"))
        .collect();
    prose.join("\n")
}

// The level its text opens with, `P0` to `P3`
fn level(prose: &str) -> Option<&str> {
    match prose.trim().split_once(": ") {
        Some((level @ ("P0" | "P1" | "P2" | "P3"), _)) => Some(level),
        _ => None,
    }
}
