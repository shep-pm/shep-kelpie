//! The representative's one call: facts in, a post to a contributor out
//!
//! Kelpie's code decides what a post says and drafts the facts. The
//! representative, an agent file of its own (`agents.representative`), puts
//! them into words, in one voice for every post. A call is always a fresh
//! session with no tools and no folder to read, so it reads and runs nothing:
//! its whole input is the prompt. Nothing here posts. The caller does, and
//! applies its own rule when this fails.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use jiff::civil::Date;
use jiff::tz::TimeZone;

use super::Runner;
use super::untrusted::fenced;
use crate::ports::{
    AgentCall, AgentError, AgentReply, Cost, Ending, Role, Session, Timestamp, Tools,
};
use crate::settings::SettingsError;
use crate::ticket::TicketId;
use crate::usage::{CallKind, Draft, Line, Spent, ended};
use crate::work_item::new_session_id;

pub(in crate::runner) mod reading;

/// How long a call may run before it is ended
const CEILING: Duration = Duration::from_secs(15 * 60);

/// What one post is made from
#[derive(Debug, Clone, Copy)]
pub struct Post<'a> {
    /// The pull request the post goes on, which the call is recorded against
    pub pull_request: u64,
    /// The kind of post, as a short instruction from the caller
    pub kind: &'a str,
    /// The facts the caller drafted, which are all the post may state
    pub facts: &'a str,
    /// The ids of the representative's own earlier comments on the pull
    /// request, read again through the forge so the call can avoid repeating them
    pub earlier: &'a [u64],
    /// The contributor's login, the one `@login` the post may leave live
    pub contributor: &'a str,
    /// What the contributor wrote, which is data and never instructions
    pub said: &'a str,
}

/// What one post on an issue is made from, as [`Post`] is for a pull request
#[derive(Debug, Clone, Copy)]
pub(super) struct IssuePost<'a> {
    /// The kind of post, as a short instruction from the caller
    pub kind: &'a str,
    /// The facts the caller drafted, which are all the post may state
    pub facts: &'a str,
    /// The ids of the representative's own earlier comments on the issue
    pub earlier: &'a [u64],
    /// The login of whoever opened the issue, the one `@login` the post may
    /// leave live
    pub author: &'a str,
    /// What they wrote, which is data and never instructions
    pub said: &'a str,
}

// Where a post goes, which the prompt says and the ledger line records
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Thread {
    PullRequest,
    Issue,
}

impl Thread {
    fn noun(self) -> &'static str {
        match self {
            Self::PullRequest => "pull request",
            Self::Issue => "issue",
        }
    }
}

/// Why a post could not be worded
#[derive(Debug)]
pub enum RepresentError {
    /// `agents.representative` names no usable agent
    Agent(SettingsError),
    /// The call could not be set up: its session id, folder or instructions
    Setup(String),
    /// The call failed or was ended
    Call(AgentError),
    /// The call answered with nothing
    Empty,
}

impl fmt::Display for RepresentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Agent(e) => write!(f, "{e}"),
            Self::Setup(why) => write!(f, "cannot set up the representative's call: {why}"),
            Self::Call(e) => write!(f, "the representative's call failed: {e}"),
            Self::Empty => f.write_str("the representative answered with nothing"),
        }
    }
}

impl core::error::Error for RepresentError {}

impl Runner {
    /// The post `post` makes, in the representative's words, with every
    /// `@login` but the contributor's put in backticks so it pings nobody
    ///
    /// The call is made now and waits for its answer. Its spend goes to the
    /// usage ledger under the `representative` role.
    ///
    /// # Errors
    ///
    /// [`RepresentError`] when the agent is not usable, the call cannot be
    /// set up, fails or is ended, or answers with nothing.
    pub fn represent(&mut self, post: &Post<'_>) -> Result<String, RepresentError> {
        self.represent_within(post, Thread::PullRequest, CEILING)
            .map(|(words, _)| words)
    }

    /// [`Self::represent`], with what the call cost where its harness priced
    /// it, for a caller that holds the call to a budget
    ///
    /// # Errors
    ///
    /// As [`Self::represent`]. A call that failed reports no cost.
    pub(in crate::runner) fn represent_priced(
        &mut self,
        post: &Post<'_>,
    ) -> Result<(String, Cost), RepresentError> {
        self.represent_within(post, Thread::PullRequest, CEILING)
    }

