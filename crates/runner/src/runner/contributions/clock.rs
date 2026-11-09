//! The quiet clock of a contribution
//!
//! Kelpie waits on the contributor once the current head's results ask something
//! of them: findings, open bot threads or red CI. The clock runs from then, in
//! [`Contribution::waiting_since`]. The contributor's comment, review reply or
//! push restarts it, as does a thumbs down on the release ask. Green CI with
//! nothing open clears it, as does a new head.
//!
//! [`Clock::due`] says when `nudge_days` and `quiet_days` have passed, or when an
//! open ask's notice has run (`release.rs`), and [`Clock::acts_at`] is that moment.

use serde::Serialize;

use super::super::Runner;
use crate::ports::Timestamp;
use crate::settings::Contributions;
use crate::state::{Contribution, StateError};

const DAY: u64 = 86_400;

/// How long a contribution has waited on its contributor
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    since: Timestamp,
    /// When the open ask went out; `None` while none is
    asked: Option<Timestamp>,
    nudge_days: u64,
    quiet_days: u64,
}

/// What a contribution's wait has come to
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Due {
    /// `nudge_days` have passed
    Nudge,
    /// `quiet_days` have passed
    Quiet,
}

impl Clock {
    /// The clock of `contribution` under `settings`, or `None` while kelpie
    /// waits on no one
    pub fn of(contribution: &Contribution, settings: &Contributions) -> Option<Self> {
        let nudge_days: u64 = settings.nudge_days.get().into();
        Some(Self {
            since: contribution.waiting_since?,
            asked: (contribution.asked.as_ref())
                .filter(|a| a.is_open())
                .map(|a| a.at),
            nudge_days,
            // The settings keep the nudge below the quiet; a pair that is not
            // still never reports quiet before the nudge.
            quiet_days: u64::from(settings.quiet_days.get()).max(nudge_days),
        })
    }

    /// What is due at `now`: nothing, the nudge, or, past it, the quiet
    pub fn due(&self, now: Timestamp) -> Option<Due> {
        if now >= self.quiet_at() {
            Some(Due::Quiet)
        } else if self.days(now) >= self.nudge_days {
            Some(Due::Nudge)
        } else {
            None
        }
    }

    /// Whole days until the contribution counts as quiet, 0 once it does
    pub(super) fn days_to_quiet(&self, now: Timestamp) -> u64 {
        self.quiet_at().0.saturating_sub(now.0).div_ceil(DAY)
    }

    /// The moment the clock says kelpie acts on the contributor's silence, which
    /// is what an ask promises and what a post that comes after it is late on
    pub(in crate::runner) fn acts_at(&self) -> Timestamp {
        self.quiet_at()
    }

    /// Whole days from a restart of the clock to its quiet
    pub(super) fn quiet_days(&self) -> u64 {
        self.quiet_days
    }

    /// Whole days an open ask gives its contributor, however late it went out:
    /// the quiet window less the nudge that asked for it, or one when the two
    /// settings are equal
    pub(super) fn notice_days(&self) -> u64 {
        self.quiet_days.saturating_sub(self.nudge_days).max(1)
    }

    // The moment the contribution counts as quiet: the whole window from the
    // wait, or the notice from an ask that went out late, whichever is later
    fn quiet_at(&self) -> Timestamp {
        let window = Timestamp(self.since.0.saturating_add(self.quiet_days * DAY));
        match self.asked {
            Some(at) => window.max(Timestamp(at.0.saturating_add(self.notice_days() * DAY))),
            None => window,
        }
    }

    // Whole days waited
    fn days(&self, now: Timestamp) -> u64 {
        now.0.saturating_sub(self.since.0) / DAY
    }

    fn status(&self, now: Timestamp) -> ClockStatus {
        ClockStatus {
            waiting_for: now.0.saturating_sub(self.since.0),
            days_to_nudge: self.nudge_days.saturating_sub(self.days(now)),
            days_to_quiet: self.days_to_quiet(now),
            due: self.due(now),
        }
    }
}

/// A contribution's clock, as `status` shows it
#[derive(Debug, Serialize)]
pub struct ClockStatus {
    /// Seconds kelpie has waited on the contributor
    pub waiting_for: u64,
    /// Whole days until the nudge, 0 once it is due
    pub days_to_nudge: u64,
    /// Whole days until the contribution counts as quiet, 0 once it does
    pub days_to_quiet: u64,
    /// What is due now, left out while nothing is
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<Due>,
}

/// A followed contribution, as `status` shows it: the record, and its clock
/// while one runs
#[derive(Debug, Serialize)]
pub struct Followed<'a> {
    #[serde(flatten)]
    pub(in crate::runner) contribution: &'a Contribution,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::runner) clock: Option<ClockStatus>,
}

impl Runner {
    // Starts the clock of each contribution whose head's results wait on the
    // contributor, and clears the ones that do not. A running clock keeps
    // its start.
    pub(super) fn follow_clock(&mut self) -> Result<(), StateError> {
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        for contribution in &mut next.contributions {
            contribution.follow_clock(now);
        }
        match next == self.state {
            true => Ok(()),
            false => self.save(next),
        }
    }

    /// The contributions followed, each with its clock, for `status`
    pub(in crate::runner) fn followed_status(&self, now: Timestamp) -> Vec<Followed<'_>> {
        let settings = &self.settings.contributions;
        (self.state.contributions.iter())
            .map(|contribution| Followed {
                contribution,
                clock: Clock::of(contribution, settings).map(|c| c.status(now)),
            })
            .collect()
    }
}
