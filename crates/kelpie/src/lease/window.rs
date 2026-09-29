//! A review window: so many summons a span, account-wide
//!
//! CodeRabbit's span is an hour, and its quota is whatever the latest review
//! footer said, one until a footer is read. Gemini's is a day of at least a
//! hundred, and no review states it. The span runs from each accepted
//! summon, not from the review it bought. A refusal quotes when the window
//! opens, and that quote overrides the book's own count until a later summon
//! is accepted.

use std::num::NonZeroU32;

use serde::Serialize;

use super::saved::{SavedRefusal, SavedWindow};
use crate::ports::Timestamp;

/// An hour, in seconds
pub const HOUR: u64 = 3600;

/// A day, in seconds
pub const DAY: u64 = 24 * HOUR;

/// A window's quota before any review states one, and its span
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terms {
    /// Summons a span
    pub quota: u32,
    /// How long one accepted summon holds its place, in seconds
    pub span: u64,
}

impl Terms {
    /// CodeRabbit's: the last footer read on shep said one an hour
    pub const CODERABBIT: Self = Self {
        quota: 1,
        span: HOUR,
    };

    /// Gemini's: "at least 100 pull request reviews per day" per
    /// installation, from Google's quotas page. Rolling, so never more.
    pub const GEMINI: Self = Self {
        quota: 100,
        span: DAY,
    };
}

/// One kind's review window
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    span: u64,
    quota: u32,
    // When the footer that stated the quota was posted.
    quota_at: Option<Timestamp>,
    summons: Vec<Timestamp>,
    refusal: Option<Refusal>,
}

// A refusal, and when the book heard it, so a later summon supersedes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Refusal {
    heard: Timestamp,
    opens: Timestamp,
}

/// A window as `status` shows it
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WindowStatus {
    /// Summons a span, from the latest footer or the window's terms
    pub quota: u32,
    /// Accepted summons in the last span, oldest first
    pub summons: Vec<Timestamp>,
    /// When it opens, or `None` while it is open
    pub opens: Option<Timestamp>,
}

impl Default for Window {
    fn default() -> Self {
        Self::new(Terms::CODERABBIT)
    }
}

impl From<SavedWindow> for Window {
    fn from(saved: SavedWindow) -> Self {
        let mut summons = saved.summons;
        summons.sort_unstable();
        summons.dedup();
        Self {
            span: HOUR,
            quota: saved.quota.get(),
            quota_at: saved.quota_at,
            summons,
            refusal: saved.refusal.map(|r| Refusal {
                heard: r.heard,
                opens: r.opens,
            }),
        }
    }
}

impl Window {
    /// An open window on `terms`
    pub fn new(terms: Terms) -> Self {
        Self {
            span: terms.span,
            quota: terms.quota.max(1),
            quota_at: None,
            summons: Vec::new(),
            refusal: None,
        }
    }

    /// Takes the span from `terms`. The book file keeps no span, so a
    /// restored window takes its kind's.
    pub fn span(&mut self, terms: Terms) {
        self.span = terms.span;
    }

    /// Takes the quota a review footer posted at `at` states
    ///
    /// The newest footer wins, whichever runner reports it last: each one
    /// reads only its own pull request's footers. A footer that states no
    /// quota is not read, so the quota is never zero.
    pub fn quota(&mut self, per_hour: u32, at: Timestamp) {
        let older = self.quota_at.is_some_and(|newest| at < newest);
        if per_hour > 0 && !older {
            self.quota = per_hour;
            self.quota_at = Some(at);
        }
    }

    /// Counts a summon accepted at `at`. The same summon counts once.
    pub fn summoned(&mut self, at: Timestamp) {
        if !self.summons.contains(&at) {
            self.summons.push(at);
            self.summons.sort_unstable();
        }
    }

    /// Takes a refusal heard at `heard` that quotes the window opening at `opens`
    ///
    /// The same quote heard again keeps its first hearing.
    pub fn refused(&mut self, heard: Timestamp, opens: Timestamp) {
        if self.refusal.is_some_and(|r| r.opens == opens) {
            return;
        }
        self.refusal = Some(Refusal { heard, opens });
    }

