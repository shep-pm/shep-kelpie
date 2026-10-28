//! Turns that end without a reply to act on: one that failed, one that ran
//! past its ceiling, and one that stopped short of a pull request or, on a
//! rework or an adoption, of a push
//!
//! Each parks the work item on a ruling whose yes puts the turn back, and
//! whose no stops the work item. A turn that stopped short is sent back to
//! the worker once before it parks. The prompt for a turn that left files
//! uncommitted lives here too, but it is a separate send-back: it parks
//! nothing, and the work item's `asked_to_commit` makes it once per turn.

use std::time::{Duration, UNIX_EPOCH};

use super::handover::{Handover, Refused};
use crate::board::closes;
use crate::ports::Timestamp;
use crate::runner::closed::{end_closed, end_covered};
use crate::runner::pull_request_file;
use crate::runner::report::{Begin, StepReport};
use crate::runner::ruling::park;
use crate::runner::{Names, Runner};
use crate::state::{ProjectState, StateError, Stuck};
use crate::ticket::TicketId;
use crate::work_item::{Phase, Review, Turn, WorkItem};
use crate::worktree;

/// The prompt that sends back a worker whose turn ended with no pull request
/// and no question
const STOPPED_SHORT: &str = "Your last turn ended with no pull request for this \
                             work item and no question. If a tool failed, try it \
                             again or find another way, and open the draft pull \
                             request once the work is done. If only the maintainer \
                             can unblock you, end your reply with a \
                             <kelpie-question> block.";

/// The prompt that resumes a worker whose turn ran past its ceiling while it
/// was still writing in its worktree
const CEILING_RESUMED: &str = "Kelpie stopped your last turn: it ran past its ceiling. \
                               Commit what builds, then carry on with the work item \
                               from where you left off.";

/// How long before its ceiling a turn must have touched its worktree to be
/// resumed with no ruling
const CEILING_WINDOW: Duration = Duration::from_secs(10 * 60);

/// How many times a work item's turns past their ceilings are resumed with no
/// ruling; the next raises one
const CEILING_RESUMES: u32 = 3;

/// The prompt that sends back a worker whose credential opens no pull request
/// and whose turn ended with none opened and no question
fn hand_over_stopped_short() -> String {
    format!(
        "Your last turn ended with no pull request for this work item and no \
         question. If a tool failed, try it again or find another way. Push the \
         branch with `git push origin HEAD` and write the pull request's title and \
         body in `{file}`: kelpie opens the draft from them when your turn ends. If \
         only the maintainer can unblock you, end your reply with a \
         <kelpie-question> block.",
        file = pull_request_file::FILE
    )
}

/// The prompt that sends back a worker whose rework or adoption turn pushed
/// nothing and asked nothing
const PUSHED_NOTHING: &str = "Your last turn ended without a new push to this \
                              pull request and with no question. If a tool \
                              failed, try it again or find another way, and push \
                              the change once it is done. If only the \
                              maintainer can unblock you, end your reply with a \
                              <kelpie-question> block.";

/// The most uncommitted files a prompt names before it counts the rest
const NAMED_FILES: usize = 10;

/// `files` as a prompt or a ruling names them: the first few, then a count
pub(in crate::runner) fn named_files(files: &[String]) -> String {
    let mut named = files
        .iter()
        .take(NAMED_FILES)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if files.len() > NAMED_FILES {
        named.push_str(&format!(" and {} more", files.len() - NAMED_FILES));
    }
    named
}

/// The prompt that sends back a worker whose turn pushed nothing and left
/// `files` uncommitted in its worktree
///
/// A turn that ends is over, and nothing the worker started in the
/// background will wake it, so the prompt says to wait in the foreground.
pub(super) fn uncommitted_prompt(files: &[String]) -> String {
    let named = named_files(files);
    format!(
        "Your last turn ended with uncommitted changes in your worktree and nothing \
         pushed: {named}. A turn that ends is over, and nothing wakes you when a \
         command you left running finishes, so run commands in the foreground and \
         wait for them. Finish the work, commit it, and push it with \
         `git push origin HEAD`, or discard what you do not want. If only the \
         maintainer can unblock you, end your reply with a <kelpie-question> block."
    )
}

/// Why a turn that stopped short twice parks
const STOPPED_TWICE: &str = "it ended twice with no pull request and no question";

/// Why a rework or adoption turn that pushed nothing twice parks
const PUSHED_NOTHING_TWICE: &str = "it ended twice with no new push and no question";

/// Whether `item` is a rework or an adoption in its implement phase, whose
/// turn pushed nothing if it ends there
///
/// A comment round's turn may push nothing, since the worker may decline.
pub(super) fn awaits_a_push(item: &WorkItem) -> bool {
    (item.rework || item.adopted)
        && item.talked.head.is_none()
        && item.pull_request.is_some()
        && matches!(item.phase, Phase::Implement)
}

impl Runner {
    pub(in crate::runner) fn turn_ceiling(&self) -> Duration {
        Duration::from_secs(u64::from(self.settings.worker.turn_timeout.get()) * 60)
    }