    /// The post `post` makes on issue `issue`, as [`Self::represent`] words one
    /// on a pull request, with the call recorded against the issue
    ///
    /// # Errors
    ///
    /// As [`Self::represent`].
    pub(in crate::runner) fn represent_issue(
        &mut self,
        issue: u64,
        post: &IssuePost<'_>,
    ) -> Result<String, RepresentError> {
        let post = Post {
            pull_request: issue,
            kind: post.kind,
            facts: post.facts,
            earlier: post.earlier,
            contributor: post.author,
            said: post.said,
        };
        self.represent_within(&post, Thread::Issue, CEILING)
            .map(|(words, _)| words)
    }

    /// `at` in words against the runner's clock, so a fact never carries a
    /// bare date for the representative to work a weekday or a gap out of
    ///
    /// "yesterday", "three days ago", "today", "tomorrow", "by Sunday (in 4
    /// days)", and past a week "by Friday 6 November". Days are UTC's, the
    /// same as the day [`Self::represent`] tells the call it is. `None` when
    /// the moment or the clock is past what a date can hold.
    #[must_use]
    pub fn moment_in_words(&self, at: Timestamp) -> Option<String> {
        moment(at, self.ports.clock.now())
    }

    // `represent`, with the call ended once it has run for `ceiling`
    fn represent_within(
        &mut self,
        post: &Post<'_>,
        thread: Thread,
        ceiling: Duration,
    ) -> Result<(String, Cost), RepresentError> {
        let agent = (self.settings.representative(&self.book)).map_err(RepresentError::Agent)?;
        let folder = &self.paths.representative;
        fs::create_dir_all(folder).map_err(|e| {
            RepresentError::Setup(format!("cannot create {}: {e}", folder.display()))
        })?;
        let session = new_session_id().map_err(|e| RepresentError::Setup(e.to_string()))?;
        let instructions = (self.paths.worker).join("representative-instructions.md");
        let text = agent.prompt.as_deref().unwrap_or_default();
        super::turn::write(&self.paths.worker, &instructions, text)
            .map_err(RepresentError::Setup)?;
        let call = AgentCall {
            issue: TicketId::from_issue(post.pull_request),
            settings: (self.paths.worker).join("representative-settings.json"),
            instructions: Some(instructions),
            ..AgentCall::new(
                Role::Representative,
                Tools::Answer,
                agent.model.harness.clone(),
                agent.model.model.as_str().to_owned(),
                agent.model.effort,
                Session::New(session),
                folder.clone(),
                self.post_prompt(post, thread),
            )
        };
        let call = self.prepared(call).map_err(RepresentError::Setup)?;
        let started = self.ports.clock.now();
        let ran = (self.ports.agents)
            .prepare(&call)
            .and_then(|()| self.run_within(&call, ceiling));
        if let Some(note) = unremoved(&call.settings) {
            self.notes.push(note);
        }
        if let Some(ended) = ended(&ran) {
            let mut draft = Draft::of(&call, agent.name.as_str(), CallKind::Post, started);
            match thread {
                Thread::PullRequest => draft.pull_request = Some(post.pull_request),
                Thread::Issue => draft.issue = TicketId::from_issue(post.pull_request),
            }
            let spent = ran.as_ref().ok().map(Spent::from);
            // A fresh session, so the call cost all of it.
            let now = self.ports.clock.now();
            let line = draft.line(now, ended, spent, Cost(0), self.pacer_lines());
            self.ledger.append(&Line::Call(line));
        }
        let reply = ran.map_err(RepresentError::Call)?;
        // A fresh session, so what it has cost is what this call did.
        let cost = reply.session_cost.unwrap_or_default();
        let words = reply.text.trim();
        match words.is_empty() {
            true => Err(RepresentError::Empty),
            false => Ok((neutered(words, post.contributor), cost)),
        }
    }

    // Runs `call` and waits for it, asking it to end once it has run for
    // `ceiling`. Nothing else holds this call, so a stalled model would
    // block the runner for good without it.
    fn run_within(&self, call: &AgentCall, ceiling: Duration) -> Result<AgentReply, AgentError> {
        let ending = Ending::default();
        let (done, finished) = mpsc::channel::<()>();
        thread::scope(|scope| {
            let held = ending.clone();
            scope.spawn(move || {
                if finished.recv_timeout(ceiling) == Err(RecvTimeoutError::Timeout) {
                    held.end();
                }
            });
            let ran = self.ports.agents.run(call, &ending);
            drop(done);
            ran
        })
    }

