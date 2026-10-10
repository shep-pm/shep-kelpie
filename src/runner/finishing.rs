//! Finishing: the open work items run to their end, the board picks
//! nothing new, then the runner stops its own sheep
//!
//! `finish` saves `finishing` in the state file, so a runner restarted
//! while finishing comes back finishing. Each open item goes on as ever,
//! through its turns, review, CI, rulings and merge, and the project
//! manager is woken for anything but a pick. The pass that finds no work
//! item open and no notice left to post clears `finishing`, logs that the
//! runner finished, posts it where a webhook is set, and leaves the stop
//! for the sheep to take, which stops itself as `pause` does. Until that
//! stop the board stays shut, in memory only, so a runner that starts again
//! picks as ever. `start` cancels it either way, except while the stop is
//! under way. A `pause` clears the saved `finishing` just before its stop,
//! so a runner it paused comes back picking.

use std::fmt;

use serde::Serialize;

use super::Runner;
use super::report::StepReport;
use crate::ports::Alert;
use crate::state::StateError;

/// What `status` shows of a runner that does not pick
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Run {
    /// `finish` holds the board back until the open work items end
    Finishing,
    /// The open work items ended, and the runner waits for its sheep to stop
    Finished,
}

/// Why `start` was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// The runner finished and its sheep's stop is under way
    Stopping,
    /// The change could not be saved
    State(StateError),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopping => f.write_str(
                "the runner finished and is stopping its sheep: `shep kelpie start` runs it \
                 again once it has stopped",
            ),
            Self::State(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for StartError {}

/// How `adopt` and `rework` are refused while the board is shut
pub(super) const NOTHING_NEW: &str = "the runner is finishing, so it takes on no pull request: `shep kelpie start` lets it pick \
     again";

/// What finishing keeps in memory
#[derive(Debug, Default)]
pub(super) struct Finish {
    // Finishing for this process only, once a pause cleared the saved flag
    held: bool,
    // The last open item ended, so the board stays shut until the stop
    finished: bool,
    // The sheep is to stop, until it takes the stop
    stop_due: bool,
    // The sheep took the stop, and has not said it failed
    stopping: bool,
}

impl Runner {
    /// Holds the board back until the open work items end. A runner that
    /// finished and is still up, with no stop under way, is asked to stop again.
    ///
    /// # Errors
    ///
    /// [`StateError`] when `finishing` cannot be saved. Nothing changes then.
    pub(super) fn begin_finishing(&mut self) -> Result<(), StateError> {
        if self.finish.finished {
            self.finish.stop_due = !self.finish.stopping;
            return Ok(());
        }
        if !self.state.finishing {
            let mut next = self.state.clone();
            next.finishing = true;
            self.save(next)?;
        }
        self.finish.held = false;
        self.pm_forget_pick();
        self.board_changed();
        Ok(())
    }

    /// Lets the board pick again, whether finishing or finished
    ///
    /// # Errors
    ///
    /// [`StartError::Stopping`] while the sheep's stop is under way, or
    /// [`StartError::State`] when `finishing` cannot be cleared. Nothing
    /// changes then.
    pub(super) fn cancel_finishing(&mut self) -> Result<(), StartError> {
        if self.finish.stopping {
            return Err(StartError::Stopping);
        }
        if self.state.finishing {
            let mut next = self.state.clone();
            next.finishing = false;
            self.save(next).map_err(StartError::State)?;
        }
        self.finish = Finish::default();
        self.board_changed();
        Ok(())
    }

    /// Clears the saved `finishing` for a pause about to stop the runner,
    /// which goes on finishing until then
    ///
    /// # Errors
    ///
    /// [`StateError`] when `finishing` cannot be cleared. Nothing changes then.
    pub(super) fn pausing(&mut self) -> Result<(), StateError> {
        if !self.state.finishing {
            return Ok(());
        }
        let mut next = self.state.clone();
        next.finishing = false;
        self.save(next)?;
        self.finish.held = true;
        Ok(())
    }

    // Finishing, saved or for this process only
    fn finishing(&self) -> bool {
        self.state.finishing || self.finish.held
    }

    /// Whether the board may pick: not while finishing or finished
    pub(super) fn picks(&self) -> bool {
        !self.finishing() && !self.finish.finished
    }

    /// What `status` shows under `run`, while the runner does not pick
    pub(super) fn run_status(&self) -> Option<Run> {
        if self.finishing() {
            Some(Run::Finishing)
        } else if self.finish.finished {
            Some(Run::Finished)
        } else {
            None
        }
    }

    /// Whether the runner finished and its sheep is to stop, true once for
    /// each time it is due. Taking it puts the stop under way.
    pub fn take_stop(&mut self) -> bool {
        let due = std::mem::take(&mut self.finish.stop_due);
        self.finish.stopping |= due;
        due
    }

    /// Records that the sheep's stop failed for `why`, so `start` and
    /// `finish` are taken again, and logs it
    pub fn stop_failed(&mut self, why: &str) {
        self.finish.stopping = false;
        let project = self.project.as_str();
        self.notes.push(format!(
            "the runner finished and cannot stop its sheep: {why}: `shep kelpie finish -p \
             {project}` tries again, and `shep kelpie start -p {project}` picks again"
        ));
    }

    /// Ends finishing once no work item is open and no notice waits to be
    /// posted: the report says so, and the sheep's stop is due
    ///
    /// # Errors
    ///
    /// [`StateError`] when `finishing` cannot be cleared. Nothing changes then.
    pub(super) fn finished_now(&mut self) -> Result<Option<StepReport>, StateError> {
        let open = !self.state.work_items.is_empty() || !self.state.notices.is_empty();
        if !self.finishing() || open {
            return Ok(None);
        }
        if self.state.finishing {
            let mut next = self.state.clone();
            next.finishing = false;
            self.save(next)?;
        }
        self.finish = Finish {
            finished: true,
            stop_due: true,
            ..Finish::default()
        };
        self.board_changed();
        self.say_finished();
        Ok(Some(StepReport::RunFinished))
    }

    // Logs that the runner finished, and posts it once where a webhook is
    // set: the runner stops next, so nothing is left to retry it.
    fn say_finished(&mut self) {
        let project = self.project.as_str().to_owned();
        let text = format!(
            "{project}'s runner finished its open work items and picked nothing new, so it \
             stops: `shep kelpie start {project}` runs it again. Nothing to answer."
        );
        self.notes.push(text.clone());
        let Some(webhook) = self.webhook.clone() else {
            return;
        };
        let alert = Alert {
            title: format!("kelpie: {project} finished"),
            text,
            reply: None,
        };
        if let Err(e) = self.ports.alerts.post(&webhook, &alert) {
            self.notes
                .push(format!("cannot post that the runner finished: {e}"));
        }
    }
}

#[cfg(test)]
mod tests;
