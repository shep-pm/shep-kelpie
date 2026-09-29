//! The worker's turns
//!
//! A turn runs in three steps, so the runner still answers triggers while
//! Claude works: begin (prepare the worktree and profile, mark the turn
//! running, save), the call, and end (record the call, save). A turn still
//! marked running when the runner starts was cut short, and its session is
//! resumed. A session cut short before it wrote anything starts over. A turn
//! that ends on a question block parks the worker on a ruling.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::Runner;
use super::adopt;
use super::instructions;
use super::question::asked;
use super::report::{Begin, StepReport};
use super::review::run_review_call;
use super::rework;
use super::ruling::park;
use super::trigger::lock;
use crate::pacer::Scope;
use crate::ports::{ClaudeCall, ClaudeError, ClaudeReply, Issue, Role, Session, Timestamp};
use crate::profile::WorkerProfile;
use crate::state::{ProjectState, Resume, RulingKind, RunState, StateError};
use crate::work_item::{CodeRabbitStage, Phase, Review, ReviewStage, Turn, WorkItem};
use crate::worktree::{self, Start};

/// The prompt for a turn resumed after the runner restarted
const CONTINUE: &str = "Kelpie restarted while your last turn was running. \
                        Carry on with the work item from where you left off.";

/// Posts a ruling to the webhook, or runs the worker's next turn if one is
/// due and the project is running
///
/// Returns what happened, or `None` when there was nothing to do. A ruling
/// is posted whether the project runs or not.
///
/// # Errors
///
/// [`StateError`] when the turn's start or end, or a post, cannot be saved.
pub fn step(runner: &Mutex<Runner>) -> Result<Option<StepReport>, StateError> {
    let (claude, reviewer, relay, alerts) = {
        let runner = lock(runner);
        (
            Arc::clone(&runner.ports.claude),
            Arc::clone(&runner.ports.reviewer),
            Arc::clone(&runner.ports.relay),
            Arc::clone(&runner.ports.alerts),
        )
    };
    let due = lock(runner).alert_due();
    if let Some(due) = due {
        // The relay is a faster, nicer path when it is reachable, but the
        // webhook is what actually keeps a ruling from being lost, so it
        // posts every ruling regardless of how the relay's send went.
        if lock(runner).relay_clear_due() {
            let _ = relay.clear();
        }
        let _ = relay.send(&due.relay_message, &due.relay_model, due.relay_effort);
        let sent = alerts.post(&due.webhook, &due.alert);
        return lock(runner).alert_sent(due.id, sent).map(Some);
    }
    let mut start_over = false;
    loop {
        let begin = lock(runner).begin_turn(start_over)?;
        match begin {
            Begin::Idle => return Ok(None),
            Begin::Report(report) => return Ok(Some(report)),
            Begin::Call(call) => {
                let result = claude.run(&call);
                if !start_over && matches!(result, Err(ClaudeError::NoSession(_))) {
                    start_over = true;
                    continue;
                }
                return lock(runner).end_turn(result);
            }
            Begin::Review(action) => {
                let reviewed = run_review_call(claude.as_ref(), reviewer.as_ref(), action);
                return lock(runner).end_review(reviewed);
            }
        }
    }
}

fn first_prompt(number: u64, issue: &Issue) -> String {
    format!(
        "Your work item is issue #{number}: {}\n\n{}\n",
        issue.title,
        issue.body.trim_end()
    )
}

