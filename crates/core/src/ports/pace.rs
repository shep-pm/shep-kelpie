//! Spacing the forge calls of every runner on one login
//!
//! GitHub limits a login, not a project, so one [`Pace`] stands for the login:
//! it holds every call while the forge's limit is used up, and lets calls
//! through no faster than [`PER_MINUTE`] a minute and [`AT_ONCE`] a second. The
//! dog keeps the one every runner reaches. A runner that cannot reach the dog
//! keeps a [`LocalPace`] of its own, which holds and spaces its calls alone.
//! Both numbers sit well under GitHub's documented secondary limits of 900
//! points a minute and 100 concurrent requests.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Mutex, PoisonError};

use super::{Clock, Timestamp};

/// The most calls let through in any minute
pub const PER_MINUTE: usize = 60;

/// The most calls let through in one second
pub const AT_ONCE: usize = 5;

/// Seconds a call counts toward the minute's cap
const MINUTE: u64 = 60;

/// What a call is told when it asks to go
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// Go now: the call is counted
    Go,
    /// Not yet: ask again after this many seconds
    Wait(u64),
    /// The forge's limit is used up until then: make no call
    Held(Timestamp),
}

/// A login's hold and pacer, shared by every runner on it
pub trait Pace: Send + Sync + fmt::Debug {
    /// Whether a call of `project`'s may go at `now`, counting it when it may
    fn pass(&self, project: &str, now: Timestamp) -> Pass;

    /// Until when every call is held, while that is after `now`
    fn held(&self, now: Timestamp) -> Option<Timestamp>;

    /// Holds every call until `until`, or the hold already standing if that is later
    fn hold(&self, until: Timestamp);
}

/// The pacing rule alone, with no clock and no hold
#[derive(Debug, Clone)]
pub struct CallPacer {
    per_minute: usize,
    at_once: usize,
    // When each call in the last minute went, and whose it was, oldest first
    grants: VecDeque<(u64, String)>,
}

impl CallPacer {
    /// A pacer that lets through `per_minute` calls a minute and `at_once` a second
    pub fn new(per_minute: usize, at_once: usize) -> Self {
        Self {
            per_minute,
            at_once,
            grants: VecDeque::new(),
        }
    }

    /// `None` when a call of `project`'s may go at `now`, which counts it, or the seconds it waits
    ///
    /// The minute's calls are shared fairly while its places run short: the
    /// minute keeps one free place for every project in it. While places are
    /// that short, `project` is held to its share of the cap and waits for its
    /// oldest call to leave; while places are ample, a project past its share
    /// goes.
    pub fn ask(&mut self, project: &str, now: Timestamp) -> Option<u64> {
        while self
            .grants
            .front()
            .is_some_and(|(at, _)| at.saturating_add(MINUTE) <= now.0)
        {
            self.grants.pop_front();
        }
        let leaves = |at: u64| at.saturating_add(MINUTE).saturating_sub(now.0).max(1);
        let mut wait = 0;
        if self.grants.len() >= self.per_minute {
            wait = wait.max(self.grants.front().map_or(1, |(at, _)| leaves(*at)));
        }
        if self.grants.iter().filter(|(at, _)| *at == now.0).count() >= self.at_once {
            wait = wait.max(1);
        }
        let mut projects: Vec<&str> = self.grants.iter().map(|(_, p)| p.as_str()).collect();
        projects.push(project);
        projects.sort_unstable();
        projects.dedup();
        let share = (self.per_minute / projects.len()).max(1);
        let free = self.per_minute.saturating_sub(self.grants.len());
        let mut own = self.grants.iter().filter(|(_, p)| p == project);
        if projects.len() > 1 && free <= projects.len() && own.clone().count() >= share {
            wait = wait.max(own.next().map_or(1, |(at, _)| leaves(*at)));
        }
        if wait > 0 {
            return Some(wait);
        }
        self.grants.push_back((now.0, project.to_owned()));
        None
    }
}

#[derive(Debug)]
struct Standing {
    pacer: CallPacer,
    until: Option<Timestamp>,
}

/// A [`Pace`] kept in this process
#[derive(Debug)]
pub struct LocalPace(Mutex<Standing>);

impl LocalPace {
    /// A pace with the limits of [`PER_MINUTE`] and [`AT_ONCE`]
    pub fn new() -> Self {
        Self::with(CallPacer::new(PER_MINUTE, AT_ONCE))
    }

    /// A pace spacing calls as `pacer` does
    pub fn with(pacer: CallPacer) -> Self {
        Self(Mutex::new(Standing { pacer, until: None }))
    }

    fn standing(&self) -> std::sync::MutexGuard<'_, Standing> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for LocalPace {
    fn default() -> Self {
        Self::new()
    }
}

impl Pace for LocalPace {
    fn pass(&self, project: &str, now: Timestamp) -> Pass {
        let mut standing = self.standing();
        if let Some(until) = standing.until.filter(|until| *until > now) {
            return Pass::Held(until);
        }
        match standing.pacer.ask(project, now) {
            None => Pass::Go,
            Some(seconds) => Pass::Wait(seconds),
        }
    }

