//! The retro a finished work item's worker is asked for
//!
//! When a work item ends, merged or closed, `finish` resumes its worker's
//! session once with the `retro` step's prompt, before the worktree goes.
//! The turn reads and searches the worktree and nothing else
//! ([`Tools::Retro`]), and its reply is the report. The runner saves the
//! reply under `retro-reports/unverified` in kelpie's home, since the
//! worker's fence cannot reach it. Kelpie never acts on a report: the
//! maintainer reads them, consolidates them and fixes what is worth fixing.
//!
//! A work item asks once. A retro that fails, times out, comes back empty or
//! is cut short by a stop is logged and skipped, never retried, and never
//! holds up the item's cleanup. It holds no slot, so the next work item is
//! not waiting on it.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use super::Runner;
use super::report::Begin;
use crate::pacer::Scope;
use crate::ports::{AgentCall, AgentError, AgentReply, Role, Session, Tools};
use crate::skills::Step;
use crate::state::StateError;
use crate::work_item::{Phase, WorkItem};

/// How long a retro gets, in seconds
pub(super) const CEILING: u64 = 15 * 60;

// How a retro names the end of the work item it looks back on
fn outcome(item: &WorkItem) -> String {
    let pull_request = item
        .pull_request
        .map_or_else(String::new, |n| format!(" Its pull request is #{n}."));
    match item.phase {
        Phase::Done { merged: true, .. } => format!("It merged.{pull_request}"),
        Phase::Done { closed: true, .. } => {
            "Its issue was closed with no change, so it has no pull request.".to_owned()
        }
        _ => format!("It ended without a merge.{pull_request}"),
    }
}

// What the retro asks of the worker. It stands alone, for a step set to run
// kelpie's own prompt, and follows the skill's command otherwise.
fn prompt(item: &WorkItem) -> String {
    format!(
        "Your work item, issue #{}, has finished. {}\n\n\
         Run a retrospective on this session: suggest improvements to the coding agent's \
         environment that would make the next run easier (navigation, automated checks, \
         coding standards, steering files, tool economy, information access). Your reply \
         is the report, in markdown. Edit no file and run no command; nothing in the report \
         is applied, and the maintainer reads it later. If nothing is worth the \
         maintainer's time, say so in a sentence.",
        item.issue,
        outcome(item)
    )
}

impl Runner {
    /// Starts the retro of the work item `finish` is ending, unless it has
    /// asked already or has none to ask
    ///
    /// Returns the call to run, or `Begin::Idle` to wait while the runner
    /// drains, and `None` when the end goes on at once: the retro is off,
    /// was asked, has no session to look back on, or cannot start (logged).
    ///
    /// # Errors
    ///
    /// [`StateError`] when the mark that it was asked cannot be saved.
    pub(super) fn retro(&mut self) -> Result<Option<Begin>, StateError> {
        let item = self.current().expect("a retro is of a work item");
        let ran = item.calls.iter().any(|c| c.role == Role::Worker);
        if item.retro || !ran || self.skills.off(Step::Retro) {
            return Ok(None);
        }
        if self.draining {
            return Ok(Some(Begin::Idle));
        }
        let issue = item.issue;
        let call = match self.pace_worker(Scope::Turn)?.holds() {
            Some(_) => Err("the pacer holds the worker's account".to_owned()),
            None => self.retro_call(),
        };
        // Marked before the call starts: a stop or a crash in the middle of
        // it does not ask again.
        self.update(|item| item.retro = true)?;
        match call {
            Ok(call) => Ok(Some(Begin::Retro(call))),
            Err(why) => {
                self.notes.push(format!("#{issue}: no retro, since {why}"));
                Ok(None)
            }
        }
    }

    // The worker's session resumed with the retro's prompt, in its worktree,
    // with tools that change nothing.
    fn retro_call(&self) -> Result<AgentCall, String> {
        let item = self.current().expect("a retro is of a work item");
        if !item.worktree.is_dir() {
            return Err("its worktree is gone".to_owned());
        }
        let prompt = self.skills.invoke(Step::Retro, &prompt(item));
        let session = Session::Resume(item.session.clone());
        let mut call = self.worker_call(item, session, Some(prompt))?;
        call.tools = Tools::Retro;
        call.settings = (self.paths.worker).join(format!("retro-settings-{}.json", item.issue));
        self.prepared(call)
    }

    /// Records the end of `issue`'s retro: its reply is saved as a report,
    /// and anything else is logged and let go
    pub(super) fn retro_ended(&mut self, issue: u64, result: Result<AgentReply, AgentError>) {
        self.flights.landed(issue);
        let skipped = |why: String| {
            format!("#{issue}: the retro is skipped and not asked again, since {why}")
        };
        let note = match result {
            Ok(reply) if reply.text.trim().is_empty() => skipped("it came back empty".into()),
            Ok(reply) => match self.save_retro(issue, &reply) {
                Ok(file) => format!(
                    "#{issue}: the retro is saved unverified in {}",
                    file.display()
                ),
                Err(why) => skipped(format!("it cannot be saved: {why}")),
            },
            Err(AgentError::Stopped) => skipped("the runner stopped during it".into()),
            Err(e) => skipped(format!("it failed: {e}")),
        };
        self.notes.push(note);
    }
    // Writes the report for `issue`'s work item, which is still open, under
    // the project's name and the time, so no two collide.
    fn save_retro(&self, issue: u64, reply: &AgentReply) -> Result<PathBuf, String> {
        let item = self.state.item(issue).ok_or("its work item is gone")?;
        let folder = &self.paths.retro_reports;
        fs::create_dir_all(folder).map_err(|e| format!("{}: {}", folder.display(), e.kind()))?;
        let now = self.ports.clock.now();
        let at = i64::try_from(now.0)
            .ok()
            .and_then(|s| jiff::Timestamp::from_second(s).ok())
            .map_or_else(
                || now.0.to_string(),
                |t| t.strftime("%Y%m%dT%H%M%SZ").to_string(),
            );
        let project = self.project.as_str();
        let file = folder.join(format!("{project}-{issue}-{at}.md"));
        let (merged, how) = match item.phase {
            Phase::Done { merged: true, .. } => ("yes", ""),
            Phase::Done { closed: true, .. } => ("no", " (the issue was closed with no change)"),
            _ => ("no", ""),
        };
        let pull_request = item
            .pull_request
            .map_or_else(|| "none".to_owned(), |n| format!("#{n}"));
        let text = format!(
            "# Retro: {project} #{issue}\n\n\
             - Project: {project}\n\
             - Issue: #{issue}\n\
             - Pull request: {pull_request}\n\
             - Merged: {merged}{how}\n\
             - Agent: {}\n\
             - Session: {}\n\n\
             ---\n\n\
             {}\n",
            item.agent.as_str(),
            reply.session_id.0,
            reply.text.trim()
        );
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
            .and_then(|mut f| f.write_all(text.as_bytes()));
        written.map_err(|e| format!("{}: {}", file.display(), e.kind()))?;
        Ok(file)
    }
}

#[cfg(test)]
mod tests;
