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
            Phase::Review(_) => return self.review_step(),
            Phase::CodeRabbit(CodeRabbitStage::Fixing { .. })
                if !matches!(item.turn, Turn::Ended { .. }) => {}
            Phase::Ci { .. } => return self.check_ci(),
            Phase::CodeRabbit(_) => return self.coderabbit_step(),
            Phase::Ruling { .. } => return Ok(Begin::Idle),
            Phase::Merge { .. } => return self.merge(),
            Phase::Done { merged } => return self.finish(*merged),
        }
        // Only a turn that has not begun waits on the pacer: one already
        // running carries on, since a turn is never interrupted, and start_over
        // resumes the same not-yet-begun turn after a session died unborn.
        let due = matches!(item.turn, Turn::Due | Turn::Next { .. });
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
    // no prompt of its own is the first, and takes the issue or the review.
    fn prepare(
        &self,
        item: &WorkItem,
        session: Session,
        prompt: Option<String>,
        timeout: Duration,
    ) -> Result<ClaudeCall, String> {
        let start = if item.rework {
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
        let pushed = self
            .state
            .work_item
            .as_ref()
            .and_then(|_| self.origin_head().ok());
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
    // has one; only these two reports carry a ruling and need the outcome.
    fn fill_comment_failed(&self, report: &mut StepReport) {
        match report {
            StepReport::Asked {
                pull_request,
                question,
                comment_failed,
                ..
            }
            | StepReport::TimedOut {
                pull_request,
                question,
                comment_failed,
                ..
            }
            | StepReport::Failed {
                pull_request,
                question,
                comment_failed,
                ..
            } => {
                *comment_failed = self.post_ruling(*pull_request, question);
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
fn failed(project: &str, next: &mut ProjectState, at: Timestamp, reason: String) -> StepReport {
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
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use serde_json::json;

    use super::*;
    use crate::ports::{Cost, Usage};
    use crate::profile::INSTRUCTIONS;
    use crate::settings::Effort;
    use crate::test::{LEFT_BEHIND, Rig, Scripted, git};

    fn usage(n: u64) -> Usage {
        Usage {
            input: n,
            cache_write: 10 * n,
            cache_read: 100 * n,
            output: 1000 * n,
        }
    }

    // A running project with issue 7 in flight
    fn with_issue_7(project: &str) -> (Rig, Mutex<Runner>) {
        let rig = Rig::new(project);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        assert_eq!(rig.ask(&runner, "add", Some("7"))["work_item"]["issue"], 7);
        (rig, runner)
    }

    #[test]
    fn the_first_turn_starts_the_workers_session_in_its_own_worktree() {
        let (rig, runner) = with_issue_7("shep");
        rig.claude
            .script([Scripted::Reply(usage(1), Cost(20_085_300))]);
        step(&runner).unwrap();

        let [seen] = rig.claude.seen().try_into().unwrap();
        let call = seen.call;
        let worktree = rig.home.path().join("kelpie/wt/shep/7");
        assert_eq!(call.role, Role::Worker);
        assert_eq!(
            (call.model.as_str(), call.effort),
            ("claude-sonnet-5", Effort::Medium)
        );
        assert!(
            matches!(call.session, Session::New(_)),
            "{:?}",
            call.session
        );
        assert_eq!(call.cwd, worktree);
        assert_eq!(
            call.prompt,
            "Your work item is issue #7: Title of #7\n\nBody of #7.\n"
        );
        let worker = rig.paths().worker;
        assert_eq!(call.settings, worker.join("settings.json"));
        assert_eq!(call.instructions, Some(worker.join("instructions.md")));
        assert_eq!(
            fs::read_to_string(worker.join("instructions.md")).unwrap(),
            INSTRUCTIONS
        );
        assert!(seen.build_existed, "the build folder came after the worker");

        assert_eq!(git(&worktree, &["branch", "--show-current"]), "kelpie/7");
        let origin_main = git(&rig.repo(), &["rev-parse", "origin/main"]);
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), origin_main);
    }

    #[test]
    fn the_branch_is_cut_from_the_latest_origin_main() {
        let rig = Rig::new("reactmap");
        let landed = rig.land_on_origin("landed.txt");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("3"));
        rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
        step(&runner).unwrap();
        let worktree = rig.home.path().join("kelpie/wt/reactmap/3");
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), landed);
    }

    #[test]
    fn the_settings_file_fences_writes_to_this_worktree_and_its_git_paths() {
        let (rig, runner) = with_issue_7("koji");
        rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
        step(&runner).unwrap();
        let [seen] = rig.claude.seen().try_into().unwrap();
        let kelpie = rig.home.path().join("kelpie");
        let git_dir = fs::canonicalize(rig.repo().join(".git")).unwrap();
        let allow = &seen.settings["sandbox"]["filesystem"]["allowWrite"];
        assert_eq!(allow[0], json!(kelpie.join("wt/koji/7")));
        assert_eq!(allow[1], json!(kelpie.join("targets/koji/7")));
        assert_eq!(allow[2], json!(git_dir.join("objects")));
        assert_eq!(allow[3], json!(git_dir.join("worktrees/7")));
        assert_eq!(
            seen.settings["sandbox"]["filesystem"]["denyWrite"][0],
            json!(git_dir.join("config"))
        );
        assert_eq!(
            seen.settings["hooks"]["PreToolUse"][1]["hooks"][0]["command"],
            "node ~/.claude/hooks/git-gh-guard.js"
        );
    }

    #[test]
    fn status_shows_the_session_and_what_the_work_item_has_cost() {
        let (rig, runner) = with_issue_7("golbat");
        rig.claude
            .script([Scripted::Reply(usage(2), Cost(20_085_300))]);
        let report = step(&runner).unwrap().unwrap();
        let session = rig.claude.calls()[0].session.id().clone();
        assert_eq!(
            report,
            StepReport::Ended {
                issue: 7,
                session: session.clone(),
                usage: usage(2),
                cost_usd: 0.0200853,
                work_item_cost_usd: 0.0200853,
                pull_request: None,
            }
        );
        let item = &rig.ask(&runner, "status", None)["work_item"];
        assert_eq!(item["session"], json!(session));
        assert_eq!(item["cost_usd"], 0.0200853);
        assert_eq!(item["calls"], 1);
        assert_eq!(item["turn"], json!({ "state": "ended", "at": Rig::EPOCH }));
    }

    #[test]
    fn a_runner_killed_mid_turn_resumes_the_same_session_in_the_same_worktree() {
        let (rig, runner) = with_issue_7("rotom");
        rig.claude.script([Scripted::Kill]);
        let killed = catch_unwind(AssertUnwindSafe(|| step(&runner)));
        assert!(killed.is_err(), "the scripted kill did not happen");
        drop(runner);

        let runner = rig.open().unwrap();
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["turn"]["state"],
            "running"
        );
        // The resumed call reports the whole session so far, the killed call
        // included, and all of it lands on the work item.
        rig.claude
            .script([Scripted::Reply(usage(1), Cost(30_000_000))]);
        step(&runner).unwrap();

        let [first, second] = rig.claude.calls().try_into().unwrap();
        assert_eq!(second.session, Session::Resume(first.session.id().clone()));
        assert_eq!(second.cwd, first.cwd);
        assert!(
            second.cwd.join(LEFT_BEHIND).exists(),
            "the worktree was replaced"
        );
        assert_eq!(second.prompt, CONTINUE);
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["cost_usd"],
            0.03
        );
    }

    #[test]
    fn a_worker_that_repoints_its_git_file_cannot_move_its_fence() {
        let (rig, runner) = with_issue_7("golbat");
        rig.claude.script([Scripted::Kill]);
        let _ = catch_unwind(AssertUnwindSafe(|| step(&runner)));
        drop(runner);

        // What a worker could do from inside its worktree: a fake git dir
        // whose common dir is a folder it wants to write.
        let worktree = rig.home.path().join("kelpie/wt/golbat/7");
        let wanted = rig.home.path().join("wanted");
        let fake = worktree.join("fake");
        for dir in ["refs", "objects"] {
            fs::create_dir_all(fake.join(dir)).unwrap();
            fs::create_dir_all(wanted.join(dir)).unwrap();
        }
        fs::write(fake.join("HEAD"), "ref: refs/heads/kelpie/7\n").unwrap();
        fs::write(fake.join("commondir"), format!("{}\n", wanted.display())).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", fake.display()),
        )
        .unwrap();

        let runner = rig.open().unwrap();
        rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
        step(&runner).unwrap();
        let [_, seen] = rig.claude.seen().try_into().unwrap();
        let git_dir = fs::canonicalize(rig.repo().join(".git")).unwrap();
        let allow = &seen.settings["sandbox"]["filesystem"]["allowWrite"];
        assert_eq!(allow[2], json!(git_dir.join("objects")));
        assert_eq!(allow[3], json!(git_dir.join("worktrees/7")));
        assert!(!allow.to_string().contains("wanted"), "{allow}");
    }

    #[test]
    fn a_turn_stopped_with_the_runner_resumes_when_it_starts_again() {
        let (rig, runner) = with_issue_7("reactmap");
        rig.claude.script([Scripted::Fail(ClaudeError::Stopped)]);
        assert_eq!(step(&runner).unwrap(), None);
        drop(runner);

        let runner = rig.open().unwrap();
        rig.claude.script([Scripted::Reply(usage(1), Cost(9))]);
        step(&runner).unwrap();
        let [first, second] = rig.claude.calls().try_into().unwrap();
        assert_eq!(second.session, Session::Resume(first.session.id().clone()));
        assert_eq!(second.prompt, CONTINUE);
    }

    #[test]
    fn a_session_killed_before_it_began_starts_over_with_the_same_id() {
        let (rig, runner) = with_issue_7("xilriws");
        rig.claude.script([Scripted::Kill]);
        let _ = catch_unwind(AssertUnwindSafe(|| step(&runner)));
        drop(runner);

        let runner = rig.open().unwrap();
        let first = rig.claude.calls()[0].session.id().clone();
        rig.claude.script([
            Scripted::Fail(ClaudeError::NoSession(first.clone())),
            Scripted::Reply(usage(1), Cost(5)),
        ]);
        step(&runner).unwrap();
        let [original, resumed, again] = rig.claude.calls().try_into().unwrap();
        assert_eq!(resumed.session, Session::Resume(first.clone()));
        assert_eq!(again.session, Session::New(first));
        assert_eq!(again.prompt, original.prompt);
    }

    #[test]
    fn a_paused_project_runs_no_turn_until_it_starts() {
        let rig = Rig::new("chelone");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "add", Some("5"));
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.claude.calls(), []);
        assert!(!rig.home.path().join("kelpie/wt/chelone/5").exists());

        rig.ask(&runner, "start", None);
        rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
        assert!(step(&runner).unwrap().is_some());
        assert_eq!(rig.claude.calls().len(), 1);
    }

    #[test]
    fn a_failed_turn_raises_a_ruling_carrying_why_and_alerts_like_the_rest() {
        let (rig, runner) = with_issue_7("zeus");
        rig.claude
            .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
        let Some(StepReport::Failed {
            issue,
            pull_request,
            id,
            question,
            ..
        }) = step(&runner).unwrap()
        else {
            panic!("the failed turn raised no ruling");
        };
        assert_eq!((issue, pull_request, id), (7, None, 1));
        assert!(
            question
                .starts_with("The worker's turn on issue #7 failed: claude failed: overloaded."),
            "{question}"
        );
        let item = &rig.ask(&runner, "status", None)["work_item"];
        assert_eq!(
            item["turn"],
            json!({ "state": "failed", "at": Rig::EPOCH, "reason": "claude failed: overloaded" })
        );
        assert_eq!(item["phase"], json!({ "state": "ruling", "id": 1 }));

        assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
        let [(_, alert)] = rig.alerts.posts().try_into().unwrap();
        assert_eq!(alert.text, question);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.claude.calls().len(), 1);
    }

    #[test]
    fn a_yes_on_a_failed_turn_resumes_its_session() {
        let (rig, runner) = with_issue_7("zeus");
        rig.claude
            .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
        step(&runner).unwrap();
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Ended { .. })
        ));
        let [failed, retried] = rig.claude.calls().try_into().unwrap();
        assert_eq!(
            retried.session,
            Session::Resume(failed.session.id().clone())
        );
        assert_eq!(
            retried.prompt,
            "Your last turn failed before it finished. \
             Carry on with the work item from where you left off."
        );
    }

    #[test]
    fn a_retry_whose_session_never_began_starts_it_over_from_the_issue() {
        let (rig, runner) = with_issue_7("zeus");
        rig.claude
            .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
        step(&runner).unwrap();
        let first = rig.claude.calls()[0].session.id().clone();
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.claude.script([
            Scripted::Fail(ClaudeError::NoSession(first.clone())),
            Scripted::Reply(usage(1), Cost(1)),
        ]);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Ended { .. })
        ));
        let [original, _, again] = rig.claude.calls().try_into().unwrap();
        assert_eq!(again.session, Session::New(first));
        assert_eq!(again.prompt, original.prompt);
    }

    #[test]
    fn a_retried_turn_gets_a_whole_ceiling_however_long_the_ruling_waited() {
        let (rig, runner) = with_issue_7("zeus");
        rig.claude
            .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
        step(&runner).unwrap();
        rig.clock.advance(2000);
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
        step(&runner).unwrap();
        let [_, retried] = rig.claude.calls().try_into().unwrap();
        assert_eq!(retried.timeout, Some(Duration::from_secs(3600)));
    }

    #[test]
    fn a_no_on_a_failed_turn_stops_the_work_item_the_way_a_timed_out_one_does() {
        let (rig, runner) = with_issue_7("rotom");
        rig.claude
            .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
        step(&runner).unwrap();
        rig.ask(&runner, "rule", Some("1 no not worth another go"));
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Finished {
                issue: 7,
                pull_request: None,
                merged: false,
                ..
            })
        ));
        assert!(!rig.worktree_7().exists());
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    }

    #[test]
    fn a_turn_past_its_ceiling_is_stopped_and_a_yes_resumes_its_session() {
        let (rig, runner) = with_issue_7("zeus");
        rig.claude.script([Scripted::Fail(ClaudeError::TimedOut)]);
        let Some(StepReport::TimedOut {
            issue,
            session,
            pull_request,
            id,
            question,
            ..
        }) = step(&runner).unwrap()
        else {
            panic!("the timed-out turn raised no ruling");
        };
        assert_eq!((issue, pull_request, id), (7, None, 1));
        assert!(
            question.starts_with(
                "The worker on issue #7 has been running past its turn's ceiling, \
                 and kelpie stopped it."
            ),
            "{question}"
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"],
            json!({ "state": "ruling", "id": 1 })
        );

        rig.ask(&runner, "rule", Some("1 yes"));
        rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Ended { .. })
        ));
        let [_, resumed] = rig.claude.calls().try_into().unwrap();
        assert_eq!(resumed.session, Session::Resume(session));
        assert_eq!(
            resumed.prompt,
            "Kelpie stopped your last turn: it ran past its ceiling. \
             Carry on with the work item from where you left off."
        );
    }

    #[test]
    fn a_no_on_a_timed_out_turn_stops_the_work_item_keeping_nothing_of_its_own() {
        let (rig, runner) = with_issue_7("rotom");
        rig.claude.script([Scripted::Fail(ClaudeError::TimedOut)]);
        step(&runner).unwrap();
        rig.ask(&runner, "rule", Some("1 no not worth waiting for"));
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Finished {
                issue: 7,
                pull_request: None,
                merged: false,
                ..
            })
        ));
        assert!(!rig.worktree_7().exists());
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    }

    #[test]
    fn a_worker_turn_carries_the_projects_timeout() {
        let (rig, runner) = with_issue_7("golbat");
        rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
        step(&runner).unwrap();
        let [seen] = rig.claude.calls().try_into().unwrap();
        assert_eq!(seen.timeout, Some(std::time::Duration::from_secs(3600)));
    }

    #[test]
    fn a_restart_before_the_ceiling_passes_resumes_with_only_the_time_left() {
        let (rig, runner) = with_issue_7("zeus");
        rig.claude.script([Scripted::Kill]);
        let _ = catch_unwind(AssertUnwindSafe(|| step(&runner)));
        drop(runner);

        rig.clock.advance(2000);
        let runner = rig.open().unwrap();
        rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
        step(&runner).unwrap();
        let [_, resumed] = rig.claude.calls().try_into().unwrap();
        assert_eq!(
            resumed.timeout,
            Some(std::time::Duration::from_secs(1600)),
            "the restart must not reset the ceiling to a fresh hour"
        );
    }

    #[test]
    fn a_restart_after_the_ceiling_passed_parks_it_with_no_call_spent() {
        let (rig, runner) = with_issue_7("chelone");
        rig.claude.script([Scripted::Kill]);
        let _ = catch_unwind(AssertUnwindSafe(|| step(&runner)));
        drop(runner);

        // The whole ceiling, and then some, passes while kelpie is down.
        rig.clock.advance(3601);
        let runner = rig.open().unwrap();
        let Some(StepReport::TimedOut { id, .. }) = step(&runner).unwrap() else {
            panic!("a turn found past its ceiling on restart raised no ruling");
        };
        assert_eq!(id, 1);
        assert_eq!(
            rig.claude.calls().len(),
            1,
            "the killed call, and no second one"
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"],
            json!({ "state": "ruling", "id": 1 })
        );
    }

    #[test]
    fn a_foreign_folder_where_the_worktree_goes_fails_the_turn_before_any_call() {
        let (rig, runner) = with_issue_7("koji");
        let folder = rig.home.path().join("kelpie/wt/koji/7");
        fs::create_dir_all(&folder).unwrap();
        let Some(StepReport::Failed { question, .. }) = step(&runner).unwrap() else {
            panic!("the turn ran in a folder kelpie did not make");
        };
        assert!(
            question.contains("is not this work item's worktree"),
            "{question}"
        );
        assert_eq!(rig.claude.calls(), []);

        // A yes retries the step that failed: the first turn, from the issue.
        fs::remove_dir_all(&folder).unwrap();
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
        step(&runner).unwrap();
        let [call] = rig.claude.calls().try_into().unwrap();
        assert!(
            matches!(call.session, Session::New(_)),
            "{:?}",
            call.session
        );
        assert_eq!(
            call.prompt,
            "Your work item is issue #7: Title of #7\n\nBody of #7.\n"
        );
    }

    #[test]
    fn a_branch_left_behind_stays_a_refusal_for_a_new_work_item_even_when_it_matches_main() {
        let (rig, runner) = with_issue_7("golbat");
        git(&rig.repo(), &["branch", "kelpie/7", "origin/main"]);
        let Some(StepReport::Failed { question, .. }) = step(&runner).unwrap() else {
            panic!("a fresh work item took a branch that was not its own");
        };
        assert!(
            question.contains("branch kelpie/7 already exists without its worktree"),
            "{question}"
        );
        assert_eq!(rig.claude.calls(), []);
    }
}
