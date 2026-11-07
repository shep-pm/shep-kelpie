//! `shep kelpie watch`: one live stream of a project's runner steps and
//! the transcripts of the sessions they name
//!
//! The runner logs a `session-started` step to the project's `steps.jsonl`
//! as each call starts, naming the session, its work item, role and agent,
//! and where its harness writes the transcript. A session is attributed by
//! that id. The steps file and every named transcript are read as they grow,
//! and their lines are merged by time. The shepherd's out log is never read.
//! Nothing here asks the shepherd or the runner.

mod claude;
mod codex;
mod goose;
mod line;
mod log;
mod pi;
mod tail;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use crate::ticket::TicketId;
use crate::transcripts;
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};

pub use line::Line;
use line::{Event, Kind};
use log::Started;
use tail::Tail;

pub use log::Format;

// The file the session `started` names wrote its transcript in, once it has
fn find(started: &Started) -> Option<PathBuf> {
    transcripts::find(
        started.format,
        &started.session,
        &started.folder,
        started.threads.as_deref(),
        started.at,
    )
}

/// Each of the `wanted` sessions the steps file `steps` names a call starting in, by session id
///
/// Each comes with its transcript's format and the file it wrote, for those
/// whose file exists. Only a wanted session's file is looked for, as a locator
/// may list a whole folder. A session logged more than once, as a resumed one
/// is, keeps its first step's folder. A goose call is left out: its stdout
/// file holds no whole session.
pub fn transcripts_of(
    steps: &str,
    wanted: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeMap<String, (Format, PathBuf)> {
    let mut found = std::collections::BTreeMap::new();
    for started in steps
        .lines()
        .filter_map(log::parse)
        .filter_map(|step| step.started())
    {
        if started.format == Format::Goose
            || !wanted.contains(&started.session)
            || found.contains_key(&started.session)
        {
            continue;
        }
        let format = started.format;
        let session = started.session.clone();
        if let Some(file) = find(&started) {
            found.insert(session, (format, file));
        }
    }
    found
}

/// Everything the session whose transcript is `file` said, in order
///
/// Only the whole lines of the file count, so one still being written is
/// passed over. A `Format::Goose` file gives none: its stdout file holds no
/// whole session, as [`transcripts_of`] leaves it out.
pub fn said(format: Format, file: &Path) -> Vec<String> {
    let events = match format {
        Format::Claude => claude::events,
        Format::Codex => codex::events,
        Format::Pi => pi::events,
        // parity: harness.transcript-said
        Format::Goose => return Vec::new(),
    };
    let lines = Tail::from_start(file.to_owned()).lines();
    let said = lines.iter().flat_map(|line| events(line));
    said.filter(|event| event.kind == Kind::Said)
        .map(|event| event.text)
        .filter(|text| !text.trim().is_empty())
        .collect()
}

/// Every step of the steps file `steps` that names `ticket`, one line each
/// with the time it was logged, a line that is not a step passed over
pub fn steps_of(steps: &str, ticket: TicketId) -> Vec<String> {
    let steps = steps.lines().filter_map(log::parse);
    (steps.filter(|step| step.names(ticket)))
        .map(|step| format!("{} {}", step.at, step.text(false)))
        .collect()
}

/// How far before its `session-started` step a transcript line may be and
/// still belong to the call it starts
///
/// shep stamps a line as it reads it, a moment after the runner wrote it.
const SLACK_MS: i64 = 500;

/// Whose lines to show
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every line of the project
    Project,
    /// One work item's sessions and steps
    Issue(TicketId),
    /// The project manager's sessions and steps
    Pm,
}

/// What a watch shows and how
#[derive(Debug, Clone)]
pub struct Options {
    /// Whose lines
    pub scope: Scope,
    /// How many of the last lines to show before following
    pub back: usize,
    /// Whole prompts, results and thinking, not one clipped line each
    pub full: bool,
    /// The zone times are shown in
    pub zone: TimeZone,
}

/// A watch on one project's steps file and the transcripts it names
#[derive(Debug)]
pub struct Watch {
    log: Tail,
    options: Options,
    sessions: Vec<Session>,
}

// A session the log named, and how far its transcript is read.
#[derive(Debug)]
struct Session {
    started: Started,
    tail: Option<Tail>,
    // A transcript line before this belongs to an earlier call of a session
    // that was resumed.
    after: Option<Timestamp>,
    // The time of the last line read, which stamps a goose error: goose gives
    // it none.
    last: Option<Timestamp>,
}

impl Session {
    // New lines, attributed, once the transcript exists.
    fn read(&mut self) -> Vec<(Event, String)> {
        if self.tail.is_none() {
            self.tail = find(&self.started).map(Tail::from_start);
        }
        let Some(tail) = &mut self.tail else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for line in tail.lines() {
            let events = match self.started.format {
                Format::Claude => claude::events(&line),
                Format::Codex => codex::events(&line),
                Format::Pi => pi::events(&line),
                Format::Goose => {
                    let since = self.last.max(self.after).unwrap_or(self.started.at);
                    goose::events(&line, since)
                }
            };
            self.last = events.iter().map(|e| e.at).chain(self.last).max();
            found.extend(events);
        }
        found
            .into_iter()
            .filter(|event| self.after.is_none_or(|after| event.at >= after))
            .map(|event| (event, self.started.who.clone()))
            .collect()
    }
}

