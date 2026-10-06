//! The board briefing: `board.md`, what the project manager's agent reads
//!
//! Code writes it from what the runner already holds, so the agent never
//! runs git or gh. It has one entry per open work item with its session's
//! activity, the rulings waiting, the ready queue in the board rule's order,
//! the events since the agent's last wake, and an overlap table of the
//! files open branches and ready issues share. Its shape follows what was
//! measured: a PM briefed this way decided as well as one that looked for
//! itself, at about half the cost.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::ports::{CallActivity, Timestamp};
use crate::state::BoardEvent;

/// How long a call in flight may show nothing before the board calls it idle
pub const IDLE_AFTER: u64 = 10 * 60;

/// How much of each ready issue's body the board quotes, in characters
const EXCERPT: usize = 600;

/// How much of a worker's closing message a work item keeps, in characters
const SUMMARY: usize = 300;

/// A named path matching more files than this on `main` names none of them
const AMBIGUOUS: usize = 3;

/// Everything one board shows
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Briefing<'a> {
    /// The project
    pub project: &'a str,
    /// When it was written
    pub now: Timestamp,
    /// How many work items may be open at once
    pub max_items: u32,
    /// `origin/main`'s commit, once git has read it
    pub main: Option<String>,
    /// The open work items, oldest first
    pub items: Vec<Item>,
    /// The rulings waiting on the maintainer, oldest first
    pub rulings: Vec<RulingLine>,
    /// The ready queue as last read, `None` before the first read
    pub ready: Option<Ready>,
    /// The board events the project manager has not read
    pub events: Vec<BoardEvent>,
    /// Whether the project manager has read the board before
    pub woken_before: bool,
    /// Whether unread events were dropped for being old
    pub dropped: bool,
    /// What `git merge-tree` finds merging two open work items' branches,
    /// by their issues, the lower first. A pair not listed merges cleanly.
    pub conflicts: BTreeMap<(u64, u64), Merge>,
}

/// What merging two open branches finds, when it is not a clean merge
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Merge {
    /// These files conflict
    Conflicts(Vec<String>),
    /// git could not say, as one too old for `merge-tree --write-tree` or a
    /// missing commit
    Unknown,
}

/// An open work item
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Its issue
    pub issue: u64,
    /// The issue's title
    pub title: String,
    /// Where it stands, in words
    pub phase: String,
    /// Its pull request, once there is one
    pub pull_request: Option<u64>,
    /// When it opened, when known
    pub opened: Option<Timestamp>,
    /// The implementer its worker runs on
    pub agent: String,
    /// Its branch
    pub branch: String,
    /// What its session is doing
    pub session: Session,
    /// The worker's last turn's closing message
    pub summary: Option<String>,
    /// The files it touches
    pub files: Files,
}

/// What a work item's session is doing
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    /// A worker's turn is in flight
    Turn {
        /// When it started
        since: Timestamp,
        /// What its harness says it last showed
        last: CallActivity,
    },
    /// A review call is in flight
    Review {
        /// When it started
        since: Timestamp,
    },
    /// No call is in flight, and this is where the turn stands
    Still(String),
}

/// The files a work item touches
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Files {
    /// Its branch's diff from `main`
    Diff(Vec<String>),
    /// It has no branch yet, and its issue's body names these
    Named(Vec<String>),
    /// Neither is known
    Unknown,
}

impl Files {
    fn list(&self) -> &[String] {
        match self {
            Self::Diff(files) | Self::Named(files) => files,
            Self::Unknown => &[],
        }
    }
}

/// A ruling waiting on the maintainer
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulingLine {
    /// Its id
    pub id: u64,
    /// The work item it parks
    pub issue: Option<u64>,
    /// Its pull request
    pub pull_request: Option<u64>,
    /// The question's first line
    pub question: String,
}

/// The ready queue as the board last read it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    /// When it was read
    pub read: Timestamp,
    /// Its issues in the board rule's order, none of them open as a work item
    pub issues: Vec<ReadyLine>,
}

/// A ready issue
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyLine {
    /// Its number
    pub number: u64,
    /// Its title
    pub title: String,
    /// Its priority, such as `P1`
    pub priority: Option<&'static str>,
    /// Why the board passes over it, if it does
    pub waits: Option<String>,
    /// The paths its body names
    pub named: Vec<String>,
    /// The start of its body, for an issue the board may take
    pub excerpt: Option<String>,
}