    /// When the window opens, or `None` when a summon may go out at `now`
    pub fn opens(&self, now: Timestamp) -> Option<Timestamp> {
        let latest = self.summons.last().copied();
        if let Some(refusal) = self.refusal
            && latest.is_none_or(|at| at <= refusal.heard)
        {
            return (now < refusal.opens).then_some(refusal.opens);
        }
        let recent = self.recent(now);
        let full = recent.len().checked_sub(self.quota as usize)?;
        Some(Timestamp(recent[full].0 + self.span))
    }

    /// The window as `status` shows it at `now`
    pub fn status(&self, now: Timestamp) -> WindowStatus {
        WindowStatus {
            quota: self.quota,
            summons: self.recent(now).to_vec(),
            opens: self.opens(now),
        }
    }

    /// Forgets summons older than a span before `now`
    pub fn prune(&mut self, now: Timestamp) {
        self.summons.retain(|at| at.0 + self.span > now.0);
    }

    /// The window as the book file keeps it
    pub fn saved(&self) -> SavedWindow {
        SavedWindow {
            quota: NonZeroU32::new(self.quota).expect("a window's quota is never zero"),
            quota_at: self.quota_at,
            summons: self.summons.clone(),
            refusal: self.refusal.map(|r| SavedRefusal {
                heard: r.heard,
                opens: r.opens,
            }),
        }
    }

