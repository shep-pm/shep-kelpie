//! Where a work item's wall time went, by phase
//!
//! Time is charged when the state is saved, to the phase the work item was
//! in before the save, so the phases always sum to the wall time. The phase
//! is read off the work item's own state by [`WorkItem::timing_phase`].

use serde::{Deserialize, Serialize};

use crate::ports::Timestamp;

mod current;
mod phase;
mod seconds;

pub use phase::TimingPhase;
pub use seconds::{Seconds, saved};

#[cfg(doc)]
use super::WorkItem;

/// What a call running outside the runner's lock is doing
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
    /// A local review round
    Local,
    /// A Claude review round
    Claude,
    /// The judge, on a review round's findings or CodeRabbit's threads
    Judge,
    /// A shots run
    Shots,
    /// A session of a deep review round
    Deep,
}

/// A work item's time so far
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timings {
    /// When the work item was created, or first loaded by a kelpie that
    /// keeps timings
    pub created: Timestamp,
    /// The instant `seconds` counts up to
    pub since: Timestamp,
    /// The seconds charged so far
    #[serde(serialize_with = "saved")]
    pub seconds: Seconds,
    /// The call in flight outside the runner's lock, while one is
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<CallKind>,
    /// Whether that call is a local round still queued for the GPU
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub queued: bool,
}

/// A work item's time up to an instant, with the open interval included
// wire format: changing this is a breaking change to `status`
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Split {
    /// The phase the open interval counts in
    pub phase: TimingPhase,
    /// Seconds since the work item was created
    pub wall: u64,
    /// Every phase, summing to `wall`
    pub seconds: Seconds,
}

impl Timings {
    /// A work item created at `now`
    pub fn starting(now: Timestamp) -> Self {
        Self {
            created: now,
            since: now,
            seconds: Seconds::default(),
            call: None,
            queued: false,
        }
    }

    /// Charges the time since `since` to `phase`, and moves `since` to `now`
    ///
    /// A `now` before `since` charges nothing and leaves `since` alone.
    pub fn charge(&mut self, now: Timestamp, phase: TimingPhase) {
        self.seconds.add(phase, now.0.saturating_sub(self.since.0));
        self.since = self.since.max(now);
    }

    /// The split at `now`, with the time since `since` in `phase`
    pub fn split(&self, now: Timestamp, phase: TimingPhase) -> Split {
        let mut counted = self.clone();
        counted.charge(now, phase);
        Split {
            phase,
            wall: counted.since.0.saturating_sub(self.created.0),
            seconds: counted.seconds,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test::a_work_item;
    use crate::work_item::WorkItem;

    #[test]
    fn a_split_sums_to_its_wall_time() {
        let mut timings = Timings::starting(Timestamp(100));
        timings.charge(Timestamp(130), TimingPhase::Worker);
        timings.charge(Timestamp(150), TimingPhase::Ci);
        let read = timings.split(Timestamp(200), TimingPhase::Ruling);
        assert_eq!(read.wall, 100);
        assert_eq!(read.seconds.total(), 100);
        assert_eq!(read.seconds.get(TimingPhase::Worker), 30);
        assert_eq!(read.seconds.get(TimingPhase::Ci), 20);
        assert_eq!(read.seconds.get(TimingPhase::Ruling), 50);
        assert_eq!(timings.since, Timestamp(150), "a read charges nothing");
    }

    #[test]
    fn a_clock_that_steps_back_charges_nothing() {
        let mut timings = Timings::starting(Timestamp(100));
        timings.charge(Timestamp(150), TimingPhase::Worker);
        timings.charge(Timestamp(120), TimingPhase::Ci);
        assert_eq!(timings.since, Timestamp(150));
        let read = timings.split(Timestamp(110), TimingPhase::Other);
        assert_eq!(read.seconds.total(), read.wall);
    }

    #[test]
    fn a_work_item_saved_without_timings_has_none() {
        let mut value = serde_json::to_value(a_work_item()).unwrap();
        value.as_object_mut().unwrap().remove("timings");
        let item: WorkItem = serde_json::from_value(value).unwrap();
        assert_eq!(item.timings, None);
        let again = serde_json::to_value(&item).unwrap();
        assert!(again.get("timings").is_none());
    }

    #[test]
    fn a_call_in_flight_is_pinned_with_its_kind_and_queue() {
        let mut item = a_work_item();
        let timings = item.timings.as_mut().unwrap();
        timings.call = Some(CallKind::Local);
        timings.queued = true;
        let value = serde_json::to_value(&item).unwrap();
        assert_eq!(value["timings"]["call"], "local");
        assert_eq!(value["timings"]["queued"], true);
        let back: WorkItem = serde_json::from_value(value).unwrap();
        assert_eq!(back.timings, item.timings);
        let kinds = [
            CallKind::Local,
            CallKind::Claude,
            CallKind::Judge,
            CallKind::Shots,
            CallKind::Deep,
        ];
        let names = kinds.map(|k| serde_json::to_value(k).unwrap());
        assert_eq!(
            names,
            [
                json!("local"),
                json!("claude"),
                json!("judge"),
                json!("shots"),
                json!("deep")
            ]
        );
    }
}
