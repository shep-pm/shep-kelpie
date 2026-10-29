//! The review: one pass down the project's reviewers, each run once, in order
//!
//! A round with no findings goes straight to the next reviewer. One with any,
//! nits (LOW) included, sends the worker all of them, at the reviewer's own
//! severity, for one fix turn (a size-skipped file's notice is only reported),
//! and the next reviewer reads the fix. A reviewer with a second look reads
//! twice, the second time shown what it found, and both lists go to one fix turn.
//! A review bot's round is [`super::review_bot`]'s, and its open threads are its
//! findings. After the last reviewer the work item goes on to [`super::gate`].

mod again;
mod attached;
mod awaited;
#[cfg(test)]
mod axes;
#[cfg(test)]
mod bots_left_out;
pub(super) mod calls;
mod checks;
mod criteria;
mod keep;
mod ran;
pub(super) mod reads;
pub(in crate::runner) use keep::prune_at_start;
pub(in crate::runner) use ran::{record_spent, run_review_call};
pub(super) mod disputes;
pub(super) mod findings;
#[cfg(test)]
mod foreign;
#[cfg(test)]
mod green;
mod lineup;
use lineup::Chosen;
pub(super) use lineup::{changed_files, touches};
#[cfg(test)]
mod local;
#[cfg(test)]
mod need;
#[cfg(test)]
mod nits;
mod outcomes;
pub(super) mod prompts;
pub(super) mod pushed;
#[cfg(test)]
mod second_look;
#[cfg(test)]
mod several;
#[cfg(test)]
mod unread;

use std::path::Path;

use super::Runner;
use super::report::{Begin, ReviewCall, ReviewResult, Reviewed, StepReport};
use super::retro::journal::Entry;
use super::review_bot::Answered;
use super::talk::At;
use super::voice::Round;
use crate::agents::{DEFECT_HUNTER, QWEN};
use crate::pacer::Scope;
use crate::ports::{Finding, RoundEnd};
use crate::settings::{AgentName, ListedReviewer};
use crate::state::{Fix, StateError, Stuck};
use crate::work_item::{CallKind, Phase, Review, ReviewCallState, ReviewStage, Turn, WorkItem};
use crate::worktree;

// A reviewer whose call fails this many times in a row is passed over for
// the rest of its pass: a reply it cannot give would otherwise stall the
// review for good.
pub(super) const ROUND_FAILURES: u32 = 3;

