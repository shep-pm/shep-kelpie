//! The open threads an item at the gate holds its merge on, until the project manager rules
//!
//! A project that names `agents.pm` puts every open thread of its own work item to the
//! project manager before the merge, nits included, and the item waits in CI for its answer.
//! The worker's deferred findings go with them, each as a thread with no forge id.
//! A thread only someone outside the project wrote in reaches no model and holds nothing.
//! A contribution's threads stay the maintainer's, with the merge ruling as it is.

use std::collections::BTreeMap;
use std::path::Path;

use crate::ports::{Finding, ForgeError, Severity};
use crate::runner::Runner;
use crate::runner::follow_up::{deferred_of, leave_deferred, read_deferred};
use crate::runner::pm::{Decision, Ruled, Wake};
use crate::runner::report::{Begin, StepReport};
use crate::runner::review::findings;
use crate::runner::review_bot::{RULED_CLOSE, RULED_FOLLOW, RULED_PUSH, RuledThread};
use crate::runner::turn::KEEP_BODY_TRUE;
use crate::runner::turn::held_up::Reason;
use crate::settings::AgentName;
use crate::state::StateError;
use crate::ticket::TicketId;
use crate::work_item::{FollowUps, Phase, Review, ReviewStage, ThreadAnswer, Turn, WorkItem};

// Replaces a reason the forge's guard refuses, as it names something of this machine's,
// after the lead of the reply it stands in for, so it is still known as kelpie's own.
const UNPOSTABLE: &str = "The project manager's reason names something on the machine kelpie \
                          runs on, so it is not posted.";

// Replaces a reason a `publish` hook of the project's refuses.
const HOOK_REFUSED: &str = "The project manager's reason was refused by a hook of the project's, \
                            so it is not posted.";

/// Each thread kelpie pushed back on, with the last remark in it that was not kelpie's own
///
/// In memory only: a restart asks about the thread once more.
#[derive(Debug, Default)]
pub(in crate::runner) struct PushedBack {
    items: BTreeMap<TicketId, BTreeMap<String, Option<u64>>>,
}

impl PushedBack {
    // Notes that kelpie pushed back on `thread`, whose last remark then was `said`
    fn note(&mut self, ticket: TicketId, thread: &str, said: Option<u64>) {
        self.items
            .entry(ticket)
            .or_default()
            .insert(thread.to_owned(), said);
    }

    // Whether `thread` is still waiting on the maintainer to write in it
    fn waiting(&self, ticket: TicketId, thread: &str, said: Option<u64>) -> bool {
        (self.items.get(&ticket))
            .and_then(|threads| threads.get(thread))
            .is_some_and(|seen| *seen == said)
    }

    // Stops holding `thread` for a reply, which the ruling settled another way
    fn forget(&mut self, ticket: TicketId, thread: &str) {
        let Some(threads) = self.items.get_mut(&ticket) else {
            return;
        };
        threads.remove(thread);
    }

    /// Forgets the work items no longer open
    pub(in crate::runner) fn keep_only(&mut self, open: &[TicketId]) {
        self.items.retain(|ticket, _| open.contains(ticket));
    }
}

impl Runner {
    /// Whether `item`'s open threads go to the project manager before its merge
    ///
    /// A project with no `agents.pm` keeps the maintainer's merge ruling, and a
    /// contribution's threads are the maintainer's to read.
    fn rules_threads(&self) -> bool {
        self.agents.pm.is_some()
            && self
                .current()
                .is_some_and(|item| item.contributor.is_none())
    }

