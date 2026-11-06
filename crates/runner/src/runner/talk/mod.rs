//! What people with write access wrote on the pull request, sent to the worker
//!
//! Kelpie reads the pull request's comments and review threads at the start of
//! each review round, when CI goes green, and every [`LATE_READ`] seconds while a
//! merge ruling waits. What someone with write access wrote is a request of the
//! worker, sent as one resumed turn, the way a merge ruling's `no <note>` is. A
//! comment that lands while a merge ruling waits withdraws it. The owner, a
//! member or a collaborator is trusted; others' and any bot's are ignored. A
//! comment from the login a ruling mentions goes to that thread instead.
//! After [`TALK_ROUNDS`] rounds the next comments go to the maintainer as a ruling.

use std::collections::BTreeSet;

use self::sort::{Left, Park, Request, Sieve, Sorted, sort};
use super::Runner;
use super::pm::held_line;
use super::report::Begin;
use super::review_bot::Answered;
use super::ruling::needs_pass;
use super::turn::KEEP_BODY_TRUE;
use super::voice::inert;
use super::words::Wants;
use crate::board::HUMAN;
use crate::state::{Resume, RulingKind, StateError};
use crate::ticket::TicketId;
use crate::work_item::{Phase, Review, TALK_ROUNDS, Turn};

mod sort;

/// What the id of a request that is a conversation comment starts with
pub(super) const COMMENT_REQUEST: &str = "comment:";
#[cfg(test)]
mod tests;

/// Where the work item stands when the comments are read
pub(super) enum At {
    /// About to start a round of the review
    Round(Review),
    /// CI is green on this head
    Ci(String),
    /// Waiting on this merge ruling
    Parked(TicketId),
}

impl Runner {
    /// Reads what people wrote on the pull request and, when there is a
    /// request in it that is new, starts the worker's turn for it
    ///
    /// `None` when there is nothing new to send, or when threads sent to
    /// the worker are not yet answered. A read that fails is told and the
    /// work item goes on without it.
    pub(super) fn talk_turn(&mut self, at: &At) -> Result<Option<Begin>, StateError> {
        let Some(item) = self.current() else {
            return Ok(None);
        };
        let Some(number) = item.pull_request else {
            return Ok(None);
        };
        // A step made again after another read has told these already
        if !item.threads_sent.is_empty() || item.talked.head.is_some() || self.reads.talked(number)
        {
            return Ok(None);
        }
        let talk = match self.read_talk(number) {
            Ok(Ok(talk)) => talk,
            Ok(Err(e)) => {
                self.notes
                    .push(format!("cannot read what people wrote on #{number}: {e}"));
                self.reads.talked_on(number);
                return Ok(None);
            }
            Err(job) => return Ok(Some(Begin::Reads(job))),
        };
        let mut own: BTreeSet<u64> = self.state.posted.iter().copied().collect();
        own.extend(self.posted.borrow().iter().copied());
        let left = self.ruling_replies(number);
        // What the maintainer writes is the project manager's to read, where
        // one is named and the maintainer's login is known without asking
        let maintainer = self
            .agents
            .pm
            .as_ref()
            .and_then(|_| self.known_maintainer());
        let sieve = Sieve {
            talked: &item.talked,
            own: &own,
            left: &left,
            maintainer: maintainer.as_deref(),
            quiet: !item.talked.read && (item.adopted || item.rework || item.talked.quiet),
            released: self.pm_released(),
        };
        let sorted = sort(&talk, &sieve);
        let capped = item.talked.capped();
        if sorted.requests.is_empty() && sorted.parks.is_empty() {
            self.note_heard(&sorted)?;
            self.notes.extend(sorted.ignored);
            self.reads.talked_on(number);
            return Ok(None);
        }
        let begin = if !sorted.parks.is_empty() {
            self.parked_by_pm(number, at, &sorted)?
        } else if capped {
            self.too_many_rounds(number, at, &sorted)?
        } else {
            self.send_requests(number, at, &sorted)?
        };
        // A read the send asks for is made and the send made again, which tells these then
        if begin
            .as_ref()
            .is_some_and(|b| !matches!(b, Begin::Reads(_)))
        {
            // A send the gate stopped saved nothing: its calls are carried out next read
            let heard: Vec<u64> = (self.current().map(|item| &item.talked.comments))
                .map(|saved| {
                    sorted
                        .comments
                        .iter()
                        .filter(|c| saved.contains(c))
                        .copied()
                        .collect()
                })
                .unwrap_or_default();
            self.pm_carried_out(&heard);
            self.notes.extend(sorted.ignored);
        }
        Ok(begin)
    }

