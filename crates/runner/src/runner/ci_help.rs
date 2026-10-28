//! How a worker is told to read why CI failed
//!
//! A worker on a repo the App covers holds a token that pushes and does
//! nothing else, so `gh pr checks` and `gh run view` are refused it. Kelpie
//! reads the end of the failed runs' logs as the login `gh` acts as, and the
//! prompt carries them. Any other worker is pointed at the commands.

use super::report::Begin;
use super::{Runner, instructions};
use crate::ports::{FailedLogs, ForgeError};

/// What kelpie found in the failed runs' logs of a head
#[derive(Debug)]
pub(super) enum Logs {
    /// The end of each failed run's log, which is empty where kelpie found no failed
    /// run to read. `omitted` counts the failed runs whose logs kelpie did not read,
    /// because more failed than it reads
    Read(FailedLogs),
    /// The forge refused the read, with its reason
    Refused(ForgeError),
}

impl Runner {
    /// Reads the end of each failed run's log at `head`, as the login `gh` acts as.
    /// The same text goes to a red run's prompt and to what its failure counts as
    /// (`super::ci_failure`). The read is made with the runner's lock let go: where it is
    /// missing, the step's `Begin::Reads`.
    pub(super) fn read_logs(&self, head: &str, merge_group: bool) -> Result<Logs, Box<Begin>> {
        Ok(match self.failed_logs(head, merge_group)? {
            Ok(logs) => Logs::Read(logs),
            Err(e) => Logs::Refused(e),
        })
    }

    /// What a red run's prompt says about reading the failure
    pub(super) fn red_help(&self, number: u64, logs: &Logs) -> String {
        let help = match self.app_covers() {
            true => self.log_help(logs),
            false => format!(
                "`gh pr checks {number}` lists the checks, and `gh run view <run id> \
                 --log-failed` shows what failed."
            ),
        };
        match &self.settings.e2e {
            Some(e2e) => format!(
                "{help} {} runs the project's end-to-end tests on their host from your \
                 worktree, and leaves the results in `{}`. If it exits without results, say so in \
                 your final message and report no e2e result.",
                instructions::e2e_command(&self.kelpie),
                e2e.results.as_path().display()
            ),
            None => help,
        }
    }

    /// What a merge queue removal's prompt says about reading the failure. A queue
    /// removal is not a work item's failed CI, so it is not counted
    /// (`super::merge::queue`): its logs are read only to be told.
    pub(super) fn queue_help(&self, head: &str) -> Result<String, Box<Begin>> {
        Ok(match self.app_covers() {
            true => self.log_help(&self.read_logs(head, true)?),
            false => "`gh run list --event merge_group` lists the queue's runs, and `gh run \
                      view <run id> --log-failed` shows what failed."
                .to_owned(),
        })
    }

