//! A work item handed off to the agent board's next pick before a due turn
//!
//! Before each due turn the runner asks the board, pinning nothing and leaving
//! out the agents the item has left. The turn goes on when its own agent is
//! ranked and finishes within the slack of the soonest finish. Otherwise the
//! item goes to the first ranked agent the pacer lets start, on a session of
//! its own, with a note of where the work stands. A pinned item never moves,
//! nor does one on an agent the board may not rank, one the board cannot
//! answer or ranks nobody for, or one whose ranking the pacer holds.

use super::super::Runner;
use super::super::pace::Walked;
use super::super::report::{Begin, StepReport};
use crate::agent_board::within_slack;
use crate::lease::door::PickAsk;
use crate::pacer::Scope;
use crate::skills::split_command;
use crate::state::StateError;
use crate::work_item::{Turn, WorkItem, new_session_id};
use crate::worktree;

/// How many of the branch's commits a handoff note lists, newest first
const NOTED_COMMITS: usize = 20;

impl Runner {
    // The step that hands the current work item off, or None when its turn goes on.
    pub(in crate::runner::turn) fn hand_off(&mut self) -> Result<Option<Begin>, StateError> {
        let Some(item) = self.current().filter(|item| !item.pinned) else {
            return Ok(None);
        };
        let ask = PickAsk {
            kind: item.kind.clone(),
            tier: item.tier,
            pin: None,
            slack: item.slack,
            agents: self.board_agents(),
            leave_out: item.left.clone(),
        };
        // An agent with no file `agents.allow` holds fails its turn, saying so.
        // One the pacer holds was ranked with that wait when the hold was found.
        if !ask.agents.contains(&item.agent) || self.turn_still_held(&item.agent) {
            return Ok(None);
        }
        let Ok(picks) = self.board_picks(&ask) else {
            return Ok(None);
        };
        let Some(soonest) = picks.iter().map(|pick| pick.finish).min() else {
            return Ok(None);
        };
        let (issue, from) = (item.issue, item.agent.clone());
        let from_finish = (picks.iter().find(|pick| pick.agent == from)).map(|pick| pick.finish);
        if from_finish.is_some_and(|own| within_slack(own, soonest, item.slack)) {
            return Ok(None);
        }
        let to = match self.first_unpaced(picks.iter().map(|pick| &pick.agent), Scope::Turn)? {
            Walked::Opens(to) if to != from => to,
            Walked::Reads(reads) => return Ok(Some(reads)),
            Walked::Opens(_) | Walked::Held { .. } => return Ok(None),
        };
        let session = match new_session_id() {
            Ok(session) => session,
            Err(e) => {
                let shown = issue.shown();
                self.notes.push(format!(
                    "{shown} stays on {from}: kelpie cannot draw a session id to hand it to {to}: {e}"
                ));
                return Ok(None);
            }
        };
        let to_finish = (picks.iter().find(|pick| pick.agent == to)).map_or(0, |pick| pick.finish);
        let mut next = self.state.clone();
        let Some(item) = next.item_mut(issue) else {
            return Ok(None);
        };
        // An agent that never ran adds nothing to the note it was handed.
        if item.handoff.is_none() {
            item.handoff = Some(self.handoff_note(item));
        }
        item.left
            .push(std::mem::replace(&mut item.agent, to.clone()));
        item.session = session;
        item.turn = Turn::Due;
        self.save(next)?;
        Ok(Some(Begin::Report(StepReport::HandedOff {
            issue,
            from,
            to,
            from_finish,
            to_finish,
        })))
    }

    // What the new session is told of `item`, as its agent left it: the
    // branch's commits, that agent's last word, and the prompt it was due.
    fn handoff_note(&self, item: &WorkItem) -> String {
        let from = &item.agent;
        let base = self.item_base(item);
        let branch = &item.branch;
        // A first turn handed off before it began has no worktree yet.
        let read = item.worktree.exists().then(|| self.commits_past_base(item));
        let commits = match read {
            None => String::new(),
            Some(Ok(commits)) if commits.is_empty() => format!(
                "The worktree and branch `{branch}` are as {from} left them, with no commits \
                 past `origin/{base}`.\n\n"
            ),
            Some(Ok(commits)) => format!(
                "The worktree and branch `{branch}` are as {from} left them. Its commits past \
                 `origin/{base}`, newest first:\n\n{}\n\n",
                listed(&commits)
            ),
            Some(Err(e)) => format!("Kelpie could not read the branch's commits: {e}\n\n"),
        };
        let said = match &item.summary {
            Some(summary) => format!("{from}'s last turn ended saying: {summary}"),
            None => format!("Kelpie holds no report of {from}'s last turn."),
        };
        let due = match &item.turn {
            Turn::Next { prompt } => {
                let (_, prompt) = split_command(prompt);
                format!("\n\nThis is what kelpie was about to send {from}:\n\n{prompt}")
            }
            _ => String::new(),
        };
        format!(
            "Kelpie handed {} to you from {from}, since another agent would finish it sooner.\
             \n\n{commits}{said}{due}",
            item.issue.shown(),
        )
    }

    // `item`'s branch's commits past its base, one `<sha> <subject>` line each.
    fn commits_past_base(&self, item: &WorkItem) -> Result<Vec<String>, String> {
        let run = worktree::trusted(&self.settings.git.checkout, &item.worktree)
            .map_err(|e| e.to_string())?;
        let tip = format!("refs/remotes/origin/{}", self.item_base(item));
        let log =
            run(&["log", "--format=%h %s", "HEAD", "--not", &tip]).map_err(|e| e.to_string())?;
        Ok(log.lines().map(str::to_owned).collect())
    }
}

/// The first prompt of a session kelpie starts in place of another: `brief`,
/// then what the lost session was sent, then the note of a handoff
pub(super) fn carried(brief: String, lost: Option<&str>, item: &WorkItem) -> String {
    let brief = match lost {
        Some(lost) => started_over(&brief, lost),
        None => brief,
    };
    match &item.handoff {
        Some(note) => format!("{brief}\n\n{note}"),
        None => brief,
    }
}

// A started-over session's brief, then what the session its harness lost was sent, past any
// command, since the brief leads with its own
fn started_over(brief: &str, lost: &str) -> String {
    let (_, lost) = split_command(lost);
    format!(
        "{brief}\n\nKelpie could not resume your earlier session on this work item, so this one \
         starts from the brief above. Your worktree is as that session left it. This is what \
         kelpie was sending it:\n\n{lost}"
    )
}

// `commits`, one a line, the first [`NOTED_COMMITS`] and a count of the rest.
fn listed(commits: &[String]) -> String {
    let shown = commits.iter().take(NOTED_COMMITS).cloned();
    let mut lines: Vec<String> = shown.collect();
    if let Some(more) = commits.len().checked_sub(NOTED_COMMITS).filter(|n| *n > 0) {
        lines.push(format!("and {more} more"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests;
