//! A review window: so many summons an hour, account-wide
//!
//! The quota is whatever the latest review footer said, one an hour until
//! a footer is read. The hour runs from each accepted summon, not from the
//! review it bought. A refusal quotes when the window opens, and that quote
//! overrides the book's own count until a later summon is accepted.

use serde::Serialize;

use crate::ports::Timestamp;

/// How long one accepted summon holds its place in the window, in seconds
pub const HOUR: u64 = 3600;

/// The quota before any footer has been read: the last one read on shep
const FIRST_QUOTA: u32 = 1;

/// One kind's review window
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    quota: u32,
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
    /// Summons an hour, from the latest footer
    pub quota: u32,
    /// Accepted summons in the last hour, oldest first
    pub summons: Vec<Timestamp>,
    /// When it opens, or `None` while it is open
    pub opens: Option<Timestamp>,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            quota: FIRST_QUOTA,
            summons: Vec::new(),
            refusal: None,
        }
    }
}

impl Window {
    /// Takes the quota a review footer states
    ///
    /// A footer that states none is not read, so the quota is never zero.
    pub fn quota(&mut self, per_hour: u32) {
        if per_hour > 0 {
            self.quota = per_hour;
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
        Some(Timestamp(recent[full].0 + HOUR))
    }

    /// The window as `status` shows it at `now`
    pub fn status(&self, now: Timestamp) -> WindowStatus {
        WindowStatus {
            quota: self.quota,
            summons: self.recent(now).to_vec(),
            opens: self.opens(now),
        }
    }

    /// Forgets summons older than an hour before `now`
    pub fn prune(&mut self, now: Timestamp) {
        self.summons.retain(|at| at.0 + HOUR > now.0);
    }

    fn recent(&self, now: Timestamp) -> &[Timestamp] {
        let start = self.summons.partition_point(|at| at.0 + HOUR <= now.0);
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
        window.quota(10);
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
        window.quota(10);
        window.summoned(at(0));
        window.summoned(at(60));
        assert_eq!(window.opens(at(120)), None);
        window.quota(1);
        assert_eq!(window.opens(at(120)), Some(at(HOUR + 60)));
    }

    #[test]
    fn a_footer_without_a_quota_leaves_it_as_it_was() {
        let mut window = Window::default();
        window.quota(0);
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

        let mut window = Window::default();
        window.quota(10);
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
        window.quota(2);
        window.summoned(at(0));
        window.summoned(at(0));
        assert_eq!(window.opens(at(1)), None);
    }

    #[test]
    fn status_shows_the_last_hours_summons_and_when_it_opens() {
        let mut window = Window::default();
        window.quota(2);
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
