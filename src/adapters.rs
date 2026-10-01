//! The real ports: the system clock, `gh`, `curl`, the `claude` command
//! line, its `/usage`, the relay session, the local review
//! round, kelpie's shots, the machine's sandbox programs, and the dog's
//! leases over the shepherd channel

use std::time::{SystemTime, UNIX_EPOCH};

use crate::ports::{Clock, Timestamp};

mod claude;
mod curl;
pub(crate) mod gh;
mod host;
mod leases;
mod local;
mod ntfy;
mod orphans;
mod process;
mod relay;
mod shots;
mod srt;
mod usage;

#[cfg(test)]
pub(crate) use claude::settings::{NO_TOOLS, settings as claude_settings};
#[cfg(test)]
pub(crate) use claude::write_settings as write_claude_settings;
pub use claude::{ClaudeCli, LambLabels};
pub use curl::Curl;
pub use gh::Gh;
pub use host::SystemHost;
pub use leases::ShepLeases;
pub use local::LocalReviewer;
pub use relay::RelayCli;
pub use shots::ShotsCli;
pub use srt::SandboxRuntime;
pub use usage::UsageMeter;

/// The machine's wall clock
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Timestamp(since_epoch.as_secs())
    }
}
