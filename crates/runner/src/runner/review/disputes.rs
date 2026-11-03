//! A worker's dispute of a finding it shows is wrong
//!
//! A fix turn that pushes nothing counts when every finding it was sent that
//! is not a nit was deferred or disputed with a reason. The disputes are shown
//! to the next reviewer, and posted on the pull request when none follows.

use std::path::Path;

use super::findings::{self, Sent};
use super::{Review, Unfixed};
use crate::ports::{Finding, ForgeError};
use crate::runner::Runner;
use crate::runner::voice::finding_line;
use crate::state::StateError;
use crate::work_item::{Dispute, ThreadAnswer, WorkItem};

/// What a fix turn that pushed nothing said of the findings it was sent that
/// are not nits, each by its index in the findings sent
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::runner) struct Answers {
    /// Shown wrong, with the worker's reason
    disputed: Vec<(usize, String)>,
    /// Disputed with a reason, though a reviewer had raised it again after
    /// an earlier dispute
    repeated: Vec<usize>,
    /// Deferred as out of scope, and not disputed, with the worker's reason
    /// when it gave one
    deferred: Vec<(usize, Option<String>)>,
    /// Neither deferred nor disputed with a reason
    unanswered: Vec<usize>,
}

/// What a fix turn that pushed nothing did with the findings it was sent,
/// when it left none unanswered
#[derive(Debug, PartialEq, Eq)]
pub(in crate::runner) struct Settled {
    /// How many it deferred, and not disputed
    pub deferred: usize,
    /// What it disputed with a reason, by the index of the finding sent
    pub disputed: Vec<(usize, String)>,
}

/// How the worker answered `sent` in its files
///
/// `already` is what the pass disputed before `round`: a finding in it that
/// was sent again cannot be disputed again, so the two readers that
/// disagree end with the maintainer. A finding both deferred and disputed
/// counts as disputed. A deferral or dispute an earlier round left in its file
/// answers nothing in this one.
///
/// # Errors
///
/// As [`findings::deferred_with_reasons`] and [`findings::disputed`].
pub(in crate::runner) fn answers(
    build: &Path,
    sent: Sent<'_>,
    round: u32,
    already: &[Dispute],
) -> Result<Answers, String> {
    let mut deferred = findings::deferred_with_reasons(build)?;
    for before in sent.deferred_before {
        if let Some(at) = (deferred.iter()).position(|d| d.finding.is_same_as(before)) {
            deferred.remove(at);
        }
    }
    let disputed = findings::disputed(build)?;
    let mut answers = Answers::default();
    let to_answer =
        (sent.findings.iter().enumerate()).filter(|(_, f)| !f.is_nit() && !f.is_skipped_for_size());
    for (at, f) in to_answer {
        let reason = (disputed.iter())
            .filter(|d| d.finding.is_same_as(f))
            .find_map(|d| d.reason.clone());
        let again = already
            .iter()
            .any(|a| a.round < round && a.finding.is_same_as(f));
        match reason {
            Some(reason) if !again => answers.disputed.push((at, reason)),
            Some(_) => answers.repeated.push(at),
            None => match deferred.iter().find(|d| d.finding.is_same_as(f)) {
                Some(d) => answers.deferred.push((at, d.reason.clone())),
                None => answers.unanswered.push(at),
            },
        }
    }
    Ok(answers)
}

impl Answers {
    /// What the worker showed wrong, with its reason, by the finding's index
    pub(in crate::runner) fn disputed(&self) -> &[(usize, String)] {
        &self.disputed
    }

    /// Those disputed again after a reviewer raised them again
    pub(in crate::runner) fn repeated_at(&self) -> &[usize] {
        &self.repeated
    }

    /// Those deferred, and not disputed, with the worker's reason when it
    /// gave one
    pub(in crate::runner) fn deferred(&self) -> &[(usize, Option<String>)] {
        &self.deferred
    }

    /// Whether the worker disputed a finding again that a reviewer had raised
    /// again after an earlier dispute
    pub(in crate::runner) fn repeated(&self) -> bool {
        !self.repeated.is_empty()
    }

    /// What the answers come to when every finding that is not a nit is
    /// deferred or disputed, and one at least is disputed
    pub(in crate::runner) fn settled(self) -> Option<Settled> {
        let whole = self.unanswered.is_empty() && self.repeated.is_empty();
        (whole && !self.disputed.is_empty()).then_some(Settled {
            deferred: self.deferred.len(),
            disputed: self.disputed,
        })
    }

