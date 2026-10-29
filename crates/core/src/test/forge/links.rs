//! The fake forge taking blocked-by links, which kelpie adds to an issue

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use super::FakeForge;
use crate::ports::ForgeError;

/// The links kelpie added, and whether adding one is refused
#[derive(Debug, Default)]
pub struct Links {
    added: Mutex<Vec<(u64, u64)>>,
    refused: AtomicBool,
}

impl FakeForge {
    /// Each link kelpie added, as the issue and the blocker it names, in order
    pub fn links_added(&self) -> Vec<(u64, u64)> {
        self.links.added.lock().unwrap().clone()
    }

    /// Makes adding a blocked-by link fail, or work again
    pub fn set_links_refused(&self, refused: bool) {
        self.links.refused.store(refused, Ordering::SeqCst);
    }

    /// Links `number` as blocked by `blocker`, which the issue and the board then show,
    /// unless `blocker` is a pull request
    pub fn link(&self, number: u64, blocker: u64) -> Result<bool, ForgeError> {
        self.ask()?;
        if self.links.refused.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("linking is refused".to_owned()));
        }
        if self.opened(blocker).is_ok() {
            return Ok(false);
        }
        self.wrote(format!("blocked #{number} by #{blocker}"));
        self.links.added.lock().unwrap().push((number, blocker));
        self.block(number, blocker);
        Ok(true)
    }
}
