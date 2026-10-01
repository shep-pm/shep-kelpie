//! Planning the board's pick before its work item opens
//!
//! The planning call runs outside the runner's lock, in a fresh session
//! that reads a detached worktree at `origin/main` and may change nothing.
//! A split opens sub-issues of the issue, in `split`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::report::{Begin, PlanOutcome, StepReport, WholeWorker};
use super::ruling::question;
use super::{Answer, RuleError, Runner};
use crate::board::{ReadyIssue, Skip};
use crate::plan::{self, Piece, Plan, Planned, Stage};
use crate::ports::{
    AgentCall, AgentError, AgentReply, Agents, Cost, Reach, Role, Session, Tools, Usage,
};
use crate::settings::MergeAuthority;
use crate::skills::Step;
use crate::state::{ProjectState, Ruling, RulingKind, StateError};
use crate::work_item::new_session_id;
use crate::worktree;

/// Where the planning call's throwaway settings go
const PLANNER_SETTINGS_FILE: &str = "planner-settings.json";

/// Forge refusals in a row before a split or a close waits on a ruling
/// instead, so one the forge will never take cannot hold up the board
const TRIES: u32 = 3;

/// What an issue kelpie closes, once its sub-issues are all closed, says
const PARENT_CLOSED: &str = "Every sub-issue of this issue is closed, so kelpie closes it too.";

/// What planning makes of the board's pick, when it is not added at once
pub(super) enum Planning {
    /// Passed over this poll
    Skip(Skip),
    /// The step does this instead of opening a work item
    Begin(Box<Begin>),
}

impl Runner {
    // None when `issue` opens its work item now: planning is off, it is a
    // sub-issue already, or it was planned whole.
    pub(super) fn plan_pick(&mut self, issue: &ReadyIssue) -> Result<Option<Planning>, StateError> {
        let number = issue.number;
        if !self.settings.planning.enabled || issue.parent.is_some() {
            return Ok(None);
        }
        if let Some(ruling) = self.split_ruling(number) {
            let skip = Skip::Planning {
                issue: number,
                ruling,
            };
            return Ok(Some(Planning::Skip(skip)));
        }
        let note = match self.plan_of(number) {
            Some(Stage::Whole) => return Ok(None),
            Some(Stage::Again { note }) => Some(note.clone()),
            Some(Stage::Splitting { pieces, .. }) => {
                let skip = Skip::Split {
                    issue: number,
                    open: pieces.len() as u64,
                };
                return Ok(Some(Planning::Skip(skip)));
            }
            // An issue closing has sub-issues, so the board never picks it.
            Some(Stage::Closing { .. }) | None => None,
        };
        let begin = match self.planning_call(number, note.as_deref()) {
            Ok((call, view)) => Begin::Plan(Box::new(call), view),
            Err(reason) => {
                self.set_plan(number, Stage::Whole)?;
                Begin::Report(StepReport::Planned {
                    issue: number,
                    outcome: PlanOutcome::Failed { reason },
                    usage: Usage::default(),
                    cost_usd: None,
                })
            }
        };
        Ok(Some(Planning::Begin(Box::new(begin))))
    }

    fn planning_call(
        &self,
        number: u64,
        note: Option<&str>,
    ) -> Result<(AgentCall, PathBuf), String> {
        let found = self.ports.forge.issue(&self.settings.forge, number);
        let found = found.map_err(|e| format!("cannot read the issue: {e}"))?;
        let session = new_session_id().map_err(|e| format!("cannot draw a session id: {e}"))?;
        let view = self.paths.plan();
        worktree::view(&self.settings.repo, &view).map_err(|e| e.to_string())?;
        let asked = plan::prompt(number, &found.title, &found.body, note);
        let model = &self.agents.planner;
        let minutes = u64::from(self.settings.worker.turn_timeout.get());
        // It reads and searches the repo, and runs no commands and no crew.
        // On Codex, which has no tool that only reads, it reads only its prompt.
        let call = AgentCall {
            role: Role::Planner,
            harness: model.harness.clone(),
            issue: number,
            model: model.model.as_str().to_owned(),
            effort: model.effort,
            session: Session::New(session),
            cwd: view.clone(),
            settings: self.paths.worker.join(PLANNER_SETTINGS_FILE),
            instructions: None,
            prompt: self.skills.invoke(Step::Planning, &asked),
            mcp_config: None,
            plugin_dirs: self.skills.plugin_dirs().to_vec(),
            timeout: Some(Duration::from_secs(minutes * 60)),
            tools: Tools::Review,
            reach: Reach::default(),
            lease: self.agents.limits.planner.lease().cloned(),
        };
        Ok((self.prepared(call)?, view))
    }

