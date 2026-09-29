//! Answering rulings from the webhook's topic
//!
//! On an ntfy webhook each ruling's alert carries a one-time code, drawn
//! from the OS's randomness and saved before the alert is posted. A reply
//! on the topic answers a ruling only when it ends with that ruling's code:
//! `<id> yes <code>`, `<id> no <note> <code>` or `<id> answer <text> <code>`,
//! the rest read by `rule`'s own parser. Anything else is ignored, so the
//! topic's URL alone answers nothing. A code answers only its own ruling,
//! and once that ruling is settled a reply carrying it runs nothing and gets
//! a line on the topic saying so. A refusal from `rule` goes to the topic
//! too. The topic is read every [`READ_EVERY`] seconds while a ruling there
//! waits, and on each step while a code is kept.

use std::collections::VecDeque;
use std::sync::Mutex;

use super::alert::backoff;
use super::report::StepReport;
use super::trigger::{lock, read_rule};
use super::{Answer, Runner};
use crate::ports::{
    Alert, AlertError, Alerts, OneTimeCode, Reply, ReplyWith, Since, Takes, Timestamp,
};
use crate::relay::Wants;
use crate::state::{Code, RulingKind, StateError};
use crate::webhook::{Webhook, WebhookKind};

/// Seconds between reads of the topic
pub const READ_EVERY: u64 = 15;

// Failed reads wait twice as long each time, up to five minutes.
const BACKOFF_MAX: u64 = 5 * 60;

// A settled ruling's code is kept a day, so a late reply is told it is settled.
const KEEP_SETTLED: u64 = 24 * 60 * 60;

/// Where reading the topic stands, kept in memory only
#[derive(Debug, Default)]
pub(super) struct Reading {
    /// Replies read and not yet handled, oldest first
    queue: VecDeque<Reply>,
    /// When the topic may be read again
    next: Option<Timestamp>,
    /// Reads failed in a row
    failures: u32,
}

/// A line for the topic, and where to post it
type Line = (Webhook, Alert);

/// What a reply did: a report and maybe a line for the topic, or nothing for
/// kelpie's own posts and anything with no text
type Handled = Option<(StepReport, Option<Line>)>;

/// Handles the oldest reply read, reading the topic first when none is
/// waiting and a read is due
///
/// Returns `None` when there is nothing to handle.
pub(super) fn answer_replies(
    runner: &Mutex<Runner>,
    alerts: &dyn Alerts,
) -> Option<Result<StepReport, StateError>> {
    loop {
        let handled = lock(runner).handle_next();
        match handled {
            // Nothing: kelpie's own post, or no text
            Some(Ok(None)) => continue,
            Some(Ok(Some((report, line)))) => {
                let failed = line.and_then(|(webhook, alert)| alerts.post(&webhook, &alert).err());
                return Some(Ok(report.with_line_failed(failed)));
            }
            Some(Err(e)) => return Some(Err(e)),
            None => {}
        }
        let (webhook, since) = lock(runner).read_due()?;
        let read = alerts.replies(&webhook, &since);
        match lock(runner).read(read) {
            Ok(None) => {}
            Ok(Some(report)) => return Some(Ok(report)),
            Err(e) => return Some(Err(e)),
        }
    }
}

/// A reply's text read as `rule`'s params and the code after them
fn read_reply(text: &str) -> Option<(u64, Answer, &str)> {
    let (params, code) = text.trim().rsplit_once(char::is_whitespace)?;
    let (id, answer) = read_rule(params.trim_end())?;
    Some((id, answer, code))
}

impl StepReport {
    // Records why the line for the topic could not be posted, if it could not.
    fn with_line_failed(mut self, failed: Option<AlertError>) -> Self {
        if let Self::ReplyRefused { line_failed, .. } | Self::ReplyToSettled { line_failed, .. } =
            &mut self
        {
            *line_failed = failed.map(|e| e.to_string());
        }
        self
    }
}

impl Runner {
    /// Whether a ruling waits on a reply on the topic, so the runner looks
    /// every [`READ_EVERY`] seconds rather than at the board's pace
    pub fn awaits_reply(&self) -> bool {
        let alerted = |code: &Code| {
            let ruling = self.state.rulings.iter().find(|r| r.id == code.ruling);
            ruling.is_some_and(|r| r.alerted)
        };
        self.ntfy().is_some() && self.state.replies.codes.iter().any(alerted)
    }

    // Whether a reply can carry `code` yet: its alert landed, or its ruling
    // is settled, perhaps after one did
    fn out(&self, code: &Code) -> bool {
        let ruling = self.state.rulings.iter().find(|r| r.id == code.ruling);
        ruling.is_none_or(|r| r.alerted)
    }

    fn ntfy(&self) -> Option<&Webhook> {
        self.webhook
            .as_ref()
            .filter(|w| w.kind == WebhookKind::Ntfy)
    }

