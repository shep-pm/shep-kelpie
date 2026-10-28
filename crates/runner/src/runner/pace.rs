//! Pacing: each account's usage is read before a call on it starts
//!
//! A new work item is paced on every account its roles spend. Its turns on
//! the worker's account, a review round on its reviewer's. Each open item's
//! own agent is included, listed or not. Each account keeps its own day and
//! its own holds. An agent limited by a lease is never paced. A reading that
//! finds a hold is trusted for [`RECHECK_SECS`] or until the hold ends. One
//! that finds no hold is never reused past its step, which reads it with the
//! lock let go. With `pacing.enabled` off the reading still runs for `status`,
//! and neither limit holds anything.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::Serialize;

use super::Runner;
use super::report::{Begin, StepReport};
use super::turn::reads::Job;
use crate::agents::Runs;
use crate::board::Skip;
use crate::pacer::{Assessment, Hold, RECHECK_SECS, Reading, Scope, assess, named};
use crate::ports::{Meter, PaddockHolder, Timestamp};
use crate::settings::{Account, AgentHarness, AgentName, GatewayName, Limit, ModelHost};
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
    StillHeld(Hold),
    /// The step reads the account's usage with the lock let go, and is made again
    Reads(Box<Job>),
}

/// How a start the pacer may hold back ended
pub(super) enum Started<T> {
    Opened(T),
    /// What the runner does instead of starting
    Held(Box<Begin>),
}

/// What a walk down the agent board's ranking found
pub(super) enum Walked {
    /// The first agent the pacer lets start
    Opens(AgentName),
    /// The step reads an agent's usage first, and is made again
    Reads(Begin),
    /// The pacer held every agent: the first hold's reason, when the soonest
    /// hold lapses, and what the first hold does instead of starting
    Held {
        reason: Option<String>,
        lapses: Option<Timestamp>,
        first: Option<Begin>,
    },
}

/// What a pass over the pull requests that ask to start found: the step it
/// begins, each that could not start, and the first the pacer held
pub(super) type Passed = (Option<Begin>, Vec<Skip>, Option<Begin>);

impl Pace {
    /// The pacer's reading of the hold, or `None` when nothing holds
    pub(super) fn reason(&self) -> Option<&str> {
        match self {
            Self::Clear | Self::Reads(_) => None,
            Self::Held(hold) | Self::StillHeld(hold) => Some(&hold.reason),
        }
    }

    /// When the pacer's hold lapses, or `None` when nothing holds
    pub(super) fn until(&self) -> Option<Timestamp> {
        match self {
            Self::Clear | Self::Reads(_) => None,
            Self::Held(hold) | Self::StillHeld(hold) => Some(hold.until),
        }
    }

