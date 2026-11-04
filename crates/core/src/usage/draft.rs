//! A call's ledger line as it starts, finished once the call ends

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{CallKind, CallLine, Ended, PacerLine, units};
use crate::ports::{
    AgentCall, AgentError, AgentReply, Cost, Metered, Role, SessionId, Timestamp, Usage,
};
use crate::settings::LocalRound;
use crate::ticket::TicketId;

/// What a call's line says from its start: who made it, on what, and when
// wire format: changing this is a change to the handover's state format
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    /// The ticket of the work item it is for, if any
    pub issue: Option<TicketId>,
    /// The work item's pull request, once it has one
    pub pull_request: Option<u64>,
    role: Role,
    kind: CallKind,
    agent: String,
    harness: String,
    model: Option<String>,
    effort: Option<String>,
    session: Option<SessionId>,
    started: Timestamp,
}

/// What a call came back with, as far as its line goes
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spent {
    /// The tokens it used
    pub usage: Usage,
    /// What its session had cost by its end, where the harness reports it
    pub session_cost: Option<Cost>,
    /// Times the harness compacted the session by itself during the call
    pub compactions: u32,
    /// What its gateway's lease used, where the gateway meters its leases
    pub metered: Option<Metered>,
}

impl From<&AgentReply> for Spent {
    fn from(reply: &AgentReply) -> Self {
        Self {
            usage: reply.usage,
            session_cost: reply.session_cost,
            compactions: reply.compactions,
            metered: reply.metered,
        }
    }
}

impl Draft {
    /// `call`, on the agent named `agent`, starting at `started`
    pub fn of(call: &AgentCall, agent: &str, kind: CallKind, started: Timestamp) -> Self {
        Self {
            issue: (matches!(call.role, Role::Worker | Role::Reviewer))
                .then_some(call.issue)
                .flatten(),
            pull_request: None,
            role: call.role,
            kind,
            agent: agent.to_owned(),
            harness: call.harness.harness().as_str().to_owned(),
            model: Some(call.model.clone()),
            effort: Some(call.effort.as_str().to_owned()),
            session: Some(call.session.id().clone()),
            started,
        }
    }

    /// A local round for the ticket `ticket`, of the reviewer named `agent`,
    /// starting at `started`
    pub fn local(local: &LocalRound, ticket: TicketId, agent: &str, started: Timestamp) -> Self {
        let model = match local {
            LocalRound::Endpoint(endpoint) => Some(endpoint.model.as_str().to_owned()),
            LocalRound::Command(_) => None,
        };
        Self {
            issue: Some(ticket),
            pull_request: None,
            role: Role::Reviewer,
            kind: CallKind::LocalRound,
            agent: agent.to_owned(),
            harness: "local".to_owned(),
            model,
            effort: None,
            session: None,
            started,
        }
    }

    /// The role it was made for
    pub fn role(&self) -> Role {
        self.role
    }

    /// Its session, if it has one
    pub fn session(&self) -> Option<&SessionId> {
        self.session.as_ref()
    }

    /// Its line, for a call that ended at `at` as `ended`, having spent
    /// `spent` if it reported anything, in a session that had cost
    /// `before` until it began
    pub fn line(
        &self,
        at: Timestamp,
        ended: Ended,
        spent: Option<Spent>,
        before: Cost,
        pacer: BTreeMap<String, PacerLine>,
    ) -> CallLine {
        let usage = spent.map(|s| s.usage).unwrap_or_default();
        let session_cost = spent.and_then(|s| s.session_cost);
        let compactions = spent.map_or(0, |s| s.compactions);
        let metered = spent.and_then(|s| s.metered);
        // A harness's own dollars win over what its lease's energy cost.
        let cost = (session_cost.map(|now| Cost(now.0.saturating_sub(before.0))))
            .or_else(|| metered.and_then(|m| m.cost));
        CallLine {
            at,
            issue: self.issue,
            pull_request: self.pull_request,
            role: self.role.into(),
            kind: self.kind.into(),
            agent: Some(self.agent.clone()),
            harness: Some(self.harness.clone()),
            model: self.model.clone(),
            effort: self.effort.clone(),
            session: self.session.as_ref().map(|s| s.0.clone()),
            usage,
            compactions,
            units: units(usage),
            cost_usd: cost.map(Cost::usd),
            energy_wh: metered.map(|m| m.wh),
            energy_source: metered.map(|m| m.source),
            unpriced: self.kind != CallKind::LocalRound && cost.is_none(),
            session_cost_usd: session_cost.map(Cost::usd),
            seconds: Some(at.0.saturating_sub(self.started.0)),
            ended,
            gpu_seconds: None,
            pacer,
            imported: false,
        }
    }
}

/// How a call that came back as `result` ended, or `None` when no model
/// was called: its settings could not be written, its harness could not
/// start, or its session had never begun
pub fn ended(result: &Result<AgentReply, AgentError>) -> Option<Ended> {
    match result {
        Ok(_) => Some(Ended::Answered),
        Err(AgentError::Setup(_) | AgentError::Spawn(..) | AgentError::NoSession(..)) => None,
        Err(AgentError::Stopped) => Some(Ended::Stopped),
        Err(AgentError::TimedOut(_)) => Some(Ended::TimedOut),
        // A host reached for part of the call, as pi on a 502 after many messages,
        // or not at all: the call ended badly either way, and its time is spent.
        Err(AgentError::Failed(..) | AgentError::Unreachable(..)) => Some(Ended::Failed),
        Err(AgentError::Unreadable(..)) => Some(Ended::Unreadable),
    }
}
