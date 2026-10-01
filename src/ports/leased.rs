//! The leases a local agent holds for each whole call
//!
//! A local model's limit is the GPU it runs on, not an account, so a call
//! on one waits for its lease and keeps it until the call ends. A review
//! round on the same lock then waits behind a worker's turn.

use std::fmt;
use std::sync::Arc;

use super::{AgentCall, AgentError, AgentReply, Agents};
use crate::lease::gpu::{GpuHold, LockHolder};
use crate::settings::LeaseName;

/// Takes and reads the locks that leases name
pub trait LocalLeases: Send + Sync {
    /// Waits for `lease`, then holds it for `what` until the hold drops
    ///
    /// # Errors
    ///
    /// [`AgentError::Stopped`] when the runner stops first, and
    /// [`AgentError::Setup`] when the lock cannot be taken.
    fn hold(&self, lease: &LeaseName, what: &str) -> Result<GpuHold, AgentError>;

    /// Who holds `lease`, or `None` when it is free
    fn holder(&self, lease: &LeaseName) -> Option<LockHolder>;
}

/// Agents whose calls hold the lease each names for as long as they run
pub struct Leased {
    agents: Arc<dyn Agents>,
    leases: Arc<dyn LocalLeases>,
}

impl Leased {
    /// `agents`, holding each call's lease from `leases`
    pub fn new(agents: Arc<dyn Agents>, leases: Arc<dyn LocalLeases>) -> Self {
        Self { agents, leases }
    }
}

impl fmt::Debug for Leased {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Leased").finish_non_exhaustive()
    }
}

impl Agents for Leased {
    fn prepare(&self, call: &AgentCall) -> Result<(), AgentError> {
        self.agents.prepare(call)
    }

    fn run(&self, call: &AgentCall) -> Result<AgentReply, AgentError> {
        let _held = match &call.lease {
            Some(lease) => {
                let what = format!(
                    "kelpie {} call for #{} in {}",
                    call.role.as_str(),
                    call.issue,
                    call.cwd.display()
                );
                Some(self.leases.hold(lease, &what)?)
            }
            None => None,
        };
        self.agents.run(call)
    }
}
