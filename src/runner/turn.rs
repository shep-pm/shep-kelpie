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
use super::alert::post_due;
use super::claude_files::Unchecked;
use super::instructions;
use super::question::asked;
use super::replies::answer_replies;
use super::report::{Begin, StepReport};
use super::review::run_review_call;
use super::rework;
use super::ruling::park;
use super::trigger::lock;
use crate::pacer::Scope;
use crate::ports::{AgentCall, AgentError, AgentReply, Cost, Issue, Reach, Role, Session, Tools};
use crate::profile::WorkerProfile;
use crate::settings::{AgentHarness, Effort, Limit};
use crate::skills::{Step, split_command};
use crate::state::{Resume, RulingKind, RunState, StateError};
use crate::work_item::{CodeRabbitStage, Phase, Review, ReviewStage, Turn, WorkItem};
use crate::worktree::{self, Start};
pub(super) use unfinished::failed;
use unfinished::{awaits_a_push, timed_out, uncommitted_prompt};

mod unfinished;

/// What a work item's worker runs on this turn
pub(super) struct WorkerAgent {
    /// Its harness
    pub(super) harness: AgentHarness,
    /// What holds its turns back
    pub(super) limit: Limit,
    /// The model, as the harness takes it
    pub(super) model: String,
    /// How hard the model thinks
    pub(super) effort: Effort,
    /// Its agent file's body, added to kelpie's own instructions
    pub(super) prompt: Option<String>,
}

/// The prompt for a turn resumed after the runner restarted
const CONTINUE: &str = "Kelpie restarted while your last turn was running. \
                        Carry on with the work item from where you left off.";

