//! Planning the board's pick before its work item opens
//!
//! The planning call runs outside the runner's lock, in a fresh session
//! that reads a detached worktree at `origin/main` and may change nothing.
//! A split opens sub-issues of the issue, one piece at a time, saving after
//! each forge change so a restart carries on where it stopped. An issue
//! whose sub-issues are all closed is closed too.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::report::{Begin, PlanOutcome, StepReport};
use super::ruling::question;
use super::{Answer, RuleError, Runner, turn};
use crate::board::{READY, ReadyIssue, Skip};
use crate::plan::{self, Piece, Plan, Planned, Stage};
use crate::ports::{Claude, ClaudeCall, ClaudeError, ClaudeReply, Role, Session, Usage};
use crate::settings::MergeAuthority;
use crate::skills::Step;
use crate::state::{ProjectState, Ruling, RulingKind, StateError};
use crate::work_item::new_session_id;
use crate::worktree;

/// The planning call's throwaway settings: it reads the repo and changes nothing
const PLANNER_SETTINGS_FILE: &str = "planner-settings.json";

/// Every tool a planning call may not use: all but Read, Grep and Glob
const PLANNER_DENIES: [&str; 9] = [
    "Agent",
    "Task",
    "Bash",
    "Edit",
    "Write",
    "MultiEdit",
    "NotebookEdit",
    "WebFetch",
    "WebSearch",
];

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
    Begin(Begin),
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
                    cost_usd: 0.0,
                })
            }
        };
        Ok(Some(Planning::Begin(begin)))
    }

    fn planning_call(
        &self,
        number: u64,
        note: Option<&str>,
    ) -> Result<(ClaudeCall, PathBuf), String> {
        let found = self.ports.forge.issue(&self.settings.forge, number);
        let found = found.map_err(|e| format!("cannot read the issue: {e}"))?;
        let settings = self.paths.worker.join(PLANNER_SETTINGS_FILE);
        let deny = serde_json::json!({ "permissions": { "deny": PLANNER_DENIES } });
        let text = serde_json::to_string_pretty(&deny).expect("settings are JSON");
        turn::write(&self.paths.worker, &settings, &text)?;
        let session = new_session_id().map_err(|e| format!("cannot draw a session id: {e}"))?;
        let view = self.paths.plan();
        worktree::view(&self.settings.repo, &view).map_err(|e| e.to_string())?;
        let asked = plan::prompt(number, &found.title, &found.body, note);
        let model = &self.settings.models.planner;
        let minutes = u64::from(self.settings.worker.turn_timeout.get());
        let call = ClaudeCall {
            role: Role::Planner,
            issue: number,
            model: model.model.as_str().to_owned(),
            effort: model.effort,
            session: Session::New(session),
            cwd: view.clone(),
            settings,
            instructions: None,
            prompt: self.skills.invoke(Step::Planning, &asked),
            mcp_config: None,
            plugin_dirs: self.skills.plugin_dirs().to_vec(),
            timeout: Some(Duration::from_secs(minutes * 60)),
        };
        Ok((call, view))
    }

    /// Records what the planning call decided, once it answers
    pub(super) fn end_plan(
        &mut self,
        call: &ClaudeCall,
        view: &Path,
        result: Result<ClaudeReply, ClaudeError>,
    ) -> Result<Option<StepReport>, StateError> {
        // One left behind is removed before the next call.
        let _ = worktree::remove_view(&self.settings.repo, view);
        let issue = call.issue;
        // A trigger may have opened its work item while the call ran.
        if matches!(result, Err(ClaudeError::Stopped)) || self.state.item(issue).is_some() {
            return Ok(None);
        }
        let (usage, cost_usd) = result.as_ref().map_or((Usage::default(), 0.0), |reply| {
            (reply.usage, reply.session_cost.usd())
        });
        let planned = result
            .map_err(|e| e.to_string())
            .and_then(|reply| plan::read(&reply.text));
        let outcome = match planned {
            Err(reason) => {
                self.set_plan(issue, Stage::Whole)?;
                PlanOutcome::Failed { reason }
            }
            Ok(Planned::Whole { why }) => {
                self.set_plan(issue, Stage::Whole)?;
                PlanOutcome::Whole { why }
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
                retried(&mut next, issue);
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

    /// Carries on the split under way, if one is and it is not parked
    pub(super) fn split_under_way(&mut self) -> Result<Option<Begin>, StateError> {
        let splitting = self.state.plans.iter();
        let Some(issue) = splitting
            .filter(|p| matches!(p.stage, Stage::Splitting { failures, .. } if failures < TRIES))
            .map(|p| p.issue)
            .next()
        else {
            return Ok(None);
        };
        let report = match self.split(issue)? {
            Err(reason) => self.split_refused(issue, reason)?,
            Ok(report) => report,
        };
        Ok(Some(Begin::Report(report)))
    }

    // Counts a refused split step, and parks the split on a ruling once the
    // forge has refused `TRIES` steps in a row, so the board goes on.
    fn split_refused(&mut self, issue: u64, reason: String) -> Result<StepReport, StateError> {
        let mut next = self.state.clone();
        let mut stuck = None;
        if let Some(Plan {
            stage: Stage::Splitting {
                opened, failures, ..
            },
            ..
        }) = next.plans.iter_mut().find(|p| p.issue == issue)
        {
            *failures += 1;
            stuck = (*failures >= TRIES).then(|| opened.clone());
        }
        let ruling = match stuck {
            Some(opened) => {
                let reason = reason.clone();
                let kind = RulingKind::SplitStuck { reason, opened };
                Some(self.raise_on_issue(next, issue, kind)?.0)
            }
            None => {
                self.save(next)?;
                None
            }
        };
        Ok(StepReport::SplitFailed {
            issue,
            reason,
            ruling,
        })
    }

    // Each piece is opened, then linked as a sub-issue with its blockers.
    // What the forge already shows is not asked for again, so a step whose
    // answer was lost is not repeated. Err is the forge's refusal.
    fn split(&mut self, issue: u64) -> Result<Result<StepReport, String>, StateError> {
        let (forge, repo) = (&self.ports.forge, &self.settings.forge);
        let parent = match forge.issue(repo, issue) {
            Ok(found) => found,
            Err(e) => return Ok(Err(format!("cannot read the issue: {e}"))),
        };
        if !parent.open || !parent.labels.iter().any(|l| l == READY) {
            let mut next = self.state.clone();
            next.plans.retain(|p| p.issue != issue);
            self.save(next)?;
            let reason = if parent.open {
                format!("#{issue} is no longer labelled `{READY}`")
            } else {
                format!("#{issue} was closed")
            };
            return Ok(Ok(StepReport::SplitDropped { issue, reason }));
        }
        let labels: Vec<&str> = parent.labels.iter().map(String::as_str).collect();
        loop {
            let Some(Stage::Splitting {
                why,
                pieces,
                opened,
                linked,
                ..
            }) = self.plan_of(issue).cloned()
            else {
                unreachable!("a split under way is splitting");
            };
            let (forge, repo) = (&self.ports.forge, &self.settings.forge);
            let Some(piece) = pieces.get(linked) else {
                let comment = plan::comment(&why, &pieces, &opened);
                // `post_comment` takes an issue as well as a pull request.
                let comment_failed = forge.post_comment(repo, issue, &comment).err();
                let mut next = self.state.clone();
                next.plans.retain(|p| p.issue != issue);
                self.save(next)?;
                return Ok(Ok(StepReport::Split {
                    issue,
                    sub_issues: opened,
                    comment_failed: comment_failed.map(|e| e.to_string()),
                }));
            };
            let Some(&number) = opened.get(linked) else {
                let made = forge.create_issue(repo, &piece.title, &piece.body, &labels);
                let number = match made {
                    Ok(number) => number,
                    Err(e) => return Ok(Err(format!("cannot open piece {}: {e}", linked + 1))),
                };
                self.change_split(issue, |opened, _| opened.push(number))?;
                continue;
            };
            let shown = match forge.issue(repo, number) {
                Ok(shown) => shown,
                Err(e) => return Ok(Err(format!("cannot read #{number}: {e}"))),
            };
            if shown.parent != Some(issue)
                && let Err(e) = forge.add_sub_issue(repo, issue, number)
            {
                return Ok(Err(format!("cannot make #{number} a sub-issue: {e}")));
            }
            let blockers = piece.blocked_by.iter();
            let blockers =
                blockers.filter_map(|&by| by.checked_sub(1).and_then(|at| opened.get(at)));
            for by in blockers.filter(|by| !shown.blocked_by.contains(by)) {
                if let Err(e) = forge.add_blocker(repo, number, *by) {
                    return Ok(Err(format!("cannot mark #{number} blocked by #{by}: {e}")));
                }
            }
            self.change_split(issue, |_, linked| *linked += 1)?;
        }
    }

    // A step that lands also clears the count of refusals in a row.
    fn change_split(
        &mut self,
        issue: u64,
        change: impl FnOnce(&mut Vec<u64>, &mut usize),
    ) -> Result<(), StateError> {
        let mut next = self.state.clone();
        let plan = next.plans.iter_mut().find(|p| p.issue == issue);
        if let Some(Plan {
            stage:
                Stage::Splitting {
                    opened,
                    linked,
                    failures,
                    ..
                },
            ..
        }) = plan
        {
            change(opened, linked);
            *failures = 0;
        }
        self.save(next)
    }

    /// Closes a ready issue whose sub-issues are all closed, if the board
    /// lists one the forge has not refused too often
    pub(super) fn close_split_done(
        &mut self,
        ready: &[ReadyIssue],
    ) -> Result<Option<Begin>, StateError> {
        let done = ready.iter().find(|i| {
            i.sub_issues.all_closed()
                && !matches!(self.plan_of(i.number), Some(Stage::Closing { failures }) if *failures >= TRIES)
        });
        let Some(issue) = done.map(|i| i.number) else {
            return Ok(None);
        };
        let closed = (self.ports.forge).close_issue(&self.settings.forge, issue, PARENT_CLOSED);
        let mut next = self.state.clone();
        let report = match closed {
            Ok(()) => {
                next.plans.retain(|p| p.issue != issue);
                self.save(next)?;
                StepReport::ParentClosed { issue }
            }
            Err(e) => {
                let reason = e.to_string();
                let failures = match self.plan_of(issue) {
                    Some(Stage::Closing { failures }) => failures + 1,
                    _ => 1,
                };
                put_plan(&mut next, issue, Stage::Closing { failures });
                let ruling = if failures >= TRIES {
                    let kind = RulingKind::CloseStuck {
                        reason: reason.clone(),
                    };
                    Some(self.raise_on_issue(next, issue, kind)?.0)
                } else {
                    self.save(next)?;
                    None
                };
                StepReport::ParentCloseFailed {
                    issue,
                    reason,
                    ruling,
                }
            }
        };
        Ok(Some(Begin::Report(report)))
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

// A yes on a stuck split: it tries again from where it stopped.
fn retried(next: &mut ProjectState, issue: u64) {
    let plan = next.plans.iter_mut().find(|p| p.issue == issue);
    if let Some(Plan {
        stage: Stage::Splitting { failures, .. },
        ..
    }) = plan
    {
        *failures = 0;
    }
}

/// Runs a planning call, and once more in a fresh session when the first
/// timed out or failed, as a usage limit does
pub(super) fn run_planning(
    claude: &dyn Claude,
    call: &ClaudeCall,
) -> Result<ClaudeReply, ClaudeError> {
    let first = claude.run(call);
    if !matches!(first, Err(ClaudeError::TimedOut | ClaudeError::Failed(_))) {
        return first;
    }
    let Ok(session) = new_session_id() else {
        return first;
    };
    let again = ClaudeCall {
        session: Session::New(session),
        ..call.clone()
    };
    claude.run(&again)
}

#[cfg(test)]
mod tests;
