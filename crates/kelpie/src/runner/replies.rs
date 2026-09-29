//! Answering rulings from the webhook's topic
//!
//! On an ntfy webhook a reply on the topic answers a ruling when it names
//! the project and ends with the maintainer's authenticator code (see
//! [`crate::totp`]): `<project> <id> yes <code>`, `<project> <id> no <note>
//! <code>` or `<project> <id> answer <text> <code>`, the part after the
//! project read by `rule`'s own parser. The code is checked against the time
//! ntfy took the reply, which no sender can set. It is never on the topic
//! before the maintainer sends it, so a reader of the topic can read rulings
//! but not answer them, and each step's code answers once, whatever the
//! reply that first sent it said: a right code is claimed before anything
//! else about the reply is read. After [`FAILURES`] wrong codes, answers
//! from the topic are off for every project until `kelpie totp --unlock`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::alert::backoff;
use super::report::StepReport;
use super::trigger::{lock, read_rule};
use super::{Answer, Runner};
use crate::ports::{Alert, AlertError, Alerts, Reply, ReplyWith, Since, Takes, Timestamp};
use crate::relay::Wants;
use crate::settings::SettingsError;
use crate::state::{LastRead, RulingKind, StateError};
use crate::totp::Secret;
use crate::totp::answers::{Answers, Claim, FAILURES, Failure};
use crate::webhook::{Webhook, WebhookKind};

/// Seconds between reads of the topic while a ruling waits on it
pub const READ_EVERY: u64 = 15;

// Failed reads wait twice as long each time, up to five minutes.
const BACKOFF_MAX: u64 = 5 * 60;

// The topic is still read for an hour after the last ruling waiting on it,
// so a late reply is told the ruling is settled.
const LATE: u64 = 60 * 60;

// A read reaches back ten minutes at most. A reply is taken on the code's
// step at the time ntfy took it, so an older one waited out a failed read.
const WINDOW: u64 = 10 * 60;

/// Where reading the topic stands, kept in memory only
#[derive(Debug, Default)]
pub(super) struct Reading {
    /// Replies read and not yet handled, oldest first
    queue: VecDeque<Reply>,
    /// When the topic may be read again
    next: Option<Timestamp>,
    /// Until when the topic is read with no ruling waiting on it
    until: Option<Timestamp>,
    /// Reads failed in a row
    failures: u32,
}

/// What a runner checks replies against: the secret's file, read afresh for
/// each reply so a rotated secret counts at once, and what every runner
/// shares about the codes sent
#[derive(Debug)]
pub(super) struct Authenticator {
    secret: PathBuf,
    answers: Answers,
}

/// A line for the topic, and where to post it
type Line = (Webhook, Alert);

/// What a reply did: a report and maybe a line for the topic, or nothing for
/// kelpie's own posts and anything with no text
type Handled = Option<(StepReport, Option<Line>)>;

/// The authenticator a runner checks replies against: on an ntfy webhook,
/// the one `kelpie totp` keeps under `folder`
///
/// `None` elsewhere. The secret need not exist yet: until it does, alerts
/// say nothing of replies and the topic is not read.
///
/// # Errors
///
/// [`SettingsError::Invalid`] when the secret's file cannot be read, others
/// may read it, or it is not one kelpie wrote.
pub(super) fn authenticator(
    webhook: Option<&Webhook>,
    folder: &Path,
) -> Result<Option<Authenticator>, SettingsError> {
    if !webhook.is_some_and(|w| w.kind == WebhookKind::Ntfy) {
        return Ok(None);
    }
    let secret = folder.join("secret");
    Secret::load(&secret).map_err(|e| SettingsError::Invalid {
        setting: "webhook",
        reason: e.to_string(),
    })?;
    Ok(Some(Authenticator {
        secret,
        answers: Answers::in_folder(folder.to_owned()),
    }))
}

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
        if let Some(report) = lock(runner).read(read) {
            return Some(Ok(report));
        }
    }
}

