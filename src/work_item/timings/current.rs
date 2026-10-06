//! The phase a work item's time counts in now

use super::{CallKind, Split, TimingPhase, Timings};
use crate::ports::Timestamp;
use crate::work_item::{Phase, Review, ReviewCallState, ReviewStage, Turn, WorkItem};

impl WorkItem {
    /// Marks a review call in flight outside the runner's lock
    pub fn call_started(&mut self, kind: CallKind, since: Timestamp) {
        self.review_call = ReviewCallState::Running { since };
        if let Some(timings) = &mut self.timings {
            timings.call = Some(kind);
            timings.queued = false;
        }
    }

    /// Marks no call in flight
    pub fn call_ended(&mut self) {
        self.review_call = ReviewCallState::Idle;
        if let Some(timings) = &mut self.timings {
            timings.call = None;
            timings.queued = false;
        }
    }

    /// The phase its time counts in while its state is as it is now
    ///
    /// `live` says whether this process runs its turn: a turn marked running
    /// that nothing runs, as after a restart or a stop, is not the worker's.
    /// A running turn or call comes first, then a ruling, then the gate the
    /// work item waits at.
    pub fn timing_phase(&self, live: bool) -> TimingPhase {
        if live && matches!(self.turn, Turn::Running { .. }) {
            return TimingPhase::Worker;
        }
        let call = self.timings.as_ref().and_then(|t| t.call);
        if matches!(self.review_call, ReviewCallState::Running { .. }) && call.is_some() {
            return TimingPhase::Review;
        }
        match &self.phase {
            Phase::Ruling { .. } => TimingPhase::Ruling,
            Phase::Review(Review {
                stage: ReviewStage::Summon { .. } | ReviewStage::Summoned { .. },
                ..
            }) => TimingPhase::Review,
            Phase::Implement | Phase::Review(_) => TimingPhase::Other,
            Phase::Ci { .. } => TimingPhase::Ci,
            Phase::Merge { .. } | Phase::Done { .. } => TimingPhase::Merge,
        }
    }

    /// Its time at `now`, with the time since its last charge in `phase`
    ///
    /// A work item with no timings has counted nothing yet.
    pub fn split(&self, now: Timestamp, phase: TimingPhase) -> Split {
        match &self.timings {
            Some(timings) => timings.split(now, phase),
            None => Timings::starting(now).split(now, phase),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_bot::Bot;
    use crate::test::a_work_item;

    fn in_phase(phase: Phase) -> WorkItem {
        let mut item = a_work_item();
        item.turn = Turn::Ended { at: Timestamp(1) };
        item.phase = phase;
        item
    }

    fn running_call(kind: CallKind, queued: bool) -> WorkItem {
        let mut item = in_phase(Phase::Implement);
        item.call_started(kind, Timestamp(2));
        item.timings.as_mut().unwrap().queued = queued;
        item
    }

    #[test]
    fn a_live_turn_is_the_workers_whatever_the_work_item_waits_for() {
        let item = a_work_item();
        assert!(matches!(item.turn, Turn::Running { .. }));
        assert_eq!(item.timing_phase(true), TimingPhase::Worker);
        let ruling = WorkItem {
            phase: Phase::Ruling { id: 1 },
            ..a_work_item()
        };
        assert_eq!(ruling.timing_phase(true), TimingPhase::Worker);
    }

    #[test]
    fn a_turn_no_process_runs_is_not_the_workers() {
        let item = a_work_item();
        assert_eq!(item.timing_phase(false), TimingPhase::Ci);
    }

    #[test]
    fn a_call_in_flight_is_a_review_queued_or_not() {
        let phase = |kind, queued| running_call(kind, queued).timing_phase(false);
        assert_eq!(phase(CallKind::Local, false), TimingPhase::Review);
        assert_eq!(phase(CallKind::Local, true), TimingPhase::Review);
        assert_eq!(phase(CallKind::Claude, false), TimingPhase::Review);
    }

    #[test]
    fn a_stale_call_kind_charges_nothing_once_the_call_ended() {
        let mut item = running_call(CallKind::Local, true);
        item.call_ended();
        let timings = item.timings.as_mut().unwrap();
        assert_eq!(timings.call, None);
        assert!(!timings.queued);
        assert_eq!(item.timing_phase(false), TimingPhase::Other);
        item.timings.as_mut().unwrap().call = Some(CallKind::Claude);
        assert_eq!(
            item.timing_phase(false),
            TimingPhase::Other,
            "review_call is idle, so the kind is ignored"
        );
    }

    #[test]
    fn a_waiting_work_item_is_named_by_its_gate() {
        let ci = Phase::Ci {
            head: None,
            since: Timestamp(1),
        };
        let bot = |stage| {
            Phase::Review(Review {
                stage,
                ..Review::first()
            })
        };
        let lease = bot(ReviewStage::Summon {
            bot: Bot::Coderabbit,
            started: Timestamp(1),
            head: "c0ffee".into(),
            readied: None,
            full: false,
        });
        let summoned = bot(ReviewStage::Summoned {
            bot: Bot::Coderabbit,
            started: Timestamp(1),
            head: "c0ffee".into(),
            at: Timestamp(1),
            full: false,
            resent: false,
        });
        let fixing = bot(ReviewStage::Fixing {
            head: Some("c0ffee".into()),
            sent: Vec::new(),
            deferred_before: Vec::new(),
        });
        let merge = Phase::Merge {
            head: "c0ffee".into(),
            readied: None,
            auto: false,
        };
        let review = Phase::Review(Review {
            stage: ReviewStage::Round,
            ..Review::first()
        });
        let of = |phase| in_phase(phase).timing_phase(false);
        assert_eq!(of(ci), TimingPhase::Ci);
        assert_eq!(of(lease), TimingPhase::Review);
        assert_eq!(of(summoned), TimingPhase::Review);
        assert_eq!(of(Phase::Ruling { id: 1 }), TimingPhase::Ruling);
        assert_eq!(of(merge), TimingPhase::Merge);
        assert_eq!(of(Phase::Done { merged: true }), TimingPhase::Merge);
        for between_steps in [Phase::Implement, review, fixing] {
            assert_eq!(of(between_steps), TimingPhase::Other);
        }
    }
}
