//! The real ports: the system clock, `gh`, the `claude` command line, its
//! `/usage`, and the maintainer's qwen-review script

use std::time::{SystemTime, UNIX_EPOCH};

use crate::ports::{Clock, Timestamp};

mod claude;
mod gh;
mod process;
mod qwen;
mod usage;

pub use claude::ClaudeCli;
pub use gh::Gh;
pub use qwen::QwenReviewer;
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