    /// What the worker disputed and deferred of `sent`, and what it left,
    /// for the question of the ruling a fix turn that fell short raises; none
    /// when it did neither
    pub(in crate::runner) fn said(&self, sent: &[Finding]) -> Option<String> {
        let places = |at: &[usize]| {
            let places: Vec<String> = (at.iter())
                .map(|at| format!("`{}:{}`", sent[*at].file, sent[*at].line))
                .collect();
            places.join(", ")
        };
        let disputed: Vec<usize> = self.disputed.iter().map(|(at, _)| *at).collect();
        let mut said = Vec::new();
        if !disputed.is_empty() {
            said.push(format!("disputed {}", places(&disputed)));
        }
        if !self.repeated.is_empty() {
            said.push(format!(
                "disputed {} again after a reviewer raised it again, which parks it",
                places(&self.repeated)
            ));
        }
        if !self.deferred.is_empty() {
            let deferred: Vec<usize> = self.deferred.iter().map(|(at, _)| *at).collect();
            said.push(format!("deferred {}", places(&deferred)));
        }
        if said.is_empty() {
            return None;
        }
        let mut said = format!("The worker {}", said.join(" and "));
        if !self.unanswered.is_empty() {
            said.push_str(&format!(
                ", and left {} neither fixed, deferred nor disputed with a reason",
                places(&self.unanswered)
            ));
        }
        said.push('.');
        Some(said)
    }
}

/// A dispute, with the bot thread it answers where the finding was one
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) struct Raised {
    /// The forge's id of the thread, for a bot's finding
    pub thread: Option<String>,
    /// The dispute
    pub dispute: Dispute,
}

/// Keeps `raised` on `item`, and the worker's reason as the answer to the bot
/// thread of each, which stays open
///
/// A dispute kept already for the same round is replaced, so a step that
/// runs again records nothing twice.
pub(in crate::runner) fn record(item: &mut WorkItem, raised: &[Raised]) {
    for Raised { thread, dispute } in raised {
        item.disputes
            .retain(|d| !(d.round == dispute.round && d.finding.is_same_as(&dispute.finding)));
        item.disputes.push(dispute.clone());
        if let Some(thread) = thread {
            item.thread_answers.retain(|a| a.thread() != thread);
            item.thread_answers.push(ThreadAnswer::Declined {
                thread: thread.clone(),
                reason: dispute.reason.clone(),
            });
        }
    }
}

/// Marks every dispute as shown to a reader
pub(in crate::runner) fn mark_shown(item: &mut WorkItem) {
    for dispute in &mut item.disputes {
        dispute.shown = true;
    }
}

impl Runner {
    // What a fix turn that moved the head disputed, kept for the next reader
    // as one that pushed nothing keeps it. A bot's thread is answered by the
    // worker's lines then, so none is recorded here.
    pub(super) fn record_pushed_disputes(
        &mut self,
        review: &Review,
        sent: Sent<'_>,
    ) -> Result<Option<String>, StateError> {
        let item = self.current().expect("a fix is a work item's");
        let (build, round) = (item.build.clone(), review.round);
        let already = item.disputes.clone();
        let answers = match answers(&build, sent, round, &already) {
            Ok(answers) => answers,
            Err(reason) => return Ok(Some(reason)),
        };
        let settled = Settled {
            deferred: 0,
            disputed: answers.disputed,
        };
        if settled.disputed.is_empty() {
            return Ok(None);
        }
        let Unfixed::Disputed { mut disputes, .. } = self.disputed_by(review, sent, settled) else {
            unreachable!("disputes are what `disputed_by` makes");
        };
        for raised in &mut disputes {
            raised.thread = None;
        }
        self.update(|item| record(item, &disputes))?;
        Ok(None)
    }

    // What `settled` found, as the fix turn's outcome: each dispute with who
    // raised it and, for a bot's finding, its thread, which `threads_sent`
    // lists in the order the findings were sent.
    pub(super) fn disputed_by(&self, review: &Review, sent: Sent<'_>, settled: Settled) -> Unfixed {
        let item = self.current().expect("a fix is a work item's");
        let by_thread = item.threads_sent.len() == sent.findings.len();
        let raised_by = (review.reviewer.as_ref()).map_or_else(String::new, ToString::to_string);
        let disputes = (settled.disputed.into_iter())
            .map(|(at, reason)| Raised {
                thread: by_thread.then(|| item.threads_sent[at].clone()),
                dispute: Dispute {
                    finding: sent.findings[at].clone(),
                    reason,
                    raised_by: raised_by.clone(),
                    round: review.round,
                    shown: false,
                },
            })
            .collect();
        Unfixed::Disputed {
            disputes,
            deferred: settled.deferred,
        }
    }
}