    fn held(&self, now: Timestamp) -> Option<Timestamp> {
        let mut standing = self.standing();
        if standing.until.is_some_and(|until| until <= now) {
            standing.until = None;
        }
        standing.until
    }

    fn hold(&self, until: Timestamp) {
        let mut standing = self.standing();
        standing.until = standing.until.max(Some(until));
    }
}

/// Waits on `clock` until `pace` lets a call of `project`'s go, or says until
/// when the forge is held
pub(super) fn admit(pace: &dyn Pace, clock: &dyn Clock, project: &str) -> Result<(), Timestamp> {
    loop {
        match pace.pass(project, clock.now()) {
            Pass::Go => return Ok(()),
            Pass::Held(until) => return Err(until),
            Pass::Wait(seconds) => clock.wait(seconds),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Timestamp = Timestamp(1_000_000);

    fn at(seconds: u64) -> Timestamp {
        Timestamp(T.0 + seconds)
    }

    #[test]
    fn calls_past_the_minutes_cap_wait_until_the_oldest_leaves_the_minute() {
        let mut pacer = CallPacer::new(3, 10);
        assert_eq!(pacer.ask("a", at(0)), None);
        assert_eq!(pacer.ask("a", at(10)), None);
        assert_eq!(pacer.ask("a", at(20)), None);

        assert_eq!(pacer.ask("a", at(30)), Some(30));
        assert_eq!(pacer.ask("a", at(59)), Some(1));
        assert_eq!(pacer.ask("a", at(60)), None);
    }

    #[test]
    fn calls_past_the_seconds_cap_wait_for_the_next_second() {
        let mut pacer = CallPacer::new(100, 2);
        assert_eq!(pacer.ask("a", at(0)), None);
        assert_eq!(pacer.ask("a", at(0)), None);
        assert_eq!(pacer.ask("a", at(0)), Some(1));
        assert_eq!(pacer.ask("a", at(1)), None);
    }

    #[test]
    fn a_busy_project_leaves_a_newcomer_its_share_of_the_cap() {
        let mut pacer = CallPacer::new(4, 10);
        for second in 0..4 {
            assert_eq!(pacer.ask("busy", at(second)), None);
        }
        // The cap is spent, so the newcomer waits for the busy project's oldest call.
        assert_eq!(pacer.ask("new", at(4)), Some(56));
        // When the busy project's first call leaves, the newcomer is let in.
        assert_eq!(pacer.ask("new", at(60)), None);
        assert_eq!(pacer.ask("busy", at(60)), Some(1));
        // The busy project is then over its half of the cap, so a freed place goes to the newcomer.
        assert_eq!(pacer.ask("busy", at(61)), Some(1));
        assert_eq!(pacer.ask("new", at(61)), None);
    }

    #[test]
    fn a_project_past_its_share_goes_while_the_minute_has_places() {
        let mut pacer = CallPacer::new(12, 10);
        assert_eq!(pacer.ask("new", at(0)), None);
        // busy is owed 6 of the 12 places and takes 9 of them while 3 stay free.
        for second in 1..10 {
            assert_eq!(pacer.ask("busy", at(second)), None);
        }
    }

    #[test]
    fn a_project_past_its_share_waits_once_places_run_short() {
        let mut pacer = CallPacer::new(12, 10);
        assert_eq!(pacer.ask("new", at(0)), None);
        for second in 1..10 {
            assert_eq!(pacer.ask("busy", at(second)), None);
        }
        // 10 of 12 places are used and 2 projects are in the minute: the 2 left
        // are the reserve each is owed, so busy waits for its oldest call.
        assert_eq!(pacer.ask("busy", at(10)), Some(51));
        // The short places go to the project under its share.
        assert_eq!(pacer.ask("new", at(10)), None);
        assert_eq!(pacer.ask("new", at(10)), None);
        // The cap is spent, so nothing goes until a call leaves the minute.
        assert_eq!(pacer.ask("new", at(10)), Some(50));
    }

    #[test]
    fn no_minute_holds_more_calls_than_the_cap_however_many_projects_ask() {
        let mut pacer = CallPacer::new(6, 3);
        let mut went = Vec::new();
        for second in 0..300 {
            for project in ["a", "b", "c"] {
                while pacer.ask(project, at(second)).is_none() {
                    went.push(second);
                }
            }
        }
        for minute in went.windows(7) {
            assert!(minute[6] - minute[0] >= 60, "{minute:?}");
        }
    }

    #[test]
    fn a_hold_stops_every_call_until_it_ends_and_never_gets_shorter() {
        let pace = LocalPace::new();
        pace.hold(at(100));
        pace.hold(at(50));

        assert_eq!(pace.pass("a", at(10)), Pass::Held(at(100)));
        assert_eq!(pace.held(at(99)), Some(at(100)));
        assert_eq!(pace.pass("b", at(100)), Pass::Go);
        assert_eq!(pace.held(at(100)), None);
    }
}
