//! Pacing: each account's usage is read before a call on it starts
//!
//! A new work item is paced on every account its roles spend, its turns on
//! the worker's, and a review round on its reviewer's. Each account keeps
//! its own day and its own holds. An agent limited by a lease is never paced.
//!
//! A reading that finds a hold is trusted for [`RECHECK_SECS`] or until the
//! hold ends, whichever comes first, so a project held for hours reads
//! usage a few times an hour rather than on every look at the board. A
//! reading that finds no hold is never reused.
//!
//! With `pacing.enabled` off the reading still runs, so `status` shows the
//! numbers, but neither limit holds anything.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::Runner;
use super::report::{Begin, StepReport};
use crate::lease::gpu::LockHolder;
use crate::pacer::{Assessment, Hold, RECHECK_SECS, Reading, Scope, assess, named};
use crate::ports::Timestamp;
use crate::settings::{Account, Limit, Runs};
use crate::state::StateError;

/// What `status` shows of the pacer
#[derive(Debug, Serialize)]
pub struct PacerStatus<'a> {
    /// Whether the limits hold anything, from `pacing.enabled`
    pub enabled: bool,
    /// Each account the worker spends or a reading was taken of
    #[serde(flatten)]
    pub accounts: BTreeMap<Account, AccountStatus<'a>>,
}

/// What `status` shows of one account's usage
#[derive(Debug, Serialize)]
pub struct AccountStatus<'a> {
    /// The last time its usage was read, and what it makes of it
    pub reading: Option<&'a Reading>,
    /// Why nothing new is starting on it, while a hold lasts
    pub holding: Option<&'a Hold>,
}

pub(super) enum Pace {
    Clear,
    /// A fresh reading found this hold
    Held(Hold),
    /// A hold from an earlier reading still stands
    StillHeld,
}

impl Pace {
    /// What the runner does instead of starting, or `None` to start
    pub(super) fn holds(self) -> Option<Begin> {
        match self {
            Self::Clear => None,
            Self::StillHeld => Some(Begin::Idle),
            Self::Held(Hold {
                kind,
                reason,
                until,
            }) => Some(Begin::Report(StepReport::Held {
                kind,
                reason,
                until,
            })),
        }
    }
}

impl Runner {
    /// Paces `scope` on the worker's own limit
    pub(super) fn pace_worker(&mut self, scope: Scope) -> Result<Pace, StateError> {
        let limit = self.agents.limits.worker.clone();
        self.pace(scope, &limit)
    }

    /// Paces `scope` on `limit`: an account's windows, or nothing for a lease
    pub(super) fn pace(&mut self, scope: Scope, limit: &Limit) -> Result<Pace, StateError> {
        let Limit::Account(account) = *limit else {
            return Ok(Pace::Clear);
        };
        let now = self.ports.clock.now();
        if let Some((read_at, last)) = self.pacing.get(&account) {
            let trusted = now.0 < read_at.0 + RECHECK_SECS;
            if trusted && last.hold(scope).is_some_and(|hold| now < hold.until) {
                return Ok(Pace::StillHeld);
            }
        }
        let prior = self.state.day_start(account);
        let mut assessment = match self.ports.meter_of(account).read(now) {
            Ok(usage) => assess(
                account,
                now,
                &usage,
                prior,
                self.settings.pacing.kickoff_hours,
            ),
            Err(e) => {
                let whose = named(account, "", "Codex ");
                Assessment::unreadable(now, format!("cannot read {whose}usage: {e}"))
            }
        };
        if assessment.day_start.is_some() && assessment.day_start != prior {
            let mut next = self.state.clone();
            *next.day_start_mut(account) = assessment.day_start;
            self.save(next)?;
        }
        if !self.settings.pacing.enabled {
            assessment.allowance = None;
            assessment.window = None;
        }
        let hold = assessment.hold(scope).cloned();
        self.pacing.insert(account, (now, assessment));
        Ok(hold.map_or(Pace::Clear, Pace::Held))
    }

