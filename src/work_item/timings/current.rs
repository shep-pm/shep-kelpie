//! The phase a work item's time counts in now

use super::{CallKind, Split, TimingPhase, Timings};
use crate::ports::Timestamp;
use crate::state::RunState;
use crate::work_item::{CodeRabbitStage, Phase, ReviewCallState, Turn, WorkItem};

impl WorkItem {
    /// Marks a review call, or a shots run, in flight outside the runner's lock
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
    /// A running turn or call comes first. Then a paused project, since
    /// nothing steps it, then a ruling, then the gate the work item waits at.
    pub fn timing_phase(&self, run: RunState, live: bool) -> TimingPhase {
        if live && matches!(self.turn, Turn::Running { .. }) {
            return TimingPhase::Worker;
        }
        if let (ReviewCallState::Running { .. }, Some(call)) =
            (self.review_call, self.timings.as_ref().and_then(|t| t.call))
        {
            let queued = self.timings.as_ref().is_some_and(|t| t.queued);
            return match call {
                CallKind::Local if queued => TimingPhase::GpuWait,
                CallKind::Local => TimingPhase::LocalRound,
                CallKind::Claude => TimingPhase::ClaudeRound,
                CallKind::Shots => TimingPhase::Shots,
                CallKind::Deep => TimingPhase::DeepRound,
            };
        }
        if run == RunState::Paused {
            return TimingPhase::Paused;
        }
        match &self.phase {
            Phase::Ruling { .. } => TimingPhase::Ruling,
            Phase::Implement | Phase::Review(_) => TimingPhase::Other,
            Phase::Ci { .. } => TimingPhase::Ci,
            Phase::CodeRabbit(CodeRabbitStage::Lease { .. }) => TimingPhase::CodeRabbitWindow,
            Phase::CodeRabbit(CodeRabbitStage::Summoned { .. }) => TimingPhase::CodeRabbitReview,
            Phase::CodeRabbit(_) => TimingPhase::Other,
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
    use crate::work_item::{Review, ReviewStage};

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
    fn a_live_turn_is_the_workers_whatever_the_project_does() {
        let item = a_work_item();
        assert!(matches!(item.turn, Turn::Running { .. }));
        for run in [RunState::Running, RunState::Paused] {
            assert_eq!(item.timing_phase(run, true), TimingPhase::Worker);
        }
        let ruling = WorkItem {
            phase: Phase::Ruling { id: 1 },
            ..a_work_item()
        };
        assert_eq!(
            ruling.timing_phase(RunState::Running, true),
            TimingPhase::Worker
        );
    }

    #[test]
    fn a_turn_no_process_runs_is_not_the_workers() {
        let item = a_work_item();
        assert_eq!(
            item.timing_phase(RunState::Paused, false),
            TimingPhase::Paused
        );
        assert_eq!(item.timing_phase(RunState::Running, false), TimingPhase::Ci);
    }

    #[test]
    fn a_call_in_flight_is_named_by_its_kind_and_a_queued_round_is_a_gpu_wait() {
        let run = RunState::Running;
        let phase = |kind, queued| running_call(kind, queued).timing_phase(run, false);
        assert_eq!(phase(CallKind::Local, false), TimingPhase::LocalRound);
        assert_eq!(phase(CallKind::Local, true), TimingPhase::GpuWait);
        assert_eq!(phase(CallKind::Claude, false), TimingPhase::ClaudeRound);
        assert_eq!(phase(CallKind::Deep, false), TimingPhase::DeepRound);
        assert_eq!(phase(CallKind::Shots, false), TimingPhase::Shots);
        assert_eq!(
            running_call(CallKind::Local, false).timing_phase(RunState::Paused, false),
            TimingPhase::LocalRound,
            "a call that is running is not paused"
        );
    }

    #[test]
    fn a_stale_call_kind_charges_nothing_once_the_call_ended() {
        let mut item = running_call(CallKind::Local, true);
        item.call_ended();
        let timings = item.timings.as_mut().unwrap();
        assert_eq!(timings.call, None);
        assert!(!timings.queued);
        assert_eq!(
            item.timing_phase(RunState::Running, false),
            TimingPhase::Other
        );
        item.timings.as_mut().unwrap().call = Some(CallKind::Claude);
        assert_eq!(
            item.timing_phase(RunState::Running, false),
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
        let lease = Phase::CodeRabbit(CodeRabbitStage::Lease {
            head: "c0ffee".into(),
            readied: None,
            full: false,
        });
        let summoned = Phase::CodeRabbit(CodeRabbitStage::Summoned {
            bot: Bot::Coderabbit,
            head: "c0ffee".into(),
            at: Timestamp(1),
            full: false,
            resent: false,
        });
        let fixing = Phase::CodeRabbit(CodeRabbitStage::Fixing {
            head: "c0ffee".into(),
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
        let run = RunState::Running;
        let of = |phase| in_phase(phase).timing_phase(run, false);
        assert_eq!(of(ci), TimingPhase::Ci);
        assert_eq!(of(lease), TimingPhase::CodeRabbitWindow);
        assert_eq!(of(summoned), TimingPhase::CodeRabbitReview);
        assert_eq!(of(Phase::Ruling { id: 1 }), TimingPhase::Ruling);
        assert_eq!(of(merge), TimingPhase::Merge);
        assert_eq!(of(Phase::Done { merged: true }), TimingPhase::Merge);
        for between_steps in [Phase::Implement, review, fixing] {
            assert_eq!(of(between_steps), TimingPhase::Other);
        }
    }

    #[test]
    fn a_paused_project_reads_paused_whatever_the_work_item_waits_for() {
        let paused = RunState::Paused;
        let ci = Phase::Ci {
            head: None,
            since: Timestamp(1),
        };
        for waiting in [ci, Phase::Implement, Phase::Ruling { id: 1 }] {
            assert_eq!(
                in_phase(waiting).timing_phase(paused, false),
                TimingPhase::Paused
            );
        }
    }
}