    /// Records what the planning call decided, once it answers
    pub(super) fn end_plan(
        &mut self,
        call: &AgentCall,
        view: &Path,
        result: Result<AgentReply, AgentError>,
    ) -> Result<Option<StepReport>, StateError> {
        // One left behind is removed before the next call.
        let _ = worktree::remove_view(&self.settings.repo, view);
        let issue = call.issue;
        // A trigger may have opened its work item while the call ran.
        if matches!(result, Err(AgentError::Stopped)) || self.state.item(issue).is_some() {
            return Ok(None);
        }
        let (usage, cost_usd) = result.as_ref().map_or((Usage::default(), None), |reply| {
            (reply.usage, reply.session_cost.map(Cost::usd))
        });
        let planned = result
            .map_err(|e| e.to_string())
            .and_then(|reply| plan::read(&reply.text));
        let outcome = match planned {
            Err(reason) => {
                self.set_plan(issue, Stage::Whole)?;
                PlanOutcome::Failed { reason }
            }
            Ok(Planned::Whole { why, worker }) => {
                self.set_plan(issue, Stage::Whole)?;
                let worker = self.apply_whole_worker(issue, worker.as_deref());
                PlanOutcome::Whole { why, worker }
            }
            Ok(Planned::Split { why, pieces }) => match self.settings.merge_authority {
                MergeAuthority::Auto => {
                    let count = pieces.len();
                    self.set_plan(issue, splitting(why, pieces))?;
                    PlanOutcome::Split { pieces: count }
                }
                MergeAuthority::Ask => self.ask_split(issue, why, pieces)?,
            },
        };
        Ok(Some(StepReport::Planned {
            issue,
            outcome,
            usage,
            cost_usd,
        }))
    }

    // Applies the plan's pick for an issue kept whole as a `worker:` label,
    // unless the issue already carries one: that wins over the pick. A
    // reply that named nothing kelpie runs, or a label the forge refuses,
    // leaves the issue to the project's own worker, with a comment saying
    // why, best effort, whether the reply left the worker out or named one
    // kelpie does not run.
    fn apply_whole_worker(&self, issue: u64, named: Option<&str>) -> WholeWorker {
        let (forge, repo) = (&self.ports.forge, &self.settings.forge);
        let found = match forge.issue(repo, issue) {
            Ok(found) => found,
            Err(e) => {
                return self
                    .default_whole_worker(issue, format!("cannot read #{issue} to label it: {e}"));
            }
        };
        if plan::already_has_worker(&found.labels) {
            return WholeWorker::Already;
        }
        if let Some(label) = plan::resolved_label(named) {
            return match forge.set_issue_label(repo, issue, &label, true) {
                Ok(()) => WholeWorker::Picked { label },
                Err(e) => self
                    .default_whole_worker(issue, format!("cannot add `{label}` to #{issue}: {e}")),
            };
        }
        self.default_whole_worker(issue, plan::fallback_reason(named))
    }

    // Leaves `issue` to the project's own worker for `reason`, and always
    // tries the comment saying so: `comment_failed` then tells a failed
    // attempt apart from one never made.
    fn default_whole_worker(&self, issue: u64, reason: String) -> WholeWorker {
        let (forge, repo) = (&self.ports.forge, &self.settings.forge);
        let body = format!(
            "Kelpie kept this issue whole. {reason}, so it runs on the project's default \
             worker."
        );
        let comment_failed = forge
            .post_comment(repo, issue, &body)
            .err()
            .map(|e| e.to_string());
        WholeWorker::Defaulted {
            reason,
            comment_failed,
        }
    }