    /// Paces a new work item on every account the project's calls spend
    ///
    /// A work item spends each role's account, so any one over its allowance
    /// or past its 5-hour mark holds it, the first such account in order.
    pub(super) fn pace_dispatch(&mut self) -> Result<Pace, StateError> {
        for account in self.spent_accounts() {
            match self.pace(Scope::Dispatch, &Limit::Account(account))? {
                Pace::Clear => {}
                held => return Ok(held),
            }
        }
        Ok(Pace::Clear)
    }

    // Each role's limit and each session reviewer's, in that order.
    fn spent_limits(&self) -> impl Iterator<Item = &Limit> {
        let limits = &self.agents.limits;
        let sessions = self.lineup.iter().filter_map(|r| match &r.runs {
            Runs::Claude(session) => Some(&session.limit),
            Runs::Local(_) => None,
        });
        [&limits.worker, &limits.reviewer, &limits.judge]
            .into_iter()
            .chain(sessions)
    }

    // The accounts the project's calls spend, each once.
    fn spent_accounts(&self) -> BTreeSet<Account> {
        let account = |limit: &Limit| match limit {
            Limit::Account(account) => Some(*account),
            Limit::Lease(_) => None,
        };
        self.spent_limits().filter_map(account).collect()
    }

    /// Who holds each lease the project's local agents take, for `status`
    pub(super) fn local_leases(&self) -> BTreeMap<String, Option<LockHolder>> {
        self.spent_limits()
            .filter_map(Limit::lease)
            .map(|lease| {
                let holder = self.ports.local_leases.holder(lease);
                (lease.as_str().to_owned(), holder)
            })
            .collect()
    }

    /// What the pacer shows in `status`: each account the project spends,
    /// and any other read this run, with what holds a new work item on it
    pub(super) fn pacer_status(&self, now: Timestamp) -> PacerStatus<'_> {
        let mut listed = self.spent_accounts();
        listed.extend(self.pacing.keys().copied());
        let accounts = listed.into_iter().map(|account| {
            let last = self.pacing.get(&account).map(|(_, last)| last);
            let status = AccountStatus {
                reading: last.and_then(|last| last.reading.as_ref()),
                holding: last
                    .and_then(|last| last.hold(Scope::Dispatch))
                    .filter(|hold| now < hold.until),
            };
            (account, status)
        });
        PacerStatus {
            enabled: self.settings.pacing.enabled,
            accounts: accounts.collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::pacer::{DayStart, HoldKind};
    use crate::ports::{Cost, MeterError, Usage};
    use crate::runner::step;
    use crate::state::{ProjectState, RunState, StateStore};
    use crate::test::{Rig, Scripted};

    const DAY: u64 = Rig::DAY;

    // A running project on a day that began with `used` percent of the week
    // spent, as a runner restarted partway through the day finds it
    fn running_since(project: &str, used: u32) -> (Rig, Mutex<Runner>) {
        running_paced(project, used, true)
    }

    fn running_paced(project: &str, used: u32, enabled: bool) -> (Rig, Mutex<Runner>) {
        let rig = Rig::new(project);
        if !enabled {
            rig.edit_settings(|s| {
                let on = "[app.dogs.kelpie.pacing]\nenabled = true";
                assert!(s.contains(on), "the example's pacing moved");
                s.replace(on, "[app.dogs.kelpie.pacing]\nenabled = false")
            });
        }
        let mut state = ProjectState::new(Timestamp(Rig::EPOCH));
        state.run = RunState::Running;
        state.pacing = Some(DayStart {
            week_resets_at: Timestamp(Rig::EPOCH + 7 * DAY),
            day: 0,
            week_used_pct: used,
        });
        StateStore::new(rig.paths().state).save(&state).unwrap();
        let runner = rig.open().unwrap();
        (rig, runner)
    }

    fn held(report: Option<StepReport>) -> (HoldKind, String, u64) {
        match report {
            Some(StepReport::Held {
                kind,
                reason,
                until,
            }) => (kind, reason, until.0),
            other => panic!("expected a hold, got {other:?}"),
        }
    }

    fn reply() -> Scripted {
        Scripted::Reply(Usage::default(), Cost(1))
    }

    fn dispatched(report: &Option<StepReport>) -> bool {
        matches!(report, Some(StepReport::Dispatched { issue: 7, .. }))
    }

    fn ended(report: &Option<StepReport>) -> bool {
        matches!(report, Some(StepReport::Ended { issue: 7, .. }))
    }

    #[test]
    fn a_ready_issue_is_dispatched_after_one_read_of_usage() {
        let (rig, runner) = running_since("shep", 0);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.meter.reads(), 0, "nothing to dispatch, nothing to read");

        rig.forge.list_ready(7, false);
        assert!(dispatched(&step(&runner).unwrap()));
        assert_eq!(rig.meter.reads(), 1);
    }

