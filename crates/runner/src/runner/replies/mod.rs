//! Answering rulings from the webhook's topic
//!
//! On an ntfy webhook a reply answers a ruling when it ends with the
//! maintainer's authenticator code (see [`crate::totp`]): `<id> yes <code>`,
//! `<id> no <note> <code>` or `<id> <text> <code>`. The code is checked
//! against the time ntfy took the reply, which no sender can set, and each
//! step's code answers once. After [`FAILURES`] wrong codes, answers from the
//! topic are off for every project until `shep kelpie totp --unlock`. A
//! webhook marked `trusted` (see [`Webhook`]) needs no code, except a yes on
//! a merge ruling, since the topic alone never merges.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::alert::backoff;
use super::report::StepReport;
use super::thread_replies::Settler;
use super::trigger::lock;
use super::words::{Wants, read_answer};
use super::{Answer, Runner};
use crate::ports::{Alert, AlertError, Alerts, Reply, ReplyWith, Since, Takes, Timestamp};
use crate::settings::SettingsError;
use crate::state::{LastRead, RulingKind, StateError};
use crate::ticket::TicketId;
use crate::totp::answers::{Answers, Claim, FAILURES, Failure};
use crate::totp::{STEP, Secret, steps_near};
use crate::webhook::{Webhook, WebhookKind};
use whose::{Named, Whose, read_reply};

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
    /// Reads failed in a row, or tries to spend the codes of the reply at
    /// the head of the queue
    failures: u32,
    /// Whether the reply at the head of the queue could not have its codes
    /// spent, and waits there until `next`
    held: bool,
}

/// What spending a reply's codes found: the secret they were checked
/// against, and the step of each right code, with whose it is
struct Spent {
    secret: Option<Secret>,
    claims: Vec<(u64, Claim)>,
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
/// the one `shep kelpie totp` keeps under `folder`
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
    // parity: webhook.replies
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

impl StepReport {
    // Records why the line for the topic could not be posted, if it could not.
    fn with_line_failed(mut self, failed: Option<AlertError>) -> Self {
        if let Self::ReplyRefused { line_failed, .. }
        | Self::ReplyToSettled { line_failed, .. }
        | Self::ReplyCodeUsed { line_failed, .. }
        | Self::RepliesLocked { line_failed }
        | Self::ReplyUnreadable { line_failed }
        | Self::ReplyTooLong { line_failed } = &mut self
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
        let webhook = self
            .webhook
            .as_ref()
            // parity: webhook.replies
            .filter(|w| w.kind == WebhookKind::Ntfy)?;
        // A trusted topic is still read once coded answers are off, since
        // the replies that need no code are not.
        if !auth.secret.exists() || (auth.answers.locked() && !webhook.trusted) {
            return None;
        }
        Some(webhook)
    }

    // Whether the wrong-code lock holds: only a trusted topic is read then.
    fn coded_answers_off(&self) -> bool {
        self.totp.as_ref().is_some_and(|auth| auth.answers.locked())
    }