    // A turn that ran past its ceiling resumes with no ruling when its worktree
    // changed in the last ten minutes, up to three times for the work item: it is
    // making progress, not stuck. A worktree git cannot be read in is told and
    // let go, which errs toward the ruling.
    pub(super) fn resumes_past_ceiling(&mut self, item: &mut WorkItem, now: Timestamp) -> bool {
        if item.ceiling_resumes >= CEILING_RESUMES {
            return false;
        }
        let since =
            UNIX_EPOCH + Duration::from_secs(now.0.saturating_sub(CEILING_WINDOW.as_secs()));
        let base = self.item_base(item);
        let touched =
            worktree::touched_since(&self.settings.git.checkout, &item.worktree, base, since);
        let issue = item.issue;
        match touched {
            Ok(true) => {}
            Ok(false) => return false,
            Err(e) => {
                eprintln!("cannot tell whether issue #{issue}'s worktree changed: {e}");
                return false;
            }
        }
        item.ceiling_resumes += 1;
        item.turn = Turn::Next {
            prompt: CEILING_RESUMED.to_owned(),
        };
        self.notes.push(format!(
            "issue #{issue}'s turn passed its ceiling while still working, resumed ({} of {CEILING_RESUMES})",
            item.ceiling_resumes
        ));
        true
    }

    // A turn ended with no pull request kelpie knows of, or on a rework or an
    // adoption with nothing pushed, and with no question. The forge is asked
    // again, since the turn's end may have missed one. With none open, an
    // issue closed with no change ends the work item; else the worker is sent back once.
    pub(super) fn stopped_short(&mut self) -> Result<Begin, StateError> {
        let item = self.current().expect("a turn is a work item's");
        let (issue, sent_back) = (item.issue, item.sent_back);
        let open = match self.ports.forge.open_pull_requests(&self.remote) {
            Ok(open) => open,
            Err(e) => {
                let reason = format!("cannot list open pull requests: {e}");
                return Ok(Begin::Report(StepReport::GateFailed { issue, reason }));
            }
        };
        let (pushed_nothing, known) = (awaits_a_push(item), item.pull_request.is_some());
        let found = (open.iter())
            .find(|pr| pr.head == item.branch)
            .filter(|_| !pushed_nothing)
            .map(|pr| pr.number);
        let (build, branch) = (item.build.clone(), item.branch.clone());
        let base = self.item_base(item).to_owned();
        // A refusal from the turn's end is told to the worker before the file
        // is tried again.
        let retry = item.known.head.is_some() && item.pull_request_refused.is_none();
        let handed_over = self.app_covers();
        // A contributor's pull request that closes the issue covers the work item, which
        // ends here rather than go back to a worker whose work is done elsewhere.
        let covered = match found {
            None if !known => self.covered_by(&open, issue, &branch),
            _ => None,
        };
        if let Some(number) = covered {
            let mut next = self.state.clone();
            end_covered(&mut next, issue);
            self.save(next)?;
            self.say_covered(issue, number, None);
            return self.begin_item(false);
        }
        // A pull request on another branch that closes the issue is work too.
        let claimed = (open.iter()).any(|pr| closes(pr, issue));
        let closed = match found {
            None if !known && !claimed => self.closed_with_no_change(issue),
            _ => Ok(false),
        };
        let mut next = self.state.clone();
        match closed {
            Ok(false) => {}
            Ok(true) => {
                end_closed(&mut next, issue);
                self.save(next)?;
                self.say_closed(issue, None);
                return self.begin_item(false);
            }
            Err(reason) => {
                return Ok(Begin::Report(StepReport::GateFailed { issue, reason }));
            }
        }
        // A hand-over worker's branch pushed with its file written is opened
        // from here, not sent back for a pull request it cannot open.
        let mut refusal = None;
        let found = match found {
            None if handed_over && retry && !pushed_nothing && !known => {
                match self.open_handed_over(&build, &branch, &base) {
                    Ok(opened) => opened,
                    Err(refused) => {
                        refusal = Some(pull_request_file::told(&refused));
                        self.started.push(StepReport::PullRequestNotOpened {
                            issue,
                            reason: refused.to_string(),
                        });
                        None
                    }
                }
            }
            found => found,
        };
        let item = next.item_mut(issue).expect("the work item checked above");
        if let Some(number) = found {
            item.pull_request = Some(number);
            // A pull request found only now was never handed over, so the
            // project's `handover` hooks judge the branch before review reads it.
            match self.handover(item) {
                Handover::Refused(why) => {
                    let refused = Refused::branch(&why);
                    if item.handover_refused {
                        let now = self.ports.clock.now();
                        let mut report = failed(
                            self.names(),
                            &mut next,
                            issue,
                            now,
                            refused.parked,
                            Vec::new(),
                            false,
                        );
                        self.save(next)?;
                        self.fill_comment_failed(&mut report);
                        return Ok(Begin::Report(report));
                    }
                    item.handover_refused = true;
                    item.turn = Turn::Next {
                        prompt: refused.prompt,
                    };
                    // The pull request still owes its first review.
                    item.resume = Some(Phase::Review(Review::first()));
                    self.save(next)?;
                    return self.begin_item(false);
                }
                Handover::Passed(head) => {
                    item.handed_over = Some(head);
                    item.handover_refused = false;
                }
                Handover::NotDue => {}
            }
            item.phase = Phase::Review(Review::first());
            self.save(next)?;
            return self.begin_item(false);
        }
        // A pull request file the forge refused is told to the worker, and to
        // the project manager if it parks.
        let refused = item.pull_request_refused.take().or(refusal);
        let (prompt, why) = match (pushed_nothing, refused) {
            (true, _) => (PUSHED_NOTHING.to_owned(), PUSHED_NOTHING_TWICE.to_owned()),
            (false, Some(refusal)) => (
                format!(
                    "Your last turn ended with no pull request: kelpie could not open one from \
                     `pull-request.md`: {refusal}. Kelpie tries again when your turn ends. If \
                     only the maintainer can unblock you, end your reply with a \
                     <kelpie-question> block."
                ),
                format!("kelpie could not open its pull request from `pull-request.md`: {refusal}"),
            ),
            (false, None) if handed_over => (hand_over_stopped_short(), STOPPED_TWICE.to_owned()),
            (false, None) => (STOPPED_SHORT.to_owned(), STOPPED_TWICE.to_owned()),
        };
        item.turn = Turn::Next { prompt };
        if !sent_back {
            item.sent_back = true;
            self.save(next)?;
            return self.begin_item(false);
        }
        let now = self.ports.clock.now();
        let mut report = failed(self.names(), &mut next, issue, now, why, Vec::new(), false);
        self.save(next)?;
        self.fill_comment_failed(&mut report);
        Ok(Begin::Report(report))
    }