/// A reply's text without its code, read as the project it names and
/// `rule`'s params
fn read_reply(text: &str) -> Option<(&str, u64, Answer)> {
    let (project, params) = text.trim().split_once(char::is_whitespace)?;
    let (id, answer) = read_rule(params.trim())?;
    Some((project, id, answer))
}

impl StepReport {
    // Records why the line for the topic could not be posted, if it could not.
    fn with_line_failed(mut self, failed: Option<AlertError>) -> Self {
        if let Self::ReplyRefused { line_failed, .. }
        | Self::ReplyToSettled { line_failed, .. }
        | Self::ReplyCodeUsed { line_failed, .. }
        | Self::RepliesLocked { line_failed } = &mut self
        {
            *line_failed = failed.map(|e| e.to_string());
        }
        self
    }
}

impl Runner {
    /// Whether an alerted ruling waits on a reply on the topic, so the
    /// runner looks every [`READ_EVERY`] seconds rather than at the board's
    /// pace
    pub fn awaits_reply(&self) -> bool {
        self.replies_on().is_some() && self.state.rulings.iter().any(|r| r.alerted)
    }

    // The topic, where replies to it are read: an ntfy webhook, a secret,
    // and answers not turned off
    fn replies_on(&self) -> Option<&Webhook> {
        let auth = self.totp.as_ref()?;
        if !auth.secret.exists() || auth.answers.locked() {
            return None;
        }
        self.webhook
            .as_ref()
            .filter(|w| w.kind == WebhookKind::Ntfy)
    }

    /// What ruling `id`'s alert says a reply takes, where replies are read
    pub(super) fn reply_with(&self, id: u64, kind: &RulingKind) -> Option<ReplyWith> {
        self.replies_on()?;
        let takes = match Wants::of(kind) {
            Wants::Answer => Takes::Answer,
            Wants::YesOrNo => Takes::YesOrNo,
        };
        let project = self.project.as_str().to_owned();
        Some(ReplyWith { project, id, takes })
    }

    /// Keeps reading the topic until [`LATE`] after `now`: a ruling's alert
    /// just landed, or a ruling still waits on a reply
    pub(super) fn read_replies_from(&mut self, now: Timestamp) {
        self.reading.until = Some(Timestamp(now.0.saturating_add(LATE)));
    }

    // The topic and where to read it from, while a ruling waits on it or
    // for a while after, once the last read was long enough ago
    fn read_due(&mut self) -> Option<(Webhook, Since)> {
        let now = self.ports.clock.now();
        if self.awaits_reply() {
            self.read_replies_from(now);
        }
        let webhook = self.replies_on()?.clone();
        let reading = &self.reading;
        if reading.until.is_none_or(|until| now >= until) || reading.next.is_some_and(|at| now < at)
        {
            return None;
        }
        // A read after an id ntfy no longer holds returns its whole cache.
        let floor = Timestamp(now.0.saturating_sub(WINDOW));
        let since = match &self.state.replies.last {
            Some(last) if last.time >= floor => Since::After(last.id.clone()),
            _ => Since::Time(floor),
        };
        Some((webhook, since))
    }

    // Queues what a read found, or says when it is tried again
    fn read(&mut self, read: Result<Vec<Reply>, AlertError>) -> Option<StepReport> {
        let now = self.ports.clock.now();
        match read {
            Ok(replies) => {
                self.reading.failures = 0;
                self.reading.next = Some(Timestamp(now.0.saturating_add(READ_EVERY)));
                self.reading.queue.extend(replies);
                None
            }
            Err(e) => {
                self.reading.failures = self.reading.failures.saturating_add(1);
                let wait = backoff(READ_EVERY, self.reading.failures, BACKOFF_MAX);
                let retry_at = Timestamp(now.0.saturating_add(wait));
                self.reading.next = Some(retry_at);
                Some(StepReport::RepliesFailed {
                    reason: e.to_string(),
                    retry_at,
                })
            }
        }
    }

