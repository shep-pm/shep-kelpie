//! Whether a work item's failed CI is one its worker could have caught
//!
//! A worker runs the project's tests in its sandbox before it pushes. A red
//! run whose failing tests a worker could run is one it could have caught.
//! `super::ci_help` reads the tail of each failed run for them. Only an
//! entry of `docs/agents/sandbox-red-tests.md` that names a test whole makes
//! it sandbox-red. The list is of this crate's modules, so it applies only
//! to kelpie's own repo. A failure kelpie never saw, or cannot read as a
//! test, counts as runnable.

use std::collections::BTreeSet;

use super::Runner;
use super::ci_help::Logs;
use crate::metrics::CiTests;
use crate::ports::FailedLogs;
use crate::state::StateError;

/// The modules and test binaries `docs/agents/sandbox-red-tests.md` lists whole:
/// every test of one is a test a worker's sandbox cannot run. The entries the doc
/// marks `some:` are left out, since kelpie cannot tell which of their tests are
/// red. `the_doc_and_the_list_agree` fails when this list and the doc's whole
/// entries part ways, so a new red module cannot slip past the count. A module
/// covers its child modules, and a `tests/<name>.rs` entry covers every test of
/// that binary.
const SANDBOX_RED: [&str; 16] = [
    "adapters::curl",
    "adapters::dev_server",
    "adapters::gpu",
    "adapters::goose::gateway_tests",
    "adapters::local::endpoint",
    "adapters::openai_gateway",
    "adapters::paddock",
    "adapters::pi::gateway_tests",
    "doctor::gateways",
    "runner::attach",
    "runner::gpu_tests",
    "runner::pm::tests::attach",
    "runner::review::local",
    "runner::slots_tests",
    "sheep::handover::tests::goose",
    "sheep::handover::tests::pi",
];

/// The repo `SANDBOX_RED` and `docs/agents/sandbox-red-tests.md` are of: kelpie's
/// own. The list is of this crate's modules, so it says nothing about another
/// project's tests, and kelpie holds no list for one. On such a repo every failing
/// test counts as one its worker could have run.
const DESCRIBED_REPO: &str = "shep-pm/shep-kelpie";

/// The runner names a step's name must carry for kelpie to read it as a test run,
/// each as a whole word of the name. `gh` prints the name a workflow gave the step
/// before each line of its log, so `Run cargo test --locked`, `Run pytest`, `npm
/// test` and `Run go test` are read as tests, and `Run cargo doc --no-deps --locked`
/// or `Run scripts/check-private.sh` are not. A word that only ends in one, such as
/// the `latest` of `Install latest gh`, is not a runner's name. A test step named
/// none of these words is read as no test failure.
const TEST_STEPS: [&str; 9] = [
    "test", "tests", "pytest", "jest", "mocha", "rspec", "e2e", "nextest", "vitest",
];

/// Where a failing test ran, as cargo's own lines name it
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// The crate's own tests, each named by its module path
    Lib,
    /// One `tests/<name>.rs` binary, whose tests are named without the file
    Test(String),
}

/// One cargo run in a log: the tests it names as failing, how many it counts as
/// failed, and the target it ran them in. Cargo prints a run's names before its
/// `test result` line, and names the target either before the run, in its
/// `Running <path>` header, or after it, in the `to rerun pass` line.
#[derive(Debug, Default)]
struct Run {
    names: BTreeSet<String>,
    failed: usize,
    target: Option<Target>,
}

impl Run {
    // Whether this run names as many failures as it counts. One that does not had
    // names cut off, or its count line fell outside the tail, and kelpie cannot
    // say the names it holds are all of them.
    fn agrees(&self) -> bool {
        self.names.len() == self.failed
    }
}

/// Whether a work item's failed CI had tests its worker could have run itself.
/// `omitted` is how many failed runs kelpie did not read logs of, and `listed` is the
/// sandbox-red list of the repo the head is on, empty where kelpie holds none.
pub(super) fn failed_tests(logs: &str, omitted: usize, listed: &[&str]) -> CiTests {
    // Kelpie did not see the whole failure set, so it cannot say the failures were
    // ones no worker could have run
    if omitted > 0 {
        return CiTests::Runnable;
    }
    let log = read(logs);
    // A test failed in a runner kelpie cannot read: pytest, jest, go test, or a cargo
    // run whose shapes the tail cut away
    if log.unreadable {
        return CiTests::Runnable;
    }
    let failed = log
        .runs
        .iter()
        .filter(|run| run.failed > 0 || !run.names.is_empty())
        .collect::<Vec<_>>();
    // Nothing the log shows as a failed test: lint, docs, fmt or a build that never
    // got as far as running one
    if failed.is_empty() {
        return CiTests::NoTests;
    }
    if failed.iter().any(|run| !run.agrees()) {
        return CiTests::Runnable;
    }
    match failed.iter().all(|run| {
        run.names
            .iter()
            .all(|name| sandbox_red(name, &run.target, listed))
    }) {
        true => CiTests::SandboxOnly,
        false => CiTests::Runnable,
    }
}