    // The logins of the people a pending ruling on the pull request asks to
    // reply in its thread, where kelpie's App reads those replies. With a
    // project manager named no comment withdraws a merge ruling: free text
    // is held for it, and the ruling waits for an answer.
    fn ruling_replies(&self, number: u64) -> Vec<Left> {
        if !self.app_covers() {
            return Vec::new();
        }
        (self.state.rulings.iter())
            .filter(|r| r.pull_request == Some(number))
            .filter_map(|r| {
                let heard = self.state.heard.get(&r.id)?;
                Some(Left {
                    login: heard.mentions.clone(),
                    takes_text: Wants::of(&r.kind) == Wants::Answer,
                    merge: matches!(r.kind, RulingKind::Merge { .. }) && self.agents.pm.is_none(),
                    after: self.agents.pm.as_ref().map(|_| heard.comment),
                })
            })
            .collect()
    }

    // Notes the comments of `sorted` as handled.
    fn note_heard(&mut self, sorted: &Sorted) -> Result<(), StateError> {
        let Some(item) = self.current() else {
            return Ok(());
        };
        let before = item.talked.clone();
        let mut after = before.clone();
        after.read = true;
        for id in &sorted.comments {
            after.hear(*id);
        }
        for (thread, id) in &sorted.threads {
            after.hear_thread(thread, *id);
        }
        if after == before && sorted.held.is_empty() {
            return Ok(());
        }
        // The words are held in the save that marks them heard
        let told = self.held_lines(sorted);
        let mut next = self.state.clone();
        if let Some(item) = self.current_in(&mut next) {
            item.talked = after;
        }
        next.pm_told.extend(told);
        self.save(next)
    }

    // The entries for the project manager's next wake that the comments held
    // in `sorted` make, each naming the ruling that waits on the pull request
    // if one does.
    fn held_lines(&self, sorted: &Sorted) -> Vec<String> {
        let Some(number) = self.current().and_then(|item| item.pull_request) else {
            return Vec::new();
        };
        let ruling = (self.state.rulings.iter())
            .find(|r| r.pull_request == Some(number))
            .map(|r| r.id);
        (sorted.held.iter())
            .map(|(id, text)| held_line(number, *id, ruling, text))
            .collect()
    }

    // Sends the requests to the worker as its next turn, in place of the
    // merge ruling that waits, if one does.
    fn send_requests(
        &mut self,
        number: u64,
        at: &At,
        sorted: &Sorted,
    ) -> Result<Option<Begin>, StateError> {
        let head = match self.pushed_head() {
            Ok(head) => head,
            Err(begin) => return Ok(Some(*begin)),
        };
        let (phase, noted_from, parked) = self.after_comments(at);
        let prompt = prompt(number, &sorted.requests);
        let ids: Vec<String> = sorted.requests.iter().map(|r| r.id.clone()).collect();
        let told = self.held_lines(sorted);
        let mut next = self.state.clone();
        next.rulings.retain(|r| Some(r.id) != parked);
        let item = self
            .current_in(&mut next)
            .expect("comments are read on a work item");
        for id in &sorted.comments {
            item.talked.hear(*id);
        }
        for (thread, id) in &sorted.threads {
            item.talked.hear_thread(thread, *id);
        }
        item.talked.read = true;
        item.talked.rounds = item.talked.rounds.saturating_add(1);
        item.talked.head = Some(head);
        item.threads_sent = ids;
        item.thread_answers.clear();
        item.turn = Turn::Next { prompt };
        item.phase = Phase::Implement;
        item.resume = phase;
        item.noted_from = noted_from.or(item.noted_from.take());
        item.late_from = None;
        let issue = item.issue;
        next.pm_told.extend(told);
        self.save(next)?;
        let Some(id) = parked else {
            return Ok(Some(Begin::Idle));
        };
        self.late_reads.remove(&issue);
        let why = "someone with write access commented on the pull request, so the worker gets \
                   the comment as a no, and the merge ruling is raised again once CI is green after it";
        self.say_withdrawn(id, issue, Some(number), why);
        // The worker's turn again, so the hand-back label comes off.
        match (self.ports.forge).set_label(&self.remote, number, HUMAN, false) {
            Ok(()) => self.update(|item| item.known.labels.retain(|l| l != HUMAN))?,
            Err(e) => eprintln!("cannot take the `{HUMAN}` label off #{number}: {e}"),
        }
        Ok(Some(Begin::Idle))
    }

