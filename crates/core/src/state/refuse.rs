//! A store whose saves stop working after a count, for tests of a step that fails partway

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{StateError, StateStore};

impl StateStore {
    /// This store, whose saves from the `saves`th on fail, counted across its clones
    #[must_use]
    pub fn refusing_after(mut self, saves: usize) -> Self {
        self.saves_left = Some(Arc::new(AtomicUsize::new(saves)));
        self
    }

    pub(super) fn refuse_when_spent(&self) -> Result<(), StateError> {
        let Some(left) = &self.saves_left else {
            return Ok(());
        };
        let spent = left.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
        spent.map(drop).map_err(|_| StateError::Write {
            path: self.path.clone(),
            kind: std::io::ErrorKind::PermissionDenied,
        })
    }
}
