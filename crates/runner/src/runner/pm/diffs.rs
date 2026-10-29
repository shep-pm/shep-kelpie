//! Each open branch's diff from `main`, written into the project manager's
//! folder before it wakes, since it runs no git, and the board section that
//! names them beside each work item's worktree
//!
//! A work item parked on red CI has its failed run's log named beside its
//! diff, and each workflow red on `main` has its log named after the items.
//! The diffs are those the board's last write read, with the lock let go, so
//! a wake runs no git. The folder is emptied first, so a diff never outlives
//! its work item. A work item parked on a follow-up ruling has merged, so its
//! file holds the merged pull request's diff instead.

use std::fmt::Write as _;
use std::path::Path;

use crate::runner::Runner;
use crate::runner::briefing::git::Reading;
use crate::runner::briefing::patch;
use crate::state::RulingKind;
use crate::ticket::TicketId;
use crate::work_item::Phase;

/// The folder in the project manager's that holds the diffs
const DIFFS: &str = "diffs";

impl Runner {
    /// Writes `diffs/<issue>.diff` in `folder` for each open work item,
    /// and returns the board's section naming them and each worktree
    ///
    /// A diff git could not give is left out, and noted there and in the log.
    pub(super) fn pm_diffs(&mut self, folder: &Path) -> String {
        let diffs = folder.join(DIFFS);
        let cleared = match std::fs::remove_dir_all(&diffs) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => std::fs::create_dir_all(&diffs),
        };
        let mut out = String::from(
            "## Code\n\nEach open work item's worktree, which you may read, and its branch's \
             diff from main, written in your folder as you woke.\n\n",
        );
        // Only `pm_call` reaches here, as a wake or `shep kelpie pm` starts,
        // so the logs are read once per call and not on every board write.
        // They do not depend on the diffs folder, so they are written first.
        let mut logs = self.pm_ci_logs(folder);
        let main_logs = self.pm_main_logs(folder);
        if let Err(e) = cleared {
            let why = format!("cannot clear {}: {}", diffs.display(), e.kind());
            self.notes
                .push(format!("the project manager's diffs: {why}"));
            let _ = writeln!(out, "(no diffs: {why})");
            for (issue, log) in logs {
                let _ = writeln!(out, "- #{issue}: {log}");
            }
            for log in main_logs {
                let _ = writeln!(out, "- {log}");
            }
            return out;
        }
        if self.state.work_items.is_empty() {
            out.push_str("(none)\n");
        }
        let items: Vec<TicketId> = (self.state.work_items.iter()).map(|i| i.issue).collect();
        for ticket in items {
            let file = diff_file(ticket);
            let read = match self.merged_branch(ticket) {
                Some(branch) => Some(self.merged_patch(&branch, ticket)),
                None => self.brief_patch(ticket).cloned(),
            };
            let written = match read {
                Some(Ok(diff)) => std::fs::write(folder.join(&file), diff)
                    .map_err(|e| format!("cannot write it: {}", e.kind())),
                Some(Err(why)) => Err(why),
                None => Err("the board has not read its branch yet".to_owned()),
            };
            let diff = match written {
                Ok(()) => format!("diff `{file}`"),
                Err(why) => {
                    self.notes.push(format!(
                        "no diff of {} for the project manager: {why}",
                        ticket.shown()
                    ));
                    format!("no diff: {why}")
                }
            };
            let worktree = self.paths.worktree(ticket);
            let _ = write!(
                out,
                "- {}: worktree `{}`, {diff}",
                ticket.shown(),
                worktree.display()
            );
            if let Some(log) = logs.remove(&ticket) {
                let _ = write!(out, ", {log}");
            }
            out.push('\n');
        }
        for log in main_logs {
            let _ = writeln!(out, "- {log}");
        }
        out
    }

    // The branch of `issue`'s work item if it is parked on a follow-up
    // ruling, since its pull request merged and the board's diff of it
    // from `main` is empty.
    fn merged_branch(&self, ticket: TicketId) -> Option<String> {
        let item = self.state.item(ticket)?;
        let Phase::Ruling { id } = item.phase else {
            return None;
        };
        let ruling = self.state.rulings.iter().find(|r| r.id == id)?;
        matches!(ruling.kind, RulingKind::FollowUp { .. }).then(|| item.branch.clone())
    }

    // What the merged pull request of `ticket`'s `branch` changed. A merged
    // diff is read by a wake for a follow-up ruling alone, once, so unlike
    // the board's diffs it runs git as the wake starts.
    fn merged_patch(&self, branch: &str, ticket: TicketId) -> Result<Vec<u8>, String> {
        let worktree = self.paths.worktree(ticket);
        let repo = Reading::of_clones(&self.settings.git.checkout, [worktree.as_path()]);
        patch::merged(&repo, branch, &worktree, self.settings.git.base())
    }
}

/// The diff of `issue`'s work item, in the project manager's folder
pub(super) fn diff_file(ticket: TicketId) -> String {
    format!("{DIFFS}/{ticket}.diff")
}
