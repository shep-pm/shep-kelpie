//! The rulings the project manager answers for the maintainer
//!
//! A ruling whose kind `rulings.pm_decides` sets `true` wakes the project
//! manager. Its comment waits for the answer, at most one wake. An answer
//! that picks an option is applied at once. It is posted headed "Answered by
//! the project manager", where a `no <note>` reopens it. An abstention, a
//! `design` flag or a merge under `ask` goes to the maintainer. The card
//! shows the project manager's pick.

use std::collections::BTreeMap;

use super::act::{WORDS, plain};
use crate::pm_answer::PmAnswer;
use crate::runner::ruling;
use crate::runner::thread_replies::Settler;
use crate::runner::voice::{RECOMMENDS, SAYS, answer_for, brief, chosen, named, settled_comment};
use crate::runner::{Answer, RuleError, Runner};
use crate::settings::{Merging, RulingName};
use crate::state::{Ruling, RulingKind, StateError, Stuck};
use crate::ticket::TicketId;
use crate::work_item::Phase;

impl Runner {
    /// Whether the project manager answers `ruling` for the maintainer
    ///
    /// Its kind is one `rulings.pm_decides` gives it, and it lists two options
    /// or more. A question and a follow-up have wakes of their own. A
    /// contribution's ruling is never the project manager's. A turn kelpie
    /// could not set up is the maintainer's alone. So is a failed turn the
    /// project manager already answered on the same work item. In a drain it
    /// goes to the maintainer.
    pub(super) fn pm_answers_ruling(&self, ruling: &Ruling) -> bool {
        let own_wake = matches!(
            ruling.kind,
            RulingKind::Question { .. } | RulingKind::FollowUp { .. }
        );
        let setup = matches!(
            ruling.kind,
            RulingKind::Stuck(Stuck::TurnFailed { setup: true, .. })
        );
        let answers = self.agents.pm.is_some()
            && !own_wake
            && !setup
            && !ruling.kind.is_contribution()
            && !self.draining()
            && self
                .settings
                .rulings
                .pm_decides(RulingName::of(&ruling.kind))
            && brief(ruling).1 >= 2;
        answers && !self.pm_answered_before(ruling)
    }

    // Whether the project manager already answered the failed turn `ruling`
    // is, on the same work item. That answer did not settle it.
    pub(super) fn pm_answered_before(&self, ruling: &Ruling) -> bool {
        let records = ruling::records(&self.paths.rulings);
        records.iter().any(|r| r.decided_by_pm(ruling))
    }

    /// How many rulings the project manager answered, by kind, for
    /// `status`; none read for a project with no project manager
    pub(in crate::runner) fn pm_answer_counts(&self) -> BTreeMap<String, u32> {
        match self.agents.pm {
            Some(_) => ruling::pm_answers(&ruling::records(&self.paths.rulings)),
            None => BTreeMap::new(),
        }
    }

    /// The option count of pending ruling `id`, for reading an answer to it
    pub(super) fn ruling_options(&self, id: TicketId) -> usize {
        (self.state.rulings.iter().find(|r| r.id == id)).map_or(0, |r| brief(r).1)
    }

    /// Hears the project manager's answers to the rulings in `asked`, and
    /// says what each did, for the log
    ///
    /// A ruling settled or gone meanwhile takes none, and one whose comment
    /// went out before the answer came is the maintainer's.
    pub(super) fn pm_answered(
        &mut self,
        asked: &[TicketId],
        answers: Vec<(TicketId, Result<PmAnswer, String>)>,
    ) -> Vec<String> {
        let mut answers: BTreeMap<TicketId, Result<PmAnswer, String>> =
            answers.into_iter().collect();
        let mut done = Vec::new();
        for &id in asked {
            self.pm.asking.remove(&id);
            let Some(ruling) = self.state.rulings.iter().find(|r| r.id == id).cloned() else {
                continue;
            };
            if !ruling.unposted || ruling.alerted {
                done.push(format!(
                    "left ruling {id}: it already went to the maintainer"
                ));
                continue;
            }
            let answer = (answers.remove(&id))
                .unwrap_or_else(|| Err("its reply held no answer to it".into()));
            let answer = match answer {
                Ok(answer) => answer,
                Err(why) => {
                    done.push(format!("gave no answer to ruling {id}: {why}"));
                    continue;
                }
            };
            let answer = PmAnswer {
                why: plain(&self.shown(answer.why), WORDS),
                ..answer
            };
            let choice = answer.choice;
            match self.pm_decides(&ruling, &answer) {
                Ok(()) => done.push(format!("answered ruling {id} with option {choice}")),
                Err(why) => {
                    self.pm_recommends(id, &answer);
                    done.push(format!(
                        "put ruling {id} to the maintainer, recommending option {choice}, since \
                         {why}"
                    ));
                }
            }
        }
        done
    }