/// How posting the disputes ended
#[derive(Debug, PartialEq, Eq)]
pub(in crate::runner) enum Posted {
    /// They are posted, or there are none to post: what the merge ruling says of them
    Told(Option<String>),
    /// The forge could not take them just now, with why: the step tries again
    Retry(String),
    /// Kelpie's guard or a project's hook refused the comment, so asking again would be
    /// refused the same: what the merge ruling says of them
    Refused(String),
}

impl Runner {
    /// Posts the disputes no reviewer was shown on pull request `number` as
    /// one comment, as the App where one covers the repo
    ///
    /// The comment names a file of the work item's build folder by its name
    /// alone. A guard's or a hook's refusal is [`Posted::Refused`], and any
    /// other failure [`Posted::Retry`].
    ///
    /// # Errors
    ///
    /// [`StateError`] when the state file cannot be written.
    pub(in crate::runner) fn post_disputes(&mut self, number: u64) -> Result<Posted, StateError> {
        let item = self.current().expect("a dispute is a work item's");
        let unshown: Vec<&Dispute> = item.disputes.iter().filter(|d| !d.shown).collect();
        if unshown.is_empty() {
            return Ok(Posted::Told(
                (item.disputes_comment).map(|id| told(None, number, id)),
            ));
        }
        let body = by_file_name(comment(&unshown), &item.build);
        let count = unshown.len();
        let posted = match self.app_covers() {
            true => (self.ports.voice.post(&self.remote, number, &body)).map(|posted| posted.id),
            false => self.comment_as_gh_id(number, &body),
        };
        match posted {
            Ok(id) => {
                self.update(|item| {
                    mark_shown(item);
                    item.disputes_comment = Some(id);
                })?;
                Ok(Posted::Told(Some(told(Some(count), number, id))))
            }
            Err(e @ (ForgeError::LocalPath { .. } | ForgeError::Hook { .. })) => {
                Ok(Posted::Refused(format!(
                    "The worker disputed {} that no reviewer read again, and they were not posted on #{number}: {e}. \
                     A yes merges without them posted.",
                    findings_of(count)
                )))
            }
            Err(e) => Ok(Posted::Retry(format!(
                "cannot post the worker's {} disputes on #{number}: {e}",
                findings_of(count)
            ))),
        }
    }
}

// A file of `build` is named by its name alone, so a reason that quotes the
// full path the fix turn's prompt gave posts nothing of this machine's. Any
// other path is left for the guard.
fn by_file_name(body: String, build: &Path) -> String {
    let mut folders = vec![build.to_string_lossy().into_owned()];
    if let Ok(canonical) = std::fs::canonicalize(build) {
        folders.push(canonical.to_string_lossy().into_owned());
    }
    folders.sort_by_key(|folder| std::cmp::Reverse(folder.len()));
    folders.dedup();
    folders.iter().fold(body, |body, folder| {
        let body = body.replace(&format!("{folder}/"), "");
        body.replace(&format!("{folder}{}", std::path::MAIN_SEPARATOR), "")
    })
}

fn findings_of(count: usize) -> String {
    match count {
        1 => "1 finding".to_owned(),
        n => format!("{n} findings"),
    }
}

// `count` is how many the comment lists, unknown where it was posted by an earlier step
fn told(count: Option<usize>, number: u64, comment: u64) -> String {
    let which = count.map_or_else(|| "findings".to_owned(), findings_of);
    format!(
        "The worker disputed {which} that no reviewer read again: they are in comment {comment} on #{number}."
    )
}

fn comment(disputes: &[&Dispute]) -> String {
    let mut text = String::from(
        "The worker disputed these findings, saying each is wrong, and no reviewer follows to read the reasons:\n",
    );
    for Dispute {
        finding,
        reason,
        raised_by,
        ..
    } in disputes
    {
        let who = if raised_by.is_empty() {
            "a reader"
        } else {
            raised_by
        };
        text.push_str(&format!(
            "\n{}\n  Raised by {who}. The worker's reason: {reason}\n",
            finding_line(finding)
        ));
    }
    text
}
