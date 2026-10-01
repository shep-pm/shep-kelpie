// A sandbox that wraps nothing, for tests of what an adapter does around it.
// It keeps each policy it was handed, so a test can read what the call asked for.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use crate::ports::{Policy, Sandbox, SandboxError};

#[derive(Debug, Clone, Default)]
pub(crate) struct OpenSandbox(Arc<Mutex<Vec<(Policy, PathBuf)>>>);

impl OpenSandbox {
    /// Each policy handed in, with the settings file it was to be written to
    pub(crate) fn wrapped(&self) -> Vec<(Policy, PathBuf)> {
        self.0.lock().unwrap().clone()
    }
}

impl Sandbox for OpenSandbox {
    fn wrap(
        &self,
        policy: &Policy,
        settings: &Path,
        command: &Command,
    ) -> Result<Command, SandboxError> {
        self.0
            .lock()
            .unwrap()
            .push((policy.clone(), settings.to_owned()));
        let mut copy = Command::new(command.get_program());
        copy.args(command.get_args());
        if let Some(dir) = command.get_current_dir() {
            copy.current_dir(dir);
        }
        for (name, value) in command.get_envs() {
            match value {
                Some(value) => copy.env(name, value),
                None => copy.env_remove(name),
            };
        }
        Ok(copy)
    }
}
