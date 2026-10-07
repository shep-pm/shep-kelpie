//! The review: one pass down the project's reviewers, each run once, in order
//!
//! A round with no findings goes straight to the next reviewer. One with any,
//! nits (LOW) included, sends the worker all of them but a size-skipped
//! file's notice, at the reviewer's own severity, for one fix turn, and the
//! next reviewer reads the fix. A fix turn sent only nits starts nothing
//! new: the pass goes on as after any fix turn, one that pushes nothing
//! declines the nits, and the worker's own head it pushes is kept, so a bot
//! that read the pull request before has its nits on that head left open
//! rather than sent again. A
//! reviewer with a second look reads twice first, the second time shown what
//! it found, and both lists go to that one fix turn. A review bot's round is
//! [`super::review_bot`]'s, and its open threads are its findings. After the
//! last reviewer the work item goes on to [`super::gate`].

pub(super) mod calls;
mod criteria;
pub(super) mod findings;
mod lineup;
#[cfg(test)]
mod local;
#[cfg(test)]
mod nits;
mod prompts;
pub(super) mod pushed;
#[cfg(test)]
mod second_look;
#[cfg(test)]
mod several;
#[cfg(test)]
mod unread;

use std::path::Path;

use super::Runner;
use super::report::{Begin, ReviewCall, ReviewResult, Reviewed, Spent, StepReport};
use super::review_bot::Resolved;
use super::ruling::park;
use crate::agents::{DEFECT_HUNTER, QWEN};
use crate::pacer::Scope;
use crate::ports::{
    AgentCall, AgentError, AgentReply, Agents, Ending, Finding, Reviewer, ReviewerError,
    RoundStage, Timestamp, read_review,
};
use crate::settings::{AgentName, ListedReviewer};
use crate::state::{Fix, StateError, Stuck};
use crate::work_item::{CallKind, Phase, Review, ReviewCallState, ReviewStage, Turn, WorkItem};
use crate::worktree;

// A reviewer whose call fails this many times in a row is passed over for
// the rest of its pass: a reply it cannot give would otherwise stall the
// review for good.
const ROUND_FAILURES: u32 = 3;

impl Runner {
    pub(super) fn review_step(&mut self) -> Result<Begin, StateError> {
        // A step that just moved the work item into its review, as an owed
        // bots' pass after green CI does, leaves it waiting when no slot is free.
        if self.unseated() {
            return Ok(Begin::Idle);
        }
        let item = self.current().expect("review runs on a work item");
        let Phase::Review(review) = item.phase.clone() else {
            unreachable!("review_step only runs in the review phase")
        };
        let number = item
            .pull_request
            .expect("review starts once a pull request is known");
        let (issue, worktree) = (item.issue, item.worktree.clone());
        let base = item.review_base();
        let build = item.build.clone();
        // A new pass reviews new code, so no fix of the last one will
        // resolve the bot threads it sent, and its bots are asked afresh.
        let fresh = review
            == Review {
                bots_only: review.bots_only,
                ..Review::first()
            };
        if fresh && (!item.threads_sent.is_empty() || !item.bots_skipped.is_empty()) {
            self.update(WorkItem::new_pass)?;
        }

        match review.stage.clone() {
            ReviewStage::Round => {
                // First, since who reviews is chosen from the worktree's diff.
                let mut review = review;
                if let Some(begin) = self.before_round(&mut review)? {
                    return Ok(begin);
                }
                if let Some(begin) = self.late_review()? {
                    return Ok(begin);
                }
                let chosen = match self.choose_reviewer(&review, &worktree, &base) {
                    Ok(Some(chosen)) => chosen,
                    Ok(None) => return self.pass_ended(&review),
                    Err(reason) => return Ok(self.gate_failed(reason)),
                };
                if let Some(bot) = chosen.bot() {
                    return self.bot_round(&chosen, bot, review);
                }
                let Some(local) = chosen.runs.local() else {
                    return self.session_call(&chosen, None);
                };
                if self.draining {
                    return Ok(Begin::Idle);
                }
                let criteria = match self.criteria(issue) {
                    Ok(criteria) => criteria,
                    Err(reason) => return Ok(self.gate_failed(reason)),
                };
                self.round_started(&chosen, CallKind::Local)?;
                Ok(Begin::Review(ReviewCall::Local {
                    local,
                    worktree,
                    base,
                    out: build.join("qwen-review"),
                    round: review.round,
                    criteria,
                }))
            }
            ReviewStage::SecondLook { first } => {
                let listed = review.reviewer.as_ref().and_then(|n| self.listed(n));
                match listed {
                    Some(chosen) => self.session_call(&chosen, Some(first)),
                    // Taken off the list since its first look, whose findings go on alone.
                    None => self.send_findings(review, first, Vec::new()),
                }
            }
            ReviewStage::Found { findings, threads } => {
                self.send_findings(review, findings, threads)
            }
            ReviewStage::Summon { .. }
            | ReviewStage::Summoned { .. }
            | ReviewStage::Settling { .. } => self.bot_step(),
            // begin_turn drives the worker's turn itself, and comes here once it ends.
            ReviewStage::Pushing => self.pushing_ended(review),
            ReviewStage::Fixing {
                head,
                sent,
                deferred_before,
            } => {
                let sent = findings::Sent {
                    findings: &sent,
                    deferred_before: &deferred_before,
                };
                self.fix_ended(number, &build, review, head, sent)
            }
        }
    }

