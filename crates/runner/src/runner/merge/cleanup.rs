//! Ending a work item that finished: its worktree, branch and build folder removed, and the work
//! item with them

use super::super::Runner;
use super::super::claim::main_fetch_key;
use super::super::report::{Begin, StepReport};
use super::super::retro::journal::Entry;
use super::super::worktree_jobs::{Change, Cleared, Removing};
use crate::state::{Finished, StateError};
use crate::work_item::Phase;
use crate::worktree::Remote;

impl Runner {
    // Removes the worktree, branch and build folder, then the work
    // item, and records its issue so the board does not take it again until the
    // forge closes it, or for one not merged, until it is ready again or edited. A pull
    // request left unmerged is handed back to the maintainer first.
    pub(super) fn cleanup(&mut self, merged: bool, off_lock: bool) -> Result<Begin, StateError> {
        // First, since the findings sit in the build folder this removes.
        if merged && let Some(begin) = self.follow_ups()? {
            return Ok(begin);
        }
        // Before anything is removed, so a refused close runs this step again.
        if let (true, Some(item)) = (merged, self.current())
            && let Some(number) = item.pull_request
        {
            let issue = item.issue;
            if let Some(reason) = self.close_merged_issue(issue, number) {
                return Ok(self.gate_failed(reason));
            }
            self.close_done_parent_of(issue, number);
        }
        self.release_all()?;
        let Some(item) = self.current() else {
            return Ok(Begin::Idle);
        };
        if let (false, Some(number)) = (merged, item.pull_request)
            && let Err(reason) = self.hand_back(number)
        {
            return Ok(self.gate_failed(reason));
        }
        let removing = Removing {
            checkout: self.settings.git.checkout.clone(),
            worktree: item.worktree.clone(),
            branch: item.branch.clone(),
            build: item.build.clone(),
            remote: merged.then_some(Remote::Origin),
            merged,
            // Read before the build folder goes, which holds the file it is in.
            friction: self.friction_of(item),
        };
        self.changing(Change::Remove(removing), off_lock)
    }

    // The worktree, branch and build folder are removed, or are not: the work item ends
    pub(in crate::runner) fn cleaned_up(&mut self, cleared: Cleared) -> Result<Begin, StateError> {
        let Cleared {
            merged,
            friction,
            outcome,
        } = cleared;
        let Some(item) = self.current() else {
            return Ok(Begin::Idle);
        };
        let removed = outcome.map_err(|e| e.to_string());
        if let Err(e) = removed.and_then(|()| self.remove_code_graph(item.issue)) {
            return Ok(self.gate_failed(format!("cannot clean up: {e}")));
        }
        // The item is gone, so a main it could not fetch warns about nothing.
        self.notes.ends(&main_fetch_key(item.issue));
        let now = self.ports.clock.now();
        let timings = item.split(now, self.timing_phase(item));
        let closed = matches!(item.phase, Phase::Done { closed: true, .. });
        let record = Finished {
            issue: item.issue,
            title: item.title.clone(),
            pull_request: item.pull_request,
            merged,
            closed,
            at: now,
            wall: timings.wall,
            seconds: timings.seconds.clone(),
            spend: Some(item.tally()),
        };
        let mut line = crate::usage::FinishedLine::of(&record, item, item.issue);
        if let Ok(agent) = self.worker_agent(item) {
            line.harness = Some(agent.harness.harness().as_str().to_owned());
            line.model = Some(agent.model);
        }
        let ended = record.clone();
        let report = StepReport::Finished {
            issue: item.issue,
            pull_request: item.pull_request,
            merged,
            closed,
            spend: Box::new(item.spend()),
            qwen: item.qwen,
            timings,
        };
        let issue = item.issue;
        let mut next = self.state.clone();
        next.work_items.retain(|open| open.issue != item.issue);
        // Rulings about this work item go with it, a question asked before
        // its pull request included.
        next.rulings.retain(|r| r.issue != Some(item.issue));
        // An issue whose item ended with no merged pull request, dropped or stopped
        // by a no, stays open, unlike a merged one the forge closes, so it goes on
        // its own list, which a label put back or an edit clears.
        let (listed, other) = match merged {
            false => (&mut next.dropped, &mut next.finished),
            true => (&mut next.finished, &mut next.dropped),
        };
        other.retain(|n| *n != item.issue);
        if !listed.contains(&item.issue) {
            listed.push(item.issue);
        }
        // Forgotten, so the board takes what the issue reads as then.
        next.dropped_text.remove(&item.issue);
        next.record_finished(record);
        self.save(next)?;
        self.append_finished(line);
        if let Some(text) = friction {
            self.journal_at(now, Entry::Friction { issue, text });
        }
        self.tell_done(&ended);
        self.mark_held(issue, false);
        self.late_reads.remove(&issue);
        self.parked_reads.remove(&issue);
        Ok(Begin::Report(report))
    }
}