    /// The gate's wait on the project manager's call about `number`'s open threads
    ///
    /// `None` when the gate may go on: no project manager, the item a contribution's,
    /// or no open thread of the project's left to rule on. `Some` when the item waits,
    /// with what it waits on, which `status` and the board name. With the ask out, or
    /// the project manager down, it waits before the pull request's talk is read.
    ///
    /// # Errors
    ///
    /// [`StateError`] when the ask it queues cannot be saved.
    pub(super) fn threads_hold(
        &mut self,
        number: u64,
        head: &str,
    ) -> Result<Option<Begin>, StateError> {
        if !self.rules_threads() {
            return Ok(None);
        }
        let Some(ticket) = self.current().map(|item| item.issue) else {
            return Ok(None);
        };
        if self.pm_threads_out(ticket) {
            return Ok(Some(self.held_for(Reason::ThreadsAsked)));
        }
        // A drain starts no wake, but it holds the item only if a thread needs one
        if !self.draining() && self.pm_held_down() {
            return Ok(Some(self.held_for(Reason::PmDown)));
        }
        let talk = match self.read_talk(number) {
            Ok(Ok(talk)) => talk,
            Ok(Err(e)) => {
                // The threads cannot be told apart from none, so the merge waits
                // for a read that works rather than going past them unruled.
                self.notes
                    .push(format!("cannot read what people wrote on #{number}: {e}"));
                return Ok(Some(self.held_for(Reason::TalkUnread)));
            }
            Err(job) => return Ok(Some(Begin::Reads(job))),
        };
        let Some(item) = self.current().cloned() else {
            return Ok(None);
        };
        let to_rule = self.threads_to_rule(&item, &talk);
        if to_rule.left_out > 0 {
            self.note_threads_left_out(number, to_rule.left_out);
        }
        let mut threads = to_rule.threads;
        match read_deferred(&item) {
            Ok(found) => threads.extend(found.iter().map(ruled_deferred)),
            Err(reason) => return Ok(Some(self.gate_failed(reason))),
        }
        let (waiting, asking) = self.split_backs(ticket, &threads);
        if asking.is_empty() {
            return Ok(match waiting.is_empty() {
                true => None,
                false => Some(self.held_for(Reason::PushBack)),
            });
        }
        let wake = Wake::Threads {
            ticket,
            pull_request: number,
            head: head.to_owned(),
            threads: asking,
        };
        // The ask goes, or waits out the item's own back-off: either way the
        // merge waits on the project manager's answer, the project manager
        // itself being checked for before the talk was read.
        self.pm_ask_threads(wake);
        let reason = match self.draining() {
            true => Reason::Draining,
            false => Reason::ThreadsAsked,
        };
        Ok(Some(self.held_for(reason)))
    }

    /// Carries out the project manager's `ruled` calls on `ticket`'s open threads
    ///
    /// A `close` and a `follow-up` settle the thread, a `push-back` replies and leaves it
    /// open, and every `fix` goes to the worker as one turn. An answer for a head the
    /// item has left acts on nothing. Returns why the forge refused, which the gate asks again.
    pub(in crate::runner) fn threads_ruled(
        &mut self,
        ticket: TicketId,
        head: &str,
        ruled: &[Ruled],
    ) -> Result<(), String> {
        let Some(item) = self.state.item(ticket).cloned() else {
            return Ok(());
        };
        let at_the_gate = match &item.phase {
            Phase::Ci { head: seen, .. } => seen.as_deref().is_none_or(|seen| seen == head),
            _ => false,
        };
        if !at_the_gate {
            self.notes.push(format!(
                "the project manager's answer on {}'s threads came after the item left the gate \
                 at {}, so it acted on none",
                ticket.shown(),
                head.get(..7).unwrap_or(head)
            ));
            return Ok(());
        }
        let Some(number) = item.pull_request else {
            return Ok(());
        };
        let mut fixes: Vec<&Ruled> = Vec::new();
        let mut failed: Option<String> = None;
        for call in ruled {
            if call.thread.id.is_empty() {
                match call.decision {
                    Decision::Fix => fixes.push(call),
                    _ => failed = self.settle_deferred(&item, call).err().or(failed),
                }
                continue;
            }
            let id = call.thread.id.as_str();
            let (lead, body, settle) = match call.decision {
                Decision::Fix => {
                    fixes.push(call);
                    continue;
                }
                Decision::Close => (
                    RULED_CLOSE.to_owned(),
                    format!("{RULED_CLOSE}{}", call.why),
                    true,
                ),
                Decision::PushBack => (
                    RULED_PUSH.to_owned(),
                    format!("{RULED_PUSH}{}", call.why),
                    false,
                ),
                Decision::FollowUp => match self.file_ruled(&item, &call.thread, &call.why) {
                    Ok((lead, said)) => (lead, said, true),
                    Err(why) => {
                        failed = Some(why);
                        continue;
                    }
                },
            };
            if let Err(why) = self.say_on(number, id, (&lead, &body), settle) {
                failed = Some(why);
                continue;
            }
            match call.decision {
                Decision::PushBack => {
                    self.pushed_back.note(ticket, id, call.thread.said);
                }
                Decision::Close | Decision::FollowUp => {
                    self.pushed_back.forget(ticket, id);
                    let id = id.to_owned();
                    self.update_of(ticket, |item| {
                        if !item.threads_accepted.contains(&id) {
                            item.threads_accepted.push(id);
                        }
                    })
                    .map_err(|e| format!("cannot save the settled thread: {e}"))?;
                }
                Decision::Fix => {}
            }
        }
        if !fixes.is_empty() {
            self.threads_fix(&item, &fixes)?;
        }
        match failed {
            Some(why) => Err(why),
            None => Ok(()),
        }
    }

