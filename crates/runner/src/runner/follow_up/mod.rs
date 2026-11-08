//! Findings a merged pull request left unfixed, filed as issues on the project
//!
//! A reviewer's finding goes to the worker, which fixes it. One left as out
//! of scope goes into the deferred findings file in its build folder. One it
//! shows is wrong goes into the disputed findings file, which files nothing.
//! On merge kelpie files only findings it sent the worker itself, in its own
//! words. A line the worker invents files nothing. Deferred findings raise a
//! follow-up ruling for the project manager or maintainer before filing.
//! A finding an open ticket holds gets a comment there instead.

use std::path::Path;

use super::Runner;
use super::report::{Begin, StepReport};
use super::review::findings;
use crate::board::briefing::cut;
use crate::board::{READY, TRIAGE};
use super::review_bot::RuledThread;
use crate::ports::{Finding, ForgeError, NewLabel, OpenTicket, Severity};
use crate::settings::TicketSource;
use crate::state::{RulingKind, StateError};
use crate::ticket::TicketId;
use crate::work_item::{FollowUps, Phase, WorkItem};

// A GitHub title stops at 256 characters, and one this long is unreadable
// in a list anyway. `cut` adds ` …` to its limit.
const TITLE_LIMIT: usize = 72;

// What a title was cut at before it ended at a clause, kept to match the
// issues filed then.
const LEGACY_TITLE_LIMIT: usize = 80;

// How long the forge may keep refusing, counted from its first refusal,
// before the maintainer is asked whether to try again. Issues switched off
// or a label that is missing would otherwise hold a merged work item for
// ever, and a passing outage of the forge's own must not cost the findings.
const REFUSAL_WINDOW: u64 = 6 * 60 * 60;

// The most of the forge's own words a ruling carries: an unreadable answer
// can be a whole issue list, and the ruling goes to the webhook.
const REASON_LIMIT: usize = 300;

// The fewest characters of a finding's `what` a body may match on
const MIN_BODY_MATCH: usize = 12;

// Made on a repo that lacks it before the first follow-up filed with it,
// since the forge refuses an issue that names a label the repo does not have.
pub(super) const TRIAGE_LABEL: NewLabel<'static> = NewLabel {
    name: TRIAGE,
    color: "d876e3",
    description: "Kelpie filed this follow-up unread; it waits on the maintainer",
};

impl Runner {
    // What `finish` does first for a merged pull request. Returns None when
    // nothing is left to file, and the work item goes on to be removed.
    pub(super) fn follow_ups(&mut self) -> Result<Option<Begin>, StateError> {
        let item = self.current().expect("a follow-up is of a work item");
        let (issue, number) = (item.issue, item.pull_request);
        let Some(number) = number else {
            return Ok(None);
        };
        let pending = match item.follow_ups.clone() {
            Some(pending) => pending,
            None => {
                let found = match read_deferred(item) {
                    Ok(found) => found,
                    Err(reason) => return Ok(Some(self.gate_failed(reason))),
                };
                let pending = FollowUps {
                    findings: found,
                    ..FollowUps::default()
                };
                let saved = pending.clone();
                self.update(|item| item.follow_ups = Some(saved))?;
                pending
            }
        };
        if pending.findings.is_empty() {
            return Ok(None);
        }
        if !pending.ruled {
            let kind = RulingKind::FollowUp {
                findings: pending.findings,
                refused: None,
            };
            return self.raise(number, kind).map(Some);
        }
        match self.file_follow_ups(issue)? {
            Ok(begin) => Ok(Some(begin)),
            // The forge would not take them: the merged work item retries them,
            // and past the window the maintainer is asked.
            Err(reason) => self.forge_refused(number, reason).map(Some),
        }
    }