impl Runner {
    fn begin_turn(&mut self, start_over: bool) -> Result<Begin, StateError> {
        if self.state.run != RunState::Running {
            return Ok(Begin::Idle);
        }
        let Some(item) = &self.state.work_item else {
            return self.dispatch();
        };
        match &item.phase {
            Phase::Implement => {}
            // A fix turn that ended goes back to its round, to check it pushed.
            Phase::Review(review)
                if matches!(review.stage, ReviewStage::Fixing { .. })
                    && !matches!(item.turn, Turn::Ended { .. }) => {}
            Phase::Review(_) => {
                if let Some(parked) = self.fence_gate()? {
                    return Ok(parked);
                }
                return self.review_step();
            }
            Phase::CodeRabbit(CodeRabbitStage::Fixing { .. })
                if !matches!(item.turn, Turn::Ended { .. }) => {}
            Phase::Ci { .. } => return self.check_ci(),
            Phase::CodeRabbit(_) => {
                if let Some(parked) = self.fence_gate()? {
                    return Ok(parked);
                }
                return self.coderabbit_step();
            }
            Phase::Ruling { .. } => return Ok(Begin::Idle),
            Phase::Merge { .. } => return self.merge(),
            Phase::Done { merged } => return self.finish(*merged),
        }
        // Only a turn that has not begun waits on the pacer: one already
        // running carries on, since a turn is never interrupted, and start_over
        // resumes the same not-yet-begun turn after a session died unborn.
        let due = matches!(item.turn, Turn::Due | Turn::Next { .. });
        if let Some(begin) = self.pushed_by_someone_else()? {
            return Ok(begin);
        }
        if due && let Some(held) = self.pace(Scope::Turn)?.holds() {
            return Ok(held);
        }
        let item = self.state.work_item.as_ref().expect("checked above");
        let id = item.session.clone();
        let now = self.ports.clock.now();
        // A turn already running when the runner starts keeps the start it
        // was saved with, so a restart can never buy it a fresh ceiling: the
        // call it resumes gets only however much of the ceiling is left.
        let (session, prompt, since) = match &item.turn {
            Turn::Due => (Session::New(id), None, now),
            Turn::Running { since } if start_over => (Session::New(id), None, *since),
            Turn::Running { since } => (Session::Resume(id), Some(CONTINUE.to_owned()), *since),
            // An adopted work item's session begins with whatever the gate sends it.
            Turn::Next { prompt } if adopt::unborn(item) => {
                let first = adopt::first_prompt(item, Some(prompt));
                (Session::New(id), Some(first), now)
            }
            // No session to resume: the retry of a turn whose call failed
            // before its session existed starts it over, as a killed one does.
            Turn::Next { .. } if start_over => (Session::New(id), None, now),
            Turn::Next { prompt } => (Session::Resume(id), Some(prompt.clone()), now),
            Turn::Ended { .. } | Turn::Failed { .. } => return Ok(Begin::Idle),
        };
        let ceiling = self.turn_ceiling();
        let elapsed = Duration::from_secs(now.0.saturating_sub(since.0));
        let remaining = ceiling.saturating_sub(elapsed);
        if remaining.is_zero() {
            // The ceiling passed while kelpie was down, before a new call
            // could even be tried: park it as a call that hit
            // `ClaudeError::TimedOut` would, with no call spent.
            return self.park_ceiling_passed(now);
        }
        let prepared = self.prepare(item, session, prompt, remaining);
        let mut next = self.state.clone();
        let mut begin = match prepared {
            Ok(call) => {
                let item = next
                    .work_item
                    .as_mut()
                    .expect("the work item checked above");
                item.turn = Turn::Running { since };
                Begin::Call(call)
            }
            Err(reason) => Begin::Report(failed(self.project.as_str(), &mut next, now, reason)),
        };
        self.save(next)?;
        if let Begin::Report(report) = &mut begin {
            self.fill_comment_failed(report);
        }
        Ok(begin)
    }

    fn turn_ceiling(&self) -> Duration {
        Duration::from_secs(u64::from(self.settings.worker.turn_timeout.get()) * 60)
    }

    fn park_ceiling_passed(&mut self, now: Timestamp) -> Result<Begin, StateError> {
        let mut next = self.state.clone();
        if let Some(item) = next.work_item.as_mut() {
            item.turn = Turn::Ended { at: now };
        }
        let mut report = timed_out(self.project.as_str(), &mut next);
        self.save(next)?;
        self.fill_comment_failed(&mut report);
        Ok(Begin::Report(report))
    }