    // Applies option `answer.choice` to `ruling`, and posts what was decided,
    // or says why the maintainer decides instead. A ruling whose answer
    // cannot be applied stays pending, for the maintainer.
    pub(super) fn pm_decides(&mut self, ruling: &Ruling, answer: &PmAnswer) -> Result<(), String> {
        let (id, key) = (ruling.id, answer.choice);
        if answer.design {
            return Err("it flagged design".into());
        }
        // The settings file refuses `merge` as `true`, so this holds only a
        // merge a hand-edited state brings.
        let merge = matches!(ruling.kind, RulingKind::Merge { .. });
        if merge && self.settings.git.merging == Merging::Ask {
            return Err("a merge under `ask` is the maintainer's".into());
        }
        let picked = named(&ruling.kind, key);
        let note = format!("Answered by the project manager: {}", answer.why);
        let Some(ruled) = answer_for(&ruling.kind, key, &note) else {
            return Err(format!("option {key} is not one it lists"));
        };
        let by = Settler::Pm {
            answer: format!("{key}, {}", picked.unwrap_or_default()),
            why: answer.why.clone(),
            unread: false,
        };
        if let Err(e) = self.rule_by(id, ruled, by) {
            return Err(format!("its answer could not be applied: {e}"));
        }
        // An answer about a head that is gone withdraws the ruling instead.
        let withdrawn = (self.threads_told(id)).is_some_and(|t| t.starts_with("is withdrawn"));
        if !withdrawn {
            self.post_answered(&with_words(ruling, answer), key);
        }
        Ok(())
    }

    // Answers `ruling` with a no whose note is the worker's next turn, as the
    // maintainer's words asked, and posts it as an answer of its own would be,
    // for a `no <note>` to reopen. A kind whose no sends the worker nothing is
    // refused, and the ruling waits.
    pub(super) fn pm_sent_back(
        &mut self,
        ruling: &Ruling,
        note: &str,
        why: &str,
    ) -> Result<String, String> {
        no_sends_worker(&ruling.kind)?;
        if note.is_empty() {
            return Err("it gave no note for the worker".into());
        }
        let note = format!("(from the project manager, on the maintainer's words) {note}");
        let ruled = Answer::No(note);
        let key = chosen(&ruling.kind, &ruled).ok_or("the ruling lists no `no` to answer")?;
        let id = ruling.id;
        let by = Settler::Pm {
            answer: "no, the worker is sent back with a note".to_owned(),
            why: why.to_owned(),
            unread: false,
        };
        match self.rule_by(id, ruled, by) {
            Ok(()) => {}
            Err(RuleError::State(e)) => return Err(format!("it could not be saved: {e}")),
            Err(e) => return Err(format!("its answer could not be applied: {e}")),
        }
        let withdrawn = (self.threads_told(id)).is_some_and(|t| t.starts_with("is withdrawn"));
        if !withdrawn {
            self.post_answered(ruling, key);
        }
        Ok(format!("sent the worker back on ruling {id}: {why}"))
    }

    // Accepts the review bot threads that hold merge ruling `ruling`, withdraws it and has the
    // work item parked on it look at CI again, as a yes would past those threads.
    pub(super) fn pm_settled(&mut self, ruling: &Ruling, why: &str) -> Result<String, String> {
        let RulingKind::Merge { held_by, .. } = &ruling.kind else {
            return Err("only a merge ruling held by review bot threads is settled".into());
        };
        if held_by.is_empty() {
            return Err("no review bot thread holds it".into());
        }
        let id = ruling.id;
        let parked = |phase: &Phase| *phase == Phase::Ruling { id };
        let Some(issue) =
            (self.state.work_items.iter().find(|i| parked(&i.phase))).map(|i| i.issue)
        else {
            return Err("no work item waits on it".into());
        };
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        next.rulings.retain(|r| r.id != id);
        if let Some(item) = next.item_mut(issue) {
            for thread in held_by {
                if !item.threads_accepted.contains(thread) {
                    item.threads_accepted.push(thread.clone());
                }
            }
            item.phase = Phase::Ci {
                head: None,
                since: now,
            };
        }
        self.save(next)
            .map_err(|e: StateError| format!("it could not be saved: {e}"))?;
        self.late_reads.remove(&issue);
        let said = format!(
            "the project manager accepted the review bot threads that held it, on the \
             maintainer's words, and CI is looked at again: {why}"
        );
        self.say_withdrawn(id, issue, ruling.pull_request, &said);
        Ok(format!("settled the threads that held ruling {id}: {why}"))
    }