    // Where the turn for comments read `at` goes once it ends, the head the
    // fix is sent unread from, and the ruling it withdraws.
    fn after_comments(&self, at: &At) -> (Option<Phase>, Option<String>, Option<TicketId>) {
        match at {
            At::Round(review) => (Some(Phase::Review(review.clone())), None, None),
            At::Ci(head) => (None, Some(head.clone()), None),
            At::Parked(id) => {
                let kind = self.state.rulings.iter().find(|r| r.id == *id);
                match kind.map(|r| &r.kind) {
                    Some(kind) if needs_pass(kind) => {
                        (Some(Phase::Review(Review::first())), None, Some(*id))
                    }
                    Some(RulingKind::Merge { head, .. }) => (None, Some(head.clone()), Some(*id)),
                    _ => (None, None, Some(*id)),
                }
            }
        }
    }

    // The comments come after the rounds kelpie sends by itself, so the
    // maintainer reads them, and their answer is what the worker is sent.
    fn too_many_rounds(
        &mut self,
        number: u64,
        at: &At,
        sorted: &Sorted,
    ) -> Result<Option<Begin>, StateError> {
        let list: Vec<String> = sorted.requests.iter().map(|r| r.quoted()).collect();
        let asked = format!(
            "People with write access commented on the pull request after {TALK_ROUNDS} rounds \
             of fixes for comments, which is as many as kelpie sends by itself:\n\n{}\n\n\
             Say what the worker should do about them.",
            list.join("\n\n")
        );
        self.put_to_maintainer(number, at, sorted, asked, false)
    }

    // The project manager parked the work item on the maintainer's words, so
    // the maintainer is asked what the worker should do, as a question.
    fn parked_by_pm(
        &mut self,
        number: u64,
        at: &At,
        sorted: &Sorted,
    ) -> Result<Option<Begin>, StateError> {
        let parks: Vec<String> = sorted.parks.iter().map(Park::quoted).collect();
        let others: Vec<String> = sorted.requests.iter().map(Request::quoted).collect();
        let more = match others.is_empty() {
            true => String::new(),
            false => format!(
                "Others commented on the pull request as well:\n\n{}\n\n",
                others.join("\n\n")
            ),
        };
        let asked = format!(
            "The project manager parked this work item on your words, so the worker waits for \
             you:\n\n{}\n\n{more}Say what the worker should do.",
            parks.join("\n\n")
        );
        self.put_to_maintainer(number, at, sorted, asked, true)
    }

    // Raises a question of `asked` to the maintainer in place of the comments
    // of `sorted`, which are handled with it, and withdraws the merge ruling
    // that waits, if one does.
    fn put_to_maintainer(
        &mut self,
        number: u64,
        at: &At,
        sorted: &Sorted,
        asked: String,
        by_pm: bool,
    ) -> Result<Option<Begin>, StateError> {
        let (phase, noted_from, parked) = self.after_comments(at);
        let resume = match (at, phase) {
            (At::Round(review), _) => Resume::Review(review.clone()),
            (_, Some(Phase::Review(_))) => Resume::ReviewFirst,
            _ => Resume::Nothing,
        };
        let told = self.held_lines(sorted);
        let mut next = self.state.clone();
        next.rulings.retain(|r| Some(r.id) != parked);
        let item = self
            .current_in(&mut next)
            .expect("comments are read on a work item");
        for id in &sorted.comments {
            item.talked.hear(*id);
        }
        for (thread, id) in &sorted.threads {
            item.talked.hear_thread(thread, *id);
        }
        item.noted_from = noted_from.or(item.noted_from.take());
        let issue = item.issue;
        next.pm_told.extend(told);
        self.save(next)?;
        if let Some(id) = parked {
            self.late_reads.remove(&issue);
            let why = "comments came in after the rounds kelpie sends by itself, so the maintainer reads them";
            self.say_withdrawn(id, issue, Some(number), why);
        }
        let kind = RulingKind::Question { asked, resume };
        // A question the project manager parked the work item on is not its to answer
        self.pm_parking(by_pm);
        let raised = self.raise(number, kind);
        self.pm_parking(false);
        Ok(Some(raised?))
    }