impl Watch {
    /// Starts a watch on the project whose folder is `folder`, with the lines
    /// it first shows: the last `options.back`, or none
    ///
    /// The steps file in the folder is read whole to learn which sessions
    /// are running, whatever `back` is.
    pub fn start(folder: PathBuf, options: Options) -> (Self, Vec<Line>) {
        let mut watch = Self {
            log: Tail::from_start(folder.join(crate::home::STEPS_FILE)),
            options,
            sessions: Vec::new(),
        };
        let mut found = watch.read_log(None);
        let back = watch.options.back;
        if back == 0 {
            // Only what is written from now on, to every transcript there is.
            found.clear();
            for session in &mut watch.sessions {
                session.tail = find(&session.started).map(Tail::at_end);
            }
        } else {
            found.extend(watch.read_recent(back));
        }
        found.sort_by_key(|(event, _)| event.at);
        let skip = found.len().saturating_sub(back);
        let lines = found.drain(skip..).map(|f| watch.line(f)).collect();
        (watch, lines)
    }

    /// The lines written since the last call, sorted by time
    pub fn poll(&mut self) -> Vec<Line> {
        let slack = Span::new().milliseconds(SLACK_MS);
        let mut found = self.read_log(Some(slack));
        for session in &mut self.sessions {
            found.extend(session.read());
        }
        found.sort_by_key(|(event, _)| event.at);
        found.into_iter().map(|f| self.line(f)).collect()
    }

    // The new steps, in scope, and the sessions they start. A session seen
    // while following reads from the call's start, `slack` before it.
    fn read_log(&mut self, following: Option<Span>) -> Vec<(Event, String)> {
        let scope = self.options.scope;
        let mut found = Vec::new();
        for text in self.log.lines() {
            let Some(step) = log::parse(&text) else {
                continue;
            };
            if step.in_scope(scope) {
                let event = Event {
                    at: step.at,
                    kind: Kind::Step,
                    text: step.text(self.options.full),
                };
                found.push((event, "runner".to_owned()));
            }
            let Some(started) = step.started().filter(|s| s.in_scope(scope)) else {
                continue;
            };
            let after = following.and_then(|slack| step.at.checked_sub(slack).ok());
            // Every goose call at one settings path writes the same file, which
            // each call empties, so only the newest call to name it is read.
            if started.format == Format::Goose {
                self.sessions.retain(|s| {
                    s.started.format != Format::Goose
                        || s.started.folder != started.folder
                        || s.started.session == started.session
                });
            }
            let goose = started.format == Format::Goose;
            match self
                .sessions
                .iter_mut()
                .find(|s| s.started.session == started.session)
            {
                // A resumed session's next call: only its new lines.
                Some(known) if following.is_some() => {
                    known.after = after;
                    if goose {
                        // The file was emptied, so the old offset is meaningless.
                        known.tail = None;
                        known.last = None;
                    }
                }
                Some(_) => {}
                None => self.sessions.push(Session {
                    started,
                    tail: None,
                    after,
                    last: None,
                }),
            }
        }
        found
    }

    // The newest lines of the sessions the log named, enough to fill `back`
    // lines, reading the most recently written transcripts first. A file's
    // last write is no earlier than any line in it, so one written before the
    // `back`th newest line found cannot hold a newer one.
    fn read_recent(&mut self, back: usize) -> Vec<(Event, String)> {
        let mut order: Vec<(Option<Timestamp>, usize)> = self
            .sessions
            .iter()
            .enumerate()
            .map(|(n, s)| (find(&s.started).and_then(|f| transcripts::written(&f)), n))
            .collect();
        order.sort_by(|a, b| b.cmp(a));
        let mut found: Vec<(Event, String)> = Vec::new();
        let mut enough: Option<Timestamp> = None;
        for (written, n) in order {
            let session = &mut self.sessions[n];
            if enough.is_some_and(|enough| written.is_none_or(|w| w < enough)) {
                // Not read, and not to be read later either.
                session.tail = find(&session.started).map(Tail::at_end);
                continue;
            }
            found.extend(session.read());
            if found.len() >= back {
                let mut times: Vec<Timestamp> = found.iter().map(|(e, _)| e.at).collect();
                times.sort_unstable_by(|a, b| b.cmp(a));
                enough = times.get(back - 1).copied();
            }
        }
        found
    }

    fn line(&self, (event, who): (Event, String)) -> Line {
        Line {
            at: event.at,
            who,
            kind: event.kind,
            text: event.text,
            full: self.options.full,
            zone: self.options.zone.clone(),
        }
    }
}
