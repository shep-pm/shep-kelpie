//! The job a timeout stopped, found in a run's job listing
//!
//! A job stopped by its `timeout-minutes` ends `cancelled`, and `gh run view
//! --log-failed` answers nothing for its run. The listing names the job and
//! the step it was in, and `gh run view --job` reads that job's log.

use serde::Deserialize;

use super::super::unreadable;
use crate::ports::ForgeError;

/// The job of a run that ended `cancelled`, and the step it was cancelled in
#[derive(Debug, PartialEq, Eq)]
pub(super) struct CancelledJob {
    /// The id `gh run view --job` takes
    pub(super) id: u64,
    name: String,
    step: String,
    /// Seconds from the job's start to its finish, where the listing gave both
    elapsed: Option<i64>,
}

impl CancelledJob {
    /// The line that tells a reader the job ran out of time and no test failed
    pub(super) fn note(&self) -> String {
        let after = self.elapsed.map_or(String::new(), |secs| match secs {
            0..=59 => format!(" after {secs}s"),
            _ => format!(" after {}m{:02}s", secs / 60, secs % 60),
        });
        format!(
            "note: job `{}` was cancelled in step `{}`{after}: its `timeout-minutes` ended it, \
             so this is a slow job and not a failing test",
            self.name, self.step
        )
    }
}

/// The first job of `listing` (`gh run view <id> --json jobs`) that ended
/// `cancelled` in a step that did too, or none
pub(super) fn cancelled_job(listing: &[u8]) -> Result<Option<CancelledJob>, ForgeError> {
    #[derive(Deserialize)]
    struct Listing {
        jobs: Vec<Job>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Job {
        database_id: u64,
        name: String,
        conclusion: String,
        started_at: Option<String>,
        completed_at: Option<String>,
        steps: Vec<Step>,
    }
    #[derive(Deserialize)]
    struct Step {
        name: String,
        conclusion: String,
    }
    let listing: Listing = serde_json::from_slice(listing).map_err(|_| unreadable(listing))?;
    Ok(listing.jobs.into_iter().find_map(|job| {
        if job.conclusion != "cancelled" {
            return None;
        }
        let step = job
            .steps
            .into_iter()
            .find(|s| s.conclusion == "cancelled")?;
        let stamp = |at: Option<String>| at?.parse::<jiff::Timestamp>().ok();
        let elapsed = match (stamp(job.started_at), stamp(job.completed_at)) {
            (Some(start), Some(end)) => Some(end.as_second() - start.as_second()),
            _ => None,
        };
        Some(CancelledJob {
            id: job.database_id,
            name: job.name,
            step: step.name,
            elapsed,
        })
    }))
}