/// Posts a ruling or a notice, handles a reply on the webhook's topic, or
/// runs the worker's next turn if one is due and the project is running
///
/// Each open work item is stepped in turn, starting after the one that did
/// something last, and one with nothing to do yields to the next. Returns
/// what happened, or `None` when there was nothing to do. A report that
/// waits, such as a forge that cannot be read, is returned only when no
/// other work item did anything. A ruling is posted, and a reply handled,
/// whether the project runs or not.
///
/// # Errors
///
/// [`StateError`] when the turn's start or end, or a post, cannot be saved.
pub fn step(runner: &Mutex<Runner>) -> Result<Option<StepReport>, StateError> {
    lock(runner).beat();
    let (claude, reviewer, alerts, turns) = {
        let runner = lock(runner);
        (
            Arc::clone(&runner.ports.agents),
            Arc::clone(&runner.ports.reviewer),
            Arc::clone(&runner.ports.alerts),
            runner.live_turns.clone(),
        )
    };
    if let Some(posted) = post_due(runner, alerts.as_ref()) {
        return posted.map(Some);
    }
    if let Some(answered) = answer_replies(runner, alerts.as_ref()) {
        return answered.map(Some);
    }
    // The work item whose session died unborn, which starts over
    let mut start_over = None;
    // Held until the step returns, past the save that ends the turn
    let mut _live = None;
    loop {
        // The call runs outside the lock, and a trigger may work on another
        // item meanwhile, so its end names the item it began on.
        let (begin, issue) = {
            let mut runner = lock(runner);
            let begin = runner.begin_turn(start_over)?;
            (begin, runner.focus)
        };
        match begin {
            Begin::Idle => return Ok(None),
            Begin::Report(report) => return Ok(Some(report)),
            Begin::Call(call) => {
                if _live.is_none() {
                    _live = issue.map(|issue| turns.enter(issue));
                }
                let result = claude.run(&call);
                if start_over.is_none() && matches!(result, Err(AgentError::NoSession(..))) {
                    // The dead call's time is the worker's, so it is saved
                    // before a pause can outrun the new turn.
                    lock(runner).save_time()?;
                    start_over = issue;
                    continue;
                }
                return lock(runner).on(issue).end_turn(result);
            }
            Begin::Review(action) => {
                // A failed save is told and let go. A GPU wait may then count as
                // `local_round`, but the phases still sum to the wall time.
                let watch = |stage| {
                    if let Err(e) = lock(runner).on(issue).round_stage(stage) {
                        eprintln!("cannot save the local round's stage: {e}");
                    }
                };
                let reviewed = run_review_call(claude.as_ref(), reviewer.as_ref(), action, &watch);
                return lock(runner).on(issue).end_review(reviewed);
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

/// What a step can work on
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Slot {
    /// The open work item for this issue
    Item(u64),
    /// The board, while a slot is free under `max_items`
    Board,
}

impl Runner {
    // Steps each open work item, and the board while a slot is free, from
    // the one after the last to act, until one acts. `start_over` is the
    // item whose session died unborn, which begins the same turn again.
    fn begin_turn(&mut self, start_over: Option<u64>) -> Result<Begin, StateError> {
        if self.state.run != RunState::Running {
            return Ok(Begin::Idle);
        }
        if let Some(issue) = start_over {
            self.focus = Some(issue);
            return self.begin_item(true);
        }
        let mut waiting = None;
        for slot in self.rotation() {
            let begin = match slot {
                Slot::Item(issue) => {
                    self.focus = Some(issue);
                    self.begin_item(false)?
                }
                Slot::Board => {
                    self.focus = None;
                    self.dispatch()?
                }
            };
            match begin {
                Begin::Idle => {}
                Begin::Report(report) if report.waits() => {
                    waiting.get_or_insert(report);
                }
                begin => {
                    self.last_acted = Some(slot);
                    return Ok(begin);
                }
            }
        }
        Ok(waiting.map_or(Begin::Idle, Begin::Report))
    }

    // The open work items oldest first, then the board while a slot is
    // free, from the one after the last to act, so one that keeps acting
    // cannot starve the rest
    fn rotation(&self) -> Vec<Slot> {
        let items = self.state.work_items.iter().map(|i| Slot::Item(i.issue));
        let board = self.slot_free().then_some(Slot::Board);
        let mut slots: Vec<Slot> = items.chain(board).collect();
        if let Some(at) = slots.iter().position(|&s| Some(s) == self.last_acted) {
            slots.rotate_left(at + 1);
        }
        slots
    }

    fn begin_item(&mut self, start_over: bool) -> Result<Begin, StateError> {
        let Some(item) = self.current() else {
            return Ok(Begin::Idle);
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
                return self.review_bot_step();
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
        // A rework can start on a branch that already changes them.
        if due && let Some(parked) = self.claude_files_changed(Unchecked::CarryOn)? {
            return Ok(parked);
        }
        if due && let Some(held) = self.pace_worker(Scope::Turn)?.holds() {
            return Ok(held);
        }
        let item = self.current().expect("checked above");
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
                let (command, then) = split_command(prompt);
                let first = adopt::first_prompt(item, Some(then));
                let first = match command {
                    Some(command) => format!("{command} {first}"),
                    None => first,
                };
                (Session::New(id), Some(first), now)
            }
            // No session to resume: the retry of a turn whose call failed
            // before its session existed starts it over, as a killed one does.
            Turn::Next { .. } if start_over => (Session::New(id), None, now),
            Turn::Next { prompt } => (Session::Resume(id), Some(prompt.clone()), now),
            // A first turn that ended with no pull request and no question
            // stopped short, maybe on a tool that failed.
            Turn::Ended { .. } if item.pull_request.is_none() || awaits_a_push(item) => {
                return self.stopped_short();
            }
            Turn::Ended { .. } | Turn::Failed { .. } => return Ok(Begin::Idle),
        };
        let ceiling = self.turn_ceiling();
        let elapsed = Duration::from_secs(now.0.saturating_sub(since.0));
        let remaining = ceiling.saturating_sub(elapsed);
        if remaining.is_zero() {
            // The ceiling passed while kelpie was down, before a new call
            // could even be tried: park it as a call that hit
            // `AgentError::TimedOut` would, with no call spent.
            return self.park_ceiling_passed(now);
        }
        let issue = item.issue;
        let prepared = self.prepare(item, session, prompt, remaining);
        let mut next = self.state.clone();
        let mut begin = match prepared {
            Ok(call) => {
                let item = self
                    .current_in(&mut next)
                    .expect("the work item checked above");
                item.turn = Turn::Running { since };
                Begin::Call(call)
            }
            Err(reason) => {
                let names = self.names();
                Begin::Report(failed(names, &mut next, issue, now, reason))
            }
        };
        self.save(next)?;
        if let Begin::Report(report) = &mut begin {
            self.fill_comment_failed(report);
        }
        Ok(begin)
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
    ) -> Result<AgentCall, String> {
        let start = if item.rework || item.adopted {
            Start::Pushed
        } else {
            Start::Main
        };
        let WorkerAgent {
            harness,
            limit,
            model,
            effort,
            prompt: agent_prompt,
        } = self.worker_agent(item)?;
        let reach = self.worker_reach(item, start)?;
        let folder = &self.paths.worker;
        let settings = folder.join("settings.json");
        let instructions = folder.join("instructions.md");
        let text = instructions::compose(
            instructions::Extra {
                agent: agent_prompt.as_deref(),
                project: self.extra_instructions.as_deref(),
            },
            &item.worktree,
            &self.skills,
            &self.kelpie,
        );
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
                self.skills
                    .invoke(Step::Implement, &first_prompt(item.issue, &issue))
            }
        };
        self.prepared(AgentCall {
            role: Role::Worker,
            harness,
            issue: item.issue,
            model,
            effort,
            session,
            cwd: item.worktree.clone(),
            settings,
            instructions: Some(instructions),
            prompt,
            timeout: Some(timeout),
            plugin_dirs: self.skills.plugin_dirs().to_vec(),
            tools: Tools::Work,
            reach,
            lease: limit.lease().cloned(),
        })
    }

    /// What a session working in `item`'s worktree reaches: the worker's own
    /// fence, with its worktree and build folder made ready
    ///
    /// # Errors
    ///
    /// Why the worktree cannot be made ready, or why agents' own files in it
    /// are refused.
    pub(super) fn worker_reach(&self, item: &WorkItem, start: Start) -> Result<Reach, String> {
        let dirs = worktree::prepare(
            &self.settings.repo,
            &item.worktree,
            &item.branch,
            start,
            &item.build,
        )
        .map_err(|e| e.to_string())?;
        if let Some(reason) = self.claude_files_refusal() {
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
            kelpie_home: &self.paths.kelpie_home,
            repo: &self.settings.repo,
            private_names: &self.settings.private_names,
            allowed_domains: &self.settings.worker.allowed_domains,
            build_env: &self.settings.worker.build_env,
            shep_home: &self.paths.shep_home,
            door: &self.paths.door,
        };
        Ok(profile.reach())
    }

    /// The agent `item`'s worker runs on: the one it opened on, as that
    /// agent's file now stands
    ///
    /// # Errors
    ///
    /// Why not, when kelpie no longer has that agent, or the worker's fence
    /// would not hold on it.
    pub(super) fn worker_agent(&self, item: &WorkItem) -> Result<WorkerAgent, String> {
        let name = &item.agent;
        let Some(agent) = self.book.get(name) else {
            return Err(format!(
                "issue #{} runs on agent {name}, which has no agent file now: write \
                 `agents/{name}.md` in kelpie's home again, or drop and add the issue",
                item.issue
            ));
        };
        self.settings
            .under_worker_fence(name, &agent.model)
            .map_err(|e| e.to_string())?;
        Ok(WorkerAgent {
            harness: agent.model.harness.clone(),
            limit: agent.limit.clone(),
            model: agent.model.model.as_str().to_owned(),
            effort: agent.model.effort,
            prompt: agent.prompt.clone(),
        })
    }

    /// `call`, once its harness has what it needs on disk
    ///
    /// Done before the call is marked running, so a failure keeps the turn
    /// it would have run for a retry.
    pub(super) fn prepared(&self, call: AgentCall) -> Result<AgentCall, String> {
        self.ports
            .agents
            .prepare(&call)
            .map_err(|e| e.to_string())?;
        Ok(call)
    }

    fn end_turn(
        &mut self,
        result: Result<AgentReply, AgentError>,
    ) -> Result<Option<StepReport>, StateError> {
        // A turn stopped with the runner stays running, to resume on restart.
        // Its time so far is saved to the worker, which still runs it.
        if matches!(result, Err(AgentError::Stopped)) {
            return self.save_time().map(|()| None);
        }
        let now = self.ports.clock.now();
        // Whatever the turn left on `origin` is the worker's own. A head
        // that cannot be read keeps the last one, which errs toward parking.
        let pushed = self.current().and_then(|_| self.own_push());
        let mut next = self.state.clone();
        let Some(item) = self.current_in(&mut next) else {
            return Ok(None);
        };
        let issue = item.issue;
        // A rework or adoption turn that moved nothing on `origin` pushed no fix.
        let before = item.known.head.clone();
        let no_push = pushed.as_ref().is_none_or(|p| Some(p) == before.as_ref());
        let pushed_nothing = before.is_some() && no_push;
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
                // A turn that left its work uncommitted and pushed nothing is
                // sent back once, before the gate can park it on a ruling.
                let asked = std::mem::take(&mut item.asked_to_commit);
                // A worktree git cannot be read in is told and let go: the
                // turn ends as it did before, which errs toward the ruling.
                let uncommitted = match (question.is_none() && no_push && !asked)
                    .then(|| worktree::uncommitted(&self.settings.repo, &item.worktree))
                {
                    Some(Ok(files)) => files,
                    Some(Err(e)) => {
                        eprintln!("cannot list issue #{issue}'s uncommitted files: {e}");
                        Vec::new()
                    }
                    None => Vec::new(),
                };
                let commit_first = !uncommitted.is_empty();
                item.turn = if commit_first {
                    item.asked_to_commit = true;
                    // The pull request this turn opened still owes its first
                    // review, which the follow-up's end is too late to see as
                    // a discovery.
                    if discovering
                        && item.pull_request.is_some()
                        && item.resume.is_none()
                        && matches!(item.phase, Phase::Implement)
                    {
                        item.resume = Some(Phase::Review(Review::first()));
                    }
                    Turn::Next {
                        prompt: uncommitted_prompt(&uncommitted),
                    }
                } else {
                    Turn::Ended { at: now }
                };
                // The turn may have changed the code CodeRabbit was satisfied with.
                item.coderabbit.satisfied = false;
                // A question leaves the phase untouched: it interrupted
                // whatever was running, before that turn could be said to
                // have ended normally, and the answer resumes exactly this,
                // captured below before `park` parks it on a ruling.
                let stopped_short = pushed_nothing && awaits_a_push(item);
                if question.is_none() && !stopped_short && !commit_first {
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
                let session = item.session.clone();
                let (work_item_cost_usd, pull_request) = (item.cost().usd(), item.pull_request);
                match question {
                    None => StepReport::Ended {
                        issue,
                        session,
                        usage: reply.usage,
                        cost_usd: cost.map(Cost::usd),
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
                        let names = self.names();
                        let (id, question) = park(names, &mut next, issue, pull_request, kind);
                        StepReport::Asked {
                            issue,
                            session,
                            usage: reply.usage,
                            cost_usd: cost.map(Cost::usd),
                            work_item_cost_usd,
                            pull_request,
                            id,
                            question,
                            comment_failed: None,
                        }
                    }
                }
            }
            Err(AgentError::TimedOut(_)) => {
                item.turn = Turn::Ended { at: now };
                timed_out(self.names(), &mut next, issue)
            }
            Err(e) => failed(self.names(), &mut next, issue, now, e.to_string()),
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

pub(super) fn write(folder: &Path, file: &Path, text: &str) -> Result<(), String> {
    fs::create_dir_all(folder)
        .and_then(|()| fs::write(file, text))
        .map_err(|e| format!("cannot write {}: {}", file.display(), e.kind()))
}

#[cfg(test)]
mod tests;