    // What the call is asked: the kind of post, the facts, the earlier posts
    // that can still be read, and the contributor's text, the last two fenced
    // as data.
    fn post_prompt(&self, post: &Post<'_>, thread: Thread) -> String {
        // A clock past what a date can hold leaves the line out, not a wrong day.
        let today = (day(self.ports.clock.now()))
            .map(|today| format!("Today is {}.\n\n", today.strftime("%A %-d %B %Y")))
            .unwrap_or_default();
        let place = match thread {
            Thread::PullRequest => "",
            Thread::Issue => {
                "The post goes on this issue. Greet the author by name and lead \
                with what kelpie found. At most one plain thanks that names what they reported, \
                never a stock \"thanks for opening this\" or \"thanks for the report\", and \
                never the opener.\n"
            }
        };
        let mut prompt = format!(
            "{today}{place}Kind of post: {}\n\nFacts:\n{}\n",
            post.kind, post.facts
        );
        let earlier: Vec<String> = (post.earlier.iter())
            .filter_map(|&id| self.ports.forge.comment_marks(&self.remote, id).ok())
            .map(|marks| marks.body)
            .collect();
        if !earlier.is_empty() {
            let what = format!(
                "Your earlier posts on this {}, oldest first,",
                thread.noun()
            );
            prompt.push_str(&format!(
                "\n{}\n",
                fenced(&what, &earlier.join("\n\n---\n\n"))
            ));
        }
        let what = format!("What @{} wrote", post.contributor);
        prompt.push_str(&format!(
            "\n{}\n\nWrite the post.",
            fenced(&what, post.said)
        ));
        prompt
    }
}

// The UTC day `at` falls on
// or `None` for a moment past what a date can hold
fn day(at: Timestamp) -> Option<Date> {
    let seconds = i64::try_from(at.0).ok()?;
    let at = jiff::Timestamp::from_second(seconds).ok()?;
    Some(at.to_zoned(TimeZone::UTC).date())
}

// `then` in words against `now`, by the UTC days between them: a past day as
// "yesterday" or how long ago, today and tomorrow as themselves, up to a week
// ahead as "by <weekday> (in N days)", and further out as "by <weekday>
// <date>". The gap is in days, so a late hour never shifts it.
fn moment(then: Timestamp, now: Timestamp) -> Option<String> {
    let (then, today) = (day(then)?, day(now)?);
    let days = i64::from((then - today).get_days());
    Some(match days {
        0 => "today".to_owned(),
        1 => "tomorrow".to_owned(),
        -1 => "yesterday".to_owned(),
        2..=7 => format!("by {} (in {days} days)", then.strftime("%A")),
        8.. => {
            let year = if then.year() == today.year() {
                ""
            } else {
                " %Y"
            };
            let format = format!("%A %-d %B{year}");
            format!("by {}", then.strftime(&format))
        }
        _ => format!("{} ago", span(-days)),
    })
}

// How long `days` (at least 2) is, by the largest of weeks, months and years
// that fits: "three days ago", "two weeks", "five months", "a year"
fn span(days: i64) -> String {
    let (n, unit) = match days {
        ..=13 => (days, "day"),
        14..=59 => (days / 7, "week"),
        60..=364 => (days / 30, "month"),
        365..=729 => return "a year".to_owned(),
        _ => (days / 365, "year"),
    };
    format!("{} {unit}s", count(n))
}

// `n` as a word up to ten, and as digits past it
fn count(n: i64) -> String {
    const WORDS: [&str; 9] = [
        "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ];
    match usize::try_from(n - 2).ok().and_then(|i| WORDS.get(i)) {
        Some(word) => (*word).to_owned(),
        None => n.to_string(),
    }
}

// What to note when the call's settings file stays: a file the call never
// wrote is not a failure, but one that cannot be removed is left behind
fn unremoved(settings: &Path) -> Option<String> {
    match fs::remove_file(settings) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            Some(format!("cannot remove {}: {e}", settings.display()))
        }
        _ => None,
    }
}

// `text` with every `@login` that is not `contributor`'s inside backticks.
// Code the reply already has is left as it is: a closed fence, and an inline
// span whose closing backticks are on its line. A span is paired within a
// line, since a list item or a heading ends one in a reader that renders it.
pub(in crate::runner) fn neutered(text: &str, contributor: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // The open fence's mark and length, and its lines so far
    let mut fence: Option<(char, usize, String)> = None;
    for line in text.split_inclusive('\n') {
        if let Some((mark, len, held)) = fence.as_mut() {
            held.push_str(line);
            if closes_fence(line, *mark, *len) {
                out.push_str(held);
                fence = None;
            }
        } else if let Some((mark, len)) = opens_fence(line) {
            fence = Some((mark, len, line.to_owned()));
        } else {
            out.push_str(&outside_spans(line, contributor));
        }
    }
    if let Some((_, _, held)) = fence {
        for line in held.split_inclusive('\n') {
            out.push_str(&outside_spans(line, contributor));
        }
    }
    out
}