/// What `logs` count a red head as. A head whose runs a timeout ended and whose logs
/// name no failing test is neither a test a worker could have run nor a run with
/// no test: it is slow. Where some failed runs were left unread, they may have named one.
pub(super) fn counted_as(logs: &FailedLogs, listed: &[&str]) -> CiTests {
    match failed_tests(&logs.text, logs.omitted, listed) {
        CiTests::NoTests if logs.timed_out > 0 => CiTests::TimedOut,
        tests => tests,
    }
}

// Whether `name` is one of the tests a worker's sandbox cannot run, given the
// target it ran in
fn sandbox_red(name: &str, target: &Option<Target>, listed: &[&str]) -> bool {
    match target {
        // An integration test binary is listed by its file, and its tests carry no
        // module path of their own to match
        Some(Target::Test(binary)) => listed.contains(&format!("tests/{binary}.rs").as_str()),
        // The crate's own tests, and any target kelpie could not name: cargo's name
        // for one is its module path, which the list covers from its module down
        Some(Target::Lib) | None => listed.iter().any(|module| {
            name == *module
                || name
                    .strip_prefix(*module)
                    .is_some_and(|rest| rest.starts_with("::"))
        }),
    }
}

/// What a log of failed runs shows
struct Log {
    /// Each cargo run the log holds, in the order cargo printed them
    runs: Vec<Run>,
    /// Whether a step whose name says it ran tests failed in output kelpie cannot
    /// read as cargo's
    unreadable: bool,
}

/// One step of a failed run, as the log's own job and step fields name it
#[derive(Debug, Default)]
struct Step {
    /// The step's name, which the workflow gave it. `None` for a line that carries
    /// none, such as the `--- run <id>` kelpie writes above each log
    name: Option<String>,
    /// Whether cargo's own test shapes appeared in it
    cargo: bool,
    /// Whether cargo or rustc said the build itself failed
    build: bool,
    /// Whether the runner marked it failed
    failed: bool,
}

impl Step {
    // A step that says it ran tests and failed in output kelpie cannot read as cargo's.
    // A step that failed on a build ran no test, whatever its name says.
    fn unreadable(&self) -> bool {
        self.failed && !self.cargo && !self.build && self.name.as_deref().is_some_and(names_tests)
    }
}

// Whether a step's name says it ran tests, on a whole word of it
fn names_tests(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.split(|c: char| !c.is_alphanumeric())
        .any(|word| TEST_STEPS.contains(&word))
}

// Every cargo run the log holds, in the order cargo printed them, and whether a step
// kelpie read as a test run failed in output it could not read
fn read(log: &str) -> Log {
    let mut runs: Vec<Run> = Vec::new();
    // The run whose tests cargo is still printing
    let mut open = Run::default();
    // Whether the line before was cargo's `failures:` heading, which the list of
    // names it ends a failing run with follows
    let mut listing = false;
    // The step the lines so far came from, and what kelpie saw of it
    let mut step = Step::default();
    let mut unreadable = false;
    for line in log.lines() {
        let (name, body) = step_and_text(line);
        if name != step.name.as_deref() {
            unreadable |= step.unreadable();
            step = Step {
                name: name.map(str::to_owned),
                ..Step::default()
            };
        }
        let text = plain(body.trim_end());
        if text.starts_with("##[error]") {
            step.failed = true;
            listing = false;
            continue;
        }
        if listing {
            match listed_name(&text) {
                Some(name) => {
                    open.names.insert(name.to_owned());
                    continue;
                }
                None => listing = false,
            }
        }
        if text == "failures:" {
            listing = true;
            step.cargo = true;
        } else if let Some(name) = block_name(&text) {
            open.names.insert(name.to_owned());
            step.cargo = true;
        } else if let Some(name) = run_name(&text) {
            open.names.insert(name.to_owned());
            step.cargo = true;
        } else if let Some(target) = run_header(&text) {
            open.target = Some(target);
            step.cargo = true;
        } else if let Some(failed) = result_counts(&text) {
            open.failed += failed;
            step.cargo = true;
            runs.push(std::mem::take(&mut open));
        } else if let Some(target) = rerun_target(&text) {
            // The line follows the `test result` line of the run it names, which is
            // the newest run in the log
            step.cargo = true;
            if let Some(run) = runs.last_mut() {
                run.target.get_or_insert(target);
            }
        } else if build_failure(&text) {
            step.build = true;
        }
    }
    unreadable |= step.unreadable();
    // A run whose count line fell outside the tail is still a run
    if !open.names.is_empty() || open.failed > 0 {
        runs.push(open);
    }
    Log { runs, unreadable }
}

// The step a line came from and its text: `gh run view --log-failed` puts the job and
// the step before it, as `failure_tail` keeps them. The job is kelpie's to ignore: the
// step names what ran.
fn step_and_text(line: &str) -> (Option<&str>, &str) {
    let mut parts = line.splitn(3, '\t');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(_), Some(step), Some(text)) => (Some(step), text),
        _ => (None, line),
    }
}

