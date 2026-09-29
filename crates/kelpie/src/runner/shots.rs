//! Kelpie's shots of a work item with a launch file
//!
//! A run is taken outside the runner's lock, like a review call, of the head
//! on `origin`: before each Claude review round, and before the merge ruling.
//! The merge ruling's run goes on the pull request's shots comment first.
//! A run that fails is reported and kept, so it never holds a gate.

use std::path::PathBuf;

use super::Runner;
use super::gate::short;
use super::report::{Begin, StepReport};
use crate::ports::{ForgeError, Timestamp};
use crate::preview;
use crate::settings::NonBlank;
use crate::shots::{SERVER_PID, ShotsJob, ShotsRecord, ShotsRun, named_routes, publish, routes};
use crate::state::StateError;
use crate::work_item::{ReviewCallKind, ReviewCallState, WorkItem};

#[cfg(test)]
mod tests;

/// Seconds before a shots post that failed is tried again
const SHOTS_RETRY: u64 = 60;

/// What `gh api` says of a comment that no longer exists
const GONE: &str = "HTTP 404";

/// What a Claude review round has to go on
pub(super) enum RoundShots {
    /// A run of the head is due first
    Take(Begin),
    /// Kelpie's run of the head, or none without a launch file
    Ready(Option<ShotsRun>),
}

impl Runner {
    /// Whether the project's `main` has a launch file, with a work item in flight
    pub(super) fn preview_on(&self) -> bool {
        self.current().is_some() && preview::enabled(&self.settings.repo)
    }

    /// The job for a run into `out`, on the settings' routes and the worker's
    pub(super) fn shots_job(&self, item: &WorkItem, out: PathBuf) -> ShotsJob {
        let preview = &self.settings.preview;
        let config = preview.configuration.as_ref().map(NonBlank::as_str);
        let named = named_routes(&self.paths.shots(item.issue));
        let env = self
            .settings
            .worker
            .build_env
            .iter()
            .map(|(name, dir)| (name.as_str().to_owned(), item.build.join(dir.as_path())))
            .collect();
        ShotsJob {
            worktree: item.worktree.clone(),
            build: item.build.clone(),
            out,
            launch: preview::launch(&self.settings.repo, config).map_err(|e| e.to_string()),
            routes: routes(&preview.routes, &named),
            domains: preview
                .domains
                .iter()
                .map(NonBlank::as_str)
                .map(str::to_owned)
                .collect(),
            env,
            server_pid: self.paths.shots(item.issue).join(SERVER_PID),
            shep_home: self.paths.shep_home.clone(),
        }
    }

    /// A run of `head`, unless kelpie already has one
    pub(super) fn shots_due(&mut self, head: &str) -> Result<Option<Begin>, StateError> {
        let item = self.current().expect("shots are a work item's");
        if item.shots.as_ref().is_some_and(|r| r.head == head) {
            return Ok(None);
        }
        let out = self.paths.shots(item.issue).join(short(head));
        let job = self.shots_job(item, out);
        self.mark_review_call_running(ReviewCallKind::Shots)?;
        Ok(Some(Begin::Shots(Box::new(job), head.to_owned())))
    }

    /// The shots a Claude review round gets: a run of the head on `origin`
    pub(super) fn round_shots(&mut self) -> Result<RoundShots, StateError> {
        if !self.preview_on() {
            return Ok(RoundShots::Ready(None));
        }
        let head = match self.origin_head() {
            Ok(head) => head,
            // The round goes on without shots, and the reviewer is told why.
            Err(reason) => {
                let why = format!("kelpie could not read the head to take shots of: {reason}");
                return Ok(RoundShots::Ready(Some(ShotsRun::failed(why))));
            }
        };
        if let Some(begin) = self.shots_due(&head)? {
            return Ok(RoundShots::Take(begin));
        }
        let item = self.current().expect("checked above");
        Ok(RoundShots::Ready(
            item.shots.as_ref().map(|r| r.run.clone()),
        ))
    }

