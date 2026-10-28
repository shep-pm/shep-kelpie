//! Opening a work item for a ticket

use std::fmt;

use super::Runner;
use super::pm::Picked;
use super::trigger::issue_list;
use crate::board::check::label_kind;
use crate::board::{AGENT_LABEL, LabelError, agent_label, old_worker_label, pinned};
use crate::ports::{ForgeError, Issue};
use crate::settings::AgentName;
use crate::state::StateError;
use crate::ticket::TicketId;
use crate::work_item::new_session_id;

/// Why `add` was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddError {
    /// No slot is free under `concurrency.active_items`, counting the items waiting for
    /// one, or one for this ticket is open already: the tickets of those in
    /// the way
    InFlight(Vec<TicketId>),
    /// The forge could not show the issue
    Forge(ForgeError),
    /// The issue is closed
    Closed(TicketId),
    /// The issue's `agent:` label cannot be used
    Label(LabelError),
    /// `finish` holds the board back, so no new work item opens
    Finishing,
    /// No random session id could be drawn, with the OS's reason
    Session(String),
    /// The work item could not be saved
    State(StateError),
}

// A label swap `add` made as it opened a work item
struct Moved {
    // The board event naming both implementers
    event: String,
    // The `agent:` label the issue had, and the one it has now
    old: String,
    new: String,
}

