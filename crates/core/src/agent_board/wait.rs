//! How long an agent waits before it can start a turn

use crate::pacer::Hold;
use crate::ports::{ModelLoad, Timestamp};

/// What says when an agent can start a turn
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability<'a> {
    /// A gateway model's load, or None when the gateway could not be read
    Gateway(Option<ModelLoad>),
    /// The pacer's hold on a plan account, or None when it holds nothing
    Plan(Option<&'a Hold>),
    /// Neither: it starts at once
    Open,
}

/// The seconds `availability` keeps an agent from starting, or None when it has no window
///
/// A gateway model serves the turns ahead of this one `slots` at a time,
/// each taking `turn_secs`. A model that lists no slots has no turn limit.
pub fn wait(availability: Availability<'_>, turn_secs: u64, now: Timestamp) -> Option<u64> {
    match availability {
        Availability::Gateway(None) => None,
        Availability::Gateway(Some(load)) if load.slots == 0 => Some(0),
        Availability::Gateway(Some(load)) => {
            let ahead = (load.held + load.waiting + 1).saturating_sub(load.slots);
            Some(ahead.div_ceil(load.slots).saturating_mul(turn_secs))
        }
        Availability::Plan(hold) => Some(hold.map_or(0, |h| h.until.0.saturating_sub(now.0))),
        Availability::Open => Some(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pacer::HoldKind;

    fn load(slots: u64, held: u64, waiting: u64) -> Availability<'static> {
        Availability::Gateway(Some(ModelLoad {
            slots,
            held,
            waiting,
        }))
    }

    #[test]
    fn a_free_slot_waits_nothing() {
        assert_eq!(wait(load(1, 0, 0), 600, Timestamp(0)), Some(0));
        assert_eq!(wait(load(2, 1, 0), 600, Timestamp(0)), Some(0));
    }

    #[test]
    fn one_turn_ahead_on_one_slot_waits_one_turn() {
        assert_eq!(wait(load(1, 1, 0), 600, Timestamp(0)), Some(600));
        assert_eq!(wait(load(1, 1, 1), 600, Timestamp(0)), Some(1200));
    }

    #[test]
    fn two_slots_serve_the_turns_ahead_two_at_a_time() {
        assert_eq!(wait(load(2, 2, 0), 600, Timestamp(0)), Some(600));
        assert_eq!(wait(load(2, 2, 1), 600, Timestamp(0)), Some(600));
        assert_eq!(wait(load(2, 2, 2), 600, Timestamp(0)), Some(1200));
    }

    #[test]
    fn a_model_with_no_slots_listed_waits_nothing() {
        assert_eq!(wait(load(0, 3, 0), 600, Timestamp(0)), Some(0));
    }

    #[test]
    fn an_unread_load_has_no_window() {
        assert_eq!(wait(Availability::Gateway(None), 600, Timestamp(0)), None);
    }

    #[test]
    fn a_held_plan_waits_until_the_hold_ends() {
        let hold = Hold {
            kind: HoldKind::Window,
            reason: "the 5-hour window is past half".into(),
            until: Timestamp(1900),
        };
        let held = Availability::Plan(Some(&hold));
        assert_eq!(wait(held, 600, Timestamp(100)), Some(1800));
        assert_eq!(
            wait(held, 600, Timestamp(2000)),
            Some(0),
            "a hold past its end"
        );
        assert_eq!(wait(Availability::Plan(None), 600, Timestamp(100)), Some(0));
    }

    #[test]
    fn an_agent_with_neither_waits_nothing() {
        assert_eq!(wait(Availability::Open, 600, Timestamp(5)), Some(0));
    }
}
