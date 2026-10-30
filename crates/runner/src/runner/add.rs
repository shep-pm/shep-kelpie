//! Opening a work item for a ticket

use std::fmt;

use super::Runner;
use super::briefing::passed_over;
use super::pace::{Started, Walked};
use super::pm::Picked;
use super::trigger::issue_list;
use crate::board::check::label_kind;
use crate::board::{
    self, LabelError, OpenPullRequest, READY, ReadyIssue, Skip, agent_label, old_worker_label,
    pinned,
};
use crate::lease::door::{Pick, PickAsk};
use crate::pacer::Scope;
use crate::ports::{ForgeError, Issue};
use crate::settings::{AgentName, GatewayKeys};
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
    /// The issue is open but carries no `ready-for-agent`, so the board would pass it over
    NotReady(TicketId),
    /// A queued one: the board would pass it over for a reason of its own, so no slot
    /// would open for it
    PassedOver(TicketId, Skip),
    /// The issue's `agent:` label cannot be used
    Label(LabelError),
    /// The agent board named no agent to open it on
    Unpicked(Unpicked),
    /// `finish` holds the board back, so no new work item opens
    Finishing,
    /// No random session id could be drawn, with the OS's reason
    Session(String),
    /// The work item could not be saved
    State(StateError),
}

/// What an `add` did with the ticket it was asked for
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Added {
    /// A work item opened for it at once, on the implementer it runs on
    Opened(AgentName),
    /// No slot was free and the issue was one kelpie dropped: it waits for the next
    /// free slot, which the board takes it into by priority
    Queued,
}

/// Where a new `add` may take `ticket`: a free slot, or the queue for the next one
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Room {
    /// A slot under `concurrency.active_items` is free
    Opens,
    /// None is free, and `ticket` is in `dropped`, so it waits for the next one
    Queues,
}

/// Why the agent board named no agent for a new work item
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unpicked {
    /// The dog that keeps it could not be asked, or its answer read
    Unreachable,
    /// It ranked no allowed implementer: what the item asked for, in words
    Nobody(String),
}

impl fmt::Display for Unpicked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable => {
                f.write_str("the agent board, which the dog keeps, cannot be reached")
            }
            Self::Nobody(wanted) => write!(
                f,
                "the agent board ranks no allowed implementer for {wanted}"
            ),
        }
    }
}

impl core::error::Error for Unpicked {}

/// The implementers the agent board ranks for a new work item, best first
pub(super) struct Ranked {
    first: AgentName,
    rest: Vec<AgentName>,
    // A pin, the project manager's or a pinned label's, was the only agent asked for
    pinned: bool,
    // The note for a project manager's pin that lapsed, told once the item opens
    note: Option<String>,
}

/// The implementer a new work item opens on, out of a [`Ranked`]
pub(super) struct Opening {
    pub(super) agent: AgentName,
    pub(super) pinned: bool,
    pub(super) note: Option<String>,
}

impl Ranked {
    /// The ranked agents, best first
    pub(super) fn agents(&self) -> impl Iterator<Item = &AgentName> {
        std::iter::once(&self.first).chain(&self.rest)
    }

    /// The item opens on the first ranked agent
    pub(super) fn first(self) -> Opening {
        let agent = self.first.clone();
        self.opening(agent)
    }