    /// Replies on the threads the last round of comments was sent, and
    /// resolves the ones the worker fixed, once its turn is over
    ///
    /// `None` when the turn ended, or there is nothing to answer.
    pub(super) fn answer_talk(&mut self) -> Result<Option<Begin>, StateError> {
        let Some(item) = self.current() else {
            return Ok(None);
        };
        let (Some(number), Some(sent_at)) = (item.pull_request, item.talked.head.clone()) else {
            return Ok(None);
        };
        if !matches!(item.turn, Turn::Ended { .. }) {
            return Ok(None);
        }
        let pushed = match self.pushed_head() {
            Ok(head) => head != sent_at,
            Err(begin) => return Ok(Some(*begin)),
        };
        match self.answer_sent(number, pushed)? {
            Answered::Done => {}
            Answered::Retry(reason) => return Ok(Some(self.gate_failed(reason))),
            Answered::LeftOpen { reason, .. } => self.notes.push(reason),
        }
        self.update(|item| item.talked.head = None)?;
        Ok(None)
    }
}

impl Park {
    // The words the project manager parked the work item on, and why.
    fn quoted(&self) -> String {
        let quoted: Vec<String> = self.text.lines().map(|l| format!("> {l}")).collect();
        let why = match self.why.is_empty() {
            true => String::new(),
            false => format!("\nIts reason: {}", inert(&self.why)),
        };
        format!("{}:\n{}{why}", self.who, quoted.join("\n"))
    }
}

impl Request {
    // What it says, as the maintainer reads it.
    fn quoted(&self) -> String {
        let place = self
            .place
            .as_ref()
            .map_or_else(String::new, |p| format!(" on {p}"));
        let said: Vec<String> = (self.said.iter())
            .map(|(who, text)| {
                let quoted: Vec<String> = text.lines().map(|l| format!("> {l}")).collect();
                format!("{who}{place}:\n{}", quoted.join("\n"))
            })
            .collect();
        said.join("\n")
    }
}

/// `prompt` with the reminder that the reply ends with the thread lines the
/// requests asked for: the turn after a question about them is told so
pub(super) fn with_thread_lines(prompt: String) -> String {
    format!(
        "{prompt}\nThe requests you were sent are still open. End your reply with one line \
         for each, as `thread <n>: fixed` or `thread <n>: declined: <the reason>`, using the \
         numbers they had.\n"
    )
}

// The worker's turn for `requests`, which it answers by their numbers.
fn prompt(number: u64, requests: &[Request]) -> String {
    let list: Vec<String> = requests
        .iter()
        .enumerate()
        .map(|(at, request)| format!("Request {}:\n{}", at + 1, request.quoted()))
        .collect();
    format!(
        "People with write access to the repository commented on pull request #{number}. \
         Each numbered request is a comment on its conversation or a review thread on its \
         diff.\n\n{}\n\n\
         Make the change each one asks for when it is clear, and push it. A request that is \
         ambiguous, or asks for an architectural change, is not yours to decide: make no \
         change for it, and end your reply with a question block that says what you need \
         to know, as `<kelpie-question>` and `</kelpie-question>` around the question.\n\n\
         Otherwise end your reply with one line for each request, using the number it has \
         here: `thread <n>: fixed` or `thread <n>: declined: <the reason>`. Kelpie replies \
         on each review thread for you and resolves the ones you fixed, and a request with \
         no line is replied to as not addressed.\n\n{KEEP_BODY_TRUE}\n",
        list.join("\n\n")
    )
}
