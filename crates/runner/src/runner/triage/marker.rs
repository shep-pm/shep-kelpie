//! The marker kelpie puts under its own triage comments, and the read that
//! finds them
//!
//! A marker is the last line of the comment, never inside its worded text, so
//! a comment that quotes marker-shaped text is not read as one kelpie wrote.
//! Only the App's own comment counts for.

use super::super::Runner;
use super::super::thread_replies::plain_login;
use super::ask;
use crate::ports::{ForgeError, Timestamp};

/// What marks a comment as kelpie's triage comment
pub(in crate::runner) const MARKER: &str = "<!-- kelpie:triage -->";

/// The same marker, naming the item the comment says covers the issue. It is
/// what lets a comment kelpie posted whose record did not save be read again
/// as the close it was, rather than as an ordinary triage.
pub(in crate::runner) fn duplicate_marker(of: u64) -> String {
    format!("{DUPLICATE_HEAD}{of} -->")
}

/// The start of the duplicate marker form
const DUPLICATE_HEAD: &str = "<!-- kelpie:triage duplicate #";

/// What a triage marker says
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    /// An ordinary triage comment, which names nothing
    Ordinary,
    /// A comment that named the item it closes this one as a duplicate of
    Duplicate(u64),
}

// What the marker kelpie appended at the end of `body` says, or `None` when its
// last line is no marker kelpie wrote. Only the end is read: the comment's own
// text is worded by a model and can hold marker-shaped text of its own, which
// is never what kelpie appended.
fn marker_says(body: &str) -> Option<Marker> {
    let last = (body.trim_end()).lines().last()?;
    if last == MARKER {
        return Some(Marker::Ordinary);
    }
    let digits = (last.strip_prefix(DUPLICATE_HEAD)?).strip_suffix(" -->")?;
    digits.parse::<u64>().ok().map(Marker::Duplicate)
}

// `words` with every marker line the representative echoed taken out, so the
// comment kelpie posts carries one marker and not two
pub(super) fn unmarked(words: &str) -> String {
    let kept: Vec<&str> = (words.lines())
        .filter(|line| marker_says(line).is_none())
        .collect();
    kept.join("\n").trim().to_owned()
}

/// The App's own triage comment on an issue, and what its marker says
#[derive(Debug)]
pub(super) struct Marked {
    pub(super) comment: u64,
    /// The item it named as covering the issue, which an ordinary marker names
    /// none
    pub(super) duplicate_of: Option<u64>,
    /// When the forge says it went up: the wait a record made from this comment
    /// saves is measured from there, not from when kelpie found the comment
    pub(super) created: Timestamp,
    /// That the comment carries the questions block: a record made from it owes
    /// the author the wait those questions opened
    pub(super) asked: bool,
}

impl Runner {
    // The App's own triage comment on `issue` and what its marker says, past
    // `after` when a comment is given: a triage asked again is not found by the
    // comment of the one before. A marker in anyone else's comment counts for
    // nothing.
    pub(super) fn marked_by_app(
        &self,
        issue: u64,
        after: Option<u64>,
    ) -> Result<Option<Marked>, ForgeError> {
        let comments = self
            .ports
            .tickets
            .issue_comments(&self.remote, super::ticket(issue)?)?;
        let Some(app) = self.app_login().map(|app| plain_login(&app)) else {
            return Ok(None);
        };
        let ours = comments.iter().filter_map(|c| {
            let marked = match marker_says(&c.body) {
                Some(Marker::Ordinary) => Marked {
                    comment: c.id,
                    duplicate_of: None,
                    created: c.created,
                    asked: ask::holds_questions(&c.body),
                },
                Some(Marker::Duplicate(of)) => Marked {
                    comment: c.id,
                    duplicate_of: Some(of),
                    created: c.created,
                    asked: ask::holds_questions(&c.body),
                },
                None => return None,
            };
            (plain_login(&c.author) == app && Some(c.id) > after).then_some(marked)
        });
        Ok(ours.min_by_key(|m| m.comment))
    }
}