    /// What the alert of the ruling numbered `number` says a reply takes, where
    /// replies are read
    pub(super) fn reply_with(&self, ticket: TicketId, kind: &RulingKind) -> Option<ReplyWith> {
        self.replies_on()?;
        let takes = match Wants::of(kind) {
            Wants::Answer => Takes::Answer,
            // A coded yes is dropped while answers are off, so the alert does not offer it.
            Wants::Merge if self.coded_answers_off() => Takes::MergeCodedOff,
            Wants::Merge => Takes::Merge,
            // A security ruling's alert names no head: a bare yes there is
            // refused for the head the runner holds.
            Wants::Trust | Wants::YesOrNo => Takes::YesOrNo,
        };
        Some(ReplyWith { ticket, takes })
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
        // A read after an id ntfy no longer holds returns its whole cache. It
        // reaches three steps past the window, so the code of a reply just too
        // old to act on is still spent before a replay of it is read. A reply
        // spends the next step's code too, which a replay can still send.
        let floor = Timestamp(now.0.saturating_sub(WINDOW + 3 * STEP));
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

    // Handles the oldest reply read, then moves past it. Its codes are spent
    // first, and when they cannot be, it stays at the head of the queue and no
    // later reply is handled before it. A save that fails between handling and
    // moving on leaves the reply to be read again, and found claimed by it.
    fn handle_next(&mut self) -> Option<Result<Handled, StateError>> {
        let now = self.ports.clock.now();
        if self.reading.held && self.reading.next.is_some_and(|at| now < at) {
            return None;
        }
        let reply = self.reading.queue.front()?.clone();
        let spent = match self.spend(&reply) {
            Ok(spent) => spent,
            Err(reason) => {
                self.reading.held = true;
                self.reading.failures = self.reading.failures.saturating_add(1);
                let wait = backoff(READ_EVERY, self.reading.failures, BACKOFF_MAX);
                let retry_at = Timestamp(now.0.saturating_add(wait));
                self.reading.next = Some(retry_at);
                return Some(Ok(Some((
                    StepReport::RepliesFailed { reason, retry_at },
                    None,
                ))));
            }
        };
        self.reading.held = false;
        self.reading.queue.pop_front();
        let handled = if reply.cut {
            self.too_long()
        } else {
            (reply.text.as_deref()).map(|text| self.handle(text, &reply, spent))
        };
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

    // Claims the step of every right code anywhere in `reply`, before anything
    // else about it is read: whether answers are on, which project it names,
    // how old it is, or how the code was typed. Every text the post carries
    // counts, its title included. A secret that cannot be read is an error.
    fn spend(&self, reply: &Reply) -> Result<Spent, String> {
        let Some(auth) = &self.totp else {
            return Ok(Spent {
                secret: None,
                claims: Vec::new(),
            });
        };
        let secret = Secret::load(&auth.secret).map_err(|e| e.to_string())?;
        let Some(secret) = secret else {
            // No secret, no code can answer anything.
            return Ok(Spent {
                secret: None,
                claims: Vec::new(),
            });
        };
        let steps = if reply.cut {
            steps_near(reply.time).to_vec()
        } else {
            secret.steps_in(&reply.said, reply.time)
        };
        let now = self.ports.clock.now();
        let mut claims = Vec::new();
        for step in steps {
            let claim = auth.answers.claim(step, &reply.id, now);
            let claim = claim.map_err(|e| format!("cannot record a code as used: {e}"))?;
            claims.push((step, claim));
        }
        Ok(Spent {
            secret: Some(secret),
            claims,
        })
    }

    // A line for the topic about the ruling `number`, or about answers
    // as a whole
    fn line(&self, number: Option<TicketId>, text: String) -> Option<Line> {
        // Not `replies_on`: the line saying answers are off goes out once they are.
        let ntfy = self
            .webhook
            .as_ref()
            // parity: webhook.replies
            .filter(|w| w.kind == WebhookKind::Ntfy);
        let webhook = ntfy?.clone();
        let project = self.project.as_str();
        let title = match number {
            Some(number) => format!("kelpie: {project} ruling {}", number.shown()),
            None => format!("kelpie: {project} answers"),
        };
        let reply = None;
        Some((webhook, Alert { title, text, reply }))
    }

    // A post kelpie cannot read in full answers nothing, and says so.
    fn too_long(&self) -> Handled {
        self.replies_on()?;
        let text = "That reply was too long to read, so it answered nothing and any code \
                    in it is spent. Send a shorter reply with a new code."
            .to_owned();
        let report = StepReport::ReplyTooLong { line_failed: None };
        Some((report, self.line(None, text)))
    }

    fn handle(&mut self, text: &str, reply: &Reply, spent: Spent) -> (StepReport, Option<Line>) {
        let ignored = (StepReport::ReplyIgnored, None);
        if self.replies_on().is_none() {
            return ignored;
        }
        let (Some(auth), Some(secret)) = (&self.totp, spent.secret) else {
            return ignored;
        };
        let project = self.project.as_str().to_owned();
        let now = self.ports.clock.now();
        // Older than the window, a reply is read only so its code is spent.
        if reply.time.0 < now.0.saturating_sub(WINDOW) {
            return ignored;
        }
        let trusted = self.webhook.as_ref().is_some_and(|w| w.trusted);
        // Only the exact shape carries a code: six digits, last. On a trusted
        // topic a reply without one is the maintainer's all the same.
        let typed = match text.trim().rsplit_once(char::is_whitespace) {
            Some((rest, typed))
                if typed.len() == 6 && typed.bytes().all(|b| b.is_ascii_digit()) =>
            {
                Some((rest, typed))
            }
            _ => None,
        };
        let (rest, claim) = match typed {
            // Coded answers are off, though uncoded ones on a trusted topic are not.
            Some(_) if auth.answers.locked() => return ignored,
            Some((rest, typed)) => match secret.verify(typed, reply.time) {
                // Every step `verify` takes was spent above, from this text.
                Some(step) => {
                    let claim = spent.claims.iter().find(|(claimed, _)| *claimed == step);
                    let Some((_, claim)) = claim else {
                        return ignored;
                    };
                    (rest, Some(*claim))
                }
                None => {
                    return match auth.answers.fail(&reply.id) {
                        Ok(Failure::LockedNow) => {
                            let text = self.locked_line(trusted);
                            let report = StepReport::RepliesLocked { line_failed: None };
                            (report, self.line(None, text))
                        }
                        Ok(Failure::Counted) | Err(_) => ignored,
                    };
                }
            },
            None if trusted => (text.trim(), None),
            None => return ignored,
        };
        let Some(read) = read_reply(rest) else {
            // Chatter on a trusted topic is not worth a line from every project.
            if claim.is_none() {
                return ignored;
            }
            // Every project reads the one topic and finds the code claimed by
            // this very reply, so each posts the line under its own title, as
            // `too_long` does.
            let text = "That reply named no ruling kelpie could read, so it answered nothing \
                        and its code is spent. Send `<ruling> <answer> <code>` with a new code."
                .to_owned();
            let report = StepReport::ReplyUnreadable { line_failed: None };
            return (report, self.line(None, text));
        };
        let Named { whose, id } = self.which(&read);
        let (number, words) = (read.ticket, read.words);
        if whose == Whose::Theirs {
            return ignored;
        }
        if claim == Some(Claim::Replayed) {
            let shown = self.ruling_number(id);
            let text = format!(
                "Ruling {} was not answered: that code was used already. \
                 Send the reply again with the next one.",
                shown.shown()
            );
            let report = StepReport::ReplyCodeUsed {
                id,
                line_failed: None,
            };
            return (report, self.line(Some(shown), text));
        }
        // Only a right code forgives the wrong ones before it.
        if claim.is_some() {
            let _ = auth.answers.forgive();
        }
        match whose {
            Whose::Unsure(why) => {
                let code = if claim.is_some() { " <code>" } else { "" };
                let reason = format!(
                    "{why}, so name the project first: `{project} {number} <answer>{code}`"
                );
                return self.reply_refused(id, reason);
            }
            Whose::Several(why) => return self.reply_refused(id, why),
            Whose::Ours | Whose::Theirs => {}
        }
        let pending = self.state.rulings.iter().find(|r| r.id == id);
        let Some(pending) = pending else {
            let text = format!(
                "Ruling {} on {project} is already settled, so that reply ran nothing.",
                number.shown()
            );
            let report = StepReport::ReplyToSettled {
                id,
                line_failed: None,
            };
            return (report, self.line(Some(number), text));
        };
        // A number can name a new ruling on the same issue or pull request,
        // so a reply from before this one was raised may answer another question.
        if pending.raised.is_some_and(|raised| reply.time < raised) {
            let asks = pending.question.lines().next().unwrap_or_default();
            let reason = format!(
                "the ruling changed since that reply was written, and it asks now: {asks}. \
                 Reply again to answer it"
            );
            return self.reply_refused(id, reason);
        }
        let wants = Wants::of(&pending.kind);
        let answer = match read_answer(wants, words) {
            Ok(answer) => answer,
            Err(reason) => return self.reply_refused(id, reason),
        };
        // The topic alone never merges: a trusted one still takes a code for that.
        let shown = pending.number();
        if claim.is_none() && wants == Wants::Merge && answer == Answer::Yes {
            // While coded answers are off, the code the line would ask for is dropped too.
            let reason = if auth.answers.locked() {
                format!(
                    "a yes on a merge ruling takes the authenticator code, and coded answers \
                     are off: turn them back on with `shep kelpie totp --unlock` on the \
                     terminal, then send `{shown} yes <code>`"
                )
            } else {
                format!(
                    "a yes on a merge ruling takes the authenticator code: `{shown} yes <code>`"
                )
            };
            return self.reply_refused(id, reason);
        }
        match self.rule_by(id, answer, Settler::Alert) {
            Ok(()) => (StepReport::ReplyAnswered { id }, None),
            Err(e) => self.reply_refused(id, e.to_string()),
        }
    }

    // The line saying coded answers are off. On a trusted topic the others are not.
    fn locked_line(&self, trusted: bool) -> String {
        if trusted {
            return format!(
                "Coded answers from ntfy are off after {FAILURES} wrong codes, and replies \
                 that need no code still work. Turn them back on with \
                 `shep kelpie totp --unlock` on the terminal."
            );
        }
        format!(
            "Answers from ntfy are off after {FAILURES} wrong codes. \
             Turn them back on with `shep kelpie totp --unlock` on the terminal. \
             Anyone can post to a topic whose name they know, so think \
             about moving to a new one."
        )
    }

    // A reply that answered nothing, and the line saying why.
    fn reply_refused(&self, id: TicketId, reason: String) -> (StepReport, Option<Line>) {
        let shown = self.ruling_number(id);
        let text = format!("Ruling {} was not answered: {reason}.", shown.shown());
        let report = StepReport::ReplyRefused {
            id,
            reason,
            line_failed: None,
        };
        (report, self.line(Some(shown), text))
    }
}

mod whose;

#[cfg(test)]
mod tests;