/// The board as `board.md` holds it
pub fn render(board: &Briefing<'_>) -> String {
    let mut out = String::new();
    let now = board.now;
    let _ = writeln!(out, "# Board: {}, {} UTC\n", board.project, date_time(now));
    let main = board
        .main
        .as_deref()
        .map_or(String::new(), |m| format!(" Main is at {}.", short(m)));
    let _ = writeln!(
        out,
        "{} of {} work items open.{main}\n",
        board.items.len(),
        board.max_items
    );
    open_work(&mut out, board);
    rulings(&mut out, &board.rulings);
    ready_queue(&mut out, board.ready.as_ref(), now);
    events(&mut out, board);
    overlap(&mut out, board);
    files(&mut out, &board.items);
    excerpts(&mut out, board.ready.as_ref());
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

fn open_work(out: &mut String, board: &Briefing<'_>) {
    out.push_str("## Open work\n\n");
    if board.items.is_empty() {
        out.push_str("(none)\n");
    }
    for item in &board.items {
        let pr = item
            .pull_request
            .map_or("no pull request yet".to_owned(), |n| format!("PR #{n}"));
        let opened = item.opened.map_or(String::new(), |t| {
            format!(", opened {} ago", age(board.now, t))
        });
        let touches = match &item.files {
            Files::Diff(files) => format!("touches {}", count(files.len(), "file")),
            Files::Named(files) => format!(
                "no commits yet, its body names {}",
                count(files.len(), "file")
            ),
            Files::Unknown => "no branch yet".to_owned(),
        };
        let _ = writeln!(
            out,
            "- #{} \"{}\": {}; {pr}{opened}, on {}; {touches}",
            item.issue, item.title, item.phase, item.agent
        );
        let _ = writeln!(out, "  - Session: {}", session(&item.session, board.now));
        if let Some(summary) = &item.summary {
            let _ = writeln!(out, "  - Last turn: \"{summary}\"");
        }
    }
    out.push('\n');
}

fn session(session: &Session, now: Timestamp) -> String {
    match session {
        Session::Turn { since, last } => {
            let running = format!("worker turn running for {}", age(now, *since));
            let Some(quiet) = quiet_for(*since, *last, now) else {
                return format!("{running}, its activity unreadable");
            };
            let gap = age(now, Timestamp(now.0.saturating_sub(quiet)));
            match last {
                _ if quiet >= IDLE_AFTER => {
                    format!("{running}, idle: no tool call or output for {gap}")
                }
                CallActivity::At(at) if at >= since => {
                    format!("{running}, last tool call or output {gap} ago")
                }
                _ => format!("{running}, no tool call or output yet"),
            }
        }
        Session::Review { since } => format!("review call running for {}", age(now, *since)),
        Session::Still(what) => what.clone(),
    }
}

/// How long a turn started at `since` has shown nothing, at `now`, or
/// `None` when its harness keeps no record of what it shows
///
/// A record from before the turn began, as a resumed session's transcript
/// is, counts from the turn's start.
pub fn quiet_for(since: Timestamp, last: CallActivity, now: Timestamp) -> Option<u64> {
    let seen = match last {
        CallActivity::Untracked => return None,
        CallActivity::Nothing => since,
        CallActivity::At(at) => at.max(since),
    };
    Some(now.0.saturating_sub(seen.0))
}

fn rulings(out: &mut String, rulings: &[RulingLine]) {
    if rulings.is_empty() {
        return;
    }
    out.push_str("## Rulings waiting on the maintainer\n\n");
    for ruling in rulings {
        let issue = ruling.issue.map_or(String::new(), |n| format!(" on #{n}"));
        let pr = ruling
            .pull_request
            .map_or(String::new(), |n| format!(" (PR #{n})"));
        let _ = writeln!(out, "- {}{issue}{pr}: {}", ruling.id, ruling.question);
    }
    out.push('\n');
}

fn ready_queue(out: &mut String, ready: Option<&Ready>, now: Timestamp) {
    let Some(ready) = ready else {
        out.push_str(
            "## Ready queue\n\nNot read since the runner started. The board reads it while \
             a slot is free.\n\n",
        );
        return;
    };
    let _ = writeln!(
        out,
        "## Ready queue, read {} ago (board rule order: priority, then oldest by number)\n",
        age(now, ready.read)
    );
    if ready.issues.is_empty() {
        out.push_str("(empty)\n");
    }
    for (n, issue) in ready.issues.iter().enumerate() {
        let waits = issue
            .waits
            .as_ref()
            .map_or(String::new(), |w| format!("; waits: {w}"));
        let named = match issue.named.as_slice() {
            [] => "names no file".to_owned(),
            named => format!("names {}", named.join(", ")),
        };
        let _ = writeln!(
            out,
            "{}. #{} [{}] \"{}\"{waits}; {named}",
            n + 1,
            issue.number,
            issue.priority.unwrap_or("no priority"),
            issue.title
        );
    }
    out.push('\n');
}

fn events(out: &mut String, board: &Briefing<'_>) {
    if board.woken_before {
        out.push_str("## Since your last wake\n\n");
    } else {
        out.push_str("## Recent events\n\n");
    }
    if board.dropped {
        out.push_str("(older unread events were dropped)\n");
    }
    if board.events.is_empty() {
        out.push_str("(none)\n");
    }
    for event in &board.events {
        let _ = writeln!(out, "- [{}] {} {}", event.id, clock(event.at), event.what);
    }
    out.push('\n');
}

fn overlap(out: &mut String, board: &Briefing<'_>) {
    out.push_str(
        "## Overlap\n\nFiles both sides touch, and the files `git merge-tree` finds in conflict \
         between two open branches. A branch's files are its diff from main; a ready issue's are \
         the paths its body names. Pairs not listed share nothing.\n\n",
    );
    let mut rows = Vec::new();
    for (i, a) in board.items.iter().enumerate() {
        for b in &board.items[i + 1..] {
            let key = (a.issue.min(b.issue), a.issue.max(b.issue));
            let merge = match board.conflicts.get(&key) {
                Some(Merge::Conflicts(files)) => files.join(", "),
                Some(Merge::Unknown) => "conflict check unavailable".to_owned(),
                None => String::new(),
            };
            let both = shared(a.files.list(), b.files.list());
            if !both.is_empty() || !merge.is_empty() {
                rows.push((label(a), label(b), merge, both.join(", ")));
            }
        }
    }
    let ready = board
        .ready
        .as_ref()
        .map_or(&[][..], |r| r.issues.as_slice());
    for issue in ready {
        for item in &board.items {
            let both = shared(&issue.named, item.files.list());
            if !both.is_empty() {
                let a = format!("#{} (ready)", issue.number);
                rows.push((a, label(item), String::new(), both.join(", ")));
            }
        }
    }
    if rows.is_empty() {
        out.push_str("(nothing shared)\n\n");
        return;
    }
    out.push_str("| A | B | In conflict | Both touch |\n| --- | --- | --- | --- |\n");
    for (a, b, conflicts, both) in rows {
        let [a, b, conflicts, both] = [a, b, conflicts, both].map(|c| c.replace('|', "\\|"));
        let _ = writeln!(out, "| {a} | {b} | {conflicts} | {both} |");
    }
    out.push('\n');
}

fn label(item: &Item) -> String {
    match item.pull_request {
        Some(pr) => format!("#{} (PR #{pr})", item.issue),
        None => format!("#{}", item.issue),
    }
}

fn shared<'a>(a: &'a [String], b: &[String]) -> Vec<&'a str> {
    let b: BTreeSet<&str> = b.iter().map(String::as_str).collect();
    a.iter()
        .map(String::as_str)
        .filter(|f| b.contains(f))
        .collect()
}