impl fmt::Display for AddError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InFlight(issues) if issues.len() == 1 => {
                write!(f, "the work item for {} is in flight", issue_list(issues))
            }
            Self::InFlight(issues) => {
                write!(f, "the work items for {} are in flight", issue_list(issues))
            }
            Self::Forge(e) => write!(f, "cannot read the issue: {e}"),
            Self::Closed(ticket) => write!(f, "{} is closed", ticket.shown()),
            Self::Label(e) => e.fmt(f),
            Self::Finishing => f.write_str(
                "the runner is finishing, so it opens no new work item: `shep kelpie start` \
                 lets it pick again",
            ),
            Self::Session(e) => write!(f, "cannot draw a session id: {e}"),
            Self::State(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for AddError {}

impl Runner {
    /// Opens a work item for `ticket`, and returns the implementer its
    /// worker runs on. Its first turn runs on a later pass.
    ///
    /// # Errors
    ///
    /// [`AddError`] when the runner is finishing, the project has
    /// `concurrency.active_items` working or one for this issue open
    /// already, the issue cannot be read or is closed, or its `agent:` label names no
    /// implementer kelpie has an agent file for, or the change cannot be saved.
    /// Nothing changes then.
    pub fn add(&mut self, ticket: TicketId) -> Result<AgentName, AddError> {
        self.add_picked(ticket, None)
    }

    // `add`, on what `picked`, the project manager's pick of `ticket`, says.
    pub(super) fn add_picked(
        &mut self,
        ticket: TicketId,
        picked: Option<&Picked>,
    ) -> Result<AgentName, AddError> {
        self.looks.poll_reset();
        self.add_allowed(ticket)?;
        let found = (self.ports.tickets.issue(&self.remote, ticket)).map_err(AddError::Forge)?;
        self.add_found(ticket, found, picked)
    }

    // Whether a work item for `ticket` may open now, before the issue is read.
    pub(super) fn add_allowed(&self, ticket: TicketId) -> Result<(), AddError> {
        if !self.picks() {
            return Err(AddError::Finishing);
        }
        if self.state.item(ticket).is_some() {
            return Err(AddError::InFlight(vec![ticket]));
        }
        if !self.slot_free() {
            return Err(AddError::InFlight(self.slot_issues()));
        }
        Ok(())
    }

    // Opens the work item for `ticket` from the issue as read, on what
    // `picked` says. The agent it names pins the work item over any label.
    // The guards are checked again, since the runner may have moved since.
    pub(super) fn add_found(
        &mut self,
        ticket: TicketId,
        found: Issue,
        picked: Option<&Picked>,
    ) -> Result<AgentName, AddError> {
        self.add_allowed(ticket)?;
        if !found.open {
            return Err(AddError::Closed(ticket));
        }
        let pin = self.pin(ticket, picked);
        let by_pm = pin.is_some();
        let (labelled, note) = match pin {
            Some(agent) => (agent, None),
            None => (self.labelled_agent(ticket, &found.labels)).map_err(AddError::Label)?,
        };
        let session = new_session_id().map_err(|e| AddError::Session(e.to_string()))?;
        let mut next = self.state.clone();
        let (agent, moved) = match by_pm {
            true => (labelled, None),
            false => self.opening_agent(ticket, &found.labels, labelled),
        };
        let mut item = self.fresh(ticket, found.title, agent.clone(), session);
        item.pinned = by_pm || pinned(&found.labels);
        item.kind = label_kind(&found.labels).map(str::to_owned);
        item.tier = picked.and_then(|p| p.tier);
        item.slack = picked.and_then(|p| p.slack);
        next.work_items.push(item);
        if let Some(moved) = &moved {
            next.record_event(self.ports.clock.now(), moved.event.clone());
        }
        if let Err(e) = self.save(next) {
            // No work item opened, so the issue keeps the label it had.
            if let Some(moved) = moved {
                let _ = self.swap_labels(ticket, &moved.new, &moved.old);
            }
            return Err(AddError::State(e));
        }
        self.notes.extend(note);
        self.mark_held(ticket, true);
        Ok(agent)
    }

    // The agent `picked` pins `ticket`'s work item to, while it is still an
    // implementer `agents.allow` holds. One that went since is noted.
    fn pin(&mut self, ticket: TicketId, picked: Option<&Picked>) -> Option<AgentName> {
        let agent = picked?.agent.clone()?;
        if self.implementer_files().contains(&agent) {
            return Some(agent);
        }
        self.notes.push(format!(
            "{}: the project manager picked it for {agent}, which is no longer an implementer \
             `agents.allow` holds, so its labels choose",
            ticket.shown()
        ));
        None
    }

    // The implementer a work item for `ticket` opens on, given the one its
    // `agent:` label names, and the move made, if any. A pinned label, or
    // one with no free implementer to move to, keeps `labelled`. A label swap
    // the forge refuses keeps it too, with a note, so label and item match.
    fn opening_agent(
        &mut self,
        ticket: TicketId,
        labels: &[String],
        labelled: AgentName,
    ) -> (AgentName, Option<Moved>) {
        let label = labels.iter().find(|l| is_agent_label(l));
        let to = label.filter(|_| !pinned(labels));
        let to = to.and_then(|label| Some((label, self.opens_elsewhere(ticket, &labelled)?)));
        let Some((label, to)) = to else {
            return (labelled, None);
        };
        let new = format!("{AGENT_LABEL}{to}");
        match self.swap_labels(ticket, label, &new) {
            Ok(()) => {
                let event = format!(
                    "{}: its `{AGENT_LABEL}{labelled}` label moved to `{new}`, since \
                     {labelled}'s model already has a wait",
                    ticket.shown()
                );
                let old = label.clone();
                (to, Some(Moved { event, old, new }))
            }
            Err(e) => {
                self.notes.push(format!(
                    "{}: stays on {labelled}, whose model already has a wait, since the \
                     forge would not move its label to `{new}`: {e}",
                    ticket.shown()
                ));
                (labelled, None)
            }
        }
    }

    // Puts the label `new` on `ticket` in place of `old`, the label it has.
    // Leaves it as it was when either change fails.
    fn swap_labels(&self, ticket: TicketId, old: &str, new: &str) -> Result<(), ForgeError> {
        self.set_agent_label(ticket, new, true)?;
        self.set_agent_label(ticket, old, false).inspect_err(|_| {
            let _ = self.set_agent_label(ticket, new, false);
        })
    }

    // Puts a label on, or takes it off, as the App where one covers the repo.
    // A local ticket keeps its labels in the store.
    fn set_agent_label(&self, ticket: TicketId, label: &str, on: bool) -> Result<(), ForgeError> {
        match ticket.github() {
            Some(number) => self.label_issue(number, label, on),
            None => (self.ports.tickets).set_issue_label(&self.remote, ticket, label, on),
        }
    }

    // The implementer a ticket with `labels` runs on: the one its `agent:`
    // label names, or the default. Beside it, the log line an old `worker:`
    // label gets once the work item opens, since it no longer picks anything.
    pub(super) fn labelled_agent(
        &self,
        ticket: TicketId,
        labels: &[String],
    ) -> Result<(AgentName, Option<String>), LabelError> {
        if let Some(agent) = agent_label(labels, &self.implementer_files())? {
            return Ok((agent, None));
        }
        let agent = self.agents.default_implementer.name.clone();
        let note = old_worker_label(labels).map(|old| {
            format!(
                "{} is labelled `{old}`, which kelpie no longer reads, so it runs on the \
                 default implementer, {agent}: an `agent:<name>` label picks another \
                 implementer",
                ticket.named().to_lowercase()
            )
        });
        Ok((agent, note))
    }

    // Every agent an `agent:` label may name for this project: each agent
    // file whose role is `implementer`, listed in `agents.implementers` or
    // not, that `agents.allow` holds. A listed one always has such a file and
    // passed that check at load, so this holds them all.
    pub(super) fn implementer_files(&self) -> Vec<AgentName> {
        let allow = self.settings.agents.allow.as_ref();
        let files = self.book.implementer_files();
        let allowed = files
            .into_iter()
            .filter(|n| allow.is_none_or(|a| a.holds(n)));
        allowed.collect()
    }
}

// Whether `label` is an `agent:` label, its prefix in any case.
fn is_agent_label(label: &str) -> bool {
    (label.get(..AGENT_LABEL.len())).is_some_and(|p| p.eq_ignore_ascii_case(AGENT_LABEL))
}