    // The forge would not take what a ruling asked for. Its thread stays open and is
    // asked for again, and the refusal is logged as a gate failure.
    pub(in crate::runner) fn gate_refused(&mut self, issue: TicketId, reason: &str) {
        self.started.push(StepReport::GateFailed {
            issue,
            reason: reason.to_owned(),
        });
    }

    // Files `thread`'s finding as a follow-up issue, and returns what kelpie replies
    // on the thread with, and its lead: the number of the issue it filed or commented on
    fn file_ruled(
        &mut self,
        item: &WorkItem,
        thread: &RuledThread,
        why: &str,
    ) -> Result<(String, String), String> {
        let filed = self.file_finding(item, finding_of(thread, why))?;
        let lead = format!("{RULED_FOLLOW}{}", filed.shown());
        Ok(match why.is_empty() {
            true => (format!("{lead}: "), lead),
            false => (format!("{lead}: "), format!("{lead}: {why}")),
        })
    }

    // Files `finding` as a follow-up issue, and returns the issue it filed or commented on
    fn file_finding(&mut self, item: &WorkItem, finding: Finding) -> Result<TicketId, String> {
        let ticket = item.issue;
        let before = item.follow_ups.clone();
        self.update_of(ticket, |item| {
            item.follow_ups = Some(FollowUps {
                findings: vec![finding],
                ruled: true,
                unread: false,
                first_refused: None,
            });
        })
        .map_err(|e| format!("cannot save the follow-up: {e}"))?;
        let filed = match self.file_follow_ups(ticket) {
            Ok(Ok(Begin::Report(StepReport::FollowUpsFiled {
                opened, commented, ..
            }))) => opened.into_iter().chain(commented).next(),
            Ok(Ok(_)) => None,
            // The forge would not take it, and the gate asks the ruling again.
            Ok(Err(reason)) => return Err(self.unfiled(ticket, before, reason)),
            Err(e) => {
                let reason = format!("cannot file the follow-up: {e}");
                return Err(self.unfiled(ticket, before, reason));
            }
        };
        filed.ok_or_else(|| self.unfiled(ticket, before, "the filing filed nothing".to_owned()))
    }

    // A filing that did not happen takes its finding back off the item, so a later
    // ruling that settles the thread another way does not file it at the merge, and
    // the worker's deferred findings are still read there
    fn unfiled(&mut self, ticket: TicketId, before: Option<FollowUps>, reason: String) -> String {
        match self.update_of(ticket, |item| item.follow_ups = before) {
            Ok(()) => reason,
            Err(e) => format!("{reason}; cannot save the item either: {e}"),
        }
    }

    // Carries out a ruling on a deferred finding, which has no thread to reply on: a
    // `follow-up` files it, and it leaves the deferred findings either way, so the merge
    // does not read it again. A `push-back` has no thread to stay open on and leaves it.
    fn settle_deferred(&mut self, item: &WorkItem, call: &Ruled) -> Result<(), String> {
        if call.decision == Decision::PushBack {
            return Ok(());
        }
        let Some(finding) = deferred_of(item, &call.thread)? else {
            return Ok(());
        };
        if call.decision == Decision::FollowUp {
            self.file_finding(item, finding.clone())?;
        }
        leave_deferred(item, &finding)
    }

