//! Findings a merged pull request left unfixed, filed as issues on the project
//!
//! The judge holds a finding, and the worker fixes it. One it leaves, because
//! the fix is out of scope for the pull request, it copies into the deferred
//! findings file in its build folder. When the pull request merges, kelpie
//! reads that file once. Under `auto` each finding is filed at once, and under
//! `ask` the maintainer is asked first. A finding an open issue already holds
//! gets a comment on that issue instead of a second one.

use std::io;
use std::path::Path;

use super::Runner;
use super::report::{Begin, StepReport};
use super::review::findings;
use crate::board::READY;
use crate::ports::{Finding, ForgeError, OpenIssue, Severity, parse_findings};
use crate::settings::MergeAuthority;
use crate::state::{RulingKind, StateError};
use crate::work_item::FollowUps;

// A GitHub title stops at 256 characters, and one this long is unreadable
// in a list anyway.
const TITLE_LIMIT: usize = 80;

// The fewest characters of a finding's `what` a body may match on
const MIN_BODY_MATCH: usize = 12;

impl Runner {
    // What `finish` does first for a merged pull request. Returns None when
    // nothing is left to file, and the work item goes on to be removed.
    pub(super) fn follow_ups(&mut self) -> Result<Option<Begin>, StateError> {
        let item = self.current().expect("a follow-up is of a work item");
        let Some(number) = item.pull_request else {
            return Ok(None);
        };
        let pending = match item.follow_ups.clone() {
            Some(pending) => pending,
            None => {
                let path = findings::deferred_path(&item.build);
                let found = match read_deferred(&path) {
                    Ok(found) => found,
                    Err(reason) => return Ok(Some(self.gate_failed(reason))),
                };
                let pending = FollowUps {
                    findings: found,
                    ruled: false,
                };
                let saved = pending.clone();
                self.update(|item| item.follow_ups = Some(saved))?;
                pending
            }
        };
        if pending.findings.is_empty() {
            return Ok(None);
        }
        if !pending.ruled && self.settings.merge_authority == MergeAuthority::Ask {
            let kind = RulingKind::FollowUp {
                findings: pending.findings,
            };
            return self.raise(number, kind).map(Some);
        }
        self.file_follow_ups(number).map(Some)
    }

    // One finding at a time, each taken off the work item once it is filed,
    // so a failure part way retries only what is left.
    fn file_follow_ups(&mut self, number: u64) -> Result<Begin, StateError> {
        let item = self.current().expect("a follow-up is of a work item");
        let (issue, worktree) = (item.issue, item.worktree.clone());
        let repo = self.settings.forge.clone();
        let mut open = match self.ports.forge.open_issues(&repo) {
            Ok(open) => open,
            Err(e) => return Ok(self.gate_failed(format!("cannot list the open issues: {e}"))),
        };
        let (mut opened, mut commented, mut skipped) = (Vec::new(), Vec::new(), 0);
        while let Some(finding) = self.next_follow_up() {
            let finding = relative(finding, &worktree);
            let title = title_of(&finding);
            let filed = match already_filed(&open, &title, &finding) {
                Some(known) => {
                    let body = comment_body(number, &finding);
                    let number = known.number;
                    let posted = self.ports.forge.post_comment(&repo, number, &body);
                    posted.map(|_| commented.push(number))
                }
                None => {
                    let body = issue_body(number, &finding);
                    let made = self
                        .ports
                        .forge
                        .create_issue(&repo, &title, &body, &[READY]);
                    made.map(|made| {
                        opened.push(made);
                        open.push(OpenIssue {
                            number: made,
                            title,
                            body,
                        });
                    })
                }
            };
            match filed {
                Ok(()) => {}
                // Never sent, and never will be: retrying would only stall the work item.
                Err(ForgeError::LocalPath) => skipped += 1,
                Err(e) => {
                    let reason = format!("cannot file a follow-up for #{number}: {e}");
                    return Ok(self.gate_failed(reason));
                }
            }
            self.update(|item| {
                if let Some(pending) = item.follow_ups.as_mut() {
                    pending.findings.remove(0);
                }
            })?;
        }
        self.update(|item| item.follow_ups = Some(FollowUps::default()))?;
        Ok(Begin::Report(StepReport::FollowUpsFiled {
            issue,
            pull_request: number,
            opened,
            commented,
            skipped,
        }))
    }

    fn next_follow_up(&self) -> Option<Finding> {
        let item = self.current()?;
        item.follow_ups.as_ref()?.findings.first().cloned()
    }
}

// A file that was never written is a worker with nothing to defer.
fn read_deferred(path: &Path) -> Result<Vec<Finding>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(parse_findings(&text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("cannot read {}: {}", path.display(), e.kind())),
    }
}

// A worker names files by the path it sees, which starts at its worktree.
fn relative(finding: Finding, worktree: &Path) -> Finding {
    let file = Path::new(&finding.file)
        .strip_prefix(worktree)
        .map_or(finding.file.clone(), |rest| rest.display().to_string());
    Finding { file, ..finding }
}

fn title_of(finding: &Finding) -> String {
    finding.what.trim().chars().take(TITLE_LIMIT).collect()
}

// Another issue's title says the same, or its body names the file and says
// what this one does. A short `what` would match half the tracker, so only
// a title can match on one.
fn already_filed<'a>(
    open: &'a [OpenIssue],
    title: &str,
    finding: &Finding,
) -> Option<&'a OpenIssue> {
    open.iter().find(|issue| {
        issue.title.trim().eq_ignore_ascii_case(title)
            || (finding.what.trim().len() >= MIN_BODY_MATCH
                && issue.body.contains(&finding.file)
                && issue.body.contains(finding.what.trim()))
    })
}

fn issue_body(number: u64, finding: &Finding) -> String {
    format!(
        "A review of #{number} confirmed this, and the pull request merged without a fix.\n\n{}",
        details(finding)
    )
}

fn comment_body(number: u64, finding: &Finding) -> String {
    format!(
        "A review of #{number} confirmed this too, and the pull request merged without a fix.\n\n{}",
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
mod tests;
