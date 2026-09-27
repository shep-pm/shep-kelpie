//! The real ports: the system clock, `gh` and the `claude` command line

use std::time::{SystemTime, UNIX_EPOCH};

use crate::ports::{Clock, Timestamp};

mod claude;
mod gh;
mod process;

pub use claude::ClaudeCli;
pub use gh::Gh;

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