    // A fresh session of `chosen`: its first look, or with `first` its second.
    fn session_call(
        &mut self,
        chosen: &ListedReviewer,
        first: Option<Vec<Finding>>,
    ) -> Result<Begin, StateError> {
        let Some((model, limit)) = chosen.runs.session() else {
            unreachable!("only a reviewer that runs sessions is called in one")
        };
        if self.draining {
            return Ok(Begin::Idle);
        }
        if let Some(held) = self.pace(Scope::Turn, limit)?.holds() {
            return Ok(held);
        }
        let item = self.current().expect("a review is a work item's");
        let (issue, worktree, base) = (item.issue, item.worktree.clone(), item.review_base());
        let criteria = match self.criteria(issue) {
            Ok(criteria) => criteria,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let diff = match calls::diff_against(&worktree, &base) {
            Ok(diff) => diff,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let body = chosen.prompt.as_deref().unwrap_or_default();
        let prompt = prompts::reviewer_prompt(body, &base, &diff, &criteria, first.as_deref());
        let call =
            calls::reviewer_call(issue, &worktree, &self.paths.worker, (model, limit), prompt);
        match call.and_then(|call| self.prepared(call)) {
            Ok(call) => {
                match first {
                    None => self.round_started(chosen, CallKind::Claude)?,
                    Some(_) => self.mark_review_call_running(CallKind::Claude)?,
                }
                Ok(Begin::Review(ReviewCall::Session(call)))
            }
            Err(reason) => Ok(self.gate_failed(reason)),
        }
    }

    // The last reviewer is done, so the work item goes on to CI. A pass no
    // reviewer read because one was down or failing marks the work item
    // unreviewed, which the merge ruling names and which `auto` will not
    // merge. One with nobody to read it, by the project's own list, is noted.
    fn pass_ended(&mut self, review: &Review) -> Result<Begin, StateError> {
        let since = self.ports.clock.now();
        let item = self.current().expect("a pass is a work item's");
        let (issue, number) = (item.issue, item.pull_request);
        let unread = review.unread.then(|| self.unread(item, review));
        // Nobody to read it, by the project's own list, sends it unread on purpose.
        let unread_by_choice = match &unread {
            Some(None) => self.origin_head().map_err(|e| eprintln!("{e}")).ok(),
            _ => None,
        };
        if let Some(None) = &unread {
            let why = match self.lineup.is_empty() {
                true => "the project lists no reviewer",
                false => "each listed reviewer's `paths`, or a bot's `rounds`, passed it over",
            };
            self.notes.push(format!(
                "issue #{issue}'s pull request goes to CI with no reviewer's read: {why}"
            ));
        }
        let reason = unread.flatten();
        let flagged = reason.clone();
        self.update(|item| {
            item.phase = Phase::Ci { head: None, since };
            if let Some(head) = unread_by_choice {
                item.send_unread(head);
            }
            if flagged.is_some() {
                item.unreviewed = flagged;
            }
        })?;
        match (reason, number) {
            (Some(reason), Some(pull_request)) => Ok(Begin::Report(StepReport::Unreviewed {
                issue,
                pull_request,
                reason,
            })),
            _ => self.check_ci(),
        }
    }

    // A fix turn that pushed nothing fixed nothing, whatever it says: the
    // findings sent still stand, so the next reviewer does not run yet,
    // unless the worker deferred every one of them as out of scope, or they
    // were all nits, which the worker may decline: a ruling over nits would
    // ask the maintainer about what never holds a merge.
    fn fix_ended(
        &mut self,
        number: u64,
        build: &Path,
        review: Review,
        head: Option<String>,
        sent: findings::Sent<'_>,
    ) -> Result<Begin, StateError> {
        let issue = self.current().expect("a fix is a work item's").issue;
        let round = review.round;
        let mut left_open = None;
        let pushed = match head {
            Some(before) => match self.origin_head() {
                Ok(now) if now == before => {
                    // A size-skipped file's notice an older kelpie sent is no finding.
                    let count = (sent.findings.iter())
                        .filter(|f| !f.is_skipped_for_size())
                        .count();
                    if sent.findings.iter().all(Finding::is_nit) {
                        return self.went_unfixed(number, review, Unfixed::NitsDeclined(count));
                    }
                    match findings::all_deferred(build, sent) {
                        Ok(true) => {
                            return self.went_unfixed(number, review, Unfixed::Deferred(count));
                        }
                        Ok(false) => {}
                        Err(reason) => return Ok(self.gate_failed(reason)),
                    }
                    let path = findings::findings_path(build);
                    let prompt = findings::again_prompt(number, round, &path);
                    let fix = Fix::Review(review);
                    return self.raise(number, Stuck::FixNotPushed { fix, prompt }.into());
                }
                // A fix that moved the head answers the bot threads it was sent.
                Ok(now) => {
                    match self.resolve_sent(number)? {
                        Resolved::Done => {}
                        Resolved::Retry(reason) => return Ok(self.gate_failed(reason)),
                        Resolved::LeftOpen { threads, reason } => {
                            left_open = Some((threads, reason));
                        }
                    }
                    // Only the worker's own commit is its fix: a push from
                    // anyone else stays unvouched for.
                    // So is a nit fix's head, which a bot's nits are not sent on.
                    if self.worktree_head().as_ref() == Some(&now) {
                        let head = now.clone();
                        let nits = sent.findings.iter().all(Finding::is_nit);
                        self.update(|item| {
                            item.send_unread(head.clone());
                            if nits {
                                item.nit_fixed(head);
                            }
                        })?;
                    }
                    Some(now)
                }
                Err(reason) => return Ok(self.gate_failed(reason)),
            },
            None => None,
        };
        let next = self.after_round(review, self.ports.clock.now());
        self.update(|item| item.phase = next)?;
        Ok(Begin::Report(match left_open {
            Some((threads, reason)) => StepReport::ThreadsLeftOpen {
                issue,
                pull_request: number,
                threads,
                reason,
            },
            None => StepReport::FixPushed {
                issue,
                pull_request: number,
                round,
                head: pushed,
            },
        }))
    }

    // Every finding sent was left for a follow-up issue, or every one was a
    // nit the worker declined, so there was nothing to push, and the next
    // reviewer reads the pull request as it stands. Bot threads sent stay
    // open, since nothing answered them.
    fn went_unfixed(
        &mut self,
        number: u64,
        review: Review,
        unfixed: Unfixed,
    ) -> Result<Begin, StateError> {
        let issue = self.current().expect("a fix is a work item's").issue;
        let round = review.round;
        let next = self.after_round(review, self.ports.clock.now());
        self.update(|item| {
            item.forget_threads();
            item.phase = next;
        })?;
        Ok(Begin::Report(match unfixed {
            Unfixed::Deferred(deferred) => StepReport::FindingsDeferred {
                issue,
                pull_request: number,
                round,
                deferred,
            },
            Unfixed::NitsDeclined(nits) => StepReport::NitsDeclined {
                issue,
                pull_request: number,
                round,
                nits,
            },
        }))
    }

    // The commit the work item's worktree has checked out, or none when git
    // cannot say, which leaves the head unrecorded and errs toward a ruling.
    pub(super) fn worktree_head(&self) -> Option<String> {
        let item = self.current()?;
        match worktree::head(&self.settings.repo, &item.worktree) {
            Ok(head) => Some(head),
            Err(e) => {
                eprintln!("cannot read issue #{}'s worktree head: {e}", item.issue);
                None
            }
        }
    }

    // Asks git rather than the forge: the forge's head lags a push by a moment.
    pub(super) fn origin_head(&self) -> Result<String, String> {
        let item = self.current().expect("a head is a work item's");
        worktree::origin_head(&self.settings.repo, &item.branch).map_err(|e| e.to_string())
    }

    // Recorded in state before the call is started, the same as a worker's
    // turn marks `Turn::Running`: `drop`
    // refuses while this is set, so a call in flight always has a work
    // item to land its result on.
    pub(super) fn mark_review_call_running(&mut self, kind: CallKind) -> Result<(), StateError> {
        let since = self.ports.clock.now();
        self.update(|item| item.call_started(kind, since))
    }

    // Marks the call running and keeps who reviews the round, so a round
    // cut short resumes with the same reviewer.
    fn round_started(&mut self, chosen: &ListedReviewer, kind: CallKind) -> Result<(), StateError> {
        let since = self.ports.clock.now();
        let name = chosen.name.clone();
        self.update(|item| {
            item.call_started(kind, since);
            if let Phase::Review(review) = &mut item.phase {
                review.reviewer = Some(name);
            }
        })
    }

    // Every finding goes, at the reviewer's own severity, nits included, and
    // a bot's threads sent with them are kept to resolve once the fix moves
    // the head. A file the script skipped for its size is reported with the
    // round but never sent, since no fix of the worker's reviews it. A round
    // left with nothing to send goes on to the next reviewer.
    fn send_findings(
        &mut self,
        review: Review,
        findings: Vec<Finding>,
        threads: Vec<String>,
    ) -> Result<Begin, StateError> {
        let item = self.current().expect("findings are a work item's");
        let issue = item.issue;
        let number = item
            .pull_request
            .expect("review runs once a pull request is known");
        let round = review.round;
        let findings: Vec<Finding> = findings
            .into_iter()
            .filter(|f| !f.is_skipped_for_size())
            .collect();
        if findings.is_empty() {
            let next = self.after_round(review, self.ports.clock.now());
            self.update(|item| item.phase = next)?;
            return Ok(Begin::Report(StepReport::ReviewFindingsSent {
                issue,
                pull_request: number,
                round,
                held: 0,
            }));
        }
        if let Err(reason) = self.bot_label_off(&review) {
            return Ok(self.gate_failed(reason));
        }
        let item = self.current().expect("findings are a work item's");
        let head = match self.origin_head() {
            Ok(head) => head,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let build = &item.build;
        let path = findings::findings_path(build);
        if let Err(reason) = findings::write_findings_file(build, &path, round, &findings) {
            return Ok(self.gate_failed(reason));
        }
        let held = findings.len();
        let prompt = findings::fix_prompt(number, round, held, &path);
        let deferred_before = match findings::deferred(build) {
            Ok(deferred) => deferred,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        self.update(|item| {
            item.record_held(&findings);
            for thread in threads {
                if !item.threads_sent.contains(&thread) {
                    item.threads_sent.push(thread);
                }
            }
            item.turn = Turn::Next { prompt };
            item.counts.fix_turns = item.counts.fix_turns.saturating_add(1);
            item.phase = Phase::Review(Review {
                stage: ReviewStage::Fixing {
                    head: Some(head),
                    sent: findings,
                    deferred_before,
                },
                ..review
            });
        })?;
        Ok(Begin::Report(StepReport::ReviewFindingsSent {
            issue,
            pull_request: number,
            round,
            held,
        }))
    }

    pub(super) fn end_review(
        &mut self,
        reviewed: Reviewed,
    ) -> Result<Option<StepReport>, StateError> {
        let Reviewed { result, spent } = reviewed;
        // A call stopped with the runner saves nothing, the same as a turn
        // in `end_turn`: its round is still due, and runs again once a
        // restart clears `review_call`.
        if matches!(result, ReviewResult::Stopped) {
            return Ok(None);
        }
        let now = self.ports.clock.now();
        let (local_round, second_look) = match self.current().map(|item| &item.phase) {
            Some(Phase::Review(review)) => {
                let listed = review.reviewer.as_ref().and_then(|n| self.listed(n));
                let second_look = listed.as_ref().is_some_and(|r| r.second_look);
                (self.is_local_round(review), second_look)
            }
            _ => (false, false),
        };
        // The round read the pushed head its start found the worktree at, if
        // nothing moved or dirtied the worktree while it ran.
        let reading = match self.current().map(|item| &item.phase) {
            Some(Phase::Review(review)) => review.reading.clone(),
            _ => None,
        };
        let read = reading
            .filter(|_| matches!(&result, ReviewResult::Findings(Ok(_))))
            .filter(|head| self.still_at(head));
        let mut next = self.state.clone();
        // Tolerated the same way `end_turn` tolerates a turn's result
        // arriving with nothing (or something else) to apply it to: the
        // guard on `drop` keeps this from happening today, but a result
        // for a work item that is no longer this one, or gone outright,
        // is silently discarded rather than panicking the runner.
        let Some(item) = self.current_in(&mut next) else {
            return Ok(None);
        };
        record_spent(item, spent, now);
        // What the call cost and that it ended are kept whatever became of its answer.
        let Phase::Review(review) = item.phase.clone() else {
            self.save(next)?;
            return Ok(None);
        };
        // Read above; the round's next start checks the worktree afresh.
        let review = Review {
            reading: None,
            ..review
        };
        if !matches!(
            review.stage,
            ReviewStage::Round | ReviewStage::SecondLook { .. }
        ) {
            self.save(next)?;
            return Ok(None);
        }
        let issue = item.issue;
        let number = item
            .pull_request
            .expect("review runs once a pull request is known");
        let round = review.round;

        // Not a failed gate to wait out: the maintainer moves the model, so
        // the round is parked on a ruling, which alerts.
        if let ReviewResult::Spilled(reason) = &result {
            let reason = reason.clone();
            let kind = Stuck::LocalModelSpilled {
                review,
                why: reason,
            }
            .into();
            let (id, question) = park(self.names(), &mut next, issue, Some(number), kind);
            self.save(next)?;
            let comment_failed = self.post_ruling(Some(number), id);
            return Ok(Some(StepReport::Ruling {
                issue,
                pull_request: number,
                id,
                question,
                comment_failed,
            }));
        }
        let ReviewResult::Findings(result) = result else {
            unreachable!("a stopped call and a spilled model return above")
        };
        if result.is_ok() && matches!(review.stage, ReviewStage::Round) {
            item.counts.review_rounds = item.counts.review_rounds.saturating_add(1);
        }

        // The script writes a line for a file it could not review, and still
        // finishes the round: those lines are not findings.
        let (result, unreviewed) = match result {
            Ok(findings) if local_round => {
                let (unreviewed, findings) = findings.into_iter().partition(Finding::is_unreviewed);
                (Ok(findings), unreviewed)
            }
            result => (result, Vec::new()),
        };
        // An older state file names no reviewer for its round.
        let reviewer = review
            .reviewer
            .clone()
            .unwrap_or_else(|| AgentName::kelpies(if local_round { QWEN } else { DEFECT_HUNTER }));
        let unreviewed: Vec<String> = unreviewed.into_iter().map(|f: Finding| f.file).collect();

        let report = match (review.stage.clone(), result) {
            // The call stays due; one that keeps failing goes no further.
            (_, Err(reason)) if review.failures + 1 < ROUND_FAILURES => {
                let failures = review.failures + 1;
                item.phase = Phase::Review(Review { failures, ..review });
                StepReport::GateFailed { issue, reason }
            }
            // A second look that keeps failing leaves the first's findings to go alone.
            (ReviewStage::SecondLook { first }, Err(reason)) => {
                item.phase = Phase::Review(Review {
                    stage: ReviewStage::Found {
                        findings: first,
                        threads: Vec::new(),
                    },
                    failures: 0,
                    ..review
                });
                StepReport::GateFailed {
                    issue,
                    reason: format!(
                        "{reviewer}'s second look on #{number} failed {ROUND_FAILURES} times, \
                         so its first look's findings go on alone: {reason}"
                    ),
                }
            }
            (_, Err(reason)) => {
                if !item.reviewers_skipped.contains(&reviewer) {
                    item.reviewers_skipped.push(reviewer.clone());
                }
                item.phase = self.after_round(review, now);
                StepReport::ReviewerSkipped {
                    issue,
                    pull_request: number,
                    round,
                    reviewer,
                    reason,
                }
            }
            (ReviewStage::Round, Ok(first)) if second_look && !local_round => {
                let findings = first.len();
                item.unreviewed = None;
                if let Some(head) = read {
                    item.reviewed(head);
                }
                item.phase = Phase::Review(Review {
                    stage: ReviewStage::SecondLook { first },
                    failures: 0,
                    unread: false,
                    ..review
                });
                StepReport::FirstLook {
                    issue,
                    pull_request: number,
                    round,
                    reviewer,
                    findings,
                }
            }
            (stage, Ok(found)) => {
                let findings = match stage {
                    ReviewStage::SecondLook { first } => both_looks(first, found),
                    _ => found,
                };
                // Every file went unreviewed, so the round looked at nothing,
                // and the pass goes on to the next reviewer.
                if findings.is_empty() && !unreviewed.is_empty() {
                    *item.local_failures.entry(reviewer.clone()).or_default() += 1;
                    // The next round leaving any of these unreviewed again counts too.
                    item.local_unreviewed.clone_from(&unreviewed);
                    item.local_unreviewed_by = Some(reviewer.clone());
                    item.phase = self.after_round(review, now);
                    StepReport::LocalRoundFailed {
                        issue,
                        pull_request: number,
                        round,
                        reviewer,
                        unreviewed,
                    }
                } else {
                    // A reviewer read the pull request, so this pass has.
                    let review = Review {
                        unread: false,
                        ..review
                    };
                    item.unreviewed = None;
                    if let Some(head) = read {
                        item.reviewed(head);
                    }
                    if local_round {
                        item.note_local_round(&reviewer, &unreviewed);
                    }
                    // Nothing found: the pass goes on to the next reviewer at once.
                    if findings.is_empty() {
                        item.phase = self.after_round(review, now);
                        StepReport::ReviewFindingsSent {
                            issue,
                            pull_request: number,
                            round,
                            held: 0,
                        }
                    } else {
                        let count = findings.len();
                        item.phase = Phase::Review(Review {
                            stage: ReviewStage::Found {
                                findings,
                                threads: Vec::new(),
                            },
                            failures: 0,
                            ..review
                        });
                        StepReport::ReviewRound {
                            issue,
                            pull_request: number,
                            round,
                            reviewer,
                            findings: count,
                            unreviewed,
                        }
                    }
                }
            }
        };
        self.save(next)?;
        Ok(Some(report))
    }
}

// Why a fix turn that pushed nothing still lets the pass go on, with how
// many findings it was sent.
enum Unfixed {
    // The worker deferred every one as out of scope.
    Deferred(usize),
    // Every one was a nit, which the worker may decline.
    NitsDeclined(usize),
}

// What both looks found, each once, the worst first. A stable sort keeps
// each look's own order within a severity.
fn both_looks(first: Vec<Finding>, more: Vec<Finding>) -> Vec<Finding> {
    let mut all = first;
    for f in more {
        if !all.iter().any(|k| k.is_same_as(&f)) {
            all.push(f);
        }
    }
    all.sort_by_key(|f| std::cmp::Reverse(f.severity));
    all
}

/// Runs `action` to its end on its own thread: a local round, or a fresh
/// session of a reviewer, which `ending` can end
///
/// A call stopped with the runner comes back as [`ReviewResult::Stopped`]
/// rather than an error, so `end_review` can tell it from a failed gate.
pub(super) fn run_review_call(
    claude: &dyn Agents,
    reviewer: &dyn Reviewer,
    action: ReviewCall,
    ending: &Ending,
    watch: &(dyn Fn(RoundStage) + Sync),
) -> Reviewed {
    match action {
        ReviewCall::Local {
            local,
            worktree,
            base,
            out,
            round,
            criteria,
        } => {
            match reviewer.round_watched(&local, &worktree, &base, &out, round, &criteria, watch) {
                Err(ReviewerError::Stopped) => stopped(),
                Err(ReviewerError::Spilled(reason)) => Reviewed {
                    result: ReviewResult::Spilled(reason),
                    spent: None,
                },
                result => Reviewed {
                    result: ReviewResult::Findings(result.map_err(|e| e.to_string())),
                    spent: Some(Spent::Local),
                },
            }
        }
        ReviewCall::Session(call) => {
            let (reply, spent) = run_claude(claude, &call, ending);
            match reply {
                Err(AgentError::Stopped) => stopped(),
                reply => Reviewed {
                    result: ReviewResult::Findings(reply.map_err(|e| e.to_string()).and_then(
                        |reply| {
                            read_review(&reply.text).map_err(|text| {
                                format!(
                                    "the reviewer's reply is neither findings nor CLEAN: {text}"
                                )
                            })
                        },
                    )),
                    spent,
                },
            }
        }
    }
}

/// Adds what a review call spent to the work item's record, and marks no
/// call in flight
///
/// A local round's time runs from when the call was marked running.
pub(super) fn record_spent(item: &mut WorkItem, spent: Option<Spent>, now: Timestamp) {
    match spent {
        Some(Spent::Claude {
            role,
            session,
            usage,
            session_cost,
        }) => {
            item.record_call(role, now, session, usage, session_cost);
        }
        Some(Spent::Local) => {
            item.qwen.rounds += 1;
            if let ReviewCallState::Running { since } = item.review_call {
                item.qwen.seconds += now.0.saturating_sub(since.0);
            }
        }
        Some(Spent::Unanswered(_)) | None => {}
    }
    item.call_ended();
}

fn stopped() -> Reviewed {
    Reviewed {
        result: ReviewResult::Stopped,
        spent: None,
    }
}

// A reply that came back cost something even if what it said is unusable.
fn run_claude(
    claude: &dyn Agents,
    call: &AgentCall,
    ending: &Ending,
) -> (Result<AgentReply, AgentError>, Option<Spent>) {
    let reply = claude.run(call, ending);
    let spent = match &reply {
        Ok(reply) => Some(Spent::Claude {
            role: call.role,
            session: call.session.id().clone(),
            usage: reply.usage,
            session_cost: reply.session_cost,
        }),
        // Its settings, harness or session never got as far as a model.
        Err(_) => crate::usage::ended(&reply).map(Spent::Unanswered),
    };
    (reply, spent)
}

#[cfg(test)]
mod tests;