    // Adds the project manager's pick and why to ruling `id`, for the card
    // the maintainer is asked with.
    pub(super) fn pm_recommends(&mut self, id: TicketId, answer: &PmAnswer) {
        let mut next = self.state.clone();
        let Some(ruling) = next.rulings.iter_mut().find(|r| r.id == id) else {
            return;
        };
        *ruling = with_words(ruling, answer);
        if let Err(e) = self.save(next) {
            self.notes.push(format!(
                "cannot save the project manager's pick on ruling {id}: {e}"
            ));
        }
    }

    // Posts the answer on the ruling's thread, and, where the maintainer is
    // known, watches it for a `no <note>`.
    fn post_answered(&mut self, ruling: &Ruling, key: char) {
        let id = ruling.id;
        let name = named(&ruling.kind, key).unwrap_or_default();
        let line = format!(
            "**Answered by the project manager: {key}, {name}**. Nothing waits on the \
             maintainer. Reply `{id} no <note>` to reopen it."
        );
        let body = settled_comment(ruling, &line);
        let Some(thread) = ruling
            .pull_request
            .or_else(|| ruling.issue.and_then(|t| t.github()))
        else {
            return;
        };
        let failed = |e: &dyn std::fmt::Display| {
            format!("cannot post the project manager's answer on #{thread}: {e}")
        };
        if !self.app_covers() {
            if let Err(e) = self.comment_as_gh(thread, &body) {
                self.notes.push(failed(&e));
            }
            return;
        }
        let posted = match self.ports.voice.post(&self.remote, thread, &body) {
            Ok(posted) => posted,
            Err(e) => return self.notes.push(failed(&e)),
        };
        match self.maintainer() {
            Ok(login) => self.watch_decided(ruling.clone(), thread, &login, posted),
            Err(e) => self.notes.push(format!(
                "ruling {id}'s answer cannot be reopened from its thread, since the \
                 maintainer's login is not known: {e}"
            )),
        }
    }
}

// Why a no to a ruling of `kind` sends the worker no note, where it does not:
// `decide` makes another move of it, or refuses it.
fn no_sends_worker(kind: &RulingKind) -> Result<(), String> {
    let why = match kind {
        RulingKind::Stuck(
            Stuck::TurnTimeout { .. } | Stuck::TurnFailed { .. } | Stuck::PushRefused { .. },
        )
        | RulingKind::AgentFiles { .. } => "a no stops the work item, since no note helps it",
        RulingKind::Stuck(Stuck::CheckQueued { .. } | Stuck::NoChecks { .. }) => {
            "a no only waits for the checks again"
        }
        RulingKind::Stuck(Stuck::OverBudget { .. }) => "a no raises the work item's cap",
        RulingKind::Stuck(Stuck::AdoptFailed { .. }) => "it takes a yes only",
        RulingKind::FollowUp { .. } | RulingKind::AgentFit(..) => "no worker is left to send",
        RulingKind::Question { .. } | RulingKind::Contribution(_) => "a no is not an answer to it",
        _ => return Ok(()),
    };
    Err(format!("a send-back is refused here: {why}"))
}

// `ruling` with the project manager's pick and why under its question, as
// the card reads them.
fn with_words(ruling: &Ruling, answer: &PmAnswer) -> Ruling {
    let name = named(&ruling.kind, answer.choice).unwrap_or_else(|| answer.choice.to_string());
    let mut words = format!("{RECOMMENDS} {name}");
    if !answer.why.is_empty() {
        words.push_str(&format!("\n{SAYS} {}", answer.why));
    }
    // A later recommendation replaces an earlier one.
    let question = ruling.question.split(&format!("\n\n{RECOMMENDS}")).next();
    Ruling {
        question: format!("{}\n\n{words}", question.unwrap_or_default()),
        ..ruling.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::ticket;

    #[test]
    fn the_project_managers_words_go_under_the_question() {
        let ruling = Ruling::new(
            ticket(3),
            "Rebase failed.",
            RulingKind::from(Stuck::Rebase {
                why: "conflicts".into(),
                review: None,
            }),
        );
        let answer = PmAnswer {
            choice: 'A',
            why: "main moved".into(),
            design: false,
        };

        let worded = with_words(&ruling, &answer);

        let name = named(&ruling.kind, 'A').expect("a rebase lists an option A");
        assert_eq!(
            worded.question,
            format!("Rebase failed.\n\nPM recommends: {name}\nPM says: main moved")
        );
    }
}
