//! The maintainer's ruling on merging a contribution
//!
//! Only the maintainer merges someone else's work, so `git.merging = "auto"`
//! does not reach a contribution. Once a poll, one whose pass has ended with green
//! CI on the same head gets a merge ruling, with no work item. It holds nothing.
//! A thumbs up or down on the clean-pass note answers it too; the first answer wins.
//! The answer is kept on the record (`ruled`) and carried out by the next poll:
//! yes merges the ruled head, a no with a note posts it, and a rework releases
//! the pull request as `how: rework` and queues it at once. A new head clears the
//! answer and withdraws the ruling.

use super::super::Runner;
use super::super::gate::short;
use super::super::thread_replies::Settler;
use super::super::voice::thumbs;
use super::post::forge_is_busy;
use super::ruling::{flagged_lines, raise_merge};
use crate::ports::Finding;
use crate::state::{Contribution, Held, How, Released, RulingKind, StateError, Verdict};
use crate::ticket::TicketId;

const THANKS: &str = "Thank the contributor: their pull request is merged. Short and warm. Credit \
    them for the work, and promise nothing.";

const DECLINED: &str = "The maintainer will not merge this pull request as it stands and left a \
    note. Tell the contributor what the note says, in your own words, kindly and plainly, so they \
    know what the maintainer wants. Never promise a merge or hint that one is coming.";

impl Runner {
    // Raises the merge ruling for each contribution that is due one, and
    // carries out each answer the maintainer gave.
    pub(super) fn follow_merge(&mut self) -> Result<(), StateError> {
        let numbers: Vec<u64> = (self.state.contributions.iter())
            .map(|c| c.pull_request)
            .collect();
        for number in numbers {
            self.merge_one(number)?;
        }
        Ok(())
    }

    fn merge_one(&mut self, number: u64) -> Result<(), StateError> {
        let Some(held) = (self.state.contributions.iter())
            .find(|c| c.pull_request == number)
            .cloned()
        else {
            return Ok(());
        };
        let Some(ruled) = held.ruled.as_ref() else {
            if self.answer_by_reaction(&held) {
                return self.merge_one(number);
            }
            return self.raise_for(&held);
        };
        // An answer is about the head the ruling named, never another.
        if ruled.head != held.head {
            return self.change(number, |held| held.ruled = None);
        }
        match &ruled.verdict {
            Verdict::Yes { merged, thanked } => self.merge_and_thank(&held, *merged, *thanked),
            Verdict::No { note: Some(note) } => self.tell_the_note(&held, note),
            Verdict::No { note: None } => Ok(()),
            Verdict::Rework { .. } => self.take_it_over(&held),
        }
    }

    // Answers the waiting merge ruling with the maintainer's reaction on the
    // note of the head's clean pass, if they gave one, and returns whether it
    // was answered. A thumbs down wins over a thumbs up. A read or an answer
    // that fails is told once and tried at the next poll.
    fn answer_by_reaction(&mut self, held: &Contribution) -> bool {
        let number = held.pull_request;
        let id = TicketId::from_pull_request(number);
        let Some(note) = (held.pass.as_ref())
            .filter(|p| p.head == held.head)
            .and_then(|p| p.note)
        else {
            return false;
        };
        let Some(ruling) = (self.state.rulings.iter()).find(|r| {
            r.id == id && matches!(&r.kind, RulingKind::Contribution(Held::Merge { head, .. }) if *head == held.head)
        }) else {
            return false;
        };
        let kind = ruling.kind.clone();
        if !self.app_covers() {
            return false;
        }
        // A reaction answers once: it stays on the comment, so a ruling raised
        // again for the same head must not be answered by it.
        let answered = format!("reacted:{number}:{note}");
        if self.post_tries.borrow().contains_key(&answered) {
            return false;
        }
        let key = format!("react:{number}:{note}");
        let marks = self.maintainer().and_then(|maintainer| {
            let marks = self.ports.forge.comment_marks(&self.remote, note)?;
            Ok((maintainer, marks))
        });
        let (maintainer, marks) = match marks {
            Ok(read) => {
                self.answer_ends(&key);
                read
            }
            Err(e) => {
                self.answer_failed(&key, number, "read the reaction on the note to", &e, false);
                return false;
            }
        };
        let theirs: Vec<bool> = (marks.thumbs.iter())
            .filter(|t| t.login.eq_ignore_ascii_case(&maintainer))
            .map(|t| t.up)
            .collect();
        let up = match (theirs.contains(&false), theirs.contains(&true)) {
            (true, _) => false,
            (false, true) => true,
            (false, false) => return false,
        };
        let Some(answer) = thumbs(&kind, up) else {
            return false;
        };
        match self.rule_by(id, answer, Settler::Thread) {
            Ok(()) => {
                self.answer_ends(&format!("refused:{number}:{note}"));
                (self.post_tries.borrow_mut()).entry(answered).or_default();
                true
            }
            Err(e) => {
                let key = format!("refused:{number}:{note}");
                let why = format!(
                    "contribution #{number}: cannot answer its merge ruling from the reaction \
                     on the note: {e}"
                );
                let mut tries = self.post_tries.borrow_mut();
                let attempt = tries.entry(key).or_default();
                if !attempt.told {
                    attempt.told = true;
                    self.notes.push(why);
                }
                false
            }
        }
    }

