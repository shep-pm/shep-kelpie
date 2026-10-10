//! The fake forge going down, or its rate limit used up, and the count of
//! calls made of it

use std::sync::atomic::Ordering;

use super::FakeForge;
use crate::ports::{ForgeError, Timestamp};

/// What every call fails with while the forge is down, and when its rate
/// limit resets where it can say
pub(super) type Down = (String, Option<Timestamp>);

impl FakeForge {
    /// How many calls the runner made, each counted as it was asked
    pub(crate) fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }

    /// How many times the board's ready issues were read
    pub(crate) fn board_reads(&self) -> usize {
        self.board_reads.load(Ordering::SeqCst)
    }

    /// Makes every call fail as gh does once the rate limit is used up,
    /// with `reset` as when it resets where the forge can say
    pub(crate) fn set_used_up(&self, reset: Option<Timestamp>) {
        let said = "GraphQL: API rate limit already exceeded for user ID 1.";
        *self.down.lock().unwrap() = Some((said.to_owned(), reset));
    }

    /// Makes every call fail as gh does with no network, or lets every call through again
    pub(crate) fn set_down(&self, down: bool) {
        let said = "error connecting to api.github.com";
        *self.down.lock().unwrap() = down.then(|| (said.to_owned(), None));
    }

    /// Counts a call, and fails it while the forge is down
    pub(super) fn ask(&self) -> Result<(), ForgeError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        match &*self.down.lock().unwrap() {
            Some((said, _)) => Err(ForgeError::Failed(said.clone())),
            None => Ok(()),
        }
    }

    /// When the used-up rate limit resets, where the forge can say
    pub(super) fn reset(&self) -> Option<Timestamp> {
        let down = self.down.lock().unwrap();
        down.as_ref().and_then(|(_, reset)| *reset)
    }
}