    /// The item opens on `agent`, one of those ranked
    pub(super) fn opening(self, agent: AgentName) -> Opening {
        Opening {
            agent,
            pinned: self.pinned,
            note: self.note,
        }
    }
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
            Self::NotReady(ticket) => write!(
                f,
                "{} is not labelled {READY}, so the board would not take it",
                ticket.shown()
            ),
            Self::PassedOver(ticket, skip) => write!(
                f,
                "the board would pass {} over: {}",
                ticket.shown(),
                passed_over(skip, &|text| text).0
            ),
            Self::Label(e) => e.fmt(f),
            Self::Unpicked(e) => e.fmt(f),
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
    /// Takes `ticket` back into kelpie's work: at once as [`Added::Opened`], on the
    /// implementer its worker runs on, or, with no slot free and the issue one kelpie
    /// dropped, as [`Added::Queued`] for the board to take by priority when one frees.
    ///
    /// # Errors
    ///
    /// [`AddError`] when the runner is finishing, the project has
    /// `concurrency.active_items` working or one for this issue open already with the
    /// issue no dropped one, the issue cannot be read or is closed, or its `agent:`
    /// label names no implementer kelpie has an agent file for, the agent board names
    /// no implementer, or the change cannot be saved. A queued one is refused too where
    /// its issue carries no `ready-for-agent`, or the board would pass it over for a
    /// reason of its own and so would take no slot for it. Nothing changes then.
    pub fn add(&mut self, ticket: TicketId) -> Result<Added, AddError> {
        match self.add_room(ticket)? {
            Room::Opens => self.add_picked(ticket, None, None).map(Added::Opened),
            Room::Queues => {
                self.looks.poll_reset();
                let found =
                    (self.ports.tickets.issue(&self.remote, ticket)).map_err(AddError::Forge)?;
                let (listed, open) = self.ask_board()?;
                self.add_read(ticket, found, listed, open)
            }
        }
    }

    // What an `add` of `ticket` may do now, before its issue is read: open a work item
    // in a free slot, or queue a dropped issue for the next one. A ticket that is no
    // dropped one is refused exactly as before.
    pub(super) fn add_room(&self, ticket: TicketId) -> Result<Room, AddError> {
        if !self.picks() {
            return Err(AddError::Finishing);
        }
        if self.state.item(ticket).is_some() {
            return Err(AddError::InFlight(vec![ticket]));
        }
        if self.slot_free() {
            return Ok(Room::Opens);
        }
        if self.is_dropped(ticket) {
            return Ok(Room::Queues);
        }
        Err(AddError::InFlight(self.slot_issues()))
    }

    // Applies an `add` of `ticket` on `found`, its issue as read, on `listed` and `open`,
    // what the board was asked with: a work item in a free slot, or `ticket` queued for
    // the next one. The guards are checked again, since the runner may have moved while
    // the issue was read.
    pub(super) fn add_read(
        &mut self,
        ticket: TicketId,
        found: Issue,
        listed: Vec<ReadyIssue>,
        open: Vec<OpenPullRequest>,
    ) -> Result<Added, AddError> {
        match self.add_room(ticket)? {
            Room::Opens => self.add_found(ticket, found, None, None).map(Added::Opened),
            Room::Queues => self
                .add_queued(ticket, &found, &listed, &open)
                .map(|()| Added::Queued),
        }
    }

    // What the board is asked with, read where the verb reads: the ready issues, which
    // show who is assigned, what each is blocked by and how many sub-issues it has, and
    // the open pull requests. A queued `add` reads them with the lock let go, in
    // `queued::ask_board_off_lock`.
    pub(super) fn ask_board(&self) -> Result<(Vec<ReadyIssue>, Vec<OpenPullRequest>), AddError> {
        let listed = (self.ports.tickets.ready_issues(&self.remote)).map_err(AddError::Forge)?;
        Ok((listed, self.open_listed().map_err(AddError::Forge)?))
    }

