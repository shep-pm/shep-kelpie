//! The worker's turns
//!
//! A turn runs in three steps, so the runner still answers triggers while
//! Claude works: begin (prepare the worktree and profile, mark the turn
//! running, save), the call, and end (record the call, save). A turn still
//! marked running when the runner starts was cut short, and its session is
//! resumed. A session cut short before it wrote anything starts over.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use super::Runner;
use super::trigger::lock;
use crate::board::{Skip, WorkerModel};
use crate::ports::{
    ClaudeCall, ClaudeError, ClaudeReply, Cost, Issue, Role, Session, SessionId, Usage,
};
use crate::profile::{INSTRUCTIONS, WorkerProfile};
use crate::state::{RunState, StateError};
use crate::work_item::{CallRecord, Turn, WorkItem};
use crate::worktree;

/// The prompt for a turn resumed after the runner restarted
const CONTINUE: &str = "Kelpie restarted while your last turn was running. \
                        Carry on with the work item from where you left off.";

/// What one step of the runner did, for its log
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "step", rename_all = "kebab-case")]
pub enum StepReport {
    /// The board's oldest free issue became the work item in flight
    Dispatched {
        /// The work item's issue
        issue: u64,
        /// The model and effort its worker runs on
        worker: WorkerModel,
        /// Older ready issues the board passed over, and why
        skipped: Vec<Skip>,
    },
    /// Nothing was dispatched: the board could not be read, or the issue it
    /// picked could not be taken
    BoardFailed {
        /// Why
        reason: String,
    },
    /// A turn ended and its call was recorded
    Ended {
        /// The work item's issue
        issue: u64,
        /// The worker's session
        session: SessionId,
        /// What the call used
        usage: Usage,
        /// What the call cost, in US dollars
        cost_usd: f64,
        /// What the work item has cost so far, in US dollars
        work_item_cost_usd: f64,
        /// The worker's draft pull request, once it has opened one
        pull_request: Option<u64>,
    },
    /// A turn could not run
    Failed {
        /// The work item's issue
        issue: u64,
        /// Why
        reason: String,
    },
}

pub(super) enum Begin {
    Idle,
    Report(StepReport),
    Call(ClaudeCall),
}

/// Runs the worker's next turn, if one is due and the project is running
///
/// Returns what happened, or `None` when there was nothing to do.
///
/// # Errors
///
/// [`StateError`] when the turn's start or end cannot be saved.
pub fn step(runner: &Mutex<Runner>) -> Result<Option<StepReport>, StateError> {
    let claude = Arc::clone(&lock(runner).ports.claude);
    let mut start_over = false;
    loop {
        let call = match lock(runner).begin_turn(start_over)? {
            Begin::Idle => return Ok(None),
            Begin::Report(report) => return Ok(Some(report)),
            Begin::Call(call) => call,
        };
        let result = claude.run(&call);
        if !start_over && matches!(result, Err(ClaudeError::NoSession(_))) {
            start_over = true;
            continue;
        }
        return lock(runner).end_turn(result);
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
        let session = match &item.turn {
            Turn::Due => Session::New(item.session.clone()),
            Turn::Running { .. } if start_over => Session::New(item.session.clone()),
            Turn::Running { .. } => Session::Resume(item.session.clone()),
            Turn::Ended { .. } | Turn::Failed { .. } => return Ok(Begin::Idle),
        };
        let prepared = self.prepare(item, session);
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let item = next
            .work_item
            .as_mut()
            .expect("the work item checked above");
        let begin = match prepared {
            Ok(call) => {
                item.turn = Turn::Running { since: now };
                Begin::Call(call)
            }
            Err(reason) => {
                item.turn = Turn::Failed {
                    at: now,
                    reason: reason.clone(),
                };
                Begin::Report(StepReport::Failed {
                    issue: item.issue,
                    reason,
                })
            }
        };
        self.save(next)?;
        Ok(begin)
    }

    // Everything the worker needs on disk before it starts: its worktree, its
    // build folder, its settings file and kelpie's instructions.
    fn prepare(&self, item: &WorkItem, session: Session) -> Result<ClaudeCall, String> {
        let dirs = worktree::prepare(
            &self.settings.repo,
            &item.worktree,
            &item.branch,
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
        write(folder, &instructions, INSTRUCTIONS)?;
        let prompt = match &session {
            Session::New(_) => {
                let issue = self
                    .ports
                    .forge
                    .issue(&self.settings.forge, item.issue)
                    .map_err(|e| format!("cannot read issue #{}: {e}", item.issue))?;
                first_prompt(item.issue, &issue)
            }
            Session::Resume(_) => CONTINUE.to_owned(),
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
        let mut next = self.state.clone();
        let Some(item) = next.work_item.as_mut() else {
            return Ok(None);
        };
        let report = match result {
            Ok(reply) => {
                if item.pull_request.is_none() {
                    item.pull_request = self.pull_request_from(&item.branch);
                }
                let before = item.session_cost(&item.session);
                let cost = Cost(reply.session_cost.0.saturating_sub(before.0));
                item.calls.push(CallRecord {
                    role: Role::Worker,
                    at: now,
                    session: item.session.clone(),
                    usage: reply.usage,
                    cost,
                    session_cost: reply.session_cost,
                });
                item.turn = Turn::Ended { at: now };
                StepReport::Ended {
                    issue: item.issue,
                    session: item.session.clone(),
                    usage: reply.usage,
                    cost_usd: cost.usd(),
                    work_item_cost_usd: item.cost().usd(),
                    pull_request: item.pull_request,
                }
            }
            Err(e) => {
                let reason = e.to_string();
                item.turn = Turn::Failed {
                    at: now,
                    reason: reason.clone(),
                };
                StepReport::Failed {
                    issue: item.issue,
                    reason,
                }
            }
        };
        self.save(next)?;
        Ok(Some(report))
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

fn write(folder: &Path, file: &Path, text: &str) -> Result<(), String> {
    fs::create_dir_all(folder)
        .and_then(|()| fs::write(file, text))
        .map_err(|e| format!("cannot write {}: {}", file.display(), e.kind()))
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use serde_json::json;

    use super::*;
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
    fn a_failed_turn_is_shown_and_not_retried() {
        let (rig, runner) = with_issue_7("zeus");
        rig.claude
            .script([Scripted::Fail(ClaudeError::Failed("overloaded".into()))]);
        let report = step(&runner).unwrap().unwrap();
        let reason = "claude failed: overloaded".to_owned();
        assert_eq!(
            report,
            StepReport::Failed {
                issue: 7,
                reason: reason.clone()
            }
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["turn"],
            json!({ "state": "failed", "at": Rig::EPOCH, "reason": reason })
        );
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.claude.calls().len(), 1);
    }

    #[test]
    fn a_foreign_folder_where_the_worktree_goes_fails_the_turn_before_any_call() {
        let (rig, runner) = with_issue_7("koji");
        fs::create_dir_all(rig.home.path().join("kelpie/wt/koji/7")).unwrap();
        let Some(StepReport::Failed { reason, .. }) = step(&runner).unwrap() else {
            panic!("the turn ran in a folder kelpie did not make");
        };
        assert!(
            reason.ends_with("is not this work item's worktree"),
            "{reason}"
        );
        assert_eq!(rig.claude.calls(), []);
    }
}
