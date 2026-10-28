//! What every harness shares: the runner's calls in flight, lamb labels and the sandbox

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::LambLabels;
use crate::adapters::process::{Processes, Sealed};
use crate::adapters::srt::SandboxRuntime;
use crate::ports::{Sandbox, Unreadable};

/// The process and sandbox base each harness adapter is made on
///
/// Clones share their calls in flight, so one clone can stop them all. An
/// adapter made on a base shares its sandbox, labels and calls, and is
/// stopped with it.
#[derive(Debug, Clone)]
pub struct Base {
    pub(in crate::adapters) processes: Processes,
    pub(crate) lambs: Option<Arc<dyn LambLabels>>,
    pub(crate) sandbox: Arc<dyn Sandbox>,
    pub(crate) home: PathBuf,
    // What a call with no fence may not read either: the shepherd's home
    pub(crate) unfenced_no_read: Vec<String>,
}

// The default home is the maintainer's: `$HOME`.
impl Default for Base {
    fn default() -> Self {
        let home = crate::home::user_home().unwrap_or_default();
        Self {
            processes: Processes::default(),
            lambs: None,
            sandbox: Arc::new(SandboxRuntime::new()),
            home,
            unfenced_no_read: Vec::new(),
        }
    }
}

impl Base {
    /// Labels each call's process with its issue and role, such as `#7 worker`
    pub fn labelling(lambs: Arc<dyn LambLabels>) -> Self {
        Self {
            lambs: Some(lambs),
            ..Self::default()
        }
    }

    /// Runs each call inside the sandbox runtime, with the harness's home at
    /// `home`, the way a runner does
    ///
    /// No call on any harness reads kelpie's Codex login in `codex_home`,
    /// apart from Codex's own link to its `auth.json`, or sees the variables
    /// in `secret_vars`.
    pub fn in_runtime(self, home: PathBuf, codex_home: &Path, secret_vars: Vec<String>) -> Self {
        let runtime = Arc::new(SandboxRuntime::new());
        let unread = vec![format!("{}/**", codex_home.display())];
        let sandbox = Unreadable::new(runtime, unread).unsetting(secret_vars);
        self.sandboxed(Arc::new(sandbox), home)
    }

    /// Keeps a call with no fence, such as a reviewer's session, out of the
    /// home folder and `shep_home`, which hold the sheep entries' `env`,
    /// `dogs.toml` and everything else of the machine the call works no part of
    #[must_use]
    pub fn unfenced_unread(mut self, home: &Path, shep_home: &Path) -> Self {
        self.unfenced_no_read = crate::profile::home_denials([home, shep_home]);
        self
    }

    /// Runs each call inside `sandbox`, with the harness's home at `home`
    #[must_use]
    pub fn sandboxed(self, sandbox: Arc<dyn Sandbox>, home: PathBuf) -> Self {
        Self {
            sandbox,
            home,
            ..self
        }
    }

    /// Refuses new calls as the runner stops, sends those in flight SIGTERM,
    /// and leaves the rest to shep's stop
    ///
    /// A call refused, or one that ends after, returns [`AgentError::Stopped`](crate::ports::AgentError::Stopped).
    pub fn stop(&self) {
        self.processes.stop();
    }

    /// Stops this runner's calls from ending, so a handover can carry them,
    /// or `None` when one is just ending
    pub fn seal(&self) -> Option<Sealed> {
        self.processes.seal()
    }
}