    #[test]
    fn an_allowance_is_spent_at_a_seventh_of_a_fresh_week() {
        // 100 / 7 is 14.29%
        for (spent, goes) in [(14, true), (15, false)] {
            let (rig, runner) = running_since("koji", 0);
            rig.forge.list_ready(7, false);
            rig.meter.set(Rig::utilization(spent, 0));
            let report = step(&runner).unwrap();
            assert_eq!(dispatched(&report), goes, "{spent}% spent: {report:?}");
        }
    }

    #[test]
    fn over_the_allowance_nothing_new_is_dispatched_and_status_says_why() {
        let (rig, runner) = running_since("golbat", 0);
        rig.forge.list_ready(7, false);
        rig.meter.set(Rig::utilization(15, 3));

        let (kind, reason, until) = held(step(&runner).unwrap());
        assert_eq!((kind, until), (HoldKind::Allowance, Rig::EPOCH + DAY));
        assert_eq!(
            reason,
            "today's allowance of 14.3% of the week is spent (15% since the day began), \
             so no new work item is dispatched until the next day; the current one continues"
        );
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"], json!(null));
        assert_eq!(
            status["pacer"],
            json!({
                "enabled": true,
                "claude": {
                    "reading": {
                        "at": Rig::EPOCH,
                        "session": { "used_pct": 3, "resets_at": Rig::EPOCH + 5 * 3600 },
                        "week": { "used_pct": 15, "resets_at": Rig::EPOCH + 7 * DAY },
                        "allowance_pct": 14.3,
                        "spent_today_pct": 15,
                        "per_hour_pct": 1.8,
                    },
                    "holding": {
                        "kind": "allowance",
                        "reason": reason,
                        "until": Rig::EPOCH + DAY,
                    },
                },
            })
        );
        assert_eq!(rig.claude.calls(), []);
    }

    #[test]
    fn a_hold_is_not_read_again_until_the_recheck() {
        let (rig, runner) = running_since("reactmap", 0);
        rig.forge.list_ready(7, false);
        rig.meter.set(Rig::utilization(15, 0));
        step(&runner).unwrap();
        for _ in 0..3 {
            rig.clock.advance(60);
            assert_eq!(step(&runner).unwrap(), None);
        }
        assert_eq!(rig.meter.reads(), 1);

        rig.clock.advance(RECHECK_SECS);
        let (kind, ..) = held(step(&runner).unwrap());
        assert_eq!((kind, rig.meter.reads()), (HoldKind::Allowance, 2));
    }

    #[test]
    fn the_allowance_refreshes_with_each_new_day() {
        let (rig, runner) = running_since("xilriws", 0);
        rig.forge.list_ready(7, false);
        rig.meter.set(Rig::utilization(15, 0));
        step(&runner).unwrap();

        // The 15% spent is now what the day began with: 85% over 6 days.
        rig.clock.advance(DAY);
        assert!(dispatched(&step(&runner).unwrap()));
        let reading = &rig.ask(&runner, "status", None)["pacer"]["claude"]["reading"];
        assert_eq!(
            (&reading["allowance_pct"], &reading["spent_today_pct"]),
            (&json!(14.2), &json!(0))
        );
    }

    #[test]
    fn the_allowance_refreshes_with_each_weekly_reset() {
        let (rig, runner) = running_since("chelone", 0);
        rig.forge.list_ready(7, false);
        rig.meter.set(Rig::utilization(15, 0));
        step(&runner).unwrap();

        rig.clock.advance(7 * DAY + 3600);
        let mut next_week = Rig::utilization(3, 0);
        next_week.week.resets_at = Timestamp(Rig::EPOCH + 14 * DAY);
        rig.meter.set(next_week);
        assert!(dispatched(&step(&runner).unwrap()));
        assert_eq!(
            rig.ask(&runner, "status", None)["pacer"]["claude"]["reading"]["allowance_pct"],
            13.9
        );
    }