    // Takes `ticket`, whose issue `found` shows, out of `dropped` so the board can pick
    // it again, and says so once. No work item opens: the board takes it by priority when
    // a slot frees, so one the board would pass over is refused first. `trim_dropped`
    // rebuilds `dropped_text` from what it keeps, so the ticket's digest goes with it.
    fn add_queued(
        &mut self,
        ticket: TicketId,
        found: &Issue,
        listed: &[ReadyIssue],
        open: &[OpenPullRequest],
    ) -> Result<(), AddError> {
        if !found.open {
            return Err(AddError::Closed(ticket));
        }
        (agent_label(&found.labels, &self.implementer_files())).map_err(AddError::Label)?;
        if !found.labels.iter().any(|l| l == READY) {
            return Err(AddError::NotReady(ticket));
        }
        self.board_takes(ticket, listed, open)?;
        let mut next = self.state.clone();
        next.dropped.retain(|t| *t != ticket);
        self.save(next).map_err(AddError::State)?;
        // The skip is cleared here, so the next pass logs no second `skip-cleared`.
        self.skipped
            .retain(|skip| !matches!(skip, Skip::Dropped { issue } if *issue == ticket));
        self.notes.push(format!(
            "{} skip-cleared by add: no slot is free, so the board takes it by priority \
             when one frees",
            ticket.shown()
        ));
        Ok(())
    }

    // Whether the board would take `ticket` into a free slot, which is what a queued add
    // promises. `dropped` is read without `ticket`, whose own skip this add clears, and
    // the order is left out, which only sorts one issue against the others.
    fn board_takes(
        &self,
        ticket: TicketId,
        listed: &[ReadyIssue],
        open: &[OpenPullRequest],
    ) -> Result<(), AddError> {
        let Some(one) = listed.iter().find(|i| i.ticket == ticket) else {
            return Err(AddError::NotReady(ticket));
        };
        let dropped: Vec<TicketId> = (self.state.dropped.iter())
            .filter(|t| **t != ticket)
            .copied()
            .collect();
        let pick = board::pick(
            std::slice::from_ref(one),
            open,
            &self.state.finished,
            &dropped,
            &self.implementer_files(),
            &[],
        );
        match pick.skipped.into_iter().next() {
            Some(skip) => Err(AddError::PassedOver(ticket, skip)),
            None => Ok(()),
        }
    }

    // Whether `ticket` is one the board is leaving be
    fn is_dropped(&self, ticket: TicketId) -> bool {
        self.state.dropped.contains(&ticket)
    }