    // One worker turn for every thread the project manager ruled `fix`, sent as a
    // round's findings are: the findings file, the threads its lines number, and the
    // fix stage. Its end goes back to the gate, as a pass that has read does.
    fn threads_fix(&mut self, item: &WorkItem, ruled: &[&Ruled]) -> Result<(), String> {
        let Some(number) = item.pull_request else {
            return Err("it has no pull request".to_owned());
        };
        let (ticket, build) = (item.issue, item.build.clone());
        let head = match &item.phase {
            Phase::Ci { head, .. } => head.clone(),
            _ => None,
        };
        let mut findings: Vec<Finding> = Vec::new();
        let mut threads: Vec<String> = Vec::new();
        for call in ruled {
            if !call.thread.id.is_empty() {
                findings.push(finding_of(&call.thread, &call.why));
                threads.push(call.thread.id.clone());
            } else if let Some(found) = deferred_of(item, &call.thread)? {
                // It has no thread to reply on, which its place in the list says.
                threads.push(format!("{DEFERRED_ID}{}", threads.len() + 1));
                findings.push(found);
            }
        }
        if findings.is_empty() {
            return Ok(());
        }
        // Every reviewer the project lists counts as having read, so the fix
        // starts no new pass and its end goes back to the gate.
        let ran: Vec<AgentName> = self.lineup.iter().map(|r| r.name.clone()).collect();
        let round = u32::try_from(ran.len()).map_or(u32::MAX, |n| n.saturating_add(1));
        let path = findings::findings_path(&build);
        findings::write_findings_file(&build, &path, round, &findings, true)?;
        let prompt = ruled_prompt(number, findings.len(), &path);
        let deferred_before = findings::deferred(&build)?;
        self.update_of(ticket, move |item| {
            item.record_held(&findings);
            item.threads_sent = threads;
            item.thread_answers.clear();
            item.turn = Turn::Next { prompt };
            item.counts.fix_turns = item.counts.fix_turns.saturating_add(1);
            item.phase = Phase::Review(Review {
                round,
                stage: ReviewStage::Fixing {
                    head,
                    sent: findings,
                    deferred_before,
                },
                reviewer: None,
                ran,
                failures: 0,
                last: None,
                unread: false,
                bots_only: false,
                reading: None,
                ci: None,
            });
        })
        .map_err(|e| format!("cannot save the fix turn: {e}"))
    }

    /// Takes the deferred findings a fix turn was sent, and the worker answered `fixed` for,
    /// out of the deferred findings file
    ///
    /// Called once the fix pushed, before the answers are forgotten. One the worker
    /// declined or did not mention stays there, so the gate asks again or the merge files it.
    ///
    /// # Errors
    /// A message naming the file when it cannot be read or written.
    pub(in crate::runner) fn leave_fixed_deferred(&self, sent: &[Finding]) -> Result<(), String> {
        let Some(item) = self.current() else {
            return Ok(());
        };
        if item.threads_sent.len() != sent.len() {
            return Ok(());
        }
        let ids = item.threads_sent.iter().zip(sent);
        for (id, finding) in ids.filter(|(id, _)| id.starts_with(DEFERRED_ID)) {
            if matches!(item.answer_of(id), Some(ThreadAnswer::Fixed { .. })) {
                leave_deferred(item, finding)?;
            }
        }
        Ok(())
    }

