//! The real ports: the system clock, `gh`, `curl` and the `claude` command line

use std::time::{SystemTime, UNIX_EPOCH};

use crate::ports::{Clock, Timestamp};

mod claude;
mod curl;
mod gh;
mod process;
mod usage;

pub use claude::ClaudeCli;
pub use curl::Curl;
pub use gh::Gh;
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
