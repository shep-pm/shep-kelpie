//! The deep review round: one pass instead of a loop of shallow ones
//!
//! A fresh session on the `deep_reviewer` role reads the whole pull request
//! for defects, and a second fresh one, shown what the first found, reads it
//! for what the first missed. Each HIGH they hold is confirmed by a session
//! that may run commands in the worktree, which writes a failing test for it;
//! one it cannot confirm goes on marked unconfirmed. One worker turn fixes
//! everything held. One re-check then reads only the fix commits against the
//! findings, and runs each failing test, since a fixer's word that it fixed
//! something is not evidence. A finding it finds unfixed goes back to the
//! worker once, and after that to a ruling. A fix that passes ends the loop.

mod pin;
mod prompts;
#[cfg(test)]
mod tests;

use super::calls::{build_call, diff_against, shots_prompt};
use super::findings;
use crate::pacer::Scope;
use crate::ports::{AgentCall, Finding, Role, Tools, read_review};
use crate::profile;
use crate::runner::Runner;
use crate::runner::report::{Begin, ReviewCall, ReviewResult, Spent, StepReport};
use crate::runner::ruling::park;
use crate::runner::shots::RoundShots;
use crate::runner::turn;
use crate::state::{Fix, RulingKind, StateError};
use crate::work_item::{Backing, CallKind, Deep, Held, Phase, Review, ReviewStage, Turn};
use crate::worktree::Start;

use super::lineup::Chosen;
use prompts::Confirmation;

/// Where a reader's throwaway settings go
const READ_SETTINGS_FILE: &str = "deep-read-settings.json";

/// Where the settings of a session that runs commands go
const WORK_SETTINGS_FILE: &str = "deep-work-settings.json";

/// Where the instructions of a session that runs commands go
const INSTRUCTIONS_FILE: &str = "deep-instructions.md";

impl Runner {
    // The deep round begins: who reviews it and that it has started are kept,
    // so a restart resumes at the step it was on.
    pub(super) fn deep_started(
        &mut self,
        chosen: &Chosen,
        review: Review,
    ) -> Result<Begin, StateError> {
        let (name, alone) = (chosen.reviewer.name.clone(), chosen.alone);
        let review = Review {
            reviewer: Some(name),
            alone,
            stage: ReviewStage::Deep(Deep::Read),
            ..review
        };
        let kept = review.clone();
        self.update(|item| {
            // It reads every file, so none is left for a local round.
            item.local_unreviewed.clear();
            item.phase = Phase::Review(kept);
        })?;
        self.deep_step(review, Deep::Read)
    }

    // The next thing the deep round does: a call outside the lock, or a
    // change of state.
    pub(super) fn deep_step(&mut self, review: Review, deep: Deep) -> Result<Begin, StateError> {
        match deep {
            Deep::Read => self.read_call(None),
            Deep::Missed { first } => self.read_call(Some(first)),
            Deep::Confirming { held, .. } => match held.iter().position(Held::to_confirm) {
                Some(at) => self.confirm_call(&held[at].finding),
                None => self.send_to_worker(review, held, false),
            },
            Deep::Sending { held, again } => self.send_to_worker(review, held, again),
            Deep::Fixing { held, head, again } => self.deep_fix_ended(review, held, head, again),
            Deep::Rechecking { held, head, .. } => self.recheck_call(&held, &head),
        }
    }

    // The pacing hold on the deep round's account, if there is one.
    fn deep_held(&mut self) -> Result<Option<Begin>, StateError> {
        let limit = self.agents.limits.deep_reviewer.clone();
        Ok(self.pace(Scope::Turn, &limit)?.holds())
    }