    /// What the runner does instead of starting, or `None` to start
    pub(super) fn holds(self) -> Option<Begin> {
        match self {
            Self::Clear => None,
            Self::StillHeld(_) => Some(Begin::Idle),
            Self::Reads(job) => Some(Begin::Reads(job)),
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
    /// Paces `scope` on the limit of the work item's worker
    pub(super) fn pace_worker(&mut self, scope: Scope) -> Result<Pace, StateError> {
        let limit = match self.current() {
            Some(item) => match self.worker_agent(item) {
                Ok(agent) => agent.limit,
                // Its turn fails at once, saying why, so nothing waits on a window.
                Err(_) => return Ok(Pace::Clear),
            },
            // A turn is begun only on a work item, so nothing waits on a window.
            None => return Ok(Pace::Clear),
        };
        self.pace(scope, &limit)
    }

    /// Paces `scope` on `limit`: an account's windows, or nothing for a lease
    pub(super) fn pace(&mut self, scope: Scope, limit: &Limit) -> Result<Pace, StateError> {
        self.pace_on(scope, limit, true)
    }

    /// [`Runner::pace`] for a caller that cannot be made again, which reads under the lock
    /// whatever its step has not read
    pub(super) fn pace_now(&mut self, scope: Scope, limit: &Limit) -> Result<Pace, StateError> {
        self.pace_on(scope, limit, true)
    }

    fn pace_on(&mut self, scope: Scope, limit: &Limit, off_lock: bool) -> Result<Pace, StateError> {
        let Limit::Account(account) = *limit else {
            return Ok(Pace::Clear);
        };
        let now = self.ports.clock.now();
        if let Some(hold) = self.still_held(account, scope, now) {
            return Ok(Pace::StillHeld(hold.clone()));
        }
        let read = match self.reads.usage(account) {
            Some(read) => read,
            None => match self.usage_job(account, now).filter(|_| off_lock) {
                Some(job) => return Ok(Pace::Reads(job)),
                None => self.ports.meter_of(account).read(now),
            },
        };
        let prior = self.state.day_start(account);
        let mut assessment = match read {
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
        if let Some(reading) = &assessment.reading {
            self.metrics.plan(account, reading);
        }
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

    // The hold an earlier reading of `account` found on `scope`, while it is trusted
    fn still_held(&self, account: Account, scope: Scope, now: Timestamp) -> Option<&Hold> {
        let (read_at, last) = self.pacing.get(&account)?;
        let trusted = now.0 < read_at.0 + RECHECK_SECS;
        last.hold(scope).filter(|hold| trusted && now < hold.until)
    }

    /// Whether a trusted hold still keeps `agent` from starting a turn
    pub(super) fn turn_still_held(&self, agent: &AgentName) -> bool {
        let Some(Limit::Account(account)) = self.agent_limit(agent) else {
            return false;
        };
        let now = self.ports.clock.now();
        self.still_held(account, Scope::Turn, now).is_some()
    }

    /// The meters a new work item is paced on, less those a trusted hold answers for
    pub(super) fn dispatch_meters(&self, now: Timestamp) -> Vec<(Account, Arc<dyn Meter>)> {
        (self.spent_accounts().into_iter())
            .filter(|&account| self.still_held(account, Scope::Dispatch, now).is_none())
            .map(|account| (account, Arc::clone(self.ports.meter_of(account))))
            .collect()
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

    /// The first of `agents`, in their order, that the pacer lets start on `scope`
    ///
    /// A new work item paces each agent as [`Runner::pace_agent`] does, a
    /// turn on each agent's own limit.
    pub(super) fn first_unpaced<'a>(
        &mut self,
        agents: impl IntoIterator<Item = &'a AgentName>,
        scope: Scope,
    ) -> Result<Walked, StateError> {
        let (mut reason, mut lapses, mut first) = (None, None, None);
        for agent in agents {
            let pace = match scope {
                Scope::Dispatch => self.pace_agent(agent)?,
                Scope::Turn => match self.agent_limit(agent) {
                    Some(limit) => self.pace(Scope::Turn, &limit)?,
                    None => Pace::Clear,
                },
            };
            if let Some(why) = pace.reason() {
                reason.get_or_insert(why.to_owned());
            }
            if let Some(until) = pace.until() {
                lapses = Some(lapses.map_or(until, |soonest: Timestamp| soonest.min(until)));
            }
            match pace.holds() {
                None => return Ok(Walked::Opens(agent.clone())),
                Some(reads @ Begin::Reads(_)) => return Ok(Walked::Reads(reads)),
                Some(held) => {
                    first.get_or_insert(held);
                }
            }
        }
        Ok(Walked::Held {
            reason,
            lapses,
            first,
        })
    }

    // The limit `agent`'s sessions run under, None when it runs none.
    fn agent_limit(&self, agent: &AgentName) -> Option<Limit> {
        let session = (self.book.get(agent)).and_then(|agent| agent.runs.session());
        session.map(|(_, limit)| limit.clone())
    }

    /// Paces a new work item on `agent`'s own account, one the project may not
    /// list, when no listed role spends it and so [`Runner::pace_dispatch`] has
    /// not read it
    ///
    /// The account is paced for this pick alone, so a hold on it holds back
    /// no pick on another agent.
    pub(super) fn pace_agent(&mut self, agent: &AgentName) -> Result<Pace, StateError> {
        let Some(limit) = self.agent_limit(agent) else {
            return Ok(Pace::Clear);
        };
        if let Limit::Account(account) = limit
            && self.spent_accounts().contains(&account)
        {
            return Ok(Pace::Clear);
        }
        self.pace(Scope::Dispatch, &limit)
    }

    // Each listed reviewer's limit that runs sessions, and each open work
    // item's own agent's. A new item's agent is paced as the board picks it.
    fn spent_limits(&self) -> impl Iterator<Item = &Limit> {
        let sessions = self.lineup.iter().filter_map(|r| r.runs.session());
        let items = self.item_runs().filter_map(|runs| runs.session());
        (sessions.map(|(_, limit)| limit)).chain(items.map(|(_, limit)| limit))
    }

    // What each open work item's worker runs on, from its agent file, which
    // the project may not list.
    fn item_runs(&self) -> impl Iterator<Item = &Runs> {
        (self.state.work_items.iter())
            .filter_map(|item| self.book.get(&item.agent))
            .map(|agent| &agent.runs)
    }

    // The accounts the project's calls spend, each once.
    fn spent_accounts(&self) -> BTreeSet<Account> {
        let account = |limit: &Limit| match limit {
            Limit::Account(account) => Some(*account),
            Limit::Unqueued | Limit::Gateway => None,
        };
        self.spent_limits().filter_map(account).collect()
    }

    /// Who holds the gateway's lease on each model the project's local agents
    /// call, for `status`, keeping what it read for a post
    ///
    /// A model is shown by its own name, or as `model@gateway` when another
    /// gateway's model has that name, so no holder hides another.
    pub(super) fn read_local_leases(&self) -> BTreeMap<String, Option<PaddockHolder>> {
        let mut shown = BTreeMap::new();
        for (gateway, model) in self.gateway_models() {
            let name = match shown.contains_key(&model) {
                true => format!("{model}@{gateway}"),
                false => model.clone(),
            };
            let up = self.gateways.upstream(&ModelHost::Gateway(gateway)).ok();
            let holder = up.and_then(|up| self.ports.local_leases.paddock_holder(&up, &model));
            shown.insert(name, holder);
        }
        self.postings.keep_holders(shown.clone());
        shown
    }

    /// The gateway's leases as they were last read, for a post
    ///
    /// Read once at the start, where a trigger could have been answered anyway,
    /// and kept after that, so no pass asks the gateway.
    pub(super) fn posted_local_leases(&self) -> BTreeMap<String, Option<PaddockHolder>> {
        let held = self.postings.holders();
        if held.is_empty() && !self.gateway_models().is_empty() {
            return self.read_local_leases();
        }
        held
    }

    // Each model the project's local agents call behind a gateway, once.
    fn gateway_models(&self) -> BTreeSet<(GatewayName, String)> {
        let behind = |harness: &AgentHarness, model: &str| {
            // parity: harness.local-model
            match &harness.server()?.host {
                ModelHost::Gateway(name) => Some((name.clone(), model.to_owned())),
                ModelHost::Url(_) => None,
            }
        };
        let implementers = self.settings.allowed_implementers(&self.book);
        let sessions = self.lineup.iter().filter_map(|r| r.runs.session());
        let items = self.item_runs().filter_map(|runs| runs.session());
        let endpoints = self.lineup.iter().filter_map(|r| match &r.runs {
            Runs::Endpoint(endpoint) => match &endpoint.host {
                ModelHost::Gateway(name) => {
                    Some((name.clone(), endpoint.model.as_str().to_owned()))
                }
                ModelHost::Url(_) => None,
            },
            _ => None,
        });
        let sessions = (implementers.iter().map(|i| &i.model))
            .chain(sessions.map(|(model, _)| model))
            .chain(items.map(|(model, _)| model))
            .filter_map(|m| behind(&m.harness, m.model.as_str()));
        sessions.chain(endpoints).collect()
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
    use crate::test::ticket;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::pacer::{DayStart, HoldKind};
    use crate::ports::{Cost, MeterError, Usage};
    use crate::runner::step;
    use crate::state::{ProjectState, StateStore};
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
        let mut state = ProjectState::new();
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
        matches!(report, Some(StepReport::Dispatched { issue, .. }) if *issue == ticket(7))
    }

    fn ended(report: &Option<StepReport>) -> bool {
        matches!(report, Some(StepReport::Ended { issue, .. }) if *issue == ticket(7))
    }

    #[test]
    fn a_ready_issue_is_dispatched_after_one_read_of_usage() {
        let (rig, runner) = running_since("shep", 0);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.meter.reads(), 0, "nothing to dispatch, nothing to read");

        rig.forge.list_ready(7, false);
        rig.next_look();
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
    fn a_pick_the_pacer_holds_shows_the_hold_in_skipped_and_on_the_board() {
        let (rig, runner) = running_since("golbat", 0);
        rig.forge.list_ready(7, false);
        rig.meter.set(Rig::utilization(15, 3));

        let (_, reason, _) = held(step(&runner).unwrap());

        let skipped = rig.ask(&runner, "status", None)["skipped"].clone();
        let hold = json!([{ "reason": "paced", "issue": "7", "hold": reason }]);
        assert_eq!(skipped, hold);
        let board = std::fs::read_to_string(rig.paths().board).unwrap();
        assert!(
            board.contains(&format!("held by the pacer: {reason}")),
            "{board}"
        );
        // A hold from the earlier reading still stands the next pass.
        rig.clock.advance(60);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.ask(&runner, "status", None)["skipped"], hold);
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