// The mark and length of the fence `line` opens, if it opens one
fn opens_fence(line: &str) -> Option<(char, usize)> {
    let line = line.trim_start();
    let mark = line.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = line.chars().take_while(|&c| c == mark).count();
    // A backtick fence's info string holds no backtick, or the line is a span.
    let info = line.trim_start_matches(mark);
    let spans = mark == '`' && info.contains('`');
    (len >= 3 && !spans).then_some((mark, len))
}

// Whether `line` ends a fence of `len` `mark`s
fn closes_fence(line: &str, mark: char, len: usize) -> bool {
    let line = line.trim();
    line.chars().count() >= len && line.chars().all(|c| c == mark)
}

// `text` with its mentions outside inline code put in backticks
fn outside_spans(text: &str, contributor: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < chars.len() {
        let here = chars[at];
        if here == '`' {
            let run = backticks(&chars, at);
            let end = closing_run(&chars, at + run, run).unwrap_or(at + run);
            out.extend(&chars[at..end]);
            at = end;
        } else if let Some(len) = link(&chars[at..]) {
            out.extend(&chars[at..at + len]);
            at += len;
        } else if here == '@'
            && can_ping(&chars, at)
            && let Some(len) = mention(&chars[at..])
        {
            let name: String = chars[at..at + len].iter().collect();
            match name[1..].eq_ignore_ascii_case(contributor) {
                true => out.push_str(&name),
                false => {
                    // A backslash that escapes the `@` would escape the backtick instead.
                    let slashes = chars[..at].iter().rev().take_while(|&&c| c == '\\').count();
                    if slashes % 2 == 1 {
                        out.pop();
                    }
                    out.push_str(&format!("`{name}`"));
                }
            }
            at += len;
        } else {
            out.push(here);
            at += 1;
        }
    }
    out
}

// The length of the web address that starts `chars`, up to the next space: an
// `@` in one is a part of the address, and wrapping it would break the link
fn link(chars: &[char]) -> Option<usize> {
    let starts = |scheme: &str| {
        let head: String = chars.iter().take(scheme.len()).collect();
        head.eq_ignore_ascii_case(scheme)
    };
    (starts("https://") || starts("http://"))
        .then(|| chars.iter().take_while(|c| !c.is_whitespace()).count())
}

// How many backticks run from `at`
fn backticks(chars: &[char], at: usize) -> usize {
    chars[at..].iter().take_while(|&&c| c == '`').count()
}

// Where the run of exactly `run` backticks that closes a span opened before
// `from` ends, if there is one
fn closing_run(chars: &[char], from: usize, run: usize) -> Option<usize> {
    let mut at = from;
    while at < chars.len() {
        if chars[at] != '`' {
            at += 1;
            continue;
        }
        let len = backticks(chars, at);
        if len == run {
            return Some(at + len);
        }
        at += len;
    }
    None
}

// Whether an `@` at `at` could ping: an email's does not. A backslash before
// it is no help, since a reader may drop the backslash and then see a mention.
fn can_ping(chars: &[char], at: usize) -> bool {
    match at.checked_sub(1).map(|before| chars[before]) {
        Some(before) => !before.is_alphanumeric(),
        None => true,
    }
}

// The length of the mention that starts with the `@` in `chars`: a login, a
// bot's `[bot]`, or an `org/team`, or `None` when no name follows
fn mention(chars: &[char]) -> Option<usize> {
    let name = |c: &char| c.is_ascii_alphanumeric() || *c == '-';
    let mut len = 1 + chars[1..].iter().take_while(|c| name(c)).count();
    while len > 1 && chars[len - 1] == '-' {
        len -= 1;
    }
    if len == 1 {
        return None;
    }
    let rest: String = chars[len..].iter().take(5).collect();
    if rest == "[bot]" {
        return Some(len + 5);
    }
    if chars.get(len) == Some(&'/') {
        let team = chars[len + 1..]
            .iter()
            .take_while(|c| name(c) || **c == '_')
            .count();
        if team > 0 {
            return Some(len + 1 + team);
        }
    }
    Some(len)
}

#[cfg(test)]
mod tests;