// `gh run view --log-failed` writes cargo's colour marks as the literal text
// `^[[1m` rather than escape bytes, which is why `strip_colour` in
// `crates/adapters/src/adapters/gh/runs/mod.rs` leaves them in. They wrap the words kelpie matches on,
// so they come off before anything is read.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, after)) = rest.split_once("^[[") {
        out.push_str(before);
        match after.find('m') {
            Some(end) if after[..end].chars().all(|c| c.is_ascii_digit() || c == ';') => {
                rest = &after[end + 1..];
            }
            _ => {
                out.push_str("^[[");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

// `    <name>`, one name of the list cargo ends a failing run with. The heading is
// followed by a blank line where cargo prints the blocks instead, which is not a
// name and ends the list.
fn listed_name(text: &str) -> Option<&str> {
    let name = text.strip_prefix("    ")?.trim();
    (!name.is_empty()).then_some(name)
}

// `---- <name> stdout ----`, the block cargo prints for a test whose output it showed
fn block_name(text: &str) -> Option<&str> {
    let inner = text.strip_prefix("---- ")?.strip_suffix(" ----")?;
    let (name, kind) = inner.rsplit_once(' ')?;
    matches!(kind, "stdout" | "stderr").then_some(name)
}

// `test <name> ... FAILED`, printed as the test ends
fn run_name(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("test ")?;
    let (name, _) = rest.rsplit_once(" ... FAILED")?;
    (!name.is_empty()).then_some(name)
}

// `     Running unittests src/lib.rs (target/debug/deps/...)`, or the
// `Running tests/<name>.rs` of one integration test binary. Cargo prints the
// header before the run, so it names the target for the names that follow.
fn run_header(text: &str) -> Option<Target> {
    let rest = text.trim_start().strip_prefix("Running ")?;
    match rest.split_whitespace().next()? {
        "unittests" => Some(Target::Lib),
        path => Some(Target::Test(
            path.strip_prefix("tests/")?.strip_suffix(".rs")?.to_owned(),
        )),
    }
}

// `test result: FAILED. 4599 passed; 2 failed; 28 ignored; ...`, or the `ok` form.
// Cargo prints one per test binary, which is where one run ends and the next
// begins.
fn result_counts(text: &str) -> Option<usize> {
    let rest = text.strip_prefix("test result: ")?.split_once(". ")?.1;
    let counts = rest
        .split(';')
        .find(|part| part.trim().ends_with("failed"))?;
    counts.split_whitespace().next()?.parse::<usize>().ok()
}

// `error: could not compile `shep-kelpie` (lib test) due to 1 previous error`,
// `error: could not document `shep-kelp``, and rustc's `error[E0061]: ...`. A step
// that printed one and none of cargo's test shapes failed while building, which is a
// build that ran no test rather than a test that failed.
fn build_failure(text: &str) -> bool {
    text.starts_with("error: could not compile")
        || text.starts_with("error: could not document")
        || text.starts_with("error[E")
}

// `error: test failed, to rerun pass `--lib``, or the `--test <name>` of one
// integration test binary. Cargo prints it while the run is still writing, so it
// is not the log's last line.
fn rerun_target(text: &str) -> Option<Target> {
    let flag = text.split_once("to rerun pass `")?.1.strip_suffix('`')?;
    match flag.strip_prefix("--test ") {
        Some(binary) => Some(Target::Test(binary.to_owned())),
        None => (flag == "--lib").then_some(Target::Lib),
    }
}

impl Runner {
    /// Counts a work item's failed CI at `head` once, by whether its worker could have caught it
    ///
    /// `logs` is what kelpie read of its failed runs. A run whose logs kelpie
    /// cannot read counts nothing: kelpie cannot say what failed there. The
    /// counted head is saved, so a restart does not count it again. It says
    /// nothing about what the gate did with the head after.
    pub(super) fn count_ci_failure(&mut self, head: &str, logs: &Logs) -> Result<(), StateError> {
        let Logs::Read(logs) = logs else {
            return Ok(());
        };
        if logs.text.trim().is_empty() {
            return Ok(());
        }
        let counted = {
            let item = self.current().expect("CI runs on a work item");
            item.counted_head.as_deref() == Some(head)
        };
        if counted {
            return Ok(());
        }
        let listed: &[&str] = match self.remote.as_str() {
            DESCRIBED_REPO => &SANDBOX_RED,
            _ => &[],
        };
        let tests = counted_as(logs, listed);
        self.metrics.ci_failure(tests);
        let counted = head.to_owned();
        self.update(|item| {
            let failures = &mut item.counts.ci_failures;
            let class = match tests {
                CiTests::Runnable => &mut failures.runnable,
                CiTests::SandboxOnly => &mut failures.sandbox_only,
                CiTests::NoTests => &mut failures.no_tests,
                CiTests::TimedOut => &mut failures.timed_out,
            };
            *class = class.saturating_add(1);
            item.counted_head = Some(counted);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::Checks;
    use crate::runner::{StepReport, step};
    use crate::test::ticket;
    use crate::test::{Rig, Scripted};
    use serde_json::json;

    // Six failed runs of this repo, each kept as `failed_run_logs` hands it:
    // `gh run view --log-failed` on gh 2.96, then `failure_tail` at 6000 bytes.

    // Run 38854765050 failed on one test of `sheep::handover::tests::pi`, which the
    // doc lists whole, and on its private-names job, which runs no test. That job's
    // home folder path is written `/home/me`, as every fixture writes the home folder.
    const SANDBOX_ONLY: &str = include_str!("../../../../../fixtures/gh-run-log-handover.txt");

    // Run 38807299334 failed on two tests of `runner::turn::tests::dev_server`, which
    // the doc lists in part: 5 of its tests. Kelpie cannot tell whether these two are
    // among those five, so it counts the run as one its worker could have caught.
    const PARTIAL_MODULE: &str =
        include_str!("../../../../../fixtures/gh-run-log-partial-module.txt");

    // Run 38967162070 failed on one test of `runner::pm::tests::lease`, which the doc
    // lists no reason for.
    const UNLISTED_TEST: &str =
        include_str!("../../../../../fixtures/gh-run-log-unlisted-test.txt");

    // Run 38983239961 failed on one test of the `tests/settings_examples.rs` binary,
    // which the doc lists no reason for. Its tail is the recorded shape of a log that
    // holds several cargo runs: a `Running tests/<name>.rs` header and a `test result`
    // line for each binary, the failing one last.
    const INTEGRATION: &str =
        include_str!("../../../../../fixtures/gh-run-log-integration-test.txt");

    // Run 38950550504 failed in `cargo doc --no-deps --locked`, with no test run at all.
    const NO_TEST: &str = include_str!("../../../../../fixtures/gh-run-log-no-test.txt");

    // Run 38567261082 failed while building its tests, and no test ran. Its
    // log holds rustc's `error[E0061]` and cargo's `error: could not
    // compile`. `failure_tail` keeps this job's part of the log when a test
    // build fails.
    const COMPILE_FAILURE: &str =
        include_str!("../../../../../fixtures/gh-run-log-compile-failure.txt");

    // What kelpie makes of a log on the repo its sandbox-red list is of, with every
    // failed run read
    fn tests_of(logs: &str) -> CiTests {
        failed_tests(logs, 0, &SANDBOX_RED)
    }

    // The doc listed `tests/upgrade.rs` whole until #1085 found it passes in a
    // worker, and lists no binary now, so these tests list it themselves.
    const LISTED_BINARY: [&str; 1] = ["tests/upgrade.rs"];

    fn tests_listing_the_binary(logs: &str) -> CiTests {
        failed_tests(logs, 0, &LISTED_BINARY)
    }

    // A failed run of a project whose tests are not cargo's, written by hand in the
    // shape the recorded logs show: the job and the step before each line, `##[group]`
    // and `##[endgroup]` around the command, and the runner's failure line at the end.
    // Its step names `pytest`, which kelpie reads as a test run it cannot parse.
    const PYTEST: &str = concat!(
        "--- run 12\n",
        "tests\tRun pytest\t##[group]Run pytest\n",
        "tests\tRun pytest\t^[[36;1mpytest -q^[[0m\n",
        "tests\tRun pytest\tshell: /usr/bin/bash -e {0}\n",
        "tests\tRun pytest\t##[endgroup]\n",
        "tests\tRun pytest\tFAILED tests/read_test.py::read_a_board - assert 0 == 1\n",
        "tests\tRun pytest\t1 failed in 0.42s\n",
        "tests\tRun pytest\t##[error]Process completed with exit code 1.\n",
    );

    // The same shape for a step that ran no test: a lint job whose output kelpie
    // cannot read either, and whose name says nothing of tests
    const LINT: &str = concat!(
        "--- run 13\n",
        "lint\tRun eslint\t##[group]Run eslint .\n",
        "lint\tRun eslint\t^[[36;1mnpx eslint .^[[0m\n",
        "lint\tRun eslint\t##[endgroup]\n",
        "lint\tRun eslint\t1 problem (1 error, 0 warnings)\n",
        "lint\tRun eslint\t##[error]Process completed with exit code 1.\n",
    );

    #[test]
    fn a_run_red_only_on_tests_a_worker_cannot_run_is_sandbox_only() {
        assert_eq!(tests_of(SANDBOX_ONLY), CiTests::SandboxOnly);
    }

    #[test]
    fn a_run_red_on_a_module_the_doc_lists_only_in_part_is_runnable() {
        assert_eq!(tests_of(PARTIAL_MODULE), CiTests::Runnable);
    }

    #[test]
    fn a_run_red_on_one_test_a_worker_could_run_is_runnable() {
        assert_eq!(tests_of(UNLISTED_TEST), CiTests::Runnable);
    }

    #[test]
    fn a_run_that_failed_with_no_test_run_counts_as_none() {
        assert_eq!(tests_of(NO_TEST), CiTests::NoTests);
    }

    // The commonest way a test job goes red with no test failing: the build of the
    // tests itself failed. Its step is named for a test run, and kelpie still reads it
    // as no test failure.
    #[test]
    fn a_test_job_that_failed_while_building_its_tests_counts_as_none() {
        assert!(names_tests("Run cargo test --locked --no-run"));
        assert_eq!(tests_of(COMPILE_FAILURE), CiTests::NoTests);

        let log = read(COMPILE_FAILURE);
        assert!(!log.unreadable, "a build failure is not an unread test run");
        assert!(log.runs.is_empty(), "{}", log.runs.len());
    }

    // A build failure in a step kelpie cannot read as cargo's is still no test failure
    // on a repo kelpie holds no sandbox-red list of, and a test failure in such a step
    // is still runnable there.
    #[test]
    fn a_build_failure_is_no_test_on_any_repo() {
        assert_eq!(failed_tests(COMPILE_FAILURE, 0, &[]), CiTests::NoTests);
        assert_eq!(failed_tests(PYTEST, 0, &[]), CiTests::Runnable);
    }

    // The list is of this crate's modules. On a repo kelpie holds no list of, the same
    // log's failures are ones its worker could have run.
    #[test]
    fn a_failing_test_on_a_repo_kelpie_holds_no_list_of_is_runnable() {
        assert_eq!(failed_tests(SANDBOX_ONLY, 0, &[]), CiTests::Runnable);
        assert_eq!(failed_tests(PARTIAL_MODULE, 0, &[]), CiTests::Runnable);
        // A run with no test in it is still no test, on any repo
        assert_eq!(failed_tests(NO_TEST, 0, &[]), CiTests::NoTests);
    }

    // A test failed in a runner kelpie cannot read. It is not a run that ran no test,
    // and kelpie cannot say the failure is one no worker could have run.
    #[test]
    fn a_test_failure_in_a_runner_kelpie_cannot_read_is_runnable() {
        assert_eq!(tests_of(PYTEST), CiTests::Runnable);
        // A step named for a runner kelpie knows, and one named for nothing at all
        assert!(names_tests("Run pytest"));
        assert!(names_tests("Run jest"));
        assert!(names_tests("Run go test"));
        assert!(names_tests("npm test"));
        assert!(names_tests("Run cargo nextest run"));
        assert!(names_tests("Run cargo test --locked --no-run"));
        assert!(names_tests("Run vitest run"));
        assert!(names_tests("Run tests"));
        assert!(!names_tests("Run cargo doc --no-deps --locked"));
        assert!(!names_tests("Run scripts/check-private.sh"));
        assert!(!names_tests("Run cargo +1.88 check --all-targets --locked"));
        // A word of the name that only ends in a runner's name is not one
        assert!(!names_tests("Install latest gh"));
        assert!(!names_tests("Run latest"));
        assert!(!names_tests("Run eslint"));
    }

    #[test]
    fn a_failed_step_that_ran_no_test_is_no_test_at_all() {
        assert_eq!(tests_of(LINT), CiTests::NoTests);
        // The recorded handover run failed on a listed test and on a script that runs
        // no test, and is still sandbox-only
        assert_eq!(tests_of(SANDBOX_ONLY), CiTests::SandboxOnly);
    }

    // Kelpie reads the logs of three failed runs. A head with more than that has
    // failures kelpie never saw, so it cannot say they were all ones no worker could
    // have run.
    #[test]
    fn a_head_with_failed_runs_kelpie_did_not_read_is_runnable() {
        assert_eq!(
            failed_tests(SANDBOX_ONLY, 1, &SANDBOX_RED),
            CiTests::Runnable,
            "one failed run left unread"
        );
        assert_eq!(failed_tests(NO_TEST, 2, &SANDBOX_RED), CiTests::Runnable);
    }

    // The same real log with everything above its count cut away, as a run with
    // more failures than this one would leave it: the names are gone, the count is not.
    #[test]
    fn a_run_whose_failing_names_are_cut_off_is_never_sandbox_only() {
        let cut = (SANDBOX_ONLY
            .lines()
            .skip_while(|line| !line.contains("test result: FAILED")))
        .collect::<Vec<_>>()
        .join("\n");
        assert_eq!(tests_of(&cut), CiTests::Runnable, "{cut}");
    }

    #[test]
    fn a_run_that_names_its_tests_and_counts_them_agrees_with_itself() {
        let log = read(UNLISTED_TEST);
        assert!(!log.unreadable);
        let runs = log.runs;
        assert_eq!(runs.len(), 1, "{runs:?}");
        assert_eq!(runs[0].failed, 1);
        assert_eq!(
            runs[0].names.iter().collect::<Vec<_>>(),
            vec!["runner::pm::tests::lease::a_long_lease_wait_wakes_it_naming_who_holds_the_lease"]
        );
        assert_eq!(runs[0].target, Some(Target::Lib));

        // The recorded multi-binary log: seven runs, the first cut at the head of the
        // tail, one of them failing, and each naming as many failures as it counts
        let log = read(INTEGRATION);
        assert!(!log.unreadable);
        let runs = log.runs;
        assert_eq!(runs.len(), 7, "{runs:?}");
        assert!(runs.iter().all(Run::agrees), "{runs:?}");
        let failing = runs.iter().filter(|run| run.failed > 0).collect::<Vec<_>>();
        assert_eq!(failing.len(), 1);
        assert_eq!(failing[0].failed, 1);
        assert_eq!(
            failing[0].names.iter().collect::<Vec<_>>(),
            vec!["every_project_setting_is_documented_in_the_settings_example"]
        );
    }

    // The two recorded logs joined, as one head red on a lib test and on an
    // integration binary would leave them, with the second one's binary named as
    // `tests/upgrade.rs`, listed whole. Only the binary's name is
    // changed, in both places cargo prints it.
    #[test]
    fn each_run_in_a_log_is_judged_on_its_own_target() {
        let upgrade = INTEGRATION.replace("settings_examples", "upgrade");
        let joined = format!("{UNLISTED_TEST}\n{upgrade}");

        let runs = read(&joined).runs;
        let failing = runs.iter().filter(|run| run.failed > 0).collect::<Vec<_>>();
        assert_eq!(failing.len(), 2, "{runs:?}");
        assert_eq!(failing[0].target, Some(Target::Lib));
        assert_eq!(
            failing[1].target,
            Some(Target::Test("upgrade".to_owned())),
            "the header names it, not the `to rerun pass` line of the run before"
        );

        // Each half on its own, then the pair: the lib test is one a worker could
        // have run, so the head is runnable whatever target the log ends on.
        assert_eq!(tests_listing_the_binary(UNLISTED_TEST), CiTests::Runnable);
        assert_eq!(tests_listing_the_binary(&upgrade), CiTests::SandboxOnly);
        assert_eq!(tests_listing_the_binary(&joined), CiTests::Runnable);
    }

    #[test]
    fn a_test_of_a_listed_integration_binary_is_sandbox_only() {
        // The recorded log above with its binary named as `tests/upgrade.rs`, listed
        // whole. Only the name the log carries is changed here.
        let listed = INTEGRATION.replace("settings_examples", "upgrade");
        assert_eq!(tests_listing_the_binary(&listed), CiTests::SandboxOnly);
        assert_eq!(tests_listing_the_binary(INTEGRATION), CiTests::Runnable);
    }

    #[test]
    fn a_module_is_listed_from_its_own_tests_down() {
        assert!(sandbox_red(
            "doctor::gateways",
            &Some(Target::Lib),
            &SANDBOX_RED
        ));
        assert!(sandbox_red(
            "doctor::gateways::tests::a_refused_key_is_missing_and_one_not_set_here_is_unsure",
            &Some(Target::Lib),
            &SANDBOX_RED
        ));
        assert!(sandbox_red(
            "runner::gpu_tests::a_changed_metrics_url_takes_effect_at_the_next_reread",
            &Some(Target::Lib),
            &SANDBOX_RED
        ));
        assert!(sandbox_red(
            "runner::pm::tests::attach::a_pid_that_runs_nothing_holds_nothing",
            &Some(Target::Lib),
            &SANDBOX_RED
        ));
        // A name that only starts with a listed module is another one
        assert!(!sandbox_red(
            "runner::attached::a_test",
            &None,
            &SANDBOX_RED
        ));
        assert!(!sandbox_red(
            "runner::pm::tests::lease::a_long_lease_wait",
            &Some(Target::Lib),
            &SANDBOX_RED
        ));
    }

    #[test]
    fn a_module_the_doc_lists_in_part_is_not_listed_at_all() {
        for name in [
            "adapters::forwarder::tests::a_reply_is_relayed",
            "adapters::goose::attach_tests::the_terminal_session_has_a_forwarder",
            "adapters::bridge::proxy::tests::an_address_that_is_not_a_loopback",
            "github::a_run_that_needs_a_port",
        ] {
            assert!(
                !sandbox_red(name, &Some(Target::Lib), &SANDBOX_RED),
                "{name}"
            );
        }
    }

    #[test]
    fn a_log_with_nothing_in_it_is_no_test_rather_than_a_run_read_wrong() {
        assert_eq!(tests_of(""), CiTests::NoTests);
        assert_eq!(tests_of("--- run 9\n"), CiTests::NoTests);
    }

    // The doc's table is what a person keeps true. Its whole entries and this list
    // are read against each other, so an entry added to one and not the other fails
    // here.
    #[test]
    fn the_doc_and_the_list_agree() {
        let doc = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/agents/sandbox-red-tests.md"
        ))
        .unwrap();
        let (whole, part) = from_the_doc(&doc);
        let mut mine = SANDBOX_RED
            .iter()
            .map(|entry| entry.to_string())
            .collect::<Vec<_>>();
        mine.sort();
        assert_eq!(whole, mine);
        assert!(!part.is_empty(), "the doc marks no entry some:");
        for entry in &part {
            assert!(!mine.contains(entry), "{entry} is whole and in part");
        }
    }

    // Every module and test binary the doc's table names, split into the entries it
    // lists whole and the ones it marks `some:`. Only the table counts: the prose
    // around it names tests that pass in a worker too.
    fn from_the_doc(doc: &str) -> (Vec<String>, Vec<String>) {
        let rows = (doc.lines())
            .skip_while(|line| !line.starts_with("| Reason | Modules |"))
            .take_while(|line| line.starts_with('|'));
        let mut whole = BTreeSet::new();
        let mut part = BTreeSet::new();
        for row in rows {
            let cells = row.split('`').collect::<Vec<_>>();
            for i in (1..cells.len()).step_by(2) {
                if !listed_in_the_crate(cells[i]) {
                    continue;
                }
                let after = cells.get(i + 1).unwrap_or(&"").trim_start();
                match after.starts_with("(some:") {
                    true => {
                        part.insert(cells[i].to_owned());
                    }
                    false => {
                        whole.insert(cells[i].to_owned());
                    }
                }
            }
        }
        // An entry listed whole in one row and in part in another is whole
        part.retain(|entry| !whole.contains(entry));
        (whole.into_iter().collect(), part.into_iter().collect())
    }

    // Whether a word the doc sets in backticks names tests of this crate: a
    // `tests/<name>.rs` binary, or a module whose first segment is one of its files
    fn listed_in_the_crate(token: &str) -> bool {
        // This crate is two folders below the workspace root
        let workspace_root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let is_word = |s: &str| {
            !s.is_empty()
                && s.starts_with(|c: char| c.is_ascii_lowercase())
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        };
        if let Some(binary) = token
            .strip_prefix("tests/")
            .and_then(|t| t.strip_suffix(".rs"))
            && is_word(binary)
        {
            return true;
        }
        let mut segments = token.split("::");
        let Some(first) = segments.next() else {
            return false;
        };
        if !is_word(first) || token.contains("::") && !segments.all(is_word) {
            return false;
        }
        // The root's own modules, and the modules of the crates under `crates/`, whose
        // tests `cargo test` runs from the root all the same
        let root = std::path::Path::new(workspace_root);
        let holds = |src: std::path::PathBuf| {
            src.join(format!("{first}.rs")).exists() || src.join(first).join("mod.rs").exists()
        };
        holds(root.join("src"))
            || std::fs::read_dir(root.join("crates"))
                .into_iter()
                .flatten()
                .flatten()
                .any(|member| holds(member.path().join("src")))
    }

    // A work item on kelpie's own repo whose CI is red, sent back for a fix turn with
    // `logs` as its failed runs' logs. The sandbox-red list is used only there, so a
    // test that needs it moves the rig's repo.
    fn red_ci(logs: &str) -> (Rig, std::sync::Mutex<Runner>, String) {
        let (rig, runner, head) = Rig::with_pull_request_set("koji", |rig| rig.remote_is_kelpie());
        rig.forge.set_ci_log(logs);
        rig.forge
            .set_checks(&head, Checks::Failed(vec!["test".to_owned()]));
        rig.verdict(&runner);
        (rig, runner, head)
    }

    fn counted(rig: &Rig, tests: &str) -> Option<f64> {
        rig.samples
            .value("kelpie_ci_failures_total", &[("tests", tests)])
    }

    // The work item keeps the same count, which its finished line carries.
    #[test]
    fn a_red_run_on_tests_a_worker_cannot_run_counts_as_sandbox_only() {
        let (rig, runner, _head) = red_ci(SANDBOX_ONLY);
        assert_eq!(counted(&rig, "sandbox-only"), Some(1.0));
        assert_eq!(counted(&rig, "runnable"), None);
        assert_eq!(counted(&rig, "none"), None);
        let kept = runner.lock().unwrap().state.work_items[0]
            .counts
            .ci_failures;
        assert_eq!((kept.sandbox_only, kept.runnable, kept.no_tests), (1, 0, 0));
    }

    #[test]
    fn a_red_run_on_a_test_a_worker_could_have_run_counts_as_runnable() {
        let (rig, _runner, _head) = red_ci(UNLISTED_TEST);
        assert_eq!(counted(&rig, "runnable"), Some(1.0));
        assert_eq!(counted(&rig, "sandbox-only"), None);
    }

    #[test]
    fn a_red_run_on_a_module_listed_in_part_counts_as_runnable() {
        let (rig, _runner, _head) = red_ci(PARTIAL_MODULE);
        assert_eq!(counted(&rig, "runnable"), Some(1.0));
        assert_eq!(counted(&rig, "sandbox-only"), None);
    }

    #[test]
    fn a_red_run_with_no_test_in_its_log_counts_as_none() {
        let (rig, _runner, _head) = red_ci(NO_TEST);
        assert_eq!(counted(&rig, "none"), Some(1.0));
        assert_eq!(counted(&rig, "sandbox-only"), None);
    }

    // The rig's project is `shep-pm/shep`, which kelpie holds no sandbox-red list of,
    // so the same log that is sandbox-only on kelpie's own repo is runnable here.
    #[test]
    fn a_red_run_on_a_repo_kelpie_holds_no_list_of_counts_as_runnable() {
        let (rig, runner, head) = Rig::with_pull_request("koji");
        rig.forge.set_ci_log(SANDBOX_ONLY);
        rig.forge
            .set_checks(&head, Checks::Failed(vec!["test".to_owned()]));
        rig.verdict(&runner);
        assert_eq!(counted(&rig, "runnable"), Some(1.0));
        assert_eq!(counted(&rig, "sandbox-only"), None);
    }

    #[test]
    fn a_red_run_with_more_failed_runs_than_kelpie_reads_counts_as_runnable() {
        let (rig, runner, head) = Rig::with_pull_request_set("koji", |rig| rig.remote_is_kelpie());
        rig.forge.set_ci_log_omitted(SANDBOX_ONLY, 1);
        rig.forge
            .set_checks(&head, Checks::Failed(vec!["test".to_owned()]));
        rig.verdict(&runner);
        assert_eq!(counted(&rig, "runnable"), Some(1.0));
        assert_eq!(counted(&rig, "sandbox-only"), None);
    }

    // `ci.fix_attempts = 0` sends a head that goes red straight to the still-red
    // ruling, with no fix turn, and a `yes` returns the work item to CI at the same
    // head, where the gate reads it red again. The head was counted once, and stays
    // counted once however many times the ruling is answered.
    #[test]
    fn a_still_red_head_a_yes_sends_back_to_ci_counts_once() {
        let (rig, runner, head) = Rig::with_pull_request_set("koji", |rig| {
            rig.remote_is_kelpie();
            rig.fix_attempts(0);
        });
        rig.forge.set_ci_log(UNLISTED_TEST);
        // The log names no file the branch changed, so kelpie would rerun it. A forge
        // that refuses the rerun leaves the red run to the cap, which is the path here.
        rig.forge.set_rerun_error("gh failed: no admin rights");
        rig.forge
            .set_checks(&head, Checks::Failed(vec!["test".to_owned()]));

        let report = rig.verdict(&runner);
        assert!(
            matches!(report, Some(StepReport::Ruling { id, .. }) if id == ticket(71)),
            "{report:?}"
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["rulings"][0]["kind"]["reason"],
            json!("still-red")
        );
        assert_eq!(counted(&rig, "runnable"), Some(1.0));

        rig.ask(&runner, "rule", Some("71 yes"));
        rig.forge
            .set_checks(&head, Checks::Failed(vec!["test".to_owned()]));
        let reports: Vec<_> = (0..4).map(|_| rig.verdict(&runner)).collect();
        assert!(
            reports
                .iter()
                .any(|r| matches!(r, Some(StepReport::Ruling { .. }))),
            "the gate read the same head red again: {reports:?}"
        );
        assert_eq!(counted(&rig, "runnable"), Some(1.0), "{reports:?}");
    }

    #[test]
    fn the_same_head_red_twice_counts_once_and_a_new_head_counts_again() {
        // The log names no file the branch changed, so kelpie reruns the failed jobs
        // rather than spend a turn, as `gate::rerun` does.
        let (rig, runner, head) = red_ci(UNLISTED_TEST);
        assert!(
            matches!(
                rig.forge.reruns().as_slice(),
                [rerun] if rerun == &head
            ),
            "{:?}",
            rig.forge.reruns()
        );

        // The rerun failed at the same head, which sends the worker back for a fix.
        let report = rig.verdict(&runner);
        assert!(
            matches!(&report, Some(StepReport::CiFailed { head: h, .. }) if h == &head),
            "{report:?}"
        );
        assert_eq!(counted(&rig, "runnable"), Some(1.0), "one head, one count");

        // The worker's fix pushes a new head, which goes red again.
        rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
        step(&runner).unwrap();
        let fixed = rig.forge.head_of("kelpie/7").unwrap();
        rig.forge
            .set_checks(&fixed, Checks::Failed(vec!["test".to_owned()]));
        rig.verdict(&runner);
        assert_eq!(counted(&rig, "runnable"), Some(2.0), "{head} then {fixed}");
    }

    mod timed_out;

    #[test]
    fn a_red_run_whose_logs_kelpie_cannot_read_counts_nothing() {
        let (rig, runner, head) = Rig::with_pull_request("koji");
        rig.forge.set_ci_log_error("gh run view failed");
        rig.forge
            .set_checks(&head, Checks::Failed(vec!["test".to_owned()]));
        rig.verdict(&runner);
        assert_eq!(rig.samples.total("kelpie_ci_failures_total"), 0.0);
    }
}