    // Kelpie's reply on a ruled thread, then the resolve where the ruling settles it.
    // The reply goes first: a resolved thread leaves the gate's reads, so a reply that
    // failed after it would never be tried again. A refused reason is replaced after
    // the reply's `lead`, so the thread settles; a hook that failed to run is retried.
    fn say_on(
        &mut self,
        number: u64,
        id: &str,
        (lead, body): (&str, &str),
        settle: bool,
    ) -> Result<(), String> {
        let app = self.app_covers();
        let (forge, voice) = (&self.ports.forge, &self.ports.voice);
        let reply = |text: &str| match app {
            true => voice.reply_to_thread(&self.remote, id, text),
            false => forge.reply_to_thread(&self.remote, id, text),
        };
        let mut posted = reply(body);
        match &posted {
            Err(ForgeError::LocalPath { .. }) => posted = reply(&format!("{lead}{UNPOSTABLE}")),
            Err(ForgeError::Hook { refusal, .. }) if !refusal.failed => {
                posted = reply(&format!("{lead}{HOOK_REFUSED}"))
            }
            _ => {}
        }
        posted.map_err(|e| format!("cannot reply to a thread on #{number}: {e}"))?;
        if settle {
            let done = match app {
                true => voice.resolve_thread(&self.remote, id),
                false => forge.resolve_thread(&self.remote, id),
            };
            done.map_err(|e| format!("cannot resolve a thread on #{number}: {e}"))?;
        }
        Ok(())
    }

    // `threads`, split into those waiting on the maintainer's reply to a push-back and
    // those to put to the project manager
    fn split_backs(
        &self,
        ticket: TicketId,
        threads: &[RuledThread],
    ) -> (Vec<RuledThread>, Vec<RuledThread>) {
        let waiting = |t: &RuledThread| self.pushed_back.waiting(ticket, &t.id, t.said);
        let kept = |keep: bool| {
            (threads.iter())
                .filter(|t| waiting(t) == keep)
                .cloned()
                .collect()
        };
        (kept(true), kept(false))
    }

    // Idle, naming what the item waits on, which `status` and the board show
    fn held_for(&mut self, reason: Reason) -> Begin {
        self.held_up.because(reason);
        Begin::Idle
    }

    // One line saying that threads no one the project trusts wrote in held nothing
    fn note_threads_left_out(&mut self, number: u64, left: usize) {
        let s = if left == 1 { "" } else { "s" };
        self.notes.push(format!(
            "{left} open thread{s} on #{number} held nothing and reached no ask: only someone \
             without write access wrote in them"
        ));
    }
}

// What the worker is sent for a ruled `fix`: the threads are the project manager's
// call, and its reply answers each by the number it was given
fn ruled_prompt(number: u64, count: usize, path: &Path) -> String {
    format!(
        "The project manager ruled on {count} open review thread(s) on your pull request \
         #{number} that you are to fix here, in {}. Fix each one, then commit and push with \
         `git push origin HEAD`. {KEEP_BODY_TRUE}\nEnd your reply with one line for each \
         thread, as `thread <n>: fixed` or `thread <n>: declined: <the reason>`, using the \
         numbers they had.",
        path.display()
    )
}

// What a wake calls the author of a deferred finding, which no one wrote on the forge
pub(in crate::runner) const DEFERRED_BY: &str = "deferred by the worker";

// The id a deferred finding sent for a fix goes by in `threads_sent`, with its place after
// it. It opens with `COMMENT_REQUEST`, which `answer_sent` takes as a comment with no
// thread to reply on.
const DEFERRED_ID: &str = "comment:deferred ";

// `finding`, which the worker deferred, as a thread with no forge id
fn ruled_deferred(finding: &Finding) -> RuledThread {
    RuledThread {
        id: String::new(),
        path: finding.file.clone(),
        line: Some(finding.line).filter(|line| *line > 0),
        author: DEFERRED_BY.to_owned(),
        severity: Some(finding.severity),
        what: finding.what.clone(),
        maintainer_wrote: false,
        said: None,
    }
}

// `thread` as a finding the worker is sent back with, at the bot's own level where
// it reads one
fn finding_of(thread: &RuledThread, why: &str) -> Finding {
    Finding {
        severity: thread.severity.unwrap_or(Severity::Medium),
        file: thread.path.clone(),
        line: thread.line.unwrap_or(0),
        what: thread.what.clone(),
        why: match why.is_empty() {
            true => thread.what.clone(),
            false => why.to_owned(),
        },
        test: None,
    }
}