    #[test]
    fn the_day_start_survives_a_restart() {
        let rig = Rig::new("rotom");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.forge.list_ready(7, false);
        step(&runner).unwrap();
        drop(runner);

        // Back with 40% spent since the read at 0% that began the day
        rig.meter.set(Rig::utilization(40, 0));
        let runner = rig.open().unwrap();
        rig.claude.script([reply()]);
        step(&runner).unwrap();
        let reading = &rig.ask(&runner, "status", None)["pacer"]["claude"]["reading"];
        assert_eq!(reading["spent_today_pct"], 40);
    }

    #[test]
    fn the_current_work_item_continues_over_the_allowance() {
        let (rig, runner) = running_since("shep", 0);
        rig.ask(&runner, "add", Some("7"));
        rig.meter.set(Rig::utilization(30, 0));
        rig.claude.script([reply()]);

        assert!(ended(&step(&runner).unwrap()));
        assert_eq!(rig.claude.calls().len(), 1);
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["pacer"]["claude"]["holding"]["kind"], "allowance");
    }

    #[test]
    fn a_turn_starts_below_half_the_window_and_parks_from_half() {
        for (session, starts) in [(49, true), (50, false)] {
            let (rig, runner) = running_since("koji", 0);
            rig.ask(&runner, "add", Some("7"));
            rig.meter.set(Rig::utilization(0, session));
            rig.claude.script([reply()]);
            let report = step(&runner).unwrap();
            assert_eq!(ended(&report), starts, "{session}%: {report:?}");
            assert_eq!(rig.claude.calls().len(), usize::from(starts));
        }
    }

    #[test]
    fn a_parked_worker_resumes_after_the_window_resets() {
        let (rig, runner) = running_since("golbat", 0);
        rig.ask(&runner, "add", Some("7"));
        rig.meter.set(Rig::utilization(0, 55));

        let (kind, reason, until) = held(step(&runner).unwrap());
        assert_eq!((kind, until), (HoldKind::Window, Rig::EPOCH + 5 * 3600));
        assert_eq!(
            reason,
            "the 5-hour window is at 55%, past the 50% mark, so no turn starts until it resets"
        );
        assert_eq!(rig.claude.calls(), []);
        let holding = &rig.ask(&runner, "status", None)["pacer"]["claude"]["holding"];
        assert_eq!(holding["kind"], "window");

        // Still parked well before the reset, without another read
        rig.clock.advance(300);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.meter.reads(), 1);

        // Past the reset the window is a new one, 3% used
        rig.clock.advance(5 * 3600);
        let mut fresh = Rig::utilization(0, 3);
        fresh.session.resets_at = Timestamp(Rig::EPOCH + 10 * 3600);
        rig.meter.set(fresh);
        rig.claude.script([reply()]);
        assert!(ended(&step(&runner).unwrap()));
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["pacer"]["claude"]["holding"], json!(null));
    }

    #[test]
    fn a_window_that_reset_early_is_noticed_at_the_recheck() {
        let (rig, runner) = running_since("reactmap", 0);
        rig.ask(&runner, "add", Some("7"));
        rig.meter.set(Rig::utilization(0, 60));
        step(&runner).unwrap();

        rig.clock.advance(RECHECK_SECS);
        rig.meter.set(Rig::utilization(0, 1));
        rig.claude.script([reply()]);
        assert!(ended(&step(&runner).unwrap()));
    }

    #[test]
    fn a_turn_under_way_is_never_interrupted() {
        let (rig, runner) = running_since("xilriws", 0);
        rig.ask(&runner, "add", Some("7"));
        rig.meter.set(Rig::utilization(0, 10));
        // The turn itself takes the window to 70% and the week past its allowance.
        rig.claude.script([Scripted::Spend(
            Rig::utilization(30, 70),
            Usage::default(),
            Cost(1),
        )]);

        assert!(ended(&step(&runner).unwrap()));
        assert_eq!(rig.meter.reads(), 1, "usage is read between turns only");
    }

    #[test]
    fn a_turn_cut_short_by_a_restart_carries_on_over_half_the_window() {
        let (rig, runner) = running_since("chelone", 0);
        rig.ask(&runner, "add", Some("7"));
        rig.claude.script([Scripted::Kill]);
        let _ = catch_unwind(AssertUnwindSafe(|| step(&runner)));
        drop(runner);

        rig.meter.set(Rig::utilization(0, 60));
        let runner = rig.open().unwrap();
        rig.claude.script([reply()]);
        assert!(ended(&step(&runner).unwrap()));
        assert_eq!(rig.meter.reads(), 1);
    }

    #[test]
    fn usage_that_cannot_be_read_holds_everything_until_it_can() {
        let (rig, runner) = running_since("rotom", 0);
        rig.forge.list_ready(7, false);
        rig.meter.fail(MeterError::Spawn("no such file".into()));

        let (kind, reason, until) = held(step(&runner).unwrap());
        assert_eq!(
            (kind, until),
            (HoldKind::Unreadable, Rig::EPOCH + RECHECK_SECS)
        );
        assert_eq!(reason, "cannot read usage: cannot run claude: no such file");
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["pacer"]["claude"]["reading"], json!(null));
        assert_eq!(step(&runner).unwrap(), None);

        rig.clock.advance(RECHECK_SECS);
        rig.meter.set(Rig::utilization(0, 0));
        assert!(dispatched(&step(&runner).unwrap()));
    }

    #[test]
    fn unreadable_usage_parks_a_worker_between_turns_too() {
        let (rig, runner) = running_since("koji", 0);
        rig.ask(&runner, "add", Some("7"));
        rig.meter.fail(MeterError::TimedOut);
        let (kind, ..) = held(step(&runner).unwrap());
        assert_eq!(kind, HoldKind::Unreadable);
        assert_eq!(rig.claude.calls(), []);
    }

    #[test]
    fn a_window_and_an_allowance_both_reached_report_the_longer_hold() {
        let (rig, runner) = running_since("shep", 0);
        rig.forge.list_ready(7, false);
        rig.meter.set(Rig::utilization(20, 60));
        let (kind, _, until) = held(step(&runner).unwrap());
        assert_eq!((kind, until), (HoldKind::Allowance, Rig::EPOCH + DAY));
    }

    #[test]
    fn with_pacing_off_a_dispatch_goes_ahead_past_the_allowance() {
        let (rig, runner) = running_paced("shep", 0, false);
        rig.forge.list_ready(7, false);
        rig.meter.set(Rig::utilization(60, 0));

        assert!(dispatched(&step(&runner).unwrap()));
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["pacer"]["enabled"], false);
        assert_eq!(status["pacer"]["claude"]["holding"], json!(null));
        let reading = &status["pacer"]["claude"]["reading"];
        assert_eq!(
            (&reading["spent_today_pct"], &reading["allowance_pct"]),
            (&json!(60), &json!(14.3))
        );
    }

    #[test]
    fn with_pacing_off_a_turn_starts_past_half_the_window() {
        let (rig, runner) = running_paced("koji", 0, false);
        rig.ask(&runner, "add", Some("7"));
        rig.meter.set(Rig::utilization(0, 80));
        rig.claude.script([reply()]);

        assert!(ended(&step(&runner).unwrap()));
        assert_eq!(rig.claude.calls().len(), 1);
        let status = rig.ask(&runner, "status", None);
        assert_eq!(
            status["pacer"]["claude"]["reading"]["session"]["used_pct"],
            80
        );
    }

    #[test]
    fn with_pacing_off_usage_that_cannot_be_read_holds_nothing() {
        let (rig, runner) = running_paced("rotom", 0, false);
        rig.forge.list_ready(7, false);
        rig.meter.fail(MeterError::TimedOut);

        assert!(dispatched(&step(&runner).unwrap()));
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["pacer"]["claude"]["holding"], json!(null));
    }
}
