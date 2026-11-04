//! The skeleton of a call, which every [`Cli`] runs through

use std::path::Path;
use std::process::Command;

use super::served::Launched;
use super::terminals::Terminals;
use super::{Cli, Launch, Served, Turn};
use crate::adapters::forwarder::Forwarder;
use crate::adapters::harness::{
    Base, Carried, Noted, adoption_of, dial_the_forwarder, kept_server,
};
use crate::adapters::process::RunError;
use crate::forwarder::Upstream;
use crate::ports::{
    Adopted, AgentCall, AgentError, AgentReply, Agents, CallActivity, Ending, Kept, NotAdopted,
    Role, SessionId, Transcript,
};
use crate::settings::{Gateways, ModelServer, SessionIds};

/// A CLI `C` run as headless calls and terminal sessions, each inside the sandbox
///
/// Clones share their calls in flight, so one clone can stop them all.
#[derive(Debug, Clone)]
pub struct CliAdapter<C: Cli> {
    pub(crate) cli: C,
    pub(crate) base: Base,
    pub(crate) gateways: Gateways,
    // What each running call keeps in memory that a handover must carry
    pub(super) carried: Carried,
    // The forwarder and settings of each session the maintainer has open in a terminal
    terminals: Terminals,
}

impl<C: Cli> CliAdapter<C> {
    /// `cli` on `base`: its sandbox, lamb labels and calls in flight
    pub fn new(cli: C, base: &Base) -> Self {
        Self {
            cli,
            base: base.clone(),
            gateways: Gateways::default(),
            carried: Carried::default(),
            terminals: Terminals::default(),
        }
    }

    /// Sends a model behind a gateway through `gateways`
    #[must_use]
    pub fn with_gateways(self, gateways: Gateways) -> Self {
        Self { gateways, ..self }
    }

    /// The CLI's own part
    pub fn cli(&self) -> &C {
        &self.cli
    }

    /// Replaces the CLI's own part, as a test changes its program
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_cli(self, cli: C) -> Self {
        Self { cli, ..self }
    }

    /// The model server `call` runs on, for a CLI that needs one
    ///
    /// # Errors
    ///
    /// [`AgentError::Setup`] when kelpie routed a call here that this CLI cannot run.
    pub(crate) fn routed<'a>(
        &self,
        call: &'a AgentCall,
    ) -> Result<Option<&'a ModelServer>, AgentError> {
        let facts = C::HARNESS.facts();
        if call.harness.harness() != C::HARNESS {
            return Err(AgentError::Setup(format!(
                "kelpie routed a call to {} that names another harness",
                facts.name
            )));
        }
        match (facts.needs_model_server, call.harness.server()) {
            (true, None) => Err(AgentError::Setup(format!(
                "kelpie routed a call to {} that names no model server",
                facts.name
            ))),
            (true, Some(server)) => Ok(Some(server)),
            (false, _) => Ok(None),
        }
    }

    /// `call`'s command, inside the sandbox, with its scratch folder emptied
    /// and, for a CLI on a model server, a forwarder that the caller keeps for
    /// as long as the command runs
    ///
    /// The forwarder names `lease`, the gateway lease the call holds, on a
    /// worker's chat calls. Any other role's call holds one too but names none.
    ///
    /// # Errors
    ///
    /// [`AgentError::Setup`] when a folder, the forwarder or the sandbox cannot be made.
    pub(crate) fn launch(
        &self,
        call: &AgentCall,
        begun: &C::Begun,
        turn: Turn,
        lease: Option<&str>,
    ) -> Result<Launched, AgentError> {
        self.launch_with(call, turn, lease, |launch| {
            self.cli.command(call, begun, launch)
        })
    }

    /// As [`CliAdapter::launch`], for a command of the CLI's that is neither
    /// the call nor the terminal session, which `command` builds
    ///
    /// # Errors
    ///
    /// As [`CliAdapter::launch`], and whatever `command` fails with.
    pub(crate) fn launch_with(
        &self,
        call: &AgentCall,
        turn: Turn,
        lease: Option<&str>,
        command: impl FnOnce(&Launch<'_>) -> Result<Command, AgentError>,
    ) -> Result<Launched, AgentError> {
        let upstream = match self.routed(call)? {
            Some(server) => Some(match lease.filter(|_| call.role == Role::Worker) {
                Some(id) => self.upstream(server)?.with_lease(id),
                None => self.upstream(server)?,
            }),
            None => None,
        };
        self.cli.ready(call)?;
        let scratch = self.cli.scratch(call);
        match crate::worktree::remove_tree(&scratch) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(AgentError::Setup(format!(
                    "cannot empty {}: {}",
                    scratch.display(),
                    e.kind()
                )));
            }
            _ => {}
        }
        let folders = self.cli.folders(call)?;
        for folder in std::iter::once(&scratch).chain(&folders) {
            std::fs::create_dir_all(folder).map_err(|e| {
                AgentError::Setup(format!("cannot make {}: {}", folder.display(), e.kind()))
            })?;
        }
        let forwarder = match &upstream {
            Some(upstream) => {
                let folder = call.settings.parent().unwrap_or(Path::new("/"));
                Some(Forwarder::open(folder, upstream.clone()).map_err(AgentError::Setup)?)
            }
            None => None,
        };
        let served = upstream
            .as_ref()
            .zip(forwarder.as_ref())
            .map(|(upstream, forwarder)| Served {
                upstream,
                socket: &forwarder.socket,
            });
        let launch = Launch { turn, served };
        let command = command(&launch)?;
        let mut policy = self.cli.policy(call, &launch)?;
        if let Some(served) = served {
            dial_the_forwarder(&mut policy, served.upstream, served.socket);
        }
        let command = (self.base.sandbox)
            .wrap(&policy, &self.cli.sandbox_settings(call), &command)
            .map_err(|e| AgentError::Setup(e.to_string()))?;
        Ok(Launched { command, forwarder })
    }

    /// What the forwarder passes the call's chat calls to, a gateway's key included
    ///
    /// # Errors
    ///
    /// [`AgentError::Setup`] when the server's gateway is not in kelpie's settings.
    pub(crate) fn upstream(&self, server: &ModelServer) -> Result<Upstream, AgentError> {
        self.gateways
            .upstream(&server.host)
            .map_err(AgentError::Setup)
    }

    // The reply with what the CLI says when asked after the call, if it has to be.
    fn completed(&self, call: &AgentCall, reply: AgentReply) -> AgentReply {
        let Some(follow) = self.cli.follow_up(call) else {
            return reply;
        };
        let asked = self
            .launch_with(call, Turn::Headless, None, follow.command)
            .map_err(|e| e.to_string())
            .and_then(|mut launched| {
                (self.base.processes)
                    .output_within(&mut launched.command, follow.within)
                    .map_err(|e| format!("{e:?}"))
            });
        (follow.read)(reply, asked)
    }

    // Notes what a handover must carry for the call, until the guard is dropped.
    fn noted(
        &self,
        call: &AgentCall,
        launched: &Launched,
        lease: Option<&str>,
    ) -> Result<Option<Noted>, AgentError> {
        let (Some(forwarder), Some(server)) = (&launched.forwarder, call.harness.server()) else {
            return Ok(None);
        };
        Ok(Some(self.carried.note(
            call,
            Kept {
                forwarder: forwarder.socket.clone(),
                server: kept_server(&server.host),
                session: call.session.id().clone(),
                lease: lease.map(str::to_owned),
                model: Some(call.model.clone()),
            },
        )))
    }
}

