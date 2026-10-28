//! A project's test command, run in the worker's sandbox under the `cargo-test` lease
//!
//! A reviewer wrote the test and the worker wrote the code under it, so the
//! command runs inside the same sandbox the agents use, held to the worker's
//! fence and never to the maintainer's reach. The run takes its place in the
//! dog's `cargo-test` queue first, from this process and through the door, and
//! gives it back when the command ends. The ceiling counts from the grant. A run with no door
//! takes no place in the queue.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::harness::fence_policy;
use super::process::{Processes, RunError};
use crate::lease::counted::CARGO_TEST;
use crate::lease::door::{Answer, Ask, Visit};
use crate::ports::{Sandbox, TestOutcome, TestRun, TestRunner, tail};

/// How long a dog has to answer an ask; a live one answers at once
const ANSWER_WITHIN: Duration = Duration::from_secs(5);

/// What a shell exits with when it cannot find the command it was told to run
const NOT_FOUND: i32 = 127;

/// Runs a test command inside the sandbox, with the lease held
#[derive(Debug, Clone)]
pub struct SandboxedTests {
    processes: Processes,
    sandbox: Arc<dyn Sandbox>,
}

impl super::Base {
    /// A test runner in the sandbox this runs its calls in, and stopped with it
    pub fn test_runner(&self) -> SandboxedTests {
        SandboxedTests {
            processes: self.processes.clone(),
            sandbox: Arc::clone(&self.sandbox),
        }
    }
}

impl TestRunner for SandboxedTests {
    fn run(&self, run: &TestRun) -> TestOutcome {
        match self.try_run(run) {
            Ok(outcome) => outcome,
            Err(reason) => TestOutcome::NotRun { reason },
        }
    }
}

impl SandboxedTests {
    fn try_run(&self, run: &TestRun) -> Result<TestOutcome, String> {
        let Some(fence) = run.reach.fence.as_deref() else {
            return Err("the run has no worker fence to be held to".to_owned());
        };
        let mut policy = fence_policy(fence);
        policy.no_keychain = run.reach.block_keychain;
        // Its stderr joins its stdout where it was written, so the tail reads in order.
        let mut command = crate::spawn::command(crate::spawn::sh());
        command
            .arg("-c")
            .arg(format!("exec 2>&1; {}", run.command))
            .current_dir(&run.cwd)
            .envs(&fence.env);
        let mut wrapped = self
            .sandbox
            .wrap(&policy, &run.settings, &command)
            .map_err(|e| e.to_string())?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("cannot start the async runtime: {e}"))?;
        // A run with no door holds no lease, as an install does.
        let visit = match &run.door {
            Some(door) => {
                let ask = Ask::new(CARGO_TEST, std::process::id(), run.command.clone());
                Some(runtime.block_on(granted(door, &ask))?)
            }
            None => None,
        };
        let ran = self.processes.output_within(&mut wrapped, run.ceiling);
        if let Some(visit) = visit {
            runtime.block_on(visit.give_back());
        }

        match ran {
            Ok(output) if output.status.success() => Ok(TestOutcome::Passed),
            Ok(output) => {
                let written = [output.stdout, output.stderr].concat();
                match output.status.code() {
                    Some(NOT_FOUND) => Ok(TestOutcome::NotRun {
                        reason: format!("the command could not start: {}", tail(&written)),
                    }),
                    _ => Ok(TestOutcome::Failed {
                        output: tail(&written),
                    }),
                }
            }
            Err(RunError::TimedOut) => Ok(TestOutcome::TimedOut),
            Err(RunError::Stopped) => Err("the runner is stopping".to_owned()),
            Err(RunError::Io(e)) => Err(format!("cannot start the command: {e}")),
        }
    }
}

// Knocks and waits in the queue for the grant, however long that takes.
async fn granted(door: &Path, ask: &Ask) -> Result<Visit, String> {
    let mut visit = Visit::knock(door, ask).await?;
    let mut first = true;
    loop {
        let answer = match first {
            true => tokio::time::timeout(ANSWER_WITHIN, visit.answer())
                .await
                .map_err(|_| "the dog took the ask and never answered".to_owned())?,
            false => visit.answer().await,
        };
        first = false;
        match answer? {
            Some(Answer::Granted) => return Ok(visit),
            Some(Answer::Queued(_)) => {}
            Some(Answer::Error(why)) => return Err(format!("the dog refused: {why}")),
            // A lease ask draws neither: they belong to an e2e run.
            Some(Answer::Output(_) | Answer::Done(_)) => {
                return Err(format!(
                    "the dog answered {CARGO_TEST} with an e2e run's line"
                ));
            }
            Some(Answer::Go | Answer::Wait(_) | Answer::Held(_)) => {
                return Err(format!(
                    "the dog answered {CARGO_TEST} with a forge pace's answer"
                ));
            }
            Some(Answer::Picks(_)) => {
                return Err(format!(
                    "the dog answered {CARGO_TEST} with the agent board's picks"
                ));
            }
            None => {
                return Err(format!(
                    "the dog closed the door before granting {CARGO_TEST}"
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests;