    fn recent(&self, now: Timestamp) -> &[Timestamp] {
        let start = self.summons.partition_point(|at| at.0 + self.span <= now.0);
        &self.summons[start..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 1_790_000_000;

    fn at(offset: u64) -> Timestamp {
        Timestamp(T + offset)
    }

    #[test]
    fn a_fresh_window_is_open_for_one_summon_an_hour() {
        let mut window = Window::default();
        assert_eq!(window.opens(at(0)), None);
        window.summoned(at(0));
        assert_eq!(window.opens(at(1)), Some(at(HOUR)));
        assert_eq!(window.opens(at(HOUR - 1)), Some(at(HOUR)));
        assert_eq!(window.opens(at(HOUR)), None);
    }

    // Measured on shep: accepted 11:36:36, finished 12:00:01, and a
    // refusal at 12:21:28 quoted 15 minutes, landing on 12:36:28.
    #[test]
    fn the_hour_runs_from_the_accepted_summon_not_the_finished_review() {
        let mut window = Window::default();
        window.summoned(at(0));
        let finished = at(23 * 60 + 25);
        assert_eq!(window.opens(finished), Some(at(HOUR)));
    }

    #[test]
    fn a_footer_quota_of_ten_lets_ten_summons_through_an_hour() {
        let mut window = Window::default();
        window.quota(10, at(0));
        for minute in 0..10 {
            assert_eq!(window.opens(at(minute * 60)), None, "summon {minute}");
            window.summoned(at(minute * 60));
        }
        assert_eq!(window.opens(at(600)), Some(at(HOUR)));
        assert_eq!(window.opens(at(HOUR)), None, "the first summon aged out");
        window.summoned(at(HOUR));
        assert_eq!(window.opens(at(HOUR)), Some(at(HOUR + 60)));
    }

    #[test]
    fn a_lower_quota_read_later_closes_the_window_at_once() {
        let mut window = Window::default();
        window.quota(10, at(0));
        window.summoned(at(0));
        window.summoned(at(60));
        assert_eq!(window.opens(at(120)), None);
        window.quota(1, at(60));
        assert_eq!(window.opens(at(120)), Some(at(HOUR + 60)));
    }

    // Two runners, each reading its own pull request's footers: the one
    // whose footer is older reports last, and must not win.
    #[test]
    fn the_newest_footer_wins_whatever_order_it_arrives_in() {
        let mut window = Window::default();
        window.quota(1, at(3000));
        window.quota(10, at(1000));
        assert_eq!(window.status(at(3000)).quota, 1);

        window.quota(10, at(4000));
        window.quota(1, at(3000));
        assert_eq!(window.status(at(4000)).quota, 10);
    }

    #[test]
    fn a_footer_without_a_quota_leaves_it_as_it_was() {
        let mut window = Window::default();
        window.quota(0, at(0));
        window.summoned(at(0));
        assert_eq!(window.opens(at(1)), Some(at(HOUR)));
    }

    #[test]
    fn a_refusal_reschedules_the_window_from_its_quoted_wait() {
        let mut window = Window::default();
        assert_eq!(window.opens(at(0)), None);
        // A summon the book never granted, such as one made by hand.
        window.refused(at(0), at(10 * 60));
        assert_eq!(window.opens(at(0)), Some(at(600)));
        assert_eq!(window.opens(at(599)), Some(at(600)));
        assert_eq!(window.opens(at(600)), None);
    }

    #[test]
    fn a_refusal_overrides_the_books_own_count_either_way() {
        let mut window = Window::default();
        window.summoned(at(0));
        window.refused(at(60), at(120));
        assert_eq!(window.opens(at(60)), Some(at(120)), "sooner than the hour");
        assert_eq!(window.opens(at(120)), None, "though the summon is recent");

        let mut window = Window::default();
        window.quota(10, at(0));
        window.summoned(at(0));
        window.refused(at(60), at(1800));
        assert_eq!(window.opens(at(60)), Some(at(1800)), "though quota remains");
    }

    #[test]
    fn a_summon_accepted_after_a_refusal_supersedes_it() {
        let mut window = Window::default();
        window.refused(at(0), at(600));
        window.summoned(at(600));
        assert_eq!(window.opens(at(601)), Some(at(600 + HOUR)));
    }

    #[test]
    fn the_same_refusal_heard_again_keeps_its_first_hearing() {
        let mut window = Window::default();
        window.refused(at(0), at(600));
        window.summoned(at(300));
        window.refused(at(400), at(600));
        assert_eq!(
            window.opens(at(400)),
            Some(at(300 + HOUR)),
            "the summon came after the refusal was first heard"
        );
    }

    #[test]
    fn the_same_summon_counts_once() {
        let mut window = Window::default();
        window.quota(2, at(0));
        window.summoned(at(0));
        window.summoned(at(0));
        assert_eq!(window.opens(at(1)), None);
    }

    #[test]
    fn a_saved_window_opens_when_the_one_it_was_saved_from_would() {
        let mut window = Window::default();
        window.quota(2, at(0));
        window.summoned(at(0));
        window.summoned(at(60));
        window.refused(at(120), at(1800));
        let restored = Window::from(window.saved());
        for now in [at(120), at(1799), at(1800), at(HOUR + 60)] {
            assert_eq!(restored.opens(now), window.opens(now), "{now:?}");
        }
        assert_eq!(restored.status(at(120)), window.status(at(120)));
    }

    #[test]
    fn geminis_window_lets_a_hundred_summons_through_a_day() {
        let mut window = Window::new(Terms::GEMINI);
        for n in 0..100 {
            assert_eq!(window.opens(at(n * 60)), None, "summon {n}");
            window.summoned(at(n * 60));
        }
        assert_eq!(window.opens(at(HOUR * 2)), Some(at(DAY)));
        window.prune(at(DAY));
        assert_eq!(window.opens(at(DAY)), None, "the first summon aged out");
    }

    #[test]
    fn a_restored_window_takes_its_kinds_span() {
        let mut window = Window::new(Terms::GEMINI);
        window.summoned(at(0));
        let mut restored = Window::from(window.saved());
        restored.span(Terms::GEMINI);
        assert_eq!(restored, window);
    }

    #[test]
    fn status_shows_the_last_hours_summons_and_when_it_opens() {
        let mut window = Window::default();
        window.quota(2, at(0));
        window.summoned(at(0));
        window.summoned(at(1200));
        assert_eq!(
            window.status(at(1300)),
            WindowStatus {
                quota: 2,
                summons: vec![at(0), at(1200)],
                opens: Some(at(HOUR)),
            }
        );
        window.prune(at(HOUR));
        assert_eq!(window.status(at(HOUR)).summons, [at(1200)]);
    }
}
