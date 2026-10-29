//! Each call's line in the usage ledger, written as the runner hears it end
//!
//! A call's draft is taken as it launches, so its line names the agent it
//! ran on even once the agent's file changes. Its cost is the change in its
//! session's: a work item's record holds a worker's or reviewer's session,
//! and the ledger itself the project manager's, which no work item records.

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::Runner;
use super::in_flight::{Open, Stopping, gpu_seconds};
use super::report::{ReviewResult, Reviewed, Spent as ReviewSpent};
use crate::ports::{AgentError, AgentReply, Cost, Role};
use crate::ticket::TicketId;
use crate::usage::{CallLine, Draft, Ended, FinishedLine, Line, PacerLine, Spent, ended};
use crate::work_item::Phase;

impl Runner {
    /// The agent `ticket`'s worker runs on
    pub(super) fn ledger_worker(&self, ticket: TicketId) -> String {
        self.state
            .item(ticket)
            .map_or_else(String::new, |item| item.agent.as_str().to_owned())
    }

    /// The reviewer whose round `ticket`'s review is at
    pub(super) fn ledger_reviewer(&self, ticket: TicketId) -> String {
        match self.state.item(ticket).map(|item| &item.phase) {
            Some(Phase::Review(review)) => {
                (review.reviewer.as_ref()).map_or_else(String::new, |name| name.as_str().to_owned())
            }
            _ => String::new(),
        }
    }

    /// `draft` with its work item's pull request, once it has one
    pub(super) fn with_pull_request(&self, mut draft: Draft) -> Draft {
        let item = draft.issue.and_then(|ticket| self.state.item(ticket));
        draft.pull_request = draft.pull_request.or(item.and_then(|i| i.pull_request));
        draft
    }

    /// What a stopping runner writes its calls in flight with, which needs
    /// no lock of the runner's
    pub fn stopping(&self) -> Stopping {
        Stopping::new(self.flights.in_flight(), self.ledger.path().to_owned())
    }

    /// Each account's last reading by the pacer
    pub(super) fn pacer_lines(&self) -> BTreeMap<String, PacerLine> {
        let read = self.pacing.iter().filter_map(|(account, (_, assessment))| {
            let reading = assessment.reading.as_ref()?;
            let line = PacerLine {
                at: reading.at,
                session_pct: reading.session.used_pct,
                session_resets_at: Some(reading.session.resets_at),
                week_pct: reading.week.used_pct,
                week_resets_at: Some(reading.week.resets_at),
            };
            Some((account.as_str().to_owned(), line))
        });
        read.collect()
    }

    // What `draft`'s session had cost before this call, by its work item's record
    fn cost_before(&self, draft: &Draft) -> Cost {
        let item = draft.issue.and_then(|ticket| self.state.item(ticket));
        match (item, draft.session()) {
            (Some(item), Some(session)) => item.session_cost(session),
            _ => Cost(0),
        }
    }

    /// The line for a worker's turn that came back as `result`, read
    /// before the turn's end is recorded, or `None` when it reached no model
    pub(super) fn turn_line(
        &self,
        open: &Open,
        result: &Result<AgentReply, AgentError>,
    ) -> Option<CallLine> {
        let ended = match ended(result)? {
            // A turn the pause ended at a tool result, whatever its harness
            // reported of the abort.
            Ended::Answered | Ended::Failed | Ended::Unreadable
                if open
                    .draft
                    .issue
                    .is_some_and(|ticket| self.paused_at_tool(ticket)) =>
            {
                Ended::Paused
            }
            ended => ended,
        };
        let spent = result.as_ref().ok().map(Spent::from);
        let now = self.ports.clock.now();
        let before = self.cost_before(&open.draft);
        Some(
            open.draft
                .line(now, ended, spent, before, self.pacer_lines()),
        )
    }

