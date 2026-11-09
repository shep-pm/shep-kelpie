//! Owning a wait a post comes after
//!
//! A stopped runner or an ignored project leaves a contributor waiting, and a
//! post that comes weeks later reads as if no time passed. `Runner::waited_line`
//! works out how many days the thread waited on kelpie or the maintainer before
//! this post, from the reader's last word against the answers kelpie and the
//! maintainer wrote, and from an ask kelpie's own record still holds open past
//! the moment it said it would act. `represent_within` states it as a fact once
//! it reaches `contributions.nudge_days`, and the representative opens with it
//! (`agents/representative.md`).

use super::super::Runner;
use super::{Post, Thread};
use crate::ports::{ThreadComment, Timestamp};
use crate::runner::contributions::clock::Clock;
use crate::runner::thread_replies::plain_login;
use crate::state::{Asked, Contribution};
use crate::ticket::TicketId;

const DAY: u64 = 86_400;

impl Runner {
    // The fact line `post` carries when its thread has waited on kelpie or the
    // maintainer for `contributions.nudge_days` or more. A thread whose comments
    // cannot be read carries none, and the post goes on without it.
    pub(super) fn waited_line(&mut self, post: &Post<'_>, thread: Thread<'_>) -> Option<String> {
        let ticket = TicketId::from_issue(post.pull_request)?;
        let comments = (self.ports.tickets)
            .issue_comments(&self.remote, ticket)
            .ok()?;
        let ours = our_logins(self);
        let now = self.ports.clock.now();
        let head = match thread {
            Thread::PullRequest => followed(self, post).map(|held| held.seen),
            Thread::Issue(_) => None,
        };
        let days = unanswered(post, &comments, &ours, head, now)
            .into_iter()
            .chain(promise(self, post, thread, &comments, now))
            .max()?;
        let nudge = u64::from(self.settings.contributions.nudge_days.get());
        (days >= nudge).then(|| {
            let plural = if days == 1 { "" } else { "s" };
            format!(
                "This thread waited {days} day{plural} on kelpie or the maintainer before this \
                 post."
            )
        })
    }
}

// The logins whose comments on a thread are ours: the account kelpie acts as,
// the App it posts through, and the maintainer
fn our_logins(runner: &mut Runner) -> Vec<String> {
    [
        runner.viewer().ok(),
        runner.maintainer().ok(),
        runner.app_login(),
    ]
    .into_iter()
    .flatten()
    .map(|login| plain_login(&login))
    .collect()
}

// The record of the contribution `post` is on, where kelpie follows it
fn followed<'a>(runner: &'a Runner, post: &Post<'_>) -> Option<&'a Contribution> {
    (runner.state.contributions.iter()).find(|c| c.pull_request == post.pull_request)
}

// How many days the reader's last word has gone unanswered, or `None` when the
// newest word on the thread is ours. With none of ours at all, the wait runs
// from their first word. The reader's own word never answers it: a maintainer's
// own issue waits on kelpie like any other, even with their login in `ours`.
fn unanswered(
    post: &Post<'_>,
    comments: &[ThreadComment],
    ours: &[String],
    head: Option<Timestamp>,
    now: Timestamp,
) -> Option<u64> {
    let reader = plain_login(post.contributor);
    let theirs: Vec<Timestamp> = (comments.iter())
        .filter(|c| plain_login(&c.author) == reader)
        .map(|c| c.created)
        .collect();
    let last = theirs.iter().copied().chain(head).max();
    let ours_last = (comments.iter())
        .filter(|c| plain_login(&c.author) != reader && ours.contains(&plain_login(&c.author)))
        .map(|c| c.created)
        .max();
    let from = match (ours_last, last) {
        (Some(ours), Some(theirs)) if theirs > ours => Some(theirs),
        (None, Some(_)) => theirs.iter().copied().chain(head).min(),
        _ => None,
    };
    from.map(|at| days(at, now))
}

// How late this post is on a promise kelpie made on its thread: a release ask
// whose quiet window closed, or a triage ask still waiting on its author past
// its own wait
fn promise(
    runner: &Runner,
    post: &Post<'_>,
    thread: Thread<'_>,
    comments: &[ThreadComment],
    now: Timestamp,
) -> Option<u64> {
    match thread {
        Thread::PullRequest => release_late(runner, post, now),
        Thread::Issue(_) => triage_late(runner, post, comments, now),
    }
}

// The days past the moment a still-open release ask's clock said kelpie would
// take silence for a yes. Short of that moment it is not late, and says nothing.
fn release_late(runner: &Runner, post: &Post<'_>, now: Timestamp) -> Option<u64> {
    let held = followed(runner, post)?;
    if !held.asked.as_ref().is_some_and(Asked::is_open) {
        return None;
    }
    let clock = Clock::of(held, &runner.settings.contributions)?;
    Some(days(clock.acts_at(), now))
}

// The days past the wait of the triage ask `post`'s issue record still holds
// open. The record is the ask's life: `triage_none` clears it when silence ends
// the round on schedule, so a settled ask is nothing to own however long its
// comment has been up. An answer inside the wait settles it the same way.
fn triage_late(
    runner: &Runner,
    post: &Post<'_>,
    comments: &[ThreadComment],
    now: Timestamp,
) -> Option<u64> {
    let held = (runner.state.triage.iter())
        .flat_map(|t| t.issues.iter())
        .find(|t| t.issue == post.pull_request)?;
    let asked_at = held
        .questions
        .as_ref()
        .map(|q| q.at)
        .or_else(|| held.asked.as_ref().map(|a| a.at))?;
    let wait = u64::from(runner.settings.triage.wait_days.get());
    let lapse = Timestamp(asked_at.0.saturating_add(wait * DAY));
    if now <= lapse {
        return None;
    }
    let reader = plain_login(post.contributor);
    let answered = (comments.iter())
        .any(|c| plain_login(&c.author) == reader && c.created > asked_at && c.created <= lapse);
    (!answered).then(|| days(lapse, now))
}

// Whole days from `from` to `to`, 0 when `to` is not past it
fn days(from: Timestamp, to: Timestamp) -> u64 {
    to.0.saturating_sub(from.0) / DAY
}
