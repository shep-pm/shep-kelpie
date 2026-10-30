//! A store whose saves stop working after a count, for tests of a step that fails partway

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{StateError, StateStore};

impl StateStore {
    /// This store, which takes `saves` saves and refuses every one after
    ///
    /// Every call to `save` counts, whether or not it would have written. The
    /// clones of the store share the count.
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

#[cfg(test)]
mod tests {
    use super::super::{ProjectState, StateError, StateStore};

    fn store_in(dir: &std::path::Path) -> StateStore {
        StateStore::new(dir.join("state.json"))
    }

    #[test]
    fn saves_after_the_count_are_refused_and_the_state_stays() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path()).refusing_after(1);
        store.save(&ProjectState::new()).unwrap();
        let before = std::fs::read_to_string(dir.path().join("state.json")).unwrap();
        let mut next = ProjectState::new();
        next.pm_seen = Some(7);

        let err = store.save(&next).unwrap_err();

        assert!(matches!(err, StateError::Write { .. }), "{err}");
        let after = std::fs::read_to_string(dir.path().join("state.json")).unwrap();
        assert_eq!(after, before);
    }

    #[test]
    fn a_clone_spends_the_same_saves() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path()).refusing_after(1);
        store.clone().save(&ProjectState::new()).unwrap();

        assert!(store.save(&ProjectState::new()).is_err());
    }

    #[test]
    fn a_store_not_made_refusing_always_saves() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());

        for _ in 0..3 {
            store.save(&ProjectState::new()).unwrap();
        }
    }
}
