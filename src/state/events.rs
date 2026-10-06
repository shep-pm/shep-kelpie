//! The board's events, which the project manager reads from its last wake on
//!
//! Each change to the board the runner saves becomes one line with an id
//! that only grows. The file keeps the newest [`EVENTS_KEPT`]. The project
//! manager's cursor names the last event it has read.

use serde::{Deserialize, Serialize};

use super::ProjectState;
use crate::ports::Timestamp;

/// How many board events the state file keeps
///
/// An event is under 200 bytes, so the list stays under about 10 KB of a
/// file written whole at every save.
pub const EVENTS_KEPT: usize = 50;

/// One change to the board
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardEvent {
    /// Its id, one past the event before
    pub id: u64,
    /// When the runner saw it
    pub at: Timestamp,
    /// What happened, as the board says it
    pub what: String,
}

impl ProjectState {
    /// Adds an event saying `what`, dropping the oldest past [`EVENTS_KEPT`]
    pub fn record_event(&mut self, at: Timestamp, what: String) {
        self.last_event += 1;
        self.events.push(BoardEvent {
            id: self.last_event,
            at,
            what,
        });
        let excess = self.events.len().saturating_sub(EVENTS_KEPT);
        self.events.drain(..excess);
    }

    /// The events after the project manager's cursor, or the newest `recent`
    /// while it has none, and whether older unread ones were dropped
    pub fn unread_events(&self, recent: usize) -> (&[BoardEvent], bool) {
        let Some(seen) = self.pm_seen else {
            let from = self.events.len().saturating_sub(recent);
            return (&self.events[from..], false);
        };
        let from = self.events.partition_point(|e| e.id <= seen);
        let dropped = self.events.first().is_some_and(|e| e.id > seen + 1);
        (&self.events[from..], dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_events(n: u64) -> ProjectState {
        let mut state = ProjectState::new(Timestamp(0));
        for i in 1..=n {
            state.record_event(Timestamp(i), format!("event {i}"));
        }
        state
    }

    fn ids(events: &[BoardEvent]) -> Vec<u64> {
        events.iter().map(|e| e.id).collect()
    }

    #[test]
    fn the_oldest_events_go_past_the_cap_and_ids_keep_growing() {
        let state = with_events(EVENTS_KEPT as u64 + 3);
        assert_eq!(state.events.len(), EVENTS_KEPT);
        assert_eq!(state.events[0].id, 4);
        assert_eq!(state.last_event, EVENTS_KEPT as u64 + 3);
    }

    #[test]
    fn with_no_cursor_the_newest_are_unread() {
        let state = with_events(5);
        let (events, dropped) = state.unread_events(2);
        assert_eq!((ids(events), dropped), (vec![4, 5], false));
    }

    #[test]
    fn the_cursor_leaves_the_events_after_it_and_says_when_some_were_dropped() {
        let mut state = with_events(5);
        state.pm_seen = Some(3);
        assert_eq!(ids(state.unread_events(2).0), [4, 5]);
        state.pm_seen = Some(5);
        assert_eq!(state.unread_events(2), (&[][..], false));

        let mut state = with_events(EVENTS_KEPT as u64 + 3);
        state.pm_seen = Some(1);
        let (events, dropped) = state.unread_events(2);
        assert_eq!((events.len(), dropped), (EVENTS_KEPT, true));
    }

    #[test]
    fn an_event_is_pinned() {
        let event = BoardEvent {
            id: 3,
            at: Timestamp(9),
            what: "#7: phase implement to review".into(),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({ "id": 3, "at": 9, "what": "#7: phase implement to review" })
        );
    }
}