    // Everything the worker needs on disk before it starts: its worktree, its
    // build folder, its settings file and kelpie's instructions. A turn with
    // no prompt of its own is the first, and takes the issue, the review or
    // what it adopted.
    fn prepare(
        &self,
        item: &WorkItem,
        session: Session,
        prompt: Option<String>,
        timeout: Duration,
    ) -> Result<ClaudeCall, String> {
        let start = if item.rework || item.adopted {
            Start::Pushed
        } else {
            Start::Main
        };
        let dirs = worktree::prepare(
            &self.settings.repo,
            &item.worktree,
            &item.branch,
            start,
            &item.build,
        )
        .map_err(|e| e.to_string())?;
        if let Some(reason) = self.claude_files_differ() {
            return Err(reason);
        }
        let profile = WorkerProfile {
            worktree: &item.worktree,
            build: &item.build,
            git_common_dir: &dirs.git_common_dir,
            git_dir: &dirs.git_dir,
            branch: &item.branch,
            kelpie: &self.kelpie,
            guard_hooks: &self.settings.worker.guard_hooks,
            allowed_domains: &self.settings.worker.allowed_domains,
            build_env: &self.settings.worker.build_env,
        };
        let folder = &self.paths.worker;
        let settings = folder.join("settings.json");
        let instructions = folder.join("instructions.md");
        let text = serde_json::to_string_pretty(&profile.settings()).expect("settings are JSON");
        write(folder, &settings, &text)?;
        let text = instructions::compose(self.extra_instructions.as_deref(), &item.worktree);
        write(folder, &instructions, &text)?;
        let prompt = match prompt {
            Some(prompt) => prompt,
            None if item.rework => rework::first_prompt(item),
            None if item.adopted => adopt::first_prompt(item, None),
            None => {
                let issue = self
                    .ports
                    .forge
                    .issue(&self.settings.forge, item.issue)
                    .map_err(|e| format!("cannot read issue #{}: {e}", item.issue))?;
                first_prompt(item.issue, &issue)
            }
        };
        Ok(ClaudeCall {
            role: Role::Worker,
            model: item.worker.model.clone(),
            effort: item.worker.effort,
            session,
            cwd: item.worktree.clone(),
            settings,
            instructions: Some(instructions),
            prompt,
            timeout: Some(timeout),
        })
    }

    fn end_turn(
        &mut self,
        result: Result<ClaudeReply, ClaudeError>,
    ) -> Result<Option<StepReport>, StateError> {
        // A turn stopped with the runner stays running, to resume on restart.
        if matches!(result, Err(ClaudeError::Stopped)) {
            return Ok(None);
        }
        let now = self.ports.clock.now();
        // Whatever the turn left on `origin` is the worker's own. A head
        // that cannot be read keeps the last one, which errs toward parking.
        let pushed = self.state.work_item.as_ref().and_then(|_| self.own_push());
        let mut next = self.state.clone();
        let Some(item) = next.work_item.as_mut() else {
            return Ok(None);
        };
        if pushed.is_some() {
            item.known.head = pushed;
        }
        let report = match result {
            Ok(reply) => {
                let question = asked(&reply.text);
                // Only true the very first time: the pull request is
                // discovered once, and every later turn that reaches here
                // (a CI-failure fix, most often) already knows it.
                let discovering = item.pull_request.is_none();
                if discovering {
                    item.pull_request = self.pull_request_from(&item.branch);
                }
                let session = item.session.clone();
                let cost =
                    item.record_call(Role::Worker, now, session, reply.usage, reply.session_cost);
                item.turn = Turn::Ended { at: now };
                // The turn may have changed the code CodeRabbit was satisfied with.
                item.coderabbit.satisfied = false;
                // A question leaves the phase untouched: it interrupted
                // whatever was running, before that turn could be said to
                // have ended normally, and the answer resumes exactly this,
                // captured below before `park` parks it on a ruling.
                if question.is_none() {
                    if let Some(resume) = item.resume.take() {
                        item.phase = resume;
                    } else {
                        match item.phase.clone() {
                            Phase::Implement if discovering && item.pull_request.is_some() => {
                                item.phase = Phase::Review(Review::first());
                            }
                            Phase::Implement if item.pull_request.is_some() => {
                                item.phase = Phase::Ci {
                                    head: None,
                                    since: now,
                                };
                            }
                            // A fix turn stays fixing: the next step checks it pushed.
                            _ => {}
                        }
                    }
                }
                let (issue, session) = (item.issue, item.session.clone());
                let (work_item_cost_usd, pull_request) = (item.cost().usd(), item.pull_request);
                match question {
                    None => StepReport::Ended {
                        issue,
                        session,
                        usage: reply.usage,
                        cost_usd: cost.usd(),
                        work_item_cost_usd,
                        pull_request,
                    },
                    Some(text) => {
                        let resume = match &item.phase {
                            Phase::Review(review) => Resume::Review(review.clone()),
                            Phase::CodeRabbit(CodeRabbitStage::Fixing { head }) => {
                                Resume::CodeRabbitFix { head: head.clone() }
                            }
                            Phase::Implement if pull_request.is_some() => Resume::ReviewFirst,
                            _ => Resume::Nothing,
                        };
                        let kind = RulingKind::Question {
                            asked: text,
                            resume,
                        };
                        let project = self.project.as_str();
                        let (_, id, question) = park(project, &mut next, pull_request, kind);
                        StepReport::Asked {
                            issue,
                            session,
                            usage: reply.usage,
                            cost_usd: cost.usd(),
                            work_item_cost_usd,
                            pull_request,
                            id,
                            question,
                            comment_failed: None,
                        }
                    }
                }
            }
            Err(ClaudeError::TimedOut) => {
                item.turn = Turn::Ended { at: now };
                timed_out(self.project.as_str(), &mut next)
            }
            Err(e) => failed(self.project.as_str(), &mut next, now, e.to_string()),
        };
        self.save(next)?;
        let mut report = report;
        self.fill_comment_failed(&mut report);
        Ok(Some(report))
    }