    // A reader's call: the defect prompt over the whole diff, with the issue,
    // and for the second reader what the first found.
    fn read_call(&mut self, first: Option<Vec<Finding>>) -> Result<Begin, StateError> {
        if let Some(held) = self.deep_held()? {
            return Ok(held);
        }
        let item = self.current().expect("the deep round is a work item's");
        let (issue, worktree, base) = (item.issue, item.worktree.clone(), item.review_base());
        let criteria = match self.criteria(issue) {
            Ok(criteria) => criteria,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let shots = match self.round_shots()? {
            RoundShots::Take(begin) => return Ok(*begin),
            RoundShots::Ready(shots) => shots,
        };
        let diff = match diff_against(&worktree, &base) {
            Ok(diff) => diff,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let mut prompt = prompts::reader_prompt(&base, &diff, &criteria, first.as_deref());
        if let Some(run) = &shots {
            prompt.push_str(&shots_prompt(run));
        }
        let (model, limit) = (
            &self.agents.deep_reviewer,
            &self.agents.limits.deep_reviewer,
        );
        let mut call =
            match build_call(Role::DeepReviewer, issue, &worktree, (model, limit), prompt) {
                Ok(call) => call,
                Err(reason) => return Ok(self.gate_failed(reason)),
            };
        call.settings = self.paths.worker.join(READ_SETTINGS_FILE);
        // It reads the worktree to check its work, and runs no command.
        call.tools = Tools::Review;
        if shots.is_some() {
            call.reach.read = vec![self.paths.shots(issue)];
        }
        self.start_call(Ok(call))
    }

    // The call that confirms one HIGH: a session that may run commands.
    fn confirm_call(&mut self, finding: &Finding) -> Result<Begin, StateError> {
        if let Some(held) = self.deep_held()? {
            return Ok(held);
        }
        // What the session adds is told against the worktree as it finds it.
        // A try stopped with the runner has left the snapshot of its first try
        // saved, which a retry keeps, so it restores to what the first found.
        let saved = matches!(
            self.current().map(|item| &item.phase),
            Some(Phase::Review(Review {
                stage: ReviewStage::Deep(Deep::Confirming {
                    before: Some(_),
                    ..
                }),
                ..
            }))
        );
        if !saved {
            let before = match self.found_before() {
                Ok(before) => before,
                Err(reason) => return Ok(self.gate_failed(reason)),
            };
            self.update(|item| {
                if let Phase::Review(Review {
                    stage: ReviewStage::Deep(Deep::Confirming { before: kept, .. }),
                    ..
                }) = &mut item.phase
                {
                    *kept = Some(before);
                }
            })?;
        }
        let base = self.current().expect("a work item's").review_base();
        let call = self.working_call(prompts::confirm_prompt(&base, finding), true);
        self.start_call(call)
    }

    // The re-check of the fix: a session that may run commands, shown only
    // what changed since the findings were sent.
    fn recheck_call(&mut self, held: &[Held], head: &str) -> Result<Begin, StateError> {
        if let Some(held) = self.deep_held()? {
            return Ok(held);
        }
        let item = self.current().expect("the deep round is a work item's");
        let (issue, number) = (item.issue, item.pull_request);
        let number = number.expect("review starts once a pull request is known");
        let (checked, deferred) = apart_deferred(&item.build, held);
        if checked.is_empty() {
            // Everything was deferred: there is nothing left to check.
            let round = match &item.phase {
                Phase::Review(review) => review.round,
                _ => 0,
            };
            let since = self.ports.clock.now();
            self.update(|item| item.phase = Phase::Ci { head: None, since })?;
            return Ok(Begin::Report(StepReport::DeepRechecked {
                issue,
                pull_request: number,
                round,
                fixed: 0,
                deferred,
                unfixed: 0,
            }));
        }
        let fix = match diff_against(&item.worktree, head) {
            Ok(fix) => fix,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let call = self.working_call(prompts::recheck_prompt(number, &checked, head, &fix), false);
        self.start_call(call)
    }

    // A session on the deep reviewer's agent with the worker's fence over this
    // work item's worktree, so it can run tests but commit and push nothing.
    // It writes its build folder, and the worktree only when it is to write
    // a test there: the re-check must not change what it verifies.
    fn working_call(&self, prompt: String, writes_tests: bool) -> Result<AgentCall, String> {
        let item = self.current().expect("the deep round is a work item's");
        let start = match item.rework || item.adopted {
            true => Start::Pushed,
            false => Start::Main,
        };
        let mut reach = self.worker_reach(item, start)?;
        if let Some(fence) = reach.fence.as_mut() {
            fence
                .write
                .retain(|p| *p == item.build || (writes_tests && *p == item.worktree));
        }
        let (model, limit) = (
            &self.agents.deep_reviewer,
            &self.agents.limits.deep_reviewer,
        );
        let mut call = build_call(
            Role::DeepReviewer,
            item.issue,
            &item.worktree,
            (model, limit),
            prompt,
        )?;
        let folder = &self.paths.worker;
        let instructions = folder.join(INSTRUCTIONS_FILE);
        turn::write(folder, &instructions, &profile::instructions(&self.kelpie))?;
        call.settings = folder.join(WORK_SETTINGS_FILE);
        call.instructions = Some(instructions);
        call.tools = Tools::Work;
        call.reach = reach;
        let minutes = u64::from(self.settings.worker.turn_timeout.get());
        call.timeout = Some(std::time::Duration::from_secs(minutes * 60));
        Ok(call)
    }

    fn start_call(&mut self, call: Result<AgentCall, String>) -> Result<Begin, StateError> {
        match call.and_then(|call| self.prepared(call)) {
            Ok(call) => {
                self.mark_review_call_running(CallKind::Deep)?;
                Ok(Begin::Review(ReviewCall::Deep(call)))
            }
            Err(reason) => Ok(self.gate_failed(reason)),
        }
    }

    // The worker's fix turn: everything held, in a file, and the head it
    // must move.
    fn send_to_worker(
        &mut self,
        review: Review,
        held: Vec<Held>,
        again: bool,
    ) -> Result<Begin, StateError> {
        let item = self.current().expect("the deep round is a work item's");
        let issue = item.issue;
        let number = item
            .pull_request
            .expect("review starts once a pull request is known");
        let build = item.build.clone();
        let head = match self.origin_head() {
            Ok(head) => head,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let path = findings::findings_path(&build);
        let text = prompts::fix_file(&held, &findings::deferred_path(&build), again);
        if let Err(reason) = turn::write(&build, &path, &text) {
            return Ok(self.gate_failed(reason));
        }
        let prompt = prompts::fix_prompt(number, held.len(), &path, again);
        let (round, count) = (review.round, held.len());
        let all: Vec<Finding> = held.iter().map(|h| h.finding.clone()).collect();
        let stage = ReviewStage::Deep(Deep::Fixing { held, head, again });
        self.update(|item| {
            item.record_held(&all);
            item.turn = Turn::Next { prompt };
            item.phase = Phase::Review(Review { stage, ..review });
        })?;
        Ok(Begin::Report(StepReport::ReviewFindingsSent {
            issue,
            pull_request: number,
            round,
            held: count,
            clean: false,
        }))
    }

    // The fix turn ended. One that pushed nothing fixed nothing, whatever
    // it says, so the findings still stand.
    fn deep_fix_ended(
        &mut self,
        review: Review,
        held: Vec<Held>,
        head: String,
        again: bool,
    ) -> Result<Begin, StateError> {
        let item = self.current().expect("the deep round is a work item's");
        let issue = item.issue;
        let number = item
            .pull_request
            .expect("review starts once a pull request is known");
        let round = review.round;
        let now = match self.origin_head() {
            Ok(now) => now,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        // What the re-check reads and runs is the worktree, so it must be what
        // was pushed: a fix left uncommitted, or committed and not pushed,
        // would be verified here and never reach the branch. The review's
        // own tests must be in it as they were written, and nothing else may
        // be left new.
        let path = findings::findings_path(&item.build);
        // A finding the worker deferred has its test taken out, so it is not pinned.
        let (pinned, _) = apart_deferred(&item.build, &held);
        let left = match self.left_behind(&pinned, &now) {
            Ok(left) => left,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        // When the worker deferred every finding there is nothing to push, so
        // an unmoved head is no failure: the worktree need only be clean.
        let nothing_to_push = pinned.is_empty();
        if (now == head && !nothing_to_push) || left.is_some() {
            let why = left.filter(|_| now != head || nothing_to_push);
            let prompt = match (&why, nothing_to_push && now == head) {
                (Some(why), true) => findings::deferred_prompt(number, round, &path, why),
                (Some(why), false) => findings::unpushed_prompt(number, round, &path, why),
                (None, _) => findings::again_prompt(number, round, &path),
            };
            let stage = ReviewStage::Deep(Deep::Fixing { held, head, again });
            let fix = Fix::Review(Review { stage, ..review });
            return self.raise(number, RulingKind::FixNotPushed { fix, prompt, why });
        }
        let stage = ReviewStage::Deep(Deep::Rechecking { held, head, again });
        self.update(|item| item.phase = Phase::Review(Review { stage, ..review }))?;
        Ok(Begin::Report(StepReport::FixPushed {
            issue,
            pull_request: number,
            round,
            head: Some(now),
        }))
    }

    /// Records what a session of the deep round said, and goes on to the next step
    pub(super) fn end_deep(
        &mut self,
        result: ReviewResult,
        spent: Option<Spent>,
    ) -> Result<Option<StepReport>, StateError> {
        let ReviewResult::Deep(reply) = result else {
            return Ok(None);
        };
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let Some(item) = self.current_in(&mut next) else {
            return Ok(None);
        };
        super::record_spent(item, spent, now);
        let Phase::Review(review) = item.phase.clone() else {
            return Ok(None);
        };
        let ReviewStage::Deep(deep) = review.stage.clone() else {
            return Ok(None);
        };
        let issue = item.issue;
        let number = item
            .pull_request
            .expect("review starts once a pull request is known");
        let round = review.round;
        let worktree = item.worktree.clone();
        let build = item.build.clone();
        let in_stage = |stage: Deep| {
            Phase::Review(Review {
                stage: ReviewStage::Deep(stage),
                ..review.clone()
            })
        };
        let failed = |what: &str, reason: String| StepReport::GateFailed {
            issue,
            reason: format!("the deep round's {what} on #{number} failed: {reason}"),
        };
        let ends = Phase::Ci {
            head: None,
            since: now,
        };
        let mut ruling = None;
        let report = match deep {
            Deep::Read => match reply.and_then(|text| read_review(&text)) {
                Ok(first) => {
                    let findings = first.len();
                    item.phase = in_stage(Deep::Missed { first });
                    StepReport::DeepRead {
                        issue,
                        pull_request: number,
                        round,
                        reader: 1,
                        findings,
                    }
                }
                Err(reason) => failed("first reader", reason),
            },
            Deep::Missed { first } => match reply.and_then(|text| read_review(&text)) {
                Ok(more) => {
                    let findings = more.len();
                    let mut all = first;
                    for f in more {
                        let same =
                            |k: &Finding| (&k.file, k.line, &k.what) == (&f.file, f.line, &f.what);
                        if !all.iter().any(same) {
                            all.push(f);
                        }
                    }
                    // The worst first, and a stable sort keeps each reader's order.
                    all.sort_by_key(|f| std::cmp::Reverse(f.severity));
                    if all.is_empty() {
                        item.phase = ends;
                        StepReport::ReviewFindingsSent {
                            issue,
                            pull_request: number,
                            round,
                            held: 0,
                            clean: true,
                        }
                    } else {
                        let held = all.into_iter().map(Held::new).collect();
                        item.phase = in_stage(Deep::Confirming { held, before: None });
                        StepReport::DeepRead {
                            issue,
                            pull_request: number,
                            round,
                            reader: 2,
                            findings,
                        }
                    }
                }
                Err(reason) => failed("second reader", reason),
            },
            Deep::Confirming { mut held, before } => {
                let at = held.iter().position(Held::to_confirm);
                let Some(at) = at else {
                    return Ok(None);
                };
                // A session that cannot say whether it confirmed leaves the
                // HIGH unconfirmed, which still reaches the worker. So does one
                // whose tests kelpie cannot pin, since a pin read badly would
                // fail the fix, or pass one that is not there.
                let confirmed = match reply {
                    Ok(text) => prompts::read_confirmation(&text, &worktree),
                    Err(reason) => {
                        Confirmation::Unconfirmed(format!("the session failed: {reason}"))
                    }
                };
                held[at].backing = match confirmed {
                    Confirmation::Test { file, command } => {
                        match self.added_by_session(
                            &file,
                            before.as_deref().unwrap_or_default(),
                            &mut held,
                        ) {
                            Ok(written) => Backing::Test {
                                file,
                                command,
                                written,
                            },
                            Err(reason) => Backing::Unconfirmed {
                                why: format!(
                                    "kelpie could not read what the session wrote: {reason}"
                                ),
                            },
                        }
                    }
                    Confirmation::Unconfirmed(why) => Backing::Unconfirmed { why },
                };
                // A session that did not confirm leaves nothing behind: what it
                // wrote is no test for the worker to commit.
                // With no snapshot saved there is nothing to put it back to.
                if let (Backing::Unconfirmed { why }, Some(saved)) =
                    (&mut held[at].backing, &before)
                    && let Err(e) = self.restore_before(saved)
                {
                    why.push_str(&format!(
                        ", and kelpie could not clean up what it left in the worktree: {e}"
                    ));
                }
                let backed = matches!(held[at].backing, Backing::Test { .. });
                let finding = format!("{}:{}", held[at].finding.file, held[at].finding.line);
                item.phase = in_stage(Deep::Confirming { held, before: None });
                StepReport::DeepConfirmed {
                    issue,
                    pull_request: number,
                    round,
                    finding,
                    backed,
                }
            }
            Deep::Rechecking { held, again, .. } => {
                // The call was about what the worker did not defer, in this order.
                let (checked, deferred) = apart_deferred(&build, &held);
                match reply.and_then(|text| prompts::read_unfixed(&text, &checked)) {
                    Err(reason) => failed("re-check", reason),
                    Ok(unfixed) => {
                        let (fixed, left) = (checked.len() - unfixed.len(), unfixed.len());
                        let report = StepReport::DeepRechecked {
                            issue,
                            pull_request: number,
                            round,
                            fixed,
                            deferred,
                            unfixed: left,
                        };
                        match (unfixed.is_empty(), again) {
                            (true, _) => item.phase = ends,
                            (false, false) => {
                                item.phase = in_stage(Deep::Sending {
                                    held: unfixed,
                                    again: true,
                                });
                            }
                            (false, true) => ruling = Some(unfixed),
                        }
                        report
                    }
                }
            }
            Deep::Sending { .. } | Deep::Fixing { .. } => return Ok(None),
        };
        let Some(unfixed) = ruling else {
            self.save(next)?;
            return Ok(Some(report));
        };
        // The worker has been sent back once already: the maintainer decides.
        // A failure to write what the ruling needs still keeps what the call
        // cost and that it ended, and the stage stays at the re-check.
        let head = self.origin_head();
        let path = findings::findings_path(&build);
        let text = prompts::fix_file(&unfixed, &findings::deferred_path(&build), true);
        let written = head.and_then(|head| {
            turn::write(&build, &path, &text)?;
            Ok(head)
        });
        let head = match written {
            Ok(head) => head,
            Err(reason) => {
                self.save(next)?;
                return Ok(Some(failed("re-check", reason)));
            }
        };
        let prompt = prompts::fix_prompt(number, unfixed.len(), &path, true);
        let said = unfixed.iter().map(unfixed_line).collect();
        let stage = ReviewStage::Deep(Deep::Fixing {
            held: unfixed,
            head,
            again: true,
        });
        let kind = RulingKind::DeepReview {
            review: Review { stage, ..review },
            unfixed: said,
            prompt,
        };
        let (id, question) = park(self.names(), &mut next, issue, Some(number), kind);
        self.save(next)?;
        let comment_failed = self.post_ruling(Some(number), id);
        Ok(Some(StepReport::Ruling {
            issue,
            pull_request: number,
            id,
            question,
            comment_failed,
        }))
    }
}

// A finding the re-check found unfixed, as a ruling names it.
fn unfixed_line(held: &Held) -> String {
    let Finding {
        file, line, what, ..
    } = &held.finding;
    match &held.still {
        Some(still) => format!("{file}:{line} {what} (still: {still})"),
        None => format!("{file}:{line} {what}"),
    }
}

// What the worker was sent less what it deferred, and how many it deferred: a
// finding it copied to the deferred-findings file, which follow-up filing
// matches by file, line and what, is filed as an issue once the pull request
// merges, so neither its fix nor its test is checked.
fn apart_deferred(build: &std::path::Path, held: &[Held]) -> (Vec<Held>, usize) {
    let deferred = pin::deferred_findings(build);
    let (deferred_held, checked): (Vec<&Held>, Vec<&Held>) = held.iter().partition(|h| {
        let f = &h.finding;
        deferred
            .iter()
            .any(|d| (&d.file, d.line, &d.what) == (&f.file, f.line, &f.what))
    });
    (checked.into_iter().cloned().collect(), deferred_held.len())
}
