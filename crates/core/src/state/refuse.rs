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
        let mut seen = left.load(Ordering::SeqCst);
        loop {
            if seen == 0 {
                return Err(StateError::Write {
                    path: self.path.clone(),
                    kind: std::io::ErrorKind::PermissionDenied,
                });
            }
            match left.compare_exchange(seen, seen - 1, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return Ok(()),
                Err(now) => seen = now,
            }
        }
    }
}