    // A ruling just raised is posted as a comment on its pull request, if it
    // has one; only these three reports carry a ruling and need the outcome.
    pub(super) fn fill_comment_failed(&self, report: &mut StepReport) {
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

    // The open pull request from `branch`. A forge that cannot be asked
    // leaves it unrecorded, and status shows none.
    fn pull_request_from(&self, branch: &str) -> Option<u64> {
        let open = self
            .ports
            .forge
            .open_pull_requests(&self.settings.forge)
            .ok()?;
        open.into_iter()
            .find(|pr| pr.head == branch)
            .map(|pr| pr.number)
    }
}

// Parks the work item on a turn-ceiling ruling and builds its report. Shared
// by a call that actually hit `ClaudeError::TimedOut` and by a restart that
// finds a turn already past its ceiling with no call spent. The caller sets
// `item.turn` beforehand: this only raises the ruling. `comment_failed` is
// filled in afterwards, once the ruling has actually been posted.
fn timed_out(project: &str, next: &mut ProjectState) -> StepReport {
    let item = next
        .work_item
        .as_mut()
        .expect("a turn ceiling is about a work item");
    let (issue, session, pull_request) = (item.issue, item.session.clone(), item.pull_request);
    let phase = Some(item.phase.clone());
    let (_, id, question) = park(
        project,
        next,
        pull_request,
        RulingKind::TurnTimeout { phase },
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
pub(super) fn failed(
    project: &str,
    next: &mut ProjectState,
    at: Timestamp,
    reason: String,
) -> StepReport {
    let item = next
        .work_item
        .as_mut()
        .expect("a failed turn is about a work item");
    let failure = Turn::Failed {
        at,
        reason: reason.clone(),
    };
    let retry = std::mem::replace(&mut item.turn, failure);
    let (issue, pull_request) = (item.issue, item.pull_request);
    let kind = RulingKind::TurnFailed {
        reason,
        phase: item.phase.clone(),
        retry,
    };
    let (_, id, question) = park(project, next, pull_request, kind);
    StepReport::Failed {
        issue,
        pull_request,
        id,
        question,
        comment_failed: None,
    }
}

pub(super) fn write(folder: &Path, file: &Path, text: &str) -> Result<(), String> {
    fs::create_dir_all(folder)
        .and_then(|()| fs::write(file, text))
        .map_err(|e| format!("cannot write {}: {}", file.display(), e.kind()))
}

#[cfg(test)]
mod tests;