    pub(super) fn park_ceiling_passed(&mut self, now: Timestamp) -> Result<Begin, StateError> {
        let mut next = self.state.clone();
        let issue = self.current().expect("a turn is a work item's").issue;
        next.item_mut(issue)
            .expect("the work item checked above")
            .turn = Turn::Ended { at: now };
        let mut report = timed_out(self.names(), &mut next, issue);
        self.save(next)?;
        self.fill_comment_failed(&mut report);
        Ok(Begin::Report(report))
    }

    // A ruling just raised is posted as a comment on its pull request, if it
    // has one; only these three reports carry a ruling and need the outcome.
    pub(in crate::runner) fn fill_comment_failed(&mut self, report: &mut StepReport) {
        match report {
            StepReport::Asked {
                pull_request,
                id,
                comment_failed,
                ..
            }
            | StepReport::TimedOut {
                pull_request,
                id,
                comment_failed,
                ..
            }
            | StepReport::Failed {
                pull_request,
                id,
                comment_failed,
                ..
            } => {
                *comment_failed = self.post_ruling(*pull_request, *id);
            }
            _ => {}
        }
    }
}

// Parks the work item on a turn-ceiling ruling and builds its report. Shared
// by a call that hit `AgentError::TimedOut` and by a restart that finds a turn
// past its ceiling with no call spent. The caller sets `item.turn` beforehand:
// this only raises the ruling. `comment_failed` is filled in after the post.
pub(super) fn timed_out(names: Names, next: &mut ProjectState, issue: TicketId) -> StepReport {
    let item = next
        .item_mut(issue)
        .expect("a turn ceiling is about an open work item");
    let (session, pull_request) = (item.session.clone(), item.pull_request);
    let phase = Some(item.phase.clone());
    let (id, question) = park(
        names,
        next,
        issue,
        pull_request,
        Stuck::TurnTimeout { phase }.into(),
    );
    StepReport::TimedOut {
        issue,
        session,
        pull_request,
        id,
        question,
        comment_failed: None,
    }
}

// Marks the turn failed and parks the work item on a ruling carrying why,
// keeping the turn as it stood so a yes can put it back. `comment_failed` is
// filled in afterwards, once the ruling has actually been posted.
pub(in crate::runner) fn failed(
    names: Names,
    next: &mut ProjectState,
    issue: TicketId,
    at: Timestamp,
    reason: String,
    restore: Vec<String>,
    setup: bool,
) -> StepReport {
    let item = next
        .item_mut(issue)
        .expect("a failed turn is about an open work item");
    let failure = Turn::Failed {
        at,
        reason: reason.clone(),
    };
    let retry = std::mem::replace(&mut item.turn, failure);
    let pull_request = item.pull_request;
    let kind = Stuck::TurnFailed {
        why: reason,
        phase: item.phase.clone(),
        retry,
        restore,
        setup,
    }
    .into();
    let (id, question) = park(names, next, issue, pull_request, kind);
    StepReport::Failed {
        issue,
        pull_request,
        id,
        question,
        comment_failed: None,
    }
}