    // Raises the ruling when the head is due one and nothing else is asked of
    // the maintainer about the pull request
    fn raise_for(&mut self, held: &Contribution) -> Result<(), StateError> {
        let number = held.pull_request;
        // A pull request kelpie has taken over is kelpie's to finish: nothing
        // is asked about merging it, even where its head moved before its work
        // item started.
        if held.released.is_some()
            || !held.awaits_merge_ruling()
            || (self.state.rulings.iter()).any(|r| r.id == TicketId::from_pull_request(number))
        {
            return Ok(());
        }
        let facts = self.merge_facts(held);
        // A pull request the forge cannot be read for keeps its rework, as
        // before.
        let rework_off =
            (self.ports.forge.reviewed(&self.remote, number)).is_ok_and(|pr| !pr.pushable());
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        raise_merge(&mut next, number, &held.head, rework_off, &facts, now);
        self.save(next)
    }

    // What the reviewers recorded on the head and what is still open
    fn merge_facts(&self, held: &Contribution) -> String {
        let Some(pass) = held.pass.as_ref().filter(|p| p.head == held.head) else {
            return String::new();
        };
        let all: Vec<Finding> = pass.findings.iter().chain(&pass.nits).cloned().collect();
        if all.is_empty() {
            return "The reviewers recorded no findings on this head.".to_owned();
        }
        let open = match self.open_threads(held).is_empty() {
            true => "",
            false => ", and the review bot's threads among them are still open",
        };
        format!(
            "The reviewers recorded these findings on this head{open}:\n{}",
            flagged_lines(&all)
        )
    }

    // Has kelpie take the pull request over, as the maintainer's `rework` answer
    // does: the record is released and the poll's own step queues it. The note
    // goes on the record, so a push before the work item starts keeps it. A release
    // that landed first is overruled: its `how` becomes `rework`.
    fn take_it_over(&mut self, held: &Contribution) -> Result<(), StateError> {
        let number = held.pull_request;
        let note = (held.ruled.as_ref()).and_then(|r| match &r.verdict {
            Verdict::Rework { note } => note.clone(),
            _ => None,
        });
        // Carried out already: the answer stays on the record until it leaves.
        if held.released.as_ref().is_some_and(|r| r.how == How::Rework) && held.rework == note {
            return Ok(());
        }
        let at = held
            .released
            .as_ref()
            .map_or_else(|| self.ports.clock.now(), |r| r.at);
        self.change(number, |held| {
            held.released = Some(Released {
                at,
                how: How::Rework,
            });
            held.rework = note.clone();
        })?;
        let tail = match &note {
            Some(note) => format!(": {note}"),
            None => String::new(),
        };
        self.notes.push(format!(
            "contribution #{number}: the maintainer answered its merge ruling `rework`, so \
             kelpie takes the pull request over{tail}"
        ));
        Ok(())
    }