impl Runner {
    pub(super) fn review_step(&mut self) -> Result<Begin, StateError> {
        // A step that just moved the work item into its review, as an owed
        // bots' pass after green CI does, leaves it waiting when no slot is free.
        if self.unseated() {
            return Ok(Begin::Idle);
        }
        // A step made again after a read finds the work item as the read left it
        let Some(item) = self.current() else {
            return Ok(Begin::Idle);
        };
        let Phase::Review(review) = item.phase.clone() else {
            unreachable!("review_step only runs in the review phase")
        };
        let Some(number) = item.pull_request else {
            return Ok(Begin::Idle);
        };
        let (ticket, worktree) = (item.issue, item.worktree.clone());
        let base = item.review_base(self.item_base(item));
        let build = item.build.clone();
        // A new pass reviews new code, so no fix of the last one will
        // resolve the bot threads it sent, its bots are asked afresh, and a
        // late round's fix is read like any other head.
        let fresh = review
            == Review {
                bots_only: review.bots_only,
                ..Review::first()
            };
        let left = !item.threads_sent.is_empty() || !item.bots_skipped.is_empty();
        if fresh && (left || item.late_from.is_some()) {
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
                if let Some(begin) = self.talk_turn(&At::Round(review.clone()))? {
                    return Ok(begin);
                }
                let chosen = match self.choose_reviewer(&review, &worktree, &base)? {
                    Chosen::Reviewer(chosen) => chosen,
                    Chosen::Ended => return self.pass_ended(&review, &[]),
                    Chosen::Unranked(unranked) => return self.pass_ended(&review, &unranked),
                    Chosen::Instead(begin) => return Ok(*begin),
                    Chosen::Failed(reason) => return Ok(self.gate_failed(reason)),
                };
                // Before any reader or bot is started on the head.
                if let Some(begin) = self.before_read(&mut review)? {
                    return Ok(begin);
                }
                if let Some(bot) = chosen.bot() {
                    return self.bot_round(&chosen, bot, review);
                }
                let Some(local) = chosen.runs.local() else {
                    return self.session_call(&chosen, None);
                };
                if self.draining() {
                    return Ok(Begin::Idle);
                }
                if let Some(parked) = self.park_over_budget()? {
                    return Ok(parked);
                }
                let criteria = match self.criteria(ticket) {
                    Ok(criteria) => criteria,
                    Err(reason) => return Ok(self.gate_failed(reason)),
                };
                // `criteria` reads the issue from the forge with the runner's
                // lock held, and a `drain` answered from the posted copy in that
                // time sets its flag there. Checked again before the round is
                // booked, so no reviewer call starts past the drain.
                if self.draining() {
                    return Ok(Begin::Idle);
                }
                self.round_started(&chosen, CallKind::Local)?;
                Ok(Begin::Review(ReviewCall::Local {
                    local,
                    repo: self.settings.git.checkout.clone(),
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
        if self.draining() {
            return Ok(Begin::Idle);
        }
        if let Some(parked) = self.park_over_budget()? {
            return Ok(parked);
        }
        if let Some(held) = self.pace(Scope::Turn, limit)?.holds() {
            return Ok(held);
        }
        let item = self.current().expect("a review is a work item's");
        let (ticket, worktree, base) = (
            item.issue,
            item.worktree.clone(),
            item.review_base(self.item_base(item)),
        );
        let build = item.build.clone();
        let criteria = match self.criteria(ticket) {
            Ok(criteria) => criteria,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        // `criteria` reads the issue from the forge with the runner's lock held,
        // and a `drain` answered from the posted copy in that time sets its flag
        // there. Checked again before the round is booked, so no reviewer call
        // starts past the drain.
        if self.draining() {
            return Ok(Begin::Idle);
        }
        let diff = match calls::diff_against(&self.settings.git.checkout, &worktree, &base) {
            Ok(diff) => diff,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let body = chosen.prompt.as_deref().unwrap_or_default();
        let tests = match self.begin_tests(model, (&worktree, &build), first.is_none()) {
            Ok(tests) => tests,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let tool = attached::writing_tool(model.harness.harness());
        let ask = match (&tests, self.test_command_for(model), tool) {
            (Some(_), Some(command), Some(tool)) => {
                Some((command.as_str(), attached::tests_folder(&build), tool))
            }
            _ => None,
        };
        let mut prompt = prompts::reviewer_prompt(
            body,
            &base,
            &diff,
            &criteria,
            first.as_deref(),
            ask.as_ref()
                .map(|(command, folder, tool)| (*command, folder.as_path(), *tool)),
        );
        prompt.push_str(&prompts::disputes_note(&item.disputes));
        let call =
            calls::reviewer_call(ticket, &worktree, &self.paths.worker, model, prompt, tests);
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
    fn pass_ended(&mut self, review: &Review, unranked: &[AgentName]) -> Result<Begin, StateError> {
        let since = self.ports.clock.now();
        let item = self.current().expect("a pass is a work item's");
        let (issue, number) = (item.issue, item.pull_request);
        let unread = review.unread.then(|| self.unread(item, review));
        // Candidates that could run, none ranked, are not a choice of the project's.
        let unread = match (unread, unranked.is_empty()) {
            (unread, true) => unread,
            (None, false) => None,
            (unread, false) => {
                let names: Vec<_> = unranked.iter().map(ToString::to_string).collect();
                let board = format!("the agent board ranked none of {}", names.join(", "));
                Some(Some(match unread.flatten() {
                    Some(missed) => format!("{missed}, and {board}"),
                    None => board,
                }))
            }
        };
        // Nobody to read it, by the project's own list, sends it unread on purpose.
        let unread_by_choice = match &unread {
            Some(None) => match self.head_on_origin() {
                Ok(head) => head.map_err(|e| eprintln!("{e}")).ok(),
                Err(begin) => return Ok(*begin),
            },
            _ => None,
        };
        if let Some(None) = &unread {
            let why = match self.lineup.is_empty() {
                true => "the project lists no reviewer",
                false => "each listed reviewer's `paths`, or a bot's `rounds`, passed it over",
            };
            self.notes.push(format!(
                "{}'s pull request goes to CI with no reviewer's read: {why}",
                issue.shown()
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

    // A fix turn that pushed nothing fixed nothing, so the findings sent still
    // stand and the next reviewer waits. Unless the worker deferred every one as
    // out of scope, or they were all nits, which it may decline: a ruling over
    // nits would ask about what never holds a merge.
    fn fix_ended(
        &mut self,
        number: u64,
        build: &Path,
        review: Review,
        head: Option<String>,
        sent: findings::Sent<'_>,
    ) -> Result<Begin, StateError> {
        // A fork worker only commits, so a fix is pushed by kelpie. The fork's
        // head unchanged would otherwise read as a fix not pushed. The push is
        // made before the read, which is fetched after it, and a step made
        // again finds the fork holding the head and pushes nothing.
        if let Some(held) = self.fork_commits_held()? {
            return Ok(held);
        }
        let read = match &head {
            Some(_) => match self.head_on_origin() {
                Ok(read) => Some(read),
                Err(begin) => return Ok(*begin),
            },
            None => None,
        };

        let issue = self.current().expect("a fix is a work item's").issue;
        let round = review.round;
        let mut left_open = None;
        let (mut checks, mut unchecked) = (Vec::new(), 0);
        let mut failing = Vec::new();
        let pushed = match head.zip(read) {
            Some((before, read)) => match read {
                // A note's turn after a test ended sent no findings: pushing nothing is no fault.
                Ok(now) if now == before && sent.findings.is_empty() => None,
                Ok(now) if now == before => {
                    // The worker sent back to a failing test is not asked about
                    // nits or deferrals: it was to change code.
                    let failing = self.still_failing(round, sent.findings);
                    let path = findings::findings_path(build);
                    let threads =
                        (self.current()).is_some_and(|item| !item.threads_sent.is_empty());
                    if failing.iter().any(|f| f.sent_back) {
                        let tests: Vec<_> = failing.into_iter().map(|f| f.test).collect();
                        let prompt = findings::still_failing_prompt(number, round, build, &tests);
                        let fix = Fix::Review(review);
                        return self.raise(
                            number,
                            Stuck::FixNotPushed {
                                fix,
                                prompt,
                                said: None,
                            }
                            .into(),
                        );
                    }
                    // A size-skipped file's notice an older kelpie sent is no finding.
                    let count = (sent.findings.iter())
                        .filter(|f| !f.is_skipped_for_size())
                        .count();
                    if sent.findings.iter().all(Finding::is_nit) {
                        return self.went_unfixed(
                            number,
                            review,
                            sent,
                            Unfixed::NitsDeclined(count),
                        );
                    }
                    let already = (self.current()).map_or_else(Vec::new, |i| i.disputes.clone());
                    let answers = match disputes::answers(build, sent, round, &already) {
                        Ok(answers) => answers,
                        Err(reason) => return Ok(self.gate_failed(reason)),
                    };
                    let said = answers.said(sent.findings);
                    let repeated = answers.repeated();
                    if let Some(settled) = answers.settled() {
                        let unfixed = self.disputed_by(&review, sent, settled);
                        return self.went_unfixed(number, review, sent, unfixed);
                    }
                    // A finding disputed again is never a deferral that goes on.
                    match findings::all_deferred(build, sent).map(|all| all && !repeated) {
                        Ok(true) => {
                            return self.went_unfixed(
                                number,
                                review,
                                sent,
                                Unfixed::Deferred(count),
                            );
                        }
                        Ok(false) => {}
                        Err(reason) => return Ok(self.gate_failed(reason)),
                    }
                    let prompt = findings::again_prompt(number, round, &path, threads);
                    let fix = Fix::Review(review);
                    return self.raise(number, Stuck::FixNotPushed { fix, prompt, said }.into());
                }
                // A fix that moved the head answers the bot threads it was sent, by what the worker said.
                Ok(now) => {
                    if let Some(reason) = self.record_pushed_disputes(&review, sent)? {
                        return Ok(self.gate_failed(reason));
                    }
                    self.journal_outcomes(&review, sent, true)?;
                    match self.answer_sent(number, true)? {
                        Answered::Done => {}
                        Answered::Retry(reason) => return Ok(self.gate_failed(reason)),
                        Answered::LeftOpen { threads, reason } => {
                            left_open = Some((threads, reason));
                        }
                    }
                    // Only the worker's own commit is its fix: a push from
                    // anyone else stays unvouched for.
                    // So is a nit fix's head, which a bot's nits are not sent on.
                    if self.worktree_head().as_ref() == Some(&now) {
                        let head = now.clone();
                        let nits =
                            !sent.findings.is_empty() && sent.findings.iter().all(Finding::is_nit);
                        self.update(|item| {
                            item.send_unread(head.clone());
                            if nits {
                                item.nit_fixed(head);
                            }
                        })?;
                        // The tests ran only at a head the worker holds: a
                        // test is not run against code that is not its own.
                        checks = self.tests_after(round, sent.findings)?;
                        failing = self.still_failing(round, sent.findings);
                    } else {
                        checks = self.round_checks(round, sent.findings);
                        unchecked = checks.len();
                    }
                    Some(now)
                }
                Err(reason) => return Ok(self.gate_failed(reason)),
            },
            None => None,
        };
        // A test that fails after the fix keeps the round open for the worker, or
        // raises its ruling.
        if let (Some(head), false) = (&pushed, failing.is_empty()) {
            if let Some(raised) = self.tests_failed(number, review.clone(), head, sent, failing)? {
                return Ok(raised);
            }
        } else {
            let next = self.after_round(review, self.ports.clock.now());
            self.update(|item| item.phase = next)?;
        }
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
                checks,
                unchecked,
            },
        }))
    }

    // Every finding sent was left for a follow-up issue, or every one was a
    // nit the worker declined, so there was nothing to push, and the next
    // reviewer reads the pull request as it stands. Bot threads sent are
    // replied to with the worker's lines and stay open, since nothing fixed them.
    fn went_unfixed(
        &mut self,
        number: u64,
        review: Review,
        sent: findings::Sent<'_>,
        unfixed: Unfixed,
    ) -> Result<Begin, StateError> {
        let issue = self.current().expect("a fix is a work item's").issue;
        let round = review.round;
        let mut left_open = None;
        if let Unfixed::Disputed { disputes, .. } = &unfixed {
            self.update(|item| disputes::record(item, disputes))?;
        }
        self.journal_outcomes(&review, sent, false)?;
        match self.answer_sent(number, false)? {
            Answered::Done => {}
            Answered::Retry(reason) => return Ok(self.gate_failed(reason)),
            Answered::LeftOpen { threads, reason } => left_open = Some((threads, reason)),
        }
        let next = self.after_round(review, self.ports.clock.now());
        self.update(|item| item.phase = next)?;
        if let Some((threads, reason)) = left_open {
            return Ok(Begin::Report(StepReport::ThreadsLeftOpen {
                issue,
                pull_request: number,
                threads,
                reason,
            }));
        }
        Ok(Begin::Report(match unfixed {
            Unfixed::Deferred(deferred) => StepReport::FindingsDeferred {
                issue,
                pull_request: number,
                round,
                deferred,
            },
            Unfixed::Disputed { disputes, deferred } => StepReport::FindingsDisputed {
                issue,
                pull_request: number,
                round,
                disputed: disputes.len(),
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
        match worktree::head(&self.settings.git.checkout, &item.worktree) {
            Ok(head) => Some(head),
            Err(e) => {
                eprintln!("cannot read {}'s worktree head: {e}", item.issue.shown());
                None
            }
        }
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

    // Every finding goes, nits included, at the reviewer's own severity; a bot's
    // threads sent with them are kept to reply on and resolve once the fix turn
    // ends. A file skipped for its size is reported but never sent, since no fix
    // reviews it. A round with nothing to send goes on to the next reviewer.
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
        // A bot's finding is its thread's, in the same order, so a finding
        // dropped takes its thread with it.
        let by_thread = threads.len() == findings.len();
        let (findings, threads): (Vec<Finding>, Vec<String>) = match by_thread {
            true => (findings.into_iter().zip(threads))
                .filter(|(f, _)| !f.is_skipped_for_size())
                .unzip(),
            false => (
                findings
                    .into_iter()
                    .filter(|f| !f.is_skipped_for_size())
                    .collect(),
                threads,
            ),
        };
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
        let head = match self.pushed_head() {
            Ok(head) => head,
            Err(begin) => return Ok(*begin),
        };
        if let Err(reason) = self.bot_label_off(&review) {
            return Ok(self.gate_failed(reason));
        }
        let item = self.current().expect("findings are a work item's");
        let build = &item.build;
        let path = findings::findings_path(build);
        let report_threads = by_thread && !threads.is_empty();
        let written = findings::write_findings_file(build, &path, round, &findings, report_threads);
        if let Err(reason) = written {
            return Ok(self.gate_failed(reason));
        }
        let held = findings.len();
        let kept = again::kept(item, &findings);
        let prompt = findings::fix_prompt(number, round, held, &path, report_threads);
        let deferred_before = match findings::deferred(build) {
            Ok(deferred) => deferred,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        // Run at the head the reviewer read, before the worker's fix turn begins.
        let checks = self.tests_before(round, &head, &findings);
        self.update(|item| {
            item.test_checks.extend(checks);
            item.record_held(&findings);
            // In the findings' order, which the worker's lines number.
            item.threads_sent = threads;
            item.thread_answers.clear();
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
        self.journal(Entry::Findings {
            issue,
            round,
            findings: kept,
        });
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
        let Reviewed {
            result,
            spent,
            ended: how,
        } = reviewed;
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
        let started_at = reading.clone();
        let read = reading
            .filter(|_| matches!(&result, ReviewResult::Findings(Ok(_))))
            .filter(|head| self.still_at(head));
        let mut next = self.state.clone();
        // Tolerated as `end_turn` tolerates a result with nothing to apply it to: the
        // guard on `drop` prevents it today, but a result for a work item no longer
        // this one is discarded rather than panicking the runner.
        let Some(item) = self.current_in(&mut next) else {
            return Ok(None);
        };
        let started = match item.review_call {
            ReviewCallState::Running { since } => since,
            ReviewCallState::Idle => now,
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
        let ticket = item.issue;
        let (worktree, build) = (item.worktree.clone(), item.build.clone());
        let number = item
            .pull_request
            .expect("review runs once a pull request is known");
        let round = review.round;

        let ReviewResult::Findings(result) = result else {
            unreachable!("a stopped call returns above")
        };
        if result.is_ok() && matches!(review.stage, ReviewStage::Round) {
            item.counts.review_rounds = item.counts.review_rounds.saturating_add(1);
        }
        // A reader the disputes were in the prompt of has read them once its round ends.
        if result.is_ok() && !local_round {
            disputes::mark_shown(item);
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
        let reviewer_name = reviewer.as_str().to_owned();
        let kept_reviewer = reviewer.clone();
        let finished = local_round.then(|| {
            let sent = result.as_ref().map_or(0, Vec::len);
            let (ended, reason) = match (&result, how) {
                (Ok(found), _) if found.is_empty() && !unreviewed.is_empty() => (
                    RoundEnd::Failed,
                    Some(format!(
                        "every file went unreviewed: {}",
                        unreviewed.join(", ")
                    )),
                ),
                (Ok(_), _) => (RoundEnd::Answered, None),
                (Err(reason), how) => (how.unwrap_or(RoundEnd::Failed), Some(reason.clone())),
            };
            (ended, reason, sent)
        });
        // A test a reviewer attached is kept only once its file has been checked.
        let asked = self.round_asked_tests(&reviewer);
        let result =
            result.map(|found| self.checked_tests(asked, (ticket, &worktree, &build), found));
        // What a round that read the pull request found, to post once it is saved.
        let mut ended: Option<Vec<Finding>> = None;

        let report = match (review.stage.clone(), result) {
            // The call stays due; one that keeps failing goes no further.
            (_, Err(reason)) if review.failures + 1 < ROUND_FAILURES => {
                let failures = review.failures + 1;
                item.phase = Phase::Review(Review { failures, ..review });
                StepReport::GateFailed { issue, reason }
            }
            // A second look that keeps failing leaves the first's findings to go alone.
            (ReviewStage::SecondLook { first }, Err(reason)) => {
                // The first look ended with these findings, which now go on alone.
                ended = Some(first.clone());
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
                    ended = Some(findings.clone());
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
        // A round that read the pull request and found nothing reports no
        // `ReviewRound`, so the journal keeps it here, with 0 findings.
        if matches!(report, StepReport::ReviewFindingsSent { held: 0, .. }) {
            self.journal(Entry::Round {
                issue,
                round,
                reviewer: reviewer_name.clone(),
                findings: 0,
            });
        }
        if let Some((how, reason, findings)) = finished {
            self.keep_round(&keep::Finished {
                reviewer: &kept_reviewer,
                round,
                ended: how,
                reason,
                findings,
                started,
                finished: now,
                head: started_at.as_deref(),
            });
        }
        if let Some(findings) = ended {
            self.post_round(&Round {
                number,
                reviewer: &reviewer_name,
                round,
                head: started_at.as_deref(),
                findings: &findings,
            });
        }
        Ok(Some(report))
    }
}

// Why a fix turn that pushed nothing still lets the pass go on, with how
// many findings it was sent.
enum Unfixed {
    // The worker deferred every one as out of scope.
    Deferred(usize),
    // The worker showed some wrong, each with a reason, and deferred the rest.
    Disputed {
        disputes: Vec<disputes::Raised>,
        deferred: usize,
    },
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

#[cfg(test)]
mod draining_tests;
#[cfg(test)]
mod tests;