    /// Files each pending finding of `issue`, one at a time, each taken off the work item
    ///
    /// A failure part way retries only what is left. Only a ruled finding goes on the
    /// board as `ready-for-agent`; an unruled one is filed unread. Returns why the
    /// forge refused, if it did, which each caller retries its own way.
    ///
    /// # Errors
    ///
    /// [`StateError`] when what it filed cannot be saved.
    pub(in crate::runner) fn file_follow_ups(
        &mut self,
        issue: TicketId,
    ) -> Result<Result<Begin, String>, StateError> {
        let Some(item) = self.state.item(issue).cloned() else {
            return Ok(Err("its work item is gone".to_owned()));
        };
        let number = item.pull_request.unwrap_or_default();
        // Filed once the item finished, and its deferred findings are done with.
        // Filed before the merge, it leaves nothing pending, or the merge would
        // skip the deferred findings still in the build folder.
        let merged = matches!(item.phase, Phase::Done { .. });
        let triaged = (item.follow_ups.as_ref()).is_some_and(|p| p.ruled && !p.unread);
        let label = if triaged { READY } else { TRIAGE };
        // READY is on every repo `add` set up, so only TRIAGE may need making.
        let mut labelled = triaged;
        let repo = self.remote.clone();
        let mut open = match self.ports.tickets.open_tickets(&repo) {
            Ok(open) => open,
            Err(e) => return Ok(Err(format!("cannot list the open issues: {e}"))),
        };
        let (mut opened, mut commented, mut skipped) = (Vec::new(), Vec::new(), 0);
        while let Some(finding) = self.next_follow_up(issue) {
            let title = title_of(&finding);
            let filed = match already_filed(&open, &title, &finding) {
                Some(known) => {
                    let body = comment_body(number, &finding);
                    let on = known.ticket;
                    let posted = self.comment_on_ticket(on, &body);
                    posted.map(|()| commented.push(on))
                }
                None => {
                    // A local store takes any label, and has no repo to make one on.
                    if !labelled && self.settings.tickets.source == TicketSource::Github {
                        if let Err(reason) = self.make_missing_labels([TRIAGE_LABEL]) {
                            return Ok(Err(reason));
                        }
                        labelled = true;
                    }
                    let body = issue_body(number, &finding, triaged);
                    let made = self.file_ticket(&title, &body, &[label]);
                    made.map(|made| {
                        opened.push(made);
                        open.push(OpenTicket {
                            ticket: made,
                            title,
                            body,
                        });
                    })
                }
            };
            match filed {
                Ok(()) => self.update_of(issue, |item| {
                    if let Some(pending) = item.follow_ups.as_mut() {
                        pending.first_refused = None;
                    }
                })?,
                // Never sent, and never will be: retrying would only stall the work item.
                Err(ForgeError::LocalPath { .. }) => skipped += 1,
                Err(e) => {
                    return Ok(Err(format!("cannot file a follow-up for #{number}: {e}")));
                }
            }
            self.update_of(issue, |item| {
                if let Some(pending) = item.follow_ups.as_mut() {
                    pending.findings.remove(0);
                }
            })?;
        }
        self.update_of(issue, |item| {
            item.follow_ups = merged.then_some(FollowUps::default());
        })?;
        Ok(Ok(Begin::Report(StepReport::FollowUpsFiled {
            issue,
            pull_request: number,
            opened,
            commented,
            skipped,
        })))
    }

    // The forge would not take them. Retried on every pass for `REFUSAL_WINDOW`
    // from the first refusal, then a ruling lists them: a yes tries again for
    // another window, and a no drops them so the work item can finish.
    fn forge_refused(&mut self, number: u64, reason: String) -> Result<Begin, StateError> {
        let now = self.ports.clock.now();
        let item = self.current().expect("a follow-up is of a work item");
        let pending = item.follow_ups.clone().unwrap_or_default();
        let first = pending.first_refused.unwrap_or(now);
        if now.0.saturating_sub(first.0) < REFUSAL_WINDOW {
            self.update(|item| {
                if let Some(pending) = item.follow_ups.as_mut() {
                    pending.first_refused = Some(first);
                }
            })?;
            return Ok(self.gate_failed(reason));
        }
        let kind = RulingKind::FollowUp {
            findings: pending.findings,
            refused: Some(reason.chars().take(REASON_LIMIT).collect()),
        };
        self.raise(number, kind)
    }

    fn next_follow_up(&self, issue: TicketId) -> Option<Finding> {
        self.state
            .item(issue)?
            .follow_ups
            .as_ref()?
            .findings
            .first()
            .cloned()
    }
}

// The findings kelpie sent the worker that the file names again, each in
// kelpie's own words and filed once. A file that was never written is a
// worker with nothing to defer. A nit is never filed, though an old state
// file may still hold one. A finding the worker disputed with a reason is not.
pub(super) fn read_deferred(item: &WorkItem) -> Result<Vec<Finding>, String> {
    let deferred = findings::deferred(&item.build)?;
    let disputed = findings::disputed(&item.build)?;
    let wrong = |held: &Finding| {
        (disputed.iter()).any(|d| d.reason.is_some() && d.finding.is_same_as(held))
            || (item.disputes.iter()).any(|d| d.finding.is_same_as(held))
    };
    let named = |held: &Finding| deferred.iter().any(|d| d.is_same_as(held));
    Ok(item
        .held
        .iter()
        .filter(|held| !held.is_nit() && named(held) && !wrong(held))
        .cloned()
        .map(|finding| relative(finding, &item.worktree))
        .collect())
}

