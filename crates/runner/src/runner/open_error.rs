//! Why a runner could not start

use std::fmt;

use crate::agents::AgentsError;
use crate::ports::ForgeError;
use crate::settings::SettingsError;
use crate::state::StateError;

/// Why a runner could not start
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// A setting is missing, malformed or does not hold
    Settings(SettingsError),
    /// An agent file cannot be read or used
    Agents(AgentsError),
    /// The forge could not be asked about the project's repo
    Forge(ForgeError),
    /// The state file could not be read
    State(StateError),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Settings(e) => e.fmt(f),
            Self::Agents(e) => e.fmt(f),
            Self::Forge(e) => write!(f, "cannot check the repo on the forge: {e}"),
            Self::State(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for OpenError {}

impl From<SettingsError> for OpenError {
    fn from(e: SettingsError) -> Self {
        Self::Settings(e)
    }
}

impl From<AgentsError> for OpenError {
    fn from(e: AgentsError) -> Self {
        Self::Agents(e)
    }
}

impl From<ForgeError> for OpenError {
    fn from(e: ForgeError) -> Self {
        Self::Forge(e)
    }
}

impl From<StateError> for OpenError {
    fn from(e: StateError) -> Self {
        Self::State(e)
    }
}
