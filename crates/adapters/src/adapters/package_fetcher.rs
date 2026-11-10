//! A worker's pinned crates, fetched on the host with `cargo fetch`
//!
//! The fetch runs outside any sandbox, so it may write the shared cargo cache a worker's own
//! sandbox may not. It runs from a folder no worker writes, since cargo and rustup read config
//! and toolchain files from the folder they run in and run what those name.

use std::ffi::OsString;

use super::process::{Processes, RunError};
use crate::ports::{CargoFetch, FetchOutcome, PackageFetcher, tail};

/// Runs `cargo fetch` on the host, stopped with the runner
#[derive(Debug, Clone)]
pub struct HostFetch {
    processes: Processes,
    program: OsString,
}

impl super::Base {
    /// A fetcher that runs on the host, and is stopped with this
    pub fn package_fetcher(&self) -> HostFetch {
        HostFetch {
            processes: self.processes.clone(),
            program: OsString::from("cargo"),
        }
    }
}

impl PackageFetcher for HostFetch {
    fn cargo_fetch(&self, run: &CargoFetch) -> FetchOutcome {
        // With the runner's own environment: the fetch is the maintainer's own cargo.
        let mut command = crate::spawn::command(&self.program);
        command
            .args(["fetch", "--locked", "--manifest-path"])
            .arg(&run.manifest)
            .current_dir(&run.cwd);
        match self.processes.output_within(&mut command, run.ceiling) {
            Ok(output) if output.status.success() => FetchOutcome::Fetched,
            Ok(output) => FetchOutcome::Failed {
                output: tail(&[output.stdout, output.stderr].concat()),
            },
            Err(RunError::TimedOut) => FetchOutcome::TimedOut,
            Err(RunError::Stopped) => FetchOutcome::NotRun {
                reason: "the runner is stopping".to_owned(),
            },
            Err(RunError::Io(e)) => FetchOutcome::NotRun {
                reason: format!("cannot start cargo: {e}"),
            },
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::*;
    use crate::ports::OUTPUT_KEPT;

    // A fetcher whose `cargo` is the script `body`, in a folder of its own, and the folder the
    // fetch is given to run in
    fn fetcher(body: &str) -> (HostFetch, tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("cargo");
        crate::test::write_script(&program, &format!("#!/bin/sh\n{body}\n"));
        let cwd = dir.path().join("build");
        std::fs::create_dir(&cwd).unwrap();
        let fetcher = HostFetch {
            processes: Processes::default(),
            program: program.into_os_string(),
        };
        (fetcher, dir, cwd)
    }

    fn fetch(cwd: &Path, ceiling: Duration) -> CargoFetch {
        CargoFetch {
            manifest: PathBuf::from("/work/7/web/Cargo.toml"),
            cwd: cwd.to_owned(),
            ceiling,
        }
    }

    #[test]
    fn a_cargo_fetch_runs_locked_against_the_manifest_from_the_folder_it_is_given() {
        let (fetcher, dir, cwd) =
            fetcher("printf '%s\\n' \"$@\" > \"$0.args\"; pwd -P > \"$0.cwd\"");

        let outcome = fetcher.cargo_fetch(&fetch(&cwd, Duration::from_secs(30)));

        assert_eq!(outcome, FetchOutcome::Fetched);
        let args = std::fs::read_to_string(dir.path().join("cargo.args")).unwrap();
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "fetch",
                "--locked",
                "--manifest-path",
                "/work/7/web/Cargo.toml"
            ]
        );
        let ran_in = std::fs::read_to_string(dir.path().join("cargo.cwd")).unwrap();
        assert_eq!(Path::new(ran_in.trim()), cwd.canonicalize().unwrap());
    }

    #[test]
    fn a_failed_fetch_returns_the_tail_of_its_output() {
        let (fetcher, _dir, cwd) = fetcher(
            "i=0; while [ $i -lt 600 ]; do echo \"Downloading crate $i\"; i=$((i+1)); done; \
             echo 'error: failed to download from the registry' >&2; exit 101",
        );

        let outcome = fetcher.cargo_fetch(&fetch(&cwd, Duration::from_secs(30)));

        let FetchOutcome::Failed { output } = outcome else {
            panic!("{outcome:?}");
        };
        assert!(output.len() <= OUTPUT_KEPT, "{}", output.len());
        assert!(
            output
                .trim_end()
                .ends_with("error: failed to download from the registry"),
            "{output}"
        );
        assert!(!output.contains("Downloading crate 0\n"), "{output}");
    }

    #[test]
    fn a_fetch_past_its_ceiling_is_ended_and_reported_timed_out() {
        let (fetcher, dir, cwd) = fetcher("sleep 30 & echo $! > \"$0.child\"; wait");

        let outcome = fetcher.cargo_fetch(&fetch(&cwd, Duration::from_millis(300)));

        assert_eq!(outcome, FetchOutcome::TimedOut);
        // The whole tree ends with it: the `sleep` the script started is gone too.
        let child = std::fs::read_to_string(dir.path().join("cargo.child")).unwrap();
        let alive = std::process::Command::new("kill")
            .args(["-0", child.trim()])
            .output()
            .unwrap()
            .status
            .success();
        assert!(!alive, "the fetch's child outlived it");
    }

    #[test]
    fn a_missing_cargo_is_not_run() {
        let (mut fetcher, dir, cwd) = fetcher("exit 0");
        fetcher.program = dir.path().join("no-such-cargo").into_os_string();

        let outcome = fetcher.cargo_fetch(&fetch(&cwd, Duration::from_secs(30)));

        let FetchOutcome::NotRun { reason } = outcome else {
            panic!("{outcome:?}");
        };
        assert!(reason.contains("cannot start cargo"), "{reason}");
    }
}