    // A ruling on a split parks no work item: none is open yet.
    fn ask_split(
        &mut self,
        issue: u64,
        why: String,
        pieces: Vec<Piece>,
    ) -> Result<PlanOutcome, StateError> {
        let mut next = self.state.clone();
        next.plans.retain(|p| p.issue != issue);
        let kind = RulingKind::Split { why, pieces };
        let (ruling, question) = self.raise_on_issue(next, issue, kind)?;
        Ok(PlanOutcome::Asked { ruling, question })
    }

    // Saves `next` with a ruling on `issue` that parks no work item.
    fn raise_on_issue(
        &mut self,
        mut next: ProjectState,
        issue: u64,
        kind: RulingKind,
    ) -> Result<(u64, String), StateError> {
        let id = next.last_ruling + 1;
        let text = question(self.names(), id, issue, None, &kind);
        next.last_ruling = id;
        next.rulings.push(Ruling {
            id,
            issue: Some(issue),
            question: text.clone(),
            pull_request: None,
            kind,
            alerted: false,
            relayed: false,
            resend: false,
        });
        self.save(next)?;
        Ok((id, text))
    }

    // On a split: a yes splits the issue, a no works it whole, and an answer
    // plans it again. On a stuck split or close, a yes tries again. Once the
    // issue's work item is open the answer only settles the ruling.
    pub(super) fn rule_plan(
        &mut self,
        mut next: ProjectState,
        id: u64,
        issue: u64,
        kind: RulingKind,
        answer: Answer,
    ) -> Result<(), RuleError> {
        let stage = match (kind, answer) {
            (RulingKind::Split { why, pieces }, Answer::Yes) => Some(splitting(why, pieces)),
            (RulingKind::Split { .. }, Answer::No(_)) => Some(Stage::Whole),
            (RulingKind::Split { .. }, Answer::Text(note)) => Some(Stage::Again { note }),
            (_, Answer::Text(_)) => return Err(RuleError::NotAQuestion(id)),
            (RulingKind::SplitStuck { .. }, Answer::Yes) => {
                split::retried(&mut next, issue);
                None
            }
            (RulingKind::SplitStuck { .. }, Answer::No(_)) => Some(Stage::Whole),
            (RulingKind::CloseStuck { .. }, Answer::Yes) => Some(Stage::Closing { failures: 0 }),
            // Left at the limit, it is never tried again.
            (_, Answer::No(_)) => None,
            (_, Answer::Yes) => None,
        };
        if let Some(stage) = stage.filter(|_| next.item(issue).is_none()) {
            put_plan(&mut next, issue, stage);
        }
        self.focus = None;
        self.save(next).map_err(RuleError::State)
    }

    fn split_ruling(&self, issue: u64) -> Option<u64> {
        let rulings = self.state.rulings.iter();
        rulings
            .filter(|r| r.issue == Some(issue) && matches!(r.kind, RulingKind::Split { .. }))
            .map(|r| r.id)
            .next()
    }

    fn plan_of(&self, issue: u64) -> Option<&Stage> {
        let plans = self.state.plans.iter();
        plans.filter(|p| p.issue == issue).map(|p| &p.stage).next()
    }

    fn set_plan(&mut self, issue: u64, stage: Stage) -> Result<(), StateError> {
        let mut next = self.state.clone();
        put_plan(&mut next, issue, stage);
        self.save(next)
    }
}

fn put_plan(next: &mut ProjectState, issue: u64, stage: Stage) {
    next.plans.retain(|p| p.issue != issue);
    next.plans.push(Plan { issue, stage });
}

fn splitting(why: String, pieces: Vec<Piece>) -> Stage {
    Stage::Splitting {
        why,
        pieces,
        opened: Vec::new(),
        linked: 0,
        failures: 0,
    }
}

/// Runs a planning call, and once more in a fresh session when the first
/// timed out or failed, as a usage limit does
pub(super) fn run_planning(
    agents: &dyn Agents,
    call: &AgentCall,
) -> Result<AgentReply, AgentError> {
    let first = agents.run(call);
    if !matches!(first, Err(AgentError::TimedOut(_) | AgentError::Failed(..))) {
        return first;
    }
    let Ok(session) = new_session_id() else {
        return first;
    };
    let again = AgentCall {
        session: Session::New(session),
        ..call.clone()
    };
    agents.run(&again)
}

mod split;
#[cfg(test)]
mod tests;