    // Merges the head the ruling named, then thanks the contributor. Each
    // step is saved when it is done, so a restart does neither twice.
    fn merge_and_thank(
        &mut self,
        held: &Contribution,
        merged: bool,
        thanked: bool,
    ) -> Result<(), StateError> {
        let number = held.pull_request;
        if !merged {
            let key = format!("merge:{number}");
            if let Err(e) = self.ports.forge.merge(&self.remote, number, &held.head) {
                self.answer_failed(
                    &key,
                    number,
                    "merge the pull request of",
                    &e,
                    !forge_is_busy(&e),
                );
                if self.answer_given_up(&key) {
                    // The maintainer is asked again, with the failure in the log.
                    self.answer_ends(&key);
                    return self.change(number, |held| held.ruled = None);
                }
                return Ok(());
            }
            self.answer_ends(&key);
            self.notes.push(format!(
                "contribution #{number}: merged at {}",
                short(&held.head)
            ));
            self.change(number, |held| {
                if let Some(ruled) = held.ruled.as_mut() {
                    ruled.verdict = Verdict::Yes {
                        merged: true,
                        thanked,
                    };
                }
            })?;
        }
        if thanked {
            return Ok(());
        }
        self.thank_for_the_merge(held)
    }

    // The thanks is a courtesy: a call or a post that fails is told in the
    // log and nothing waits on it. The merged pull request leaves the record
    // at the next poll, so a thanks that failed is not tried again.
    fn thank_for_the_merge(&mut self, held: &Contribution) -> Result<(), StateError> {
        let number = held.pull_request;
        let key = format!("merged:{number}");
        if self.app_covers() && !self.answer_given_up(&key) {
            let posted = match self.sent_before(&key) {
                Some(id) => Some(id),
                None => {
                    let facts = format!(
                        "Pull request #{number} by @{}. The maintainer merged it at {}.\n",
                        held.author,
                        short(&held.head)
                    );
                    self.word_and_post(held, THANKS, &facts, &key, "thank")?
                }
            };
            if posted.is_none() {
                return Ok(());
            }
            return self.keep_record(number, &key, posted, set_thanked);
        }
        self.answer_ends(&key);
        self.change(number, set_thanked)
    }

    // Has the representative word the maintainer's note for the contributor
    // and post it. A note that cannot be posted after a few tries is dropped,
    // told in the log, so the head is not held for ever.
    fn tell_the_note(&mut self, held: &Contribution, note: &str) -> Result<(), StateError> {
        let number = held.pull_request;
        let key = format!("declined:{number}");
        if self.app_covers() && !self.answer_given_up(&key) {
            let posted = match self.sent_before(&key) {
                Some(id) => Some(id),
                None => {
                    let facts = format!(
                        "Pull request #{number} by @{}, at {}. The maintainer decided not to \
                         merge it as it stands. The maintainer's note, to put to the contributor \
                         in your words:\n{note}\n",
                        held.author,
                        short(&held.head)
                    );
                    self.word_and_post(held, DECLINED, &facts, &key, "tell")?
                }
            };
            if posted.is_none() {
                return Ok(());
            }
            return self.keep_record(number, &key, posted, set_told);
        }
        self.answer_ends(&key);
        self.change(number, set_told)
    }
}

// Records that the contributor was thanked for the merge
fn set_thanked(held: &mut Contribution) {
    let Some(ruled) = held.ruled.as_mut() else {
        return;
    };
    if let Verdict::Yes { merged, .. } = ruled.verdict {
        ruled.verdict = Verdict::Yes {
            merged,
            thanked: true,
        };
    }
}

// Records that the note was put to the contributor, so it is kept no more
fn set_told(held: &mut Contribution) {
    if let Some(ruled) = held.ruled.as_mut() {
        ruled.verdict = Verdict::No { note: None };
    }
}

// Whether `kind` is the merge ruling of a contribution
pub(super) fn is_merge_ruling(kind: &RulingKind) -> bool {
    matches!(kind, RulingKind::Contribution(Held::Merge { .. }))
}
