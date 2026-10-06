//! Letting go of the calls in flight as the runner stops

use super::{ClaudeCli, LocalReviewer};

/// Refuses new calls on `claude` and `reviewer`, sends each call in flight
/// SIGTERM, and leaves the rest to shep's stop, which ends every lamb once
/// the runner exits
pub fn stop_calls(claude: &ClaudeCli, reviewer: &LocalReviewer) {
    claude.stop();
    reviewer.stop();
}