    // What a worker whose token cannot read CI is told about logs kelpie read
    fn log_help(&self, logs: &Logs) -> String {
        match logs {
            Logs::Read(logs) if logs.text.trim().is_empty() => "Your credential cannot read CI, \
                and kelpie found no failed run to read for you: reproduce the failing checks \
                with the repo's own commands."
                .to_owned(),
            Logs::Read(logs) => {
                let more = match logs.omitted {
                    0 => String::new(),
                    1 => " One more failed run's log is not shown here.".to_owned(),
                    n => format!(" {n} more failed runs' logs are not shown here."),
                };
                format!(
                    "Your credential cannot read CI, so kelpie read the end of each failed run's \
                     log for you:\n\n{}\n{more}",
                    logs.text
                )
            }
            Logs::Refused(e) => format!(
                "Your credential cannot read CI, and kelpie could not read it for you ({e}): \
                 reproduce the failing checks with the repo's own commands."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ports::Checks;
    use crate::test::Rig;

    // The prompt a red run sends the worker back with
    fn red_prompt(
        rig: &Rig,
        runner: &std::sync::Mutex<crate::runner::Runner>,
        head: &str,
    ) -> String {
        rig.forge
            .set_checks(head, Checks::Failed(vec!["test".to_owned()]));
        rig.verdict(runner);
        rig.claude.script([crate::test::Scripted::Say("fixed")]);
        crate::runner::step(runner).unwrap();
        rig.claude.calls().last().unwrap().prompt.clone()
    }

    #[test]
    fn on_a_covered_repo_the_prompt_carries_the_failed_logs_instead_of_commands_that_fail() {
        let (rig, runner, head) = Rig::with_pull_request_set("koji", Rig::with_app);
        rig.forge
            .set_ci_log("--- run 9\nerror[E0308]: mismatched types\n --> work.txt:1:1");

        let prompt = red_prompt(&rig, &runner, &head);

        assert!(
            prompt.contains("error[E0308]: mismatched types"),
            "{prompt}"
        );
        assert!(!prompt.contains("gh run view"), "{prompt}");
    }

    #[test]
    fn a_fix_prompt_carries_each_failed_runs_log_once() {
        let (rig, runner, head) = Rig::with_pull_request_set("koji", Rig::with_app);
        rig.forge.set_ci_log(
            "--- run 9\nerror[E0308]: mismatched types\n --> work.txt:1:1\n\
             --- run 8\nerror[E0599]: no method\n --> work.txt:2:1",
        );

        let prompt = red_prompt(&rig, &runner, &head);

        assert_eq!(prompt.matches("error[E0308]").count(), 1, "{prompt}");
        assert_eq!(prompt.matches("error[E0599]").count(), 1, "{prompt}");
    }

    #[test]
    fn a_fix_prompt_carries_a_timed_out_runs_tail_once() {
        let (rig, runner, head) = Rig::with_pull_request_set("koji", Rig::with_app);
        rig.forge.set_ci_log_timed_out(
            "--- run 9\nnote: job `test-macos` was cancelled in step `Run cargo test --locked` \
             after 20m27s: its `timeout-minutes` ended it, so this is a slow job and not a \
             failing test\ntest-macos\tUNKNOWN STEP\t##[error]The operation was canceled.",
            1,
        );

        let prompt = red_prompt(&rig, &runner, &head);

        assert_eq!(
            prompt.matches("its `timeout-minutes` ended it").count(),
            1,
            "{prompt}"
        );
        assert_eq!(
            prompt.matches("The operation was canceled.").count(),
            1,
            "{prompt}"
        );
        assert!(!prompt.contains("found no failed run"), "{prompt}");
    }

    const E2E: &str = "`/opt/kelpie/bin/kelpie lease e2e` runs the project's end-to-end tests";

    #[test]
    fn the_red_prompt_names_the_e2e_command_only_when_configured() {
        // Not covered by the App: the worker reads CI itself.
        let (rig, runner, head) = Rig::with_pull_request("koji");
        let prompt = red_prompt(&rig, &runner, &head);
        assert!(!prompt.contains("lease e2e"), "{prompt}");

        let (rig, runner, head) = Rig::with_pull_request_set("koji", Rig::e2e_on);
        let prompt = red_prompt(&rig, &runner, &head);
        assert!(prompt.contains("--log-failed"), "{prompt}");
        assert!(prompt.contains(E2E), "{prompt}");

        // Covered by the App: kelpie carries the logs.
        let (rig, runner, head) = Rig::with_pull_request_set("koji", Rig::with_app);
        rig.forge.set_ci_log("--- run 9\nerror[E0308] work.txt");
        let prompt = red_prompt(&rig, &runner, &head);
        assert!(!prompt.contains("lease e2e"), "{prompt}");

        let (rig, runner, head) = Rig::with_pull_request_set("koji", |rig| {
            rig.with_app();
            rig.e2e_on();
        });
        rig.forge.set_ci_log("--- run 9\nerror[E0308] work.txt");
        let prompt = red_prompt(&rig, &runner, &head);
        assert!(prompt.contains("error[E0308]"), "{prompt}");
        assert!(prompt.contains(E2E), "{prompt}");
    }

    #[test]
    fn with_no_app_the_prompt_points_at_the_commands() {
        let (rig, runner, head) = Rig::with_pull_request("koji");

        let prompt = red_prompt(&rig, &runner, &head);

        assert!(
            prompt.contains("`gh pr checks 71` lists the checks"),
            "{prompt}"
        );
    }
}
