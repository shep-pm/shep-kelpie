//! A pull request head the forge is slow to catch up to
//!
//! The forge's head lags a push by a moment, and a step idles until the two
//! match. A forge that stops updating the pull request never lets them, so one
//! alert names both heads once the lag passes [`LAG_LIMIT`].

use std::cell::RefCell;
use std::collections::BTreeMap;

use super::super::Runner;
use super::super::report::Begin;
use super::short;
use crate::ports::{Alert, Timestamp};
use crate::ticket::TicketId;

/// Seconds a forge's head may differ from the branch before kelpie says so
pub(super) const LAG_LIMIT: u64 = 15 * 60;

/// When each work item's head first lagged, and whether that was said, in memory only
///
/// A restart forgets a lag, so it costs one more wait and no wrong state.
#[derive(Debug, Default)]
pub(in crate::runner) struct Lags(RefCell<BTreeMap<TicketId, Lag>>);

#[derive(Debug, Clone, Copy)]
struct Lag {
    since: Timestamp,
    alerted: bool,
}

impl Lags {
    /// Forgets the lag of the work item for `ticket`, whose head matches or which ended
    pub(in crate::runner) fn forget(&self, ticket: TicketId) {
        self.0.borrow_mut().remove(&ticket);
    }
}

impl Runner {
    // The step finds the forge's head `forged` still not the branch's. Past the limit, it
    // alerts once.
    pub(in crate::runner) fn lag_goes_on(&self, forged: &str) -> Result<(), Box<Begin>> {
        let Some(ticket) = self.current().map(|item| item.issue) else {
            return Ok(());
        };
        let now = self.ports.clock.now();
        let lag = *self.lags.0.borrow_mut().entry(ticket).or_insert(Lag {
            since: now,
            alerted: false,
        });
        if lag.alerted || now.0.saturating_sub(lag.since.0) < LAG_LIMIT {
            return Ok(());
        }
        let pushed = self.head_on_origin()?;
        self.lags.0.borrow_mut().insert(
            ticket,
            Lag {
                alerted: true,
                ..lag
            },
        );
        self.say_lagging(ticket, forged, pushed);
        Ok(())
    }

    // Logs that the forge never caught up to the branch, and posts it where a webhook is set
    fn say_lagging(&self, ticket: TicketId, forged: &str, pushed: Result<String, String>) {
        let number = self.current().and_then(|item| item.pull_request);
        let pull_request =
            number.map_or_else(|| "its pull request".to_owned(), |n| format!("#{n}"));
        let branch = match pushed {
            Ok(head) => short(&head).to_owned(),
            Err(e) => format!("unreadable ({e})"),
        };
        let text = format!(
            "The forge has shown {pull_request} at {} for {} minutes, but its branch on \
             origin is at {branch}. Reopen the pull request or push again.",
            short(forged),
            LAG_LIMIT / 60
        );
        self.notes.push(format!("{} {text}", ticket.shown()));
        let Some(webhook) = self.webhook.clone() else {
            return;
        };
        let alert = Alert {
            title: format!(
                "kelpie: {} {} lags the forge",
                self.project.as_str(),
                ticket.shown()
            ),
            text,
            reply: None,
        };
        if let Err(e) = self.ports.alerts.post(&webhook, &alert) {
            self.notes
                .push(format!("cannot post that {pull_request} lags: {e}"));
        }
    }
}
