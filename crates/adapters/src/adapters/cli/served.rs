//! A call on a model server: its gateway lease, and its forwarder across a handover

use std::process::{Command, Output};

use super::{Cli, CliAdapter};
use crate::adapters::forwarder::Forwarder;
use crate::adapters::harness::host_of;
use crate::adapters::process::RunError;
use crate::ports::{
    Adopted, AgentCall, AgentError, AgentReply, Ending, Kept, Metered, Stand, Taking, Unheld, Wait,
};
use crate::settings::ModelHost;

/// A command inside its sandbox, and the forwarder its CLI dials
#[derive(Debug)]
pub(crate) struct Launched {
    pub(crate) command: Command,
    /// Kept for as long as the command runs, for a CLI on a model server
    pub(crate) forwarder: Option<Forwarder>,
}

impl<C: Cli> CliAdapter<C> {
    // A call on a gateway's model holds a lease on it. Its ceiling counts from
    // the grant, and a lease the gateway took back ended the call, whatever the
    // run says. The run gets the lease's id, so its chat calls name their turn,
    // and the answer says what the lease used, for a gateway that meters it.
    pub(crate) fn leased<T>(
        &self,
        call: &AgentCall,
        ending: &Ending,
        run: impl FnOnce(Option<&str>) -> T,
    ) -> Result<(T, Option<Metered>), AgentError> {
        let harness = C::HARNESS;
        let Some(server) = self.routed(call)? else {
            return Ok((run(None), None));
        };
        let ModelHost::Gateway(name) = &server.host else {
            return Ok((run(None), None));
        };
        let upstream = self.upstream(server)?;
        let port = crate::adapters::port_named(&self.gateways, name).map_err(AgentError::Setup)?;
        let note = format!("kelpie {}", call.label());
        let tell = |stand| match stand {
            Stand::Asking | Stand::Turn => ending.wait(Wait::Lease),
            Stand::Busy => ending.wait(Wait::Busy),
            Stand::Granted => ending.begin(),
        };
        let taking = Taking {
            model: &call.model,
            note: &note,
            give_up: &|| ending.asked() || self.base.processes.stopping(),
            tell: &tell,
            ending,
        };
        port.around_leased(&upstream, &taking, run)
            .map_err(|unheld| match unheld {
                Unheld::GaveUp if self.base.processes.stopping() => AgentError::Stopped,
                Unheld::GaveUp => AgentError::TimedOut(harness),
                Unheld::Refused(why) => AgentError::Setup(format!(
                    "cannot take a lease on {} from gateway {name}: {why}",
                    call.model
                )),
                Unheld::Lost(why) => AgentError::Failed(harness, why),
            })
    }

    /// Finishes `adopted` as `run` finishes a call it started: with its
    /// forwarder open again and its lease, if it held one, renewed, for a CLI
    /// on a model server, and with what that lease used on its reply
    pub(crate) fn adopt_call(
        &self,
        adopted: &Adopted,
        ending: &Ending,
    ) -> Result<AgentReply, AgentError> {
        let harness = C::HARNESS;
        let facts = harness.facts();
        let on_a_server = facts.needs_model_server && !adopted.api;
        let kept = match (on_a_server, adopted.kept.as_ref()) {
            (true, None) => {
                let why = format!("the carried {} call names no forwarder", facts.name);
                return Err(self.unserved(adopted, ending, why));
            }
            (true, kept) => kept,
            (false, _) => None,
        };
        let begun = match self.cli.adopted_begin(adopted) {
            Ok(begun) => begun,
            Err(why) => return Err(self.unserved(adopted, ending, why)),
        };
        let wait = || self.base.processes.output_of_adopted(adopted, ending);
        let (waited, metered) = match kept {
            Some(kept) => self.wait_served(adopted, kept, ending, wait)?,
            None => (wait(), None),
        };
        let output = waited.map_err(|e| match e {
            RunError::Io(e) => AgentError::Failed(harness, e.to_string()),
            RunError::Stopped => AgentError::Stopped,
            RunError::TimedOut => AgentError::TimedOut(harness),
        })?;
        let reply = self.cli.parse_adopted(adopted, begun, &output)?;
        Ok(AgentReply { metered, ..reply })
    }

    // Waits out the adopted call with its forwarder open and its lease renewed,
    // and says what the lease used.
    fn wait_served(
        &self,
        adopted: &Adopted,
        kept: &Kept,
        ending: &Ending,
        wait: impl FnOnce() -> Result<Output, RunError>,
    ) -> Result<(Result<Output, RunError>, Option<Metered>), AgentError> {
        let harness = C::HARNESS;
        let served = host_of(&kept.server).and_then(|host| {
            let upstream = self.gateways.upstream(&host)?;
            let leased = match &kept.lease {
                Some(id) => upstream.clone().with_lease(id),
                None => upstream.clone(),
            };
            let forwarder = Forwarder::reopen(&kept.forwarder, leased)?;
            Ok((host, upstream, forwarder))
        });
        // The forwarder stays until the call has ended.
        let (host, upstream, _forwarder) = match served {
            Ok(served) => served,
            Err(why) => return Err(self.unserved(adopted, ending, why)),
        };
        let (Some(id), ModelHost::Gateway(name)) = (&kept.lease, &host) else {
            return Ok((wait(), None));
        };
        let port = match crate::adapters::port_named(&self.gateways, name) {
            Ok(port) => port,
            Err(why) => return Err(self.unserved(adopted, ending, why)),
        };
        match port.around_resumed(&upstream, id, ending, wait) {
            Ok(waited) => Ok(waited),
            Err(Unheld::Refused(why)) => {
                let why = format!("cannot resume the lease {id} on gateway {name}: {why}");
                Err(self.unserved(adopted, ending, why))
            }
            Err(Unheld::GaveUp) => Err(AgentError::TimedOut(harness)),
            Err(Unheld::Lost(why)) => Err(AgentError::Failed(harness, why)),
        }
    }

    // A call that cannot be served is ended and waited out, so no process is
    // left running with no one to finish it.
    fn unserved(&self, adopted: &Adopted, ending: &Ending, why: String) -> AgentError {
        ending.end();
        let _ = self.base.processes.output_of_adopted(adopted, ending);
        AgentError::Setup(why)
    }
}