impl<C: Cli> Agents for CliAdapter<C> {
    fn prepare(&self, call: &AgentCall) -> Result<(), AgentError> {
        let facts = C::HARNESS.facts();
        let upstream = match self.routed(call)? {
            Some(server) => Some(self.upstream(server)?),
            None => None,
        };
        self.cli.refuse(call)?;
        // parity: harness.guard-hooks
        if !facts.runs_guard_hooks
            && call
                .reach
                .fence
                .as_ref()
                .is_some_and(|f| !f.hooks.is_empty())
        {
            return Err(AgentError::Setup(format!(
                "{} cannot run `worker.guard_hooks`, which are Claude Code hooks: remove them, \
                 move the check to a `command` or `write` hook in `hooks`, which runs on every \
                 harness, or put the worker on Claude Code",
                facts.name
            )));
        }
        self.cli.write_config(call, upstream.as_ref())
    }

    fn run(&self, call: &AgentCall, ending: &Ending) -> Result<AgentReply, AgentError> {
        if let Some(reply) = self.cli.answer(call) {
            return reply;
        }
        // A session that is not there never waits for a lease.
        let begun = self.cli.begin(call, Turn::Headless)?;
        let label = call.label();
        let spawned = |pid| {
            if let Some(lambs) = &self.base.lambs {
                lambs.label(pid, &label);
            }
        };
        let (output, metered) = self.leased(call, ending, |lease| {
            // The forwarder stays until the call has ended.
            let mut launched = self.launch(call, &begun, Turn::Headless, lease)?;
            let _noted = self.noted(call, &launched, lease)?;
            (self.base.processes)
                .output_of_call(&mut launched.command, call, ending, &spawned)
                .map_err(|e| match e {
                    RunError::Io(e) => AgentError::Spawn(C::HARNESS, e.to_string()),
                    RunError::Stopped => AgentError::Stopped,
                    RunError::TimedOut => AgentError::TimedOut(C::HARNESS),
                })
        })?;
        let reply = self.cli.parse(call, begun, &output?)?;
        Ok(self.completed(call, AgentReply { metered, ..reply }))
    }

    fn adoption(&self, call: &AgentCall) -> Result<Adopted, NotAdopted> {
        let facts = C::HARNESS.facts();
        let mut adopted = adoption_of(call, C::HARNESS)?;
        if facts.needs_model_server {
            let kept = self.carried.kept_by(&adopted.record);
            adopted.kept = Some(kept.ok_or("this image holds no forwarder for the call")?);
        }
        if facts.sessions == SessionIds::Cli {
            adopted.session = Some(call.session.id().clone());
        }
        Ok(adopted)
    }

    fn adopt(&self, adopted: &Adopted, ending: &Ending) -> Result<AgentReply, AgentError> {
        self.adopt_call(adopted, ending)
    }

    fn transcript(&self, call: &AgentCall) -> Option<Transcript> {
        self.cli.transcript(call)
    }

    fn last_active(&self, call: &AgentCall) -> CallActivity {
        self.cli.last_active(call)
    }

    fn foreground(&self, call: &AgentCall) -> Result<Command, AgentError> {
        let begun = self.cli.begin(call, Turn::Terminal)?;
        // The session holds no gateway lease: the maintainer may leave it open
        // for hours, and a lease is for a call's span.
        let launched = self.launch(call, &begun, Turn::Terminal, None)?;
        self.cli.started(call, &begun)?;
        self.terminals.keep(call, launched.forwarder);
        Ok(launched.command)
    }

    fn foreground_ended(&self, session: &SessionId) {
        if let Some(settings) = self.terminals.end(session) {
            self.cli.ended(&settings, session);
        }
    }
}
