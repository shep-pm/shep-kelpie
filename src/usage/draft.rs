//! A call's ledger line as it starts, finished once the call ends

use std::collections::BTreeMap;

use super::{CallKind, CallLine, Ended, PacerLine, units};
use crate::ports::{AgentCall, AgentError, AgentReply, Cost, Role, SessionId, Timestamp, Usage};
use crate::settings::LocalRound;

/// What a call's line says from its start: who made it, on what, and when
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    /// The issue of the work item it is for, if any
    pub issue: Option<u64>,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spent {
    /// The tokens it used
    pub usage: Usage,
    /// What its session had cost by its end, where the harness reports it
    pub session_cost: Option<Cost>,
}

impl From<&AgentReply> for Spent {
    fn from(reply: &AgentReply) -> Self {
        Self {
            usage: reply.usage,
            session_cost: reply.session_cost,
        }
    }
}

impl Draft {
    /// `call`, on the agent named `agent`, starting at `started`
    pub fn of(call: &AgentCall, agent: &str, kind: CallKind, started: Timestamp) -> Self {
        Self {
            issue: matches!(call.role, Role::Worker | Role::Reviewer).then_some(call.issue),
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

    /// A local round for `issue`, of the reviewer named `agent`, starting at `started`
    pub fn local(local: &LocalRound, issue: u64, agent: &str, started: Timestamp) -> Self {
        let model = match local {
            LocalRound::Endpoint(endpoint) => Some(endpoint.model.as_str().to_owned()),
            LocalRound::Command(command) => {
                (command.ollama_model.as_ref()).map(|model| model.as_str().to_owned())
            }
        };
        Self {
            issue: Some(issue),
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
        let cost = session_cost.map(|now| Cost(now.0.saturating_sub(before.0)));
        CallLine {
            at,
            issue: self.issue,
            pull_request: self.pull_request,
            role: self.role,
            kind: self.kind,
            agent: Some(self.agent.clone()),
            harness: Some(self.harness.clone()),
            model: self.model.clone(),
            effort: self.effort.clone(),
            session: self.session.as_ref().map(|s| s.0.clone()),
            usage,
            units: units(usage),
            cost_usd: cost.map(Cost::usd),
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
        Err(AgentError::Failed(..)) => Some(Ended::Failed),
        Err(AgentError::Unreadable(..)) => Some(Ended::Unreadable),
    }
}
