//! The project manager's call on each open review thread of an item at the gate
//!
//! A wake lists the threads, numbered from 1, and the answer rules on each by
//! its number. A thread the answer says nothing valid of stays open, and the
//! item's next ask waits out a back-off that grows as the wake's `none`s do.

use std::fmt::Write as _;

use serde_json::Value;

use super::Wake;
use super::act::{WORDS, plain};
use super::answer::{printable, ticket};
use super::case::case_file;
use super::red::short;
use super::wakes::quiet_backoff;
use crate::board::briefing::ticket_ref;
use crate::ports::Timestamp;
use crate::runner::Runner;
use crate::runner::review_bot::RuledThread;
use crate::state::StateError;
use crate::ticket::TicketId;

/// What a `threads` entry holds, as a dropped one names it
pub(super) const THREADS: &str = "an `item`, a `thread` number from the wake, a `decision` of \
                                  fix, follow-up, close or push-back, and a `why` that close \
                                  and push-back need";

/// What the project manager decides about one open thread
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Decision {
    /// The worker fixes it in this pull request
    Fix,
    /// It is filed to fix later
    FollowUp,
    /// The thread is wrong: kelpie replies with the `why` and resolves it
    Close,
    /// The maintainer's own thread stays open, and kelpie replies with the `why`
    PushBack,
}

impl Decision {
    /// The decision as the answer spells it
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Fix => "fix",
            Self::FollowUp => "follow-up",
            Self::Close => "close",
            Self::PushBack => "push-back",
        }
    }
}

/// The project manager's call on one open thread, as the answer gives it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ThreadCall {
    /// The work item's ticket
    pub(super) item: TicketId,
    /// The thread's number from 1 in the wake
    pub(super) thread: usize,
    /// What to do
    pub(super) decision: Decision,
    /// Why, which kelpie replies with on a close or a push-back
    pub(super) why: String,
}

/// A call that held, with the thread it is about
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Ruled {
    /// The thread
    pub(super) thread: RuledThread,
    /// What to do
    pub(super) decision: Decision,
    /// Why
    pub(super) why: String,
}

/// How many `none`s in a row an item's threads were asked for, and when its
/// next ask may go
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the step that asks is a later slice")
)]
#[derive(Debug, Clone, Copy)]
pub(super) struct Quiet {
    nones: u32,
    until: Timestamp,
}

pub(super) fn thread_call(value: &Value) -> Option<ThreadCall> {
    let item = ticket(value.get("item")?)?;
    let thread = usize::try_from(value.get("thread")?.as_u64()?).ok()?;
    let decision = match value.get("decision")?.as_str()?.trim() {
        "fix" => Decision::Fix,
        "follow-up" => Decision::FollowUp,
        "close" => Decision::Close,
        "push-back" => Decision::PushBack,
        _ => return None,
    };
    let why = match value.get("why") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => printable(text.trim()),
        Some(_) => return None,
    };
    let needs_why = matches!(decision, Decision::Close | Decision::PushBack);
    if thread == 0 || (needs_why && why.is_empty()) {
        return None;
    }
    Some(ThreadCall {
        item,
        thread,
        decision,
        why,
    })
}

/// What a wake says of `ticket`'s open `threads`: one line each, none left out
pub(super) fn wake_text(
    ticket: TicketId,
    pull_request: u64,
    head: &str,
    threads: &[RuledThread],
) -> String {
    let one_line = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let s = if threads.len() == 1 { "" } else { "s" };
    let mut out = format!(
        "{}'s pull request #{pull_request} waits on you for {} open review thread{s} at {}. \
         Rule on each by its number with `threads`: `close` means the thread is wrong. Its \
         whole thread is in `{}`:\n",
        ticket_ref(ticket),
        threads.len(),
        short(head),
        case_file(ticket)
    );
    for (n, thread) in threads.iter().enumerate() {
        let file = one_line(&thread.path);
        let at = match thread.line {
            Some(line) => format!("{file}:{line}"),
            None => file,
        };
        let graded = match &thread.severity {
            Some(severity) => format!(", {}", format!("{severity:?}").to_uppercase()),
            None => String::new(),
        };
        let wrote = match thread.maintainer_wrote {
            true => ", the maintainer wrote in it",
            false => "",
        };
        let _ = write!(
            out,
            "\n{}. {at} ({}{graded}{wrote}): {}",
            n + 1,
            one_line(&thread.author),
            one_line(&thread.what)
        );
    }
    out
}

