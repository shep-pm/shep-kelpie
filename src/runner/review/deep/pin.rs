//! What the confirming sessions added to the worktree, and whether the pushed
//! head still holds it
//!
//! A session's tests go in the worktree and not in a commit: the worker is to
//! make them pass and commit them. Kelpie pins what each session added, as the
//! lines it added to each file in order, taken against the worktree as that
//! session found it. Two sessions may write into the same file, and a fix may
//! change the file elsewhere, so a whole-file pin would fail a correct fix. The
//! re-check waits until the pushed head holds every pinned line, in order.

use std::path::Path;

use crate::ports::{Finding, parse_findings};
use crate::runner::Runner;
use crate::runner::gate::short;
use crate::work_item::{Backing, Found, Held, Written};
use crate::worktree::{self, WorktreeError};

fn wrong(e: WorktreeError) -> String {
    e.to_string()
}

impl Runner {
    // The worktree's files that differ from its head right now, so what the
    // session about to run adds can be told from what was there.
    pub(super) fn found_before(&self) -> Result<Vec<Found>, String> {
        let item = self.current().expect("the deep round is a work item's");
        let differing = worktree::differing(&self.settings.repo, &item.worktree).map_err(wrong)?;
        Ok(differing
            .into_iter()
            .map(|(path, blob)| Found { path, blob })
            .collect())
    }

    // What the session that confirmed with `file` added: the lines it added to
    // each file, against `before`. A line it took out of what an earlier
    // session added leaves that session's pin, since the head cannot hold it.
    //
    // Errors when git cannot say, or when it added nothing to `file`, so the
    // finding is left unconfirmed and not backed by a test kelpie cannot pin.
    pub(super) fn added_by_session(
        &self,
        file: &str,
        before: &[Found],
        held: &mut [Held],
    ) -> Result<Vec<Written>, String> {
        let item = self.current().expect("the deep round is a work item's");
        let (repo, tree) = (&self.settings.repo, &item.worktree);
        let mut written = Vec::new();
        for (path, after) in worktree::differing(repo, tree).map_err(wrong)? {
            let was = match before.iter().find(|f| f.path == path) {
                Some(found) => Some(found.blob.clone()),
                None => worktree::blob_at_head(repo, tree, &path).map_err(wrong)?,
            };
            if was.as_deref() == Some(after.as_str()) {
                continue;
            }
            let (added, removed) = match was {
                Some(was) => worktree::lines_changed(repo, tree, &was, &after).map_err(wrong)?,
                None => (
                    worktree::lines_of(repo, tree, &after).map_err(wrong)?,
                    Vec::new(),
                ),
            };
            for gone in &removed {
                drop_from_pins(held, &path, gone);
            }
            if !added.is_empty() {
                written.push(Written { path, added });
            }
        }
        if !written.iter().any(|w| w.path == file) {
            return Err(format!("it added no line to {file}"));
        }
        Ok(written)
    }

    // Why the worktree is not what was pushed, if it is not: it is not at the
    // pushed head, a file is changed or new and uncommitted, or the pushed head
    // lacks a line a session added for one of `held`, `held` being what the
    // worker was told to fix, less what it deferred.
    pub(super) fn left_behind(
        &self,
        held: &[Held],
        pushed: &str,
    ) -> Result<Option<String>, String> {
        let item = self.current().expect("the deep round is a work item's");
        let (repo, tree) = (&self.settings.repo, &item.worktree);
        let at = worktree::head(repo, tree).map_err(wrong)?;
        if at != pushed {
            return Ok(Some(format!(
                "its worktree is at {}, not at the pushed head {}",
                short(&at),
                short(pushed)
            )));
        }
        let uncommitted = worktree::uncommitted(repo, tree).map_err(wrong)?;
        if !uncommitted.is_empty() {
            return Ok(Some(format!(
                "{} are changed or new and not committed",
                uncommitted.join(", ")
            )));
        }
        for pinned in held {
            let Backing::Test { written, .. } = &pinned.backing else {
                continue;
            };
            let finding = format!("{}:{}", pinned.finding.file, pinned.finding.line);
            for entry in written {
                let Some(blob) = worktree::blob_at_head(repo, tree, &entry.path).map_err(wrong)?
                else {
                    return Ok(Some(format!(
                        "the review's test for {finding}, in {}, is not in the pushed head",
                        entry.path
                    )));
                };
                let lines = worktree::lines_of(repo, tree, &blob).map_err(wrong)?;
                if let Some(missing) = first_missing(&lines, &entry.added) {
                    return Ok(Some(format!(
                        "the review's test for {finding}, in {}, is not in the pushed head \
                         as it was written: `{missing}` is gone",
                        entry.path
                    )));
                }
            }
        }
        Ok(None)
    }
}

// The first of `wanted` that `lines` does not hold after the one before it.
fn first_missing<'a>(lines: &[String], wanted: &'a [String]) -> Option<&'a String> {
    let mut at = 0;
    for line in wanted {
        match lines[at..].iter().position(|l| l == line) {
            Some(found) => at += found + 1,
            None => return Some(line),
        }
    }
    None
}

// Takes `line` out of the lines an earlier session pinned in `path`, once.
fn drop_from_pins(held: &mut [Held], path: &str, line: &str) {
    let mut gone = false;
    for held in held {
        let Backing::Test { written, .. } = &mut held.backing else {
            continue;
        };
        if !gone {
            for entry in written.iter_mut().filter(|e| e.path == path) {
                if let Some(at) = entry.added.iter().position(|l| l == line) {
                    entry.added.remove(at);
                    gone = true;
                    break;
                }
            }
        }
        written.retain(|e| !e.added.is_empty());
    }
}

/// The findings in the deferred-findings file in `build`
///
/// Read as follow-up filing reads them, so a finding is deferred where that
/// would file it: by its file, line and what, whatever else the worker's copy of
/// the line says or however it wraps it.
pub(super) fn deferred_findings(build: &Path) -> Vec<Finding> {
    let text = std::fs::read_to_string(crate::runner::review::findings::deferred_path(build));
    parse_findings(&text.unwrap_or_default())
}