    // `add`, on what `picked`, the project manager's pick of `ticket`, says,
    // on `opening` where dispatch chose it already.
    pub(super) fn add_picked(
        &mut self,
        ticket: TicketId,
        picked: Option<&Picked>,
        opening: Option<Opening>,
    ) -> Result<AgentName, AddError> {
        self.looks.poll_reset();
        self.add_allowed(ticket)?;
        let found = (self.ports.tickets.issue(&self.remote, ticket)).map_err(AddError::Forge)?;
        self.add_found(ticket, found, picked, opening)
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
    // `picked` says, on `opening`, or else on the agent board's first pick.
    // The guards are checked again, since the runner may have moved since.
    pub(super) fn add_found(
        &mut self,
        ticket: TicketId,
        found: Issue,
        picked: Option<&Picked>,
        opening: Option<Opening>,
    ) -> Result<AgentName, AddError> {
        self.add_allowed(ticket)?;
        if !found.open {
            return Err(AddError::Closed(ticket));
        }
        let opening = match opening {
            Some(opening) => opening,
            None => self.ranked(ticket, &found.labels, picked)?.first(),
        };
        let session = new_session_id().map_err(|e| AddError::Session(e.to_string()))?;
        let mut next = self.state.clone();
        let agent = opening.agent;
        let mut item = self.fresh(ticket, found.title, agent.clone(), session);
        item.pinned = opening.pinned;
        item.kind = label_kind(&found.labels).map(str::to_owned);
        item.tier = picked.and_then(|p| p.tier);
        item.slack = picked.and_then(|p| p.slack);
        next.work_items.push(item);
        self.save(next).map_err(AddError::State)?;
        self.notes.extend(opening.note);
        self.mark_held(ticket, true);
        Ok(agent)
    }

    // The implementers the agent board ranks for a work item for `ticket`,
    // whose issue has `labels`, on what `picked` says. A pin, the project
    // manager's or else a pinned `agent:` label's, is the only one asked for.
    pub(super) fn ranked(
        &self,
        ticket: TicketId,
        labels: &[String],
        picked: Option<&Picked>,
    ) -> Result<Ranked, AddError> {
        let files = self.implementer_files();
        let (pin, note) = pin(ticket, picked, &files);
        let pin = match pin {
            Some(pin) => Some(pin),
            // A label naming no implementer is refused, pinned or not.
            None => (agent_label(labels, &files))
                .map_err(AddError::Label)?
                .filter(|_| pinned(labels)),
        };
        let ask = PickAsk {
            kind: label_kind(labels).map(str::to_owned),
            tier: picked.and_then(|p| p.tier),
            pin: pin.clone(),
            slack: Some(self.slack(picked.and_then(|p| p.slack))),
            agents: self.board_agents_of(files),
            leave_out: Vec::new(),
        };
        let picks = self.board_picks(&ask).map_err(AddError::Unpicked)?;
        let mut agents = picks.into_iter().map(|pick| pick.agent);
        let Some(first) = agents.next() else {
            return Err(AddError::Unpicked(Unpicked::Nobody(wanted(&ask))));
        };
        Ok(Ranked {
            first,
            rest: agents.collect(),
            pinned: pin.is_some(),
            note,
        })
    }

    // The minutes of slack a work item allows: its own, from the project
    // manager's pick, else the project's `agents.slack`.
    pub(super) fn slack(&self, own: Option<u32>) -> u32 {
        own.unwrap_or(self.settings.agents.slack)
    }

    // What the agent board ranks for `ask`, best first, of the agents `ask`
    // allows and pins. Empty when it ranks none of them.
    pub(super) fn board_picks(&self, ask: &PickAsk) -> Result<Vec<Pick>, Unpicked> {
        let picks = (self.ports.agent_board.pick(ask)).ok_or(Unpicked::Unreachable)?;
        // The project's allow list holds whatever the dog answers.
        let allowed = (picks.into_iter())
            .filter(|pick| ask.agents.contains(&pick.agent))
            .filter(|pick| ask.pin.as_ref().is_none_or(|pin| *pin == pick.agent));
        Ok(allowed.collect())
    }

    // What the agent board ranks for a work item no project manager picked, a
    // rework or an adoption, for `ticket`, whose issue has `labels`. Its note is
    // the log line an old `worker:` label gets once the work item opens, since it
    // no longer picks anything.
    pub(super) fn board_ranked(
        &self,
        ticket: TicketId,
        labels: &[String],
    ) -> Result<Ranked, AddError> {
        let mut ranked = self.ranked(ticket, labels, None)?;
        ranked.note = old_worker_label(labels).map(|old| {
            format!(
                "{} is labelled `{old}`, which kelpie no longer reads, so the agent board \
                 picks its implementer: an `agent:<name>!` label pins one",
                ticket.named().to_lowercase()
            )
        });
        Ok(ranked)
    }

    // The opening on the first agent of `ranked` the pacer lets start, walked as
    // dispatch walks it, or what the runner does instead: read a usage first, or
    // the first hold's step.
    pub(super) fn paced_opening(&mut self, ranked: Ranked) -> Result<Started<Opening>, StateError> {
        let walked = self.first_unpaced(ranked.agents(), Scope::Dispatch)?;
        Ok(match walked {
            Walked::Opens(agent) => Started::Opened(ranked.opening(agent)),
            Walked::Reads(reads) => Started::Held(Box::new(reads)),
            Walked::Held {
                first: Some(held), ..
            } => Started::Held(Box::new(held)),
            // A ranking holds an agent, so a walk that opened none held one.
            Walked::Held { first: None, .. } => Started::Opened(ranked.first()),
        })
    }

    // Why a work item whose issue has `labels` has no agent to run on, where it
    // has none: its `agent:` label names no implementer, or more than one.
    pub(super) fn label_refuses(&self, labels: &[String]) -> Option<LabelError> {
        agent_label(labels, &self.implementer_files()).err()
    }

    // The implementers the agent board may rank, by name: `implementer_files`
    // but those no turn can run on (`left_out`).
    pub(super) fn board_agents(&self) -> Vec<AgentName> {
        self.board_agents_of(self.implementer_files())
    }

    // `files`, the project's `implementer_files`, but those `left_out`.
    fn board_agents_of(&self, files: Vec<AgentName>) -> Vec<AgentName> {
        let refused = self.left_out();
        let refused: Vec<&AgentName> = refused.iter().map(|(name, _)| name).collect();
        (files.into_iter())
            .filter(|name| !refused.contains(&name))
            .collect()
    }

    // The implementers the agent board is never asked about, each with why: the
    // worker's fence would not hold on it, or the gateways cannot route to it.
    fn left_out(&self) -> Vec<(AgentName, String)> {
        (self.settings).refused_implementers(&self.book, &self.gateways, GatewayKeys::Held)
    }

    // The reviewers the pass never picks from, each with why: the gateway's key is missing.
    fn reviewers_left_out(&self) -> Vec<(AgentName, String)> {
        (self.settings).refused_reviewers(&self.book, &self.gateways)
    }

    // The names `left_out` and `reviewers_left_out` hold now
    pub(super) fn left_out_names(&self) -> Vec<AgentName> {
        let both = self.left_out().into_iter().chain(self.reviewers_left_out());
        both.map(|(name, _)| name).collect()
    }

    // A log line for each implementer or reviewer left out that `before` does not
    // name, so a reread tells only the ones it newly leaves out.
    pub(super) fn left_out_notes(&self, before: &[AgentName]) -> Vec<String> {
        let line = |role: &str, (name, why): (AgentName, String)| {
            format!("{role} {name} is left out of the agent board's ask: {why}")
        };
        let new = |(name, _): &(AgentName, String)| !before.contains(name);
        let implementers = self.left_out().into_iter().filter(new);
        let reviewers = self.reviewers_left_out().into_iter().filter(new);
        let implementers = implementers.map(|left| line("implementer", left));
        let reviewers = reviewers.map(|left| line("reviewer", left));
        implementers.chain(reviewers).collect()
    }

    // Every agent an `agent:` label may name for this project, by name: each
    // agent file whose role is `implementer` that `agents.allow` holds.
    pub(super) fn implementer_files(&self) -> Vec<AgentName> {
        let allowed = self.settings.allowed_implementers(&self.book).into_iter();
        allowed.map(|i| i.name).collect()
    }
}

// The agent `picked` pins its work item to, while it is one of `files`, the
// implementers `agents.allow` holds, and the note for an agent it names that
// is no implementer now.
fn pin(
    ticket: TicketId,
    picked: Option<&Picked>,
    files: &[AgentName],
) -> (Option<AgentName>, Option<String>) {
    let Some(picked) = picked else {
        return (None, None);
    };
    let pin = (picked.agent.as_ref())
        .filter(|agent| files.contains(agent))
        .cloned();
    let lapsed = picked.agent.as_ref().filter(|_| pin.is_none());
    let note = lapsed.map(|agent| {
        format!(
            "{}: the project manager picked it for {agent}, which is no longer an \
             implementer `agents.allow` holds, so the agent board picks",
            ticket.shown()
        )
    });
    (pin, note)
}

// What `ask` wanted, in words: its kind, its least tier and its pin.
fn wanted(ask: &PickAsk) -> String {
    let kind = match &ask.kind {
        Some(kind) => format!("a `{kind}` item"),
        None => "an item of no kind".to_owned(),
    };
    let tier = match ask.tier {
        Some(tier) => format!("tier {} or above", tier.as_str()),
        None => "any tier".to_owned(),
    };
    let pin = match &ask.pin {
        Some(pin) => format!(", pinned to {pin}"),
        None => String::new(),
    };
    format!("{kind} at {tier}{pin}")
}