    /// What ruling `id`'s alert carries for a reply on a webhook that takes
    /// replies, drawing and saving its code the first time
    ///
    /// A code the OS cannot draw leaves the ruling to be answered some other
    /// way: its alert goes out with no reply.
    pub(super) fn reply_with(
        &mut self,
        id: u64,
        kind: &RulingKind,
    ) -> Result<Option<ReplyWith>, StateError> {
        if self.ntfy().is_none() {
            return Ok(None);
        }
        let held = self.state.replies.codes.iter().find(|c| c.ruling == id);
        let code = match held {
            Some(held) => held.code.clone(),
            None => {
                let Ok(code) = OneTimeCode::draw() else {
                    return Ok(None);
                };
                let mut next = self.state.clone();
                next.replies.codes.push(Code {
                    ruling: id,
                    code: code.clone(),
                    drawn: self.ports.clock.now(),
                    settled: None,
                });
                self.save(next)?;
                code
            }
        };
        let takes = match (Wants::of(kind), kind) {
            (Wants::Answer, _) => Takes::Answer,
            (Wants::YesOrNo, RulingKind::Merge { .. }) => Takes::YesOrNo { yes: "Merge" },
            (Wants::YesOrNo, _) => Takes::YesOrNo { yes: "Yes" },
        };
        Ok(Some(ReplyWith { id, code, takes }))
    }

    // The topic and where to read it from, when a code is kept and the
    // last read was long enough ago
    fn read_due(&self) -> Option<(Webhook, Since)> {
        let webhook = self.ntfy()?;
        let now = self.ports.clock.now();
        if self.reading.next.is_some_and(|at| now < at) {
            return None;
        }
        let codes = &self.state.replies.codes;
        let first = codes
            .first()
            .filter(|_| codes.iter().any(|c| self.out(c)))?;
        let since = match &self.state.replies.after {
            Some(id) => Since::After(id.clone()),
            None => Since::Time(first.drawn),
        };
        Some((webhook.clone(), since))
    }

    // Queues what a read found, marks the codes of rulings now settled, and
    // lets go of those settled a day ago
    fn read(
        &mut self,
        read: Result<Vec<Reply>, AlertError>,
    ) -> Result<Option<StepReport>, StateError> {
        let now = self.ports.clock.now();
        let replies = match read {
            Ok(replies) => replies,
            Err(e) => {
                self.reading.failures = self.reading.failures.saturating_add(1);
                let wait = backoff(READ_EVERY, self.reading.failures, BACKOFF_MAX);
                let retry_at = Timestamp(now.0.saturating_add(wait));
                self.reading.next = Some(retry_at);
                return Ok(Some(StepReport::RepliesFailed {
                    reason: e.to_string(),
                    retry_at,
                }));
            }
        };
        self.reading.failures = 0;
        self.reading.next = Some(Timestamp(now.0.saturating_add(READ_EVERY)));
        self.reading.queue.extend(replies);
        let mut codes = self.state.replies.codes.clone();
        for code in &mut codes {
            if code.settled.is_none() && !self.state.rulings.iter().any(|r| r.id == code.ruling) {
                code.settled = Some(now);
            }
        }
        codes.retain(|c| {
            c.settled
                .is_none_or(|at| now.0.saturating_sub(at.0) < KEEP_SETTLED)
        });
        if codes != self.state.replies.codes {
            let mut next = self.state.clone();
            next.replies.codes = codes;
            self.save(next)?;
        }
        Ok(None)
    }

    // Handles the oldest reply read, then moves past it. A save that fails
    // between the two leaves the reply to be read again, when a code that
    // answered it finds its ruling settled.
    fn handle_next(&mut self) -> Option<Result<Handled, StateError>> {
        let reply = self.reading.queue.pop_front()?;
        let handled = reply.text.as_deref().map(|text| self.handle(text));
        let mut next = self.state.clone();
        next.replies.after = Some(reply.id);
        if let Err(e) = self.save(next) {
            return Some(Err(e));
        }
        Some(Ok(handled))
    }

    fn handle(&mut self, text: &str) -> (StepReport, Option<Line>) {
        let project = self.project.as_str().to_owned();
        let Some((id, answer, typed)) = read_reply(text) else {
            return (StepReport::ReplyIgnored, None);
        };
        let codes = &self.state.replies.codes;
        let held = codes.iter().find(|c| c.ruling == id);
        if !held.is_some_and(|held| held.code.matches(typed)) {
            return (StepReport::ReplyIgnored, None);
        }
        let webhook = self.ntfy().cloned().expect("replies are read from ntfy");
        let line = |text: String| {
            let title = format!("kelpie: {project} ruling {id}");
            Some((
                webhook.clone(),
                Alert {
                    title,
                    text,
                    reply: None,
                },
            ))
        };
        if !self.state.rulings.iter().any(|r| r.id == id) {
            let text = format!("Ruling {id} is already settled, so that reply ran nothing.");
            return (
                StepReport::ReplyToSettled {
                    id,
                    line_failed: None,
                },
                line(text),
            );
        }
        match self.rule_and_tell(id, answer) {
            Ok(()) => (StepReport::ReplyAnswered { id }, None),
            Err(e) => {
                let reason = e.to_string();
                let text = format!("Ruling {id} was not answered: {reason}.");
                let report = StepReport::ReplyRefused {
                    id,
                    reason,
                    line_failed: None,
                };
                (report, line(text))
            }
        }
    }
}

#[cfg(test)]
mod tests;