// The deferred finding `thread` stands for, none when the worker took it out of its file
// since the thread was made
pub(super) fn deferred_of(item: &WorkItem, thread: &RuledThread) -> Result<Option<Finding>, String> {
    let line = thread.line.unwrap_or(0);
    let found = read_deferred(item)?;
    Ok(found
        .into_iter()
        .find(|f| f.file == thread.path && f.line == line && f.what == thread.what))
}

// Takes `finding` out of the deferred findings file, so the merge does not read it again
pub(super) fn leave_deferred(item: &WorkItem, finding: &Finding) -> Result<(), String> {
    findings::drop_deferred(&item.build, |line| {
        line.is_same_as(finding) || relative(line.clone(), &item.worktree).is_same_as(finding)
    })
}

// A reviewer names files by the path it sees, which can start at the worktree.
fn relative(finding: Finding, worktree: &Path) -> Finding {
    let file = Path::new(&finding.file)
        .strip_prefix(worktree)
        .map_or(finding.file.clone(), |rest| rest.display().to_string());
    Finding { file, ..finding }
}

// The first clause of `what`, as its `. `, `: ` or `; ` ends it. The dot of
// a dotted abbreviation such as `e.g.` does not end one.
fn title_of(finding: &Finding) -> String {
    let what = finding.what.trim();
    let stops = (what.match_indices(['.', ':', ';']))
        .filter(|(at, _)| what[at + 1..].starts_with(' ') && *at > 0)
        .filter(|(at, stop)| *stop != "." || !abbreviation(&what[..*at]));
    let end = stops.map(|(at, _)| at).next();
    let clause = end.map_or(what, |at| what[..at].trim_end());
    if clause.chars().count() <= TITLE_LIMIT {
        return clause.to_owned();
    }
    cut(clause, TITLE_LIMIT - " …".chars().count())
}

// Whether `head` ends in a word of single letters between dots, as `e.g`
fn abbreviation(head: &str) -> bool {
    let word = head.rsplit(char::is_whitespace).next().unwrap_or(head);
    word.contains('.') && word.split('.').all(|part| part.chars().count() == 1)
}

// Another issue whose body names the file, and whose title says the same or
// whose body says what this one does. A short `what` would match half the
// tracker, so only a title can match on one.
fn already_filed<'a>(
    open: &'a [OpenTicket],
    title: &str,
    finding: &Finding,
) -> Option<&'a OpenTicket> {
    let what = finding.what.trim();
    let legacy: String = what.chars().take(LEGACY_TITLE_LIMIT).collect();
    open.iter().find(|issue| {
        let named = issue.title.trim();
        // A title shorter than `what` could be any of several findings.
        let titled = named.eq_ignore_ascii_case(what)
            || ((named.eq_ignore_ascii_case(title) || named.eq_ignore_ascii_case(&legacy))
                && issue.body.contains(what));
        issue.body.contains(&finding.file)
            && (titled || (what.len() >= MIN_BODY_MATCH && issue.body.contains(what)))
    })
}

fn issue_body(number: u64, finding: &Finding, triaged: bool) -> String {
    let body = report(number, "this", finding);
    if triaged {
        return body;
    }
    format!(
        "{body}\nNobody has read this yet. It waits on triage, and kelpie takes it \
         only once it is labelled `{READY}`.\n"
    )
}

fn comment_body(number: u64, finding: &Finding) -> String {
    report(number, "this too", finding)
}

fn report(number: u64, confirmed: &str, finding: &Finding) -> String {
    format!(
        "A review of #{number} confirmed {confirmed}, and the pull request merged without a fix.\n\n{}",
        details(finding)
    )
}

fn details(finding: &Finding) -> String {
    let place = match finding.line {
        0 => format!("`{}`", finding.file),
        line => format!("`{}:{line}`", finding.file),
    };
    let severity = match finding.severity {
        Severity::Low => "low",
        Severity::Medium => "medium",
        Severity::High => "high",
    };
    format!(
        "- Where: {place}\n- What: {}\n- Why it matters: {}\n- Severity: {severity}\n",
        finding.what.trim(),
        finding.why.trim()
    )
}

#[cfg(test)]
pub(super) mod tests;