    // Handles the oldest reply read, then moves past it. A save that fails
    // between the two leaves the reply to be read again, when its step is
    // found claimed by it and it is handled again.
    fn handle_next(&mut self) -> Option<Result<Handled, StateError>> {
        let reply = self.reading.queue.pop_front()?;
        let handled = (reply.text.as_deref()).map(|text| self.handle(text, &reply));
        let mut next = self.state.clone();
        next.replies.last = Some(LastRead {
            id: reply.id,
            time: reply.time,
        });
        if let Err(e) = self.save(next) {
            return Some(Err(e));
        }
        Some(Ok(handled))
    }

    fn handle(&mut self, text: &str, reply: &Reply) -> (StepReport, Option<Line>) {
        let ignored = (StepReport::ReplyIgnored, None);
        let Some(webhook) = self.replies_on().cloned() else {
            return ignored;
        };
        let Some(auth) = &self.totp else {
            return ignored;
        };
        let Some((rest, typed)) = text.trim().rsplit_once(char::is_whitespace) else {
            return ignored;
        };
        if typed.len() != 6 || !typed.bytes().all(|b| b.is_ascii_digit()) {
            return ignored;
        }
        let project = self.project.as_str().to_owned();
        let line = |id: Option<u64>, text: String| {
            let title = match id {
                Some(id) => format!("kelpie: {project} ruling {id}"),
                None => "kelpie: answers are off".to_owned(),
            };
            let reply = None;
            Some((webhook.clone(), Alert { title, text, reply }))
        };
        let Ok(Some(secret)) = Secret::load(&auth.secret) else {
            return ignored;
        };
        // A right code is claimed before anything else is read, so no later
        // reply can reuse it, whatever this one said.
        let claim = match secret.verify(typed, reply.time) {
            Some(step) => auth.answers.claim(step, &reply.id, self.ports.clock.now()),
            None => {
                return match auth.answers.fail(&reply.id) {
                    Ok(Failure::LockedNow) => {
                        let text = format!(
                            "Answers from ntfy are off after {FAILURES} wrong codes. \
                             Turn them back on with `kelpie totp --unlock` on the terminal."
                        );
                        let report = StepReport::RepliesLocked { line_failed: None };
                        (report, line(None, text))
                    }
                    Ok(Failure::Counted) | Err(_) => ignored,
                };
            }
        };
        let named = read_reply(rest).filter(|(named, ..)| *named == project);
        let Some((_, id, answer)) = named else {
            return ignored;
        };
        let refused = |reason: String| {
            let text = format!("Ruling {id} was not answered: {reason}.");
            let report = StepReport::ReplyRefused {
                id,
                reason,
                line_failed: None,
            };
            (report, line(Some(id), text))
        };
        match claim {
            Ok(Claim::Ours) => {}
            Ok(Claim::Replayed) => {
                let text = format!(
                    "Ruling {id} was not answered: that code was used already. \
                     Send the reply again with the next one."
                );
                let report = StepReport::ReplyCodeUsed {
                    id,
                    line_failed: None,
                };
                return (report, line(Some(id), text));
            }
            Err(e) => return refused(format!("the code could not be recorded as used: {e}")),
        }
        let _ = auth.answers.forgive();
        let pending = self.state.rulings.iter().any(|r| r.id == id);
        if !pending {
            if id > self.state.last_ruling {
                return ignored;
            }
            let text =
                format!("Ruling {id} on {project} is already settled, so that reply ran nothing.");
            let report = StepReport::ReplyToSettled {
                id,
                line_failed: None,
            };
            return (report, line(Some(id), text));
        }
        match self.rule_and_tell(id, answer) {
            Ok(()) => (StepReport::ReplyAnswered { id }, None),
            Err(e) => refused(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests;