    /// The line for a review call that came back as `reviewed`, read before
    /// its end is recorded, or `None` when it reached no model
    pub(super) fn review_line(&self, open: &Open, reviewed: &Reviewed) -> Option<CallLine> {
        let now = self.ports.clock.now();
        let (ended, spent) = match (&reviewed.result, &reviewed.spent) {
            (ReviewResult::Stopped, _) => (Ended::Stopped, None),
            (_, Some(ReviewSpent::Unanswered(ended))) => (*ended, None),
            (ReviewResult::Findings(Ok(_)), spent) => (Ended::Answered, spent.as_ref()),
            // It answered, with what reads as no review.
            (ReviewResult::Findings(Err(_)), Some(ReviewSpent::Claude { .. })) => {
                (Ended::Unreadable, reviewed.spent.as_ref())
            }
            (ReviewResult::Findings(Err(_)), Some(ReviewSpent::Local)) => (Ended::Failed, None),
            // A session whose settings, harness or session never got as far as a model
            (ReviewResult::Findings(Err(_)), None) => return None,
        };
        let spent = match spent {
            Some(ReviewSpent::Claude {
                usage,
                session_cost,
                ..
            }) => Some(Spent {
                usage: *usage,
                session_cost: *session_cost,
                // A reviewer's session has no compact window, so no hook counts a compaction.
                compactions: 0,
            }),
            _ => None,
        };
        let before = self.cost_before(&open.draft);
        let mut line = open
            .draft
            .line(now, ended, spent, before, self.pacer_lines());
        if open.draft.session().is_none() {
            line.gpu_seconds = gpu_seconds(open.ran_from, now, line.seconds);
        }
        Some(line)
    }

    /// Writes `line` as the item's finished line, then asks the agent board
    /// whether the agent it names still fits the kind it names
    ///
    /// The line goes first, so the board reads the item into its records.
    pub(super) fn append_finished(&mut self, line: FinishedLine) {
        let (agent, kind, number) = (line.agent.clone(), line.kind.clone(), line.pull_request);
        self.ledger.append(&Line::Finished(line));
        self.agent_fit(agent.as_deref(), kind.as_deref(), number);
    }

    /// Writes the line for a call no work item records, such as the project
    /// manager's, that came back as `result`
    pub(super) fn plain_line(&mut self, open: &Open, result: &Result<AgentReply, AgentError>) {
        let Some(ended) = ended(result) else {
            return;
        };
        let spent = result.as_ref().ok().map(Spent::from);
        let before = (open.draft.session())
            .and_then(|session| self.ledger.session_cost(session))
            .unwrap_or(Cost(0));
        let now = self.ports.clock.now();
        let line = open
            .draft
            .line(now, ended, spent, before, self.pacer_lines());
        self.ledger.append(&Line::Call(line));
    }

    /// Writes the line for a call whose thread panicked, as failed
    pub(super) fn panicked_line(&mut self, open: &Open) {
        let line = open.unreported(self.ports.clock.now(), Ended::Failed);
        self.append_call(line, false);
    }

    /// Writes `line`, naming its work item's pull request if it has one by
    /// now. An `unreported` session's call, which no call record holds, is
    /// counted on its work item, so the finished tally has every call.
    pub(super) fn append_call(&mut self, mut line: CallLine, unreported: bool) {
        let item = line.issue.and_then(|ticket| self.state.item(ticket));
        line.pull_request = line.pull_request.or(item.and_then(|i| i.pull_request));
        let counted = (line.issue.zip(line.session.as_ref()))
            .and_then(|(ticket, _)| Some((ticket, line.role.known()?)));
        self.ledger.append(&Line::Call(line));
        if let (true, Some(counted)) = (unreported, counted) {
            self.count_unreported(&[counted]);
        }
    }

    // Counts each of `calls` on its work item as a call that reported no
    // usage. A save that fails is told and let go: only the tally is short.
    fn count_unreported(&mut self, calls: &[(TicketId, Role)]) {
        let mut next = self.state.clone();
        let mut changed = false;
        for &(ticket, role) in calls {
            let Some(item) = next.item_mut(ticket) else {
                continue;
            };
            let count = match role {
                Role::Worker => &mut item.counts.worker_unreported,
                Role::Reviewer => &mut item.counts.reviewer_unreported,
                Role::Pm | Role::Representative | Role::Triager => continue,
            };
            *count = count.saturating_add(1);
            changed = true;
        }
        if changed && let Err(e) = self.save(next) {
            eprintln!("cannot count the calls that reported no usage: {e}");
        }
    }
}

/// Counts the calls a stop wrote `stopped` lines for on their work items
///
/// Run only once the loop has let go of the runner.
pub fn count_stopped(runner: &Mutex<Runner>, stopped: &[(TicketId, Role)]) {
    if !stopped.is_empty() {
        super::trigger::lock(runner).count_unreported(stopped);
    }
}
