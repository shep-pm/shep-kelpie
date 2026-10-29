//! The fake forge going down, or its rate limit used up, and the count of
//! calls made of it

use std::sync::atomic::Ordering;

use super::FakeForge;
use super::held;
use crate::ports::{Cooldown, ForgeError, RateBucket, RateLimits, Timestamp};

/// What every call fails with while the forge is down, and when its rate
/// limit resets where it can say
pub type Down = (String, Option<Timestamp>);

/// What reading the rate limits answers: limits to spare until a test sets
/// others, and none while it is down
#[derive(Debug)]
pub struct Rate {
    limits: RateLimits,
    down: bool,
    reads: usize,
}

impl Default for Rate {
    fn default() -> Self {
        let spare = RateBucket {
            used: 0,
            remaining: 5_000,
            reset: Timestamp(0),
        };
        Self {
            limits: RateLimits {
                core: spare,
                graphql: spare,
                search: spare,
            },
            down: false,
            reads: 0,
        }
    }
}

impl FakeForge {
    /// How many calls the runner made, each counted as it was asked
    pub fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }

    /// How many times the board's ready issues were read
    pub fn board_reads(&self) -> usize {
        self.board_reads.load(Ordering::SeqCst)
    }

    /// How many times the open issues were listed
    pub fn issue_listings(&self) -> usize {
        self.issue_listings.load(Ordering::SeqCst)
    }

    /// How many times one issue was read through the tickets port
    pub fn issue_reads(&self) -> usize {
        self.issue_reads.load(Ordering::SeqCst)
    }

    /// Makes every call fail as gh does once the rate limit is used up,
    /// with `reset` as when it resets where the forge can say
    pub fn set_used_up(&self, reset: Option<Timestamp>) {
        let said = "GraphQL: API rate limit already exceeded for user ID 1.";
        *self.down.lock().unwrap() = Some((said.to_owned(), reset));
    }

    /// Makes every call fail as gh does when GitHub's secondary limit is
    /// passed, with `cooldown` as the headers of the answer
    pub fn set_limited(&self, cooldown: Cooldown) {
        *self.limited.lock().unwrap() = Some(cooldown);
    }

    /// Lets every call through again after [`Self::set_limited`]
    pub fn set_unlimited(&self) {
        *self.limited.lock().unwrap() = None;
    }

    /// Makes every call fail with `said`, as gh prints it
    pub fn set_failing(&self, said: &str) {
        *self.down.lock().unwrap() = Some((said.to_owned(), None));
    }

    /// Makes every call fail as gh does with no network, or lets every call through again
    pub fn set_down(&self, down: bool) {
        let said = "error connecting to api.github.com";
        *self.down.lock().unwrap() = down.then(|| (said.to_owned(), None));
    }

    /// Counts a call, and fails it while the forge is down
    ///
    /// A call the test holds open waits here, which is where a pass holds the
    /// runner's lock.
    pub fn ask(&self) -> Result<(), ForgeError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.wait_if_held(held::At::Any);
        if let Some(cooldown) = *self.limited.lock().unwrap() {
            let said = "gh: You have exceeded a secondary rate limit. (HTTP 403)";
            return Err(ForgeError::Limited {
                said: format!("gh failed: {said}"),
                cooldown,
            });
        }
        match &*self.down.lock().unwrap() {
            Some((said, _)) => Err(ForgeError::Failed(said.clone())),
            None => Ok(()),
        }
    }

    /// When the used-up rate limit resets, where the forge can say
    pub fn reset(&self) -> Option<Timestamp> {
        let down = self.down.lock().unwrap();
        down.as_ref().and_then(|(_, reset)| *reset)
    }
}

impl FakeForge {
    /// Makes reading the rate limits answer `limits`
    pub fn set_rate_limits(&self, limits: RateLimits) {
        self.rate.lock().unwrap().limits = limits;
    }

    /// Makes reading the rate limits fail, or lets it work again. It fails
    /// by itself, apart from the calls [`Self::set_down`] fails.
    pub fn set_rate_limit_down(&self, down: bool) {
        self.rate.lock().unwrap().down = down;
    }

    /// How many times the rate limits were read
    pub fn rate_limit_reads(&self) -> usize {
        self.rate.lock().unwrap().reads
    }

    // Free of the limits, so it answers while the forge is used up, with the
    // core limit at nothing left and resetting when the forge said.
    pub(super) fn read_rate_limit(&self) -> Result<RateLimits, ForgeError> {
        let used_up = self.reset();
        self.wait_if_held(held::At::RateLimit);
        let mut rate = self.rate.lock().unwrap();
        rate.reads += 1;
        if rate.down {
            return Err(ForgeError::Failed(
                "error connecting to api.github.com".to_owned(),
            ));
        }
        let mut limits = rate.limits;
        if let Some(reset) = used_up {
            limits.core = RateBucket {
                used: 5_000,
                remaining: 0,
                reset,
            };
        }
        Ok(limits)
    }
}