    /// Keeps a run of `head` on the work item
    pub(super) fn end_shots(
        &mut self,
        head: String,
        run: ShotsRun,
    ) -> Result<Option<StepReport>, StateError> {
        let mut next = self.state.clone();
        let Some(item) = self.current_in(&mut next) else {
            return Ok(None);
        };
        item.review_call = ReviewCallState::Idle;
        let report = StepReport::Shots {
            issue: item.issue,
            head: head.clone(),
            shots: run.files().count(),
            problems: run.all_problems(),
        };
        item.shots = Some(ShotsRecord {
            head,
            run,
            posted: false,
            retry_at: None,
        });
        self.save(next)?;
        Ok(Some(report))
    }

    /// Before the merge ruling on `head`: its run, then its comment
    pub(super) fn shots_before_merge(
        &mut self,
        number: u64,
        head: &str,
    ) -> Result<Option<Begin>, StateError> {
        if !self.preview_on() {
            return Ok(None);
        }
        if let Some(begin) = self.shots_due(head)? {
            return Ok(Some(begin));
        }
        let item = self.current().expect("checked above");
        let record = item.shots.as_ref().expect("a run of this head");
        // A post that failed does not hold the ruling; `retry_shots` tries again.
        if record.posted || record.retry_at.is_some() {
            return Ok(None);
        }
        self.post_shots(number)
            .map(|report| Some(Begin::Report(report)))
    }

    /// While parked on a ruling: posts shots whose post failed, once it is due
    pub(super) fn retry_shots(&mut self) -> Result<Begin, StateError> {
        let item = self.current().expect("a ruling is a work item's");
        let now = self.ports.clock.now();
        let due = item
            .shots
            .as_ref()
            .is_some_and(|r| !r.posted && r.retry_at.is_some_and(|at| at <= now));
        match item.pull_request {
            Some(number) if due => self.post_shots(number).map(Begin::Report),
            _ => Ok(Begin::Idle),
        }
    }

    // Pushes the work item's shots and puts them on the pull request's one
    // shots comment, recording whether that worked.
    fn post_shots(&mut self, number: u64) -> Result<StepReport, StateError> {
        let item = self.current().expect("shots are a work item's");
        let record = item.shots.clone().expect("a run to post");
        let head = record.head.as_str();
        let (issue, comment) = (item.issue, item.shots_comment);
        let mut failures = Vec::new();
        let commit = if record.run.files().next().is_some() {
            let message = format!("Shots of {} for #{number}", short(head));
            let pushed = publish::push(
                &self.settings.repo,
                &publish::branch(number),
                &record.run,
                &message,
            );
            pushed.map_err(|e| failures.push(e)).ok()
        } else {
            None
        };
        let forge = &self.settings.forge;
        let body = publish::comment(forge, number, head, commit.as_deref(), &record.run);
        // Only a comment someone deleted gets a new one: any other failure
        // would leave two shots comments on the pull request.
        let posted = match comment {
            Some(id) => match self.ports.forge.edit_comment(forge, id, &body) {
                Err(ForgeError::Failed(e)) if e.contains(GONE) => {
                    self.ports.forge.post_comment(forge, number, &body)
                }
                edited => edited.map(|()| id),
            },
            None => self.ports.forge.post_comment(forge, number, &body),
        };
        let posted = posted.map_err(|e| failures.push(e.to_string())).ok();
        let done = posted.is_some() && failures.is_empty();
        let retry_at = Timestamp(self.ports.clock.now().0 + SHOTS_RETRY);
        self.update(|item| {
            if let Some(shots) = item.shots.as_mut() {
                shots.posted = done;
                shots.retry_at = (!done).then_some(retry_at);
            }
            item.shots_comment = posted.or(item.shots_comment);
        })?;
        Ok(match posted {
            Some(comment) if done => StepReport::ShotsPosted {
                issue,
                pull_request: number,
                head: head.to_owned(),
                comment,
            },
            _ => StepReport::ShotsNotPosted {
                issue,
                pull_request: number,
                reason: failures.join("; "),
            },
        })
    }
}