fn files(out: &mut String, items: &[Item]) {
    let with_files: Vec<&Item> = items
        .iter()
        .filter(|i| !i.files.list().is_empty())
        .collect();
    if with_files.is_empty() {
        return;
    }
    out.push_str("## Files\n\n");
    for item in with_files {
        let how = match item.files {
            Files::Named(_) => ", named",
            _ => "",
        };
        let _ = writeln!(
            out,
            "- #{} ({}{how}): {}",
            item.issue,
            item.branch,
            item.files.list().join(", ")
        );
    }
    out.push('\n');
}

fn excerpts(out: &mut String, ready: Option<&Ready>) {
    let quoted: Vec<&ReadyLine> = ready
        .map_or(&[][..], |r| r.issues.as_slice())
        .iter()
        .filter(|i| i.excerpt.is_some())
        .collect();
    if quoted.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "## Ready issues, the first {EXCERPT} characters of each body\n"
    );
    for issue in quoted {
        let text = issue.excerpt.as_deref().unwrap_or_default();
        let _ = writeln!(out, "### #{} {}\n\n{text}\n", issue.number, issue.title);
    }
}

/// The start of an issue's body, blank runs closed up, cut at a word past
/// 600 characters, each line quoted so none of it reads as the board's own
pub fn excerpt(body: &str) -> String {
    let mut text = String::new();
    let mut blank = 0;
    for line in body.trim().lines() {
        let line = line.trim_end();
        blank = if line.is_empty() { blank + 1 } else { 0 };
        if blank < 2 {
            text.push_str(line);
            text.push('\n');
        }
    }
    let cut = cut(text.trim_end(), EXCERPT);
    let quoted = cut.lines().map(|line| match line {
        "" => ">".to_owned(),
        line => format!("> {line}"),
    });
    quoted.collect::<Vec<_>>().join("\n")
}

