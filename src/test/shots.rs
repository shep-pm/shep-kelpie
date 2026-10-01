//! The rig's shots: every job recorded, each run scripted or planned
//!
//! A run it plans writes a small file at each shot's path, so a publish has
//! real bytes to commit.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::Hold;
use crate::ports::Shots;
use crate::shots::{ShotsJob, ShotsRun, plan};

/// What the stand-in answers for its next run
#[derive(Debug, Clone)]
pub(crate) enum ScriptedShots {
    /// Every shot taken, each with these problems on its page
    Problems(Vec<String>),
    /// Every shot taken, each of a page the dev server answered with this status
    Status(u16),
    /// Nothing captured, for this reason
    Fail(&'static str),
    /// Blocks until the test releases it, then takes every shot cleanly
    Hold(Hold),
}

/// Takes every shot cleanly once its script runs out
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeShots {
    jobs: Arc<Mutex<Vec<ShotsJob>>>,
    stopped: Arc<Mutex<Vec<std::path::PathBuf>>>,
    swept: Arc<Mutex<Vec<Vec<std::path::PathBuf>>>>,
    script: Arc<Mutex<VecDeque<ScriptedShots>>>,
}

impl FakeShots {
    /// Every job asked of it, in order
    pub(crate) fn jobs(&self) -> Vec<ShotsJob> {
        self.jobs.lock().unwrap().clone()
    }

    /// Every recorded server it was asked to stop, by its file, in order
    pub(crate) fn stopped(&self) -> Vec<std::path::PathBuf> {
        self.stopped.lock().unwrap().clone()
    }

    /// The folders each sweep for orphaned servers covered, in order
    pub(crate) fn swept(&self) -> Vec<Vec<std::path::PathBuf>> {
        self.swept.lock().unwrap().clone()
    }

    /// Queues answers for its next runs, oldest first
    pub(crate) fn script(&self, runs: impl IntoIterator<Item = ScriptedShots>) {
        self.script.lock().unwrap().extend(runs);
    }
}

impl Shots for FakeShots {
    fn stop_left(&self, server_pid: &std::path::Path) {
        self.stopped.lock().unwrap().push(server_pid.to_owned());
    }

    fn stop_orphans(&self, folders: &[std::path::PathBuf]) {
        self.swept.lock().unwrap().push(folders.to_vec());
    }

    fn take(&self, job: &ShotsJob) -> ShotsRun {
        self.jobs.lock().unwrap().push(job.clone());
        let (status, problems) = match self.script.lock().unwrap().pop_front() {
            Some(ScriptedShots::Fail(reason)) => return ShotsRun::failed(reason),
            Some(ScriptedShots::Problems(problems)) => (200, problems),
            Some(ScriptedShots::Status(status)) => (status, Vec::new()),
            Some(ScriptedShots::Hold(hold)) => {
                hold.block();
                (200, Vec::new())
            }
            None => (200, Vec::new()),
        };
        std::fs::create_dir_all(&job.out).unwrap();
        let mut shots = plan(&job.routes, &job.out);
        for shot in &mut shots {
            let file = shot.file.as_ref().expect("a planned shot has a file");
            std::fs::write(file, format!("png of {}\n", file.display())).unwrap();
            shot.status = Some(status);
            shot.problems.clone_from(&problems);
        }
        ShotsRun {
            shots,
            ..ShotsRun::default()
        }
    }
}