impl Runner {
    /// Queues `wake`, which asks the project manager what becomes of its
    /// item's open threads, and says whether the item now waits on it
    ///
    /// The ticket is noted until the call ends. False is returned, and
    /// nothing queued, when no call can run, the runner drains, or a wake
    /// that answered nothing still holds the item's next ask back.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the step that asks is a later slice")
    )]
    pub(super) fn pm_ask_threads(&mut self, wake: Wake) -> bool {
        let Wake::Threads { ticket, .. } = &wake else {
            return false;
        };
        let ticket = *ticket;
        if self.pm.threads_out.contains(&ticket) {
            return true;
        }
        let now = self.ports.clock.now();
        let held = (self.pm.threads_quiet.get(&ticket)).is_some_and(|quiet| now < quiet.until);
        if held || self.draining() || !self.pm_available() {
            return false;
        }
        self.pm.threads_out.insert(ticket);
        self.pm_due(wake);
        true
    }

    /// Whether `ticket`'s open threads were put to the project manager and
    /// its call has not ended
    pub(super) fn pm_threads_out(&self, ticket: TicketId) -> bool {
        self.pm.threads_out.contains(&ticket)
    }

    /// Checks the answer's `calls` against the threads `wakes` carried, hands
    /// those that hold to [`Runner::threads_ruled`], and ends each ask
    ///
    /// Returns what it did and what it dropped, each in words. An item with a
    /// thread left unanswered holds its next ask back, and one with a thread
    /// answered starts the count again.
    ///
    /// # Errors
    ///
    /// [`StateError`] when what it did cannot be saved.
    pub(super) fn rule_threads(
        &mut self,
        wakes: &[Wake],
        calls: &[ThreadCall],
    ) -> Result<(Vec<String>, Vec<String>), StateError> {
        let (mut acted, mut dropped) = (Vec::new(), Vec::new());
        let mut seen: Vec<(TicketId, usize)> = Vec::new();
        let mut held: Vec<(TicketId, Ruled)> = Vec::new();
        for call in calls {
            let ThreadCall {
                item,
                thread: number,
                decision,
                why,
            } = call;
            let (named, how) = (ticket_ref(*item), decision.as_str());
            let asked = wakes.iter().find_map(|wake| match wake {
                Wake::Threads {
                    ticket, threads, ..
                } if ticket == item => Some(threads),
                _ => None,
            });
            let Some(asked) = asked else {
                dropped.push(format!(
                    "{how} {named} thread {number}: no ask of it waits on you"
                ));
                continue;
            };
            let Some(thread) = asked.get(number - 1) else {
                dropped.push(format!(
                    "{how} {named} thread {number}: the wake lists no such thread"
                ));
                continue;
            };
            if seen.contains(&(*item, *number)) {
                dropped.push(format!(
                    "{how} {named} thread {number}: the answer names it twice"
                ));
                continue;
            }
            seen.push((*item, *number));
            if *decision == Decision::Close && thread.maintainer_wrote {
                dropped.push(format!(
                    "close {named} thread {number}: the maintainer wrote in it, so a close is \
                     refused and it stays open; give `push-back` with a `why` instead"
                ));
                continue;
            }
            let why = plain(&self.shown(why.clone()), WORDS);
            let because = match why.is_empty() {
                true => String::new(),
                false => format!(": {why}"),
            };
            acted.push(format!(
                "ruled thread {number} of {named} as {how}{because}"
            ));
            held.push((
                *item,
                Ruled {
                    thread: thread.clone(),
                    decision: *decision,
                    why: call.why.clone(),
                },
            ));
        }
        for wake in wakes {
            let Wake::Threads {
                ticket, threads, ..
            } = wake
            else {
                continue;
            };
            let ruled: Vec<Ruled> = (held.iter())
                .filter(|(item, _)| item == ticket)
                .map(|(_, ruled)| ruled.clone())
                .collect();
            if !ruled.is_empty() {
                self.threads_ruled(*ticket, &ruled)?;
            }
            self.threads_asked_ended(*ticket, !ruled.is_empty(), ruled.len() < threads.len());
        }
        Ok((acted, dropped))
    }

    /// Carries out the project manager's `ruled` decisions on `ticket`'s threads
    ///
    /// # Errors
    ///
    /// [`StateError`] when what it did cannot be saved.
    pub(super) fn threads_ruled(
        &mut self,
        ticket: TicketId,
        ruled: &[Ruled],
    ) -> Result<(), StateError> {
        let _ = (ticket, ruled);
        Ok(())
    }

    // Ends `ticket`'s ask, and counts the wake as one more `none` when a
    // thread went unanswered and none was answered, or as the first after an answer.
    fn threads_asked_ended(&mut self, ticket: TicketId, answered: bool, left: bool) {
        self.pm.threads_out.remove(&ticket);
        if !left {
            self.pm.threads_quiet.remove(&ticket);
            return;
        }
        let before = match answered {
            true => 0,
            false => (self.pm.threads_quiet.get(&ticket)).map_or(0, |quiet| quiet.nones),
        };
        let nones = before.saturating_add(1);
        let now = self.ports.clock.now();
        let until = Timestamp(now.0.saturating_add(quiet_backoff(nones)));
        self.pm.threads_quiet.insert(ticket, Quiet { nones, until });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::Severity;
    use crate::test::ticket;

    fn thread(id: &str, path: &str, line: Option<u32>, what: &str) -> RuledThread {
        RuledThread {
            id: id.to_owned(),
            path: path.to_owned(),
            line,
            author: "coderabbitai".to_owned(),
            severity: None,
            what: what.to_owned(),
            maintainer_wrote: false,
        }
    }

    #[test]
    fn a_wake_for_open_threads_names_each_and_its_case() {
        let mut graded = thread("T2", "src/main.rs", None, "The handle\nleaks.");
        graded.severity = Some(Severity::High);
        graded.author = "octocat".to_owned();
        graded.maintainer_wrote = true;
        let threads = [
            thread("T1", "src/lib.rs", Some(9), "This looks racy."),
            graded,
        ];

        let text = wake_text(ticket(7), 71, "0123456789abcdef", &threads);

        let mut lines = text.lines();
        let lead = lines.next().unwrap();
        assert!(lead.contains("#7's pull request #71"), "{text}");
        assert!(lead.contains("2 open review threads at 0123456"), "{text}");
        assert!(lead.contains("`cases/7.md`"), "{text}");
        assert!(lead.contains("`close` means the thread is wrong"), "{text}");
        let listed: Vec<&str> = lines.filter(|l| !l.is_empty()).collect();
        assert_eq!(
            listed,
            [
                "1. src/lib.rs:9 (coderabbitai): This looks racy.",
                "2. src/main.rs (octocat, HIGH, the maintainer wrote in it): The handle leaks.",
            ]
        );
    }

    #[test]
    fn a_wake_names_every_open_thread_however_many() {
        let threads: Vec<RuledThread> = (1..=60)
            .map(|n| {
                thread(
                    &format!("T{n}"),
                    "src/lib.rs",
                    Some(n),
                    &format!("Finding {n}."),
                )
            })
            .collect();

        let text = wake_text(ticket(7), 71, "abc", &threads);

        assert!(text.contains("60 open review threads"), "{text}");
        for n in 1..=60 {
            let line = format!("\n{n}. src/lib.rs:{n} (coderabbitai): Finding {n}.");
            assert!(text.contains(&line), "{line:?} missing from\n{text}");
        }
        assert!(!text.contains("left out"), "{text}");
    }
}