/// A worker's closing message on one line, cut at a word past
/// 300 characters, or `None` when it said nothing
pub fn summary(text: &str) -> Option<String> {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!line.is_empty()).then(|| cut(&line, SUMMARY))
}

fn cut(text: &str, chars: usize) -> String {
    let Some((end, _)) = text.char_indices().nth(chars) else {
        return text.to_owned();
    };
    let head = &text[..end];
    let head = head
        .rsplit_once(char::is_whitespace)
        .map_or(head, |(h, _)| h);
    format!("{} …", head.trim_end())
}

/// The paths an issue's body names in backticks, as files on `main`
///
/// A name matches a file on `main` by its whole path or its last parts, as
/// `board.rs` matches `src/board.rs`. One matching more than three
/// files names none, and one matching none counts only if it has a `/`.
pub fn named_paths(body: &str, tree: &[String]) -> Vec<String> {
    let mut found = BTreeSet::new();
    for span in body.split('`').skip(1).step_by(2) {
        let Some(name) = path_like(span) else {
            continue;
        };
        let suffix = format!("/{name}");
        let hits: Vec<&String> = tree
            .iter()
            .filter(|f| **f == name || f.ends_with(&suffix))
            .collect();
        match hits.len() {
            0 if name.contains('/') => {
                found.insert(name.to_owned());
            }
            1..=AMBIGUOUS => found.extend(hits.into_iter().cloned()),
            _ => {}
        }
    }
    found.into_iter().collect()
}

// A span that reads as a path: no spaces, path characters only, and a
// folder or an extension. A `:line` after it is dropped.
fn path_like(span: &str) -> Option<&str> {
    let span = span.split_once(':').map_or(span, |(path, line)| {
        if line.chars().all(|c| c.is_ascii_digit()) {
            path
        } else {
            ""
        }
    });
    let span = span.strip_prefix("./").unwrap_or(span);
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-');
    if span.is_empty() || span.starts_with(['-', '/', '.']) || !span.chars().all(allowed) {
        return None;
    }
    let last = span.rsplit('/').next().unwrap_or(span);
    let extension = last
        .rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && (1..=5).contains(&ext.len()));
    (span.contains('/') || extension).then_some(span)
}

/// How long ago `then` was at `now`, as `45m` or `3h05m`
pub fn age(now: Timestamp, then: Timestamp) -> String {
    let seconds = now.0.saturating_sub(then.0);
    let (hours, minutes) = (seconds / 3600, seconds % 3600 / 60);
    if hours == 0 {
        format!("{minutes}m")
    } else {
        format!("{hours}h{minutes:02}m")
    }
}

fn count(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

fn short(commit: &str) -> &str {
    commit.get(..10).unwrap_or(commit)
}

// The time of day in UTC, as `14:05`
fn clock(t: Timestamp) -> String {
    let seconds = t.0 % 86_400;
    format!("{:02}:{:02}", seconds / 3600, seconds % 3600 / 60)
}

// The date and time in UTC, as `2026-10-05 14:05`
fn date_time(t: Timestamp) -> String {
    let at = i64::try_from(t.0)
        .ok()
        .and_then(|s| jiff::Timestamp::from_second(s).ok());
    match at {
        Some(at) => at
            .to_string()
            .get(..16)
            .unwrap_or_default()
            .replace('T', " "),
        None => t.0.to_string(),
    }
}

#[cfg(test)]
mod tests;
