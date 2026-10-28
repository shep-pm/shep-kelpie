use super::*;
use crate::ports::FailedLogs;

// Run 38275563600's job-log tail as `failed_run_logs` hands it: the note that says
// the job's timeout cancelled it, then lines of passing tests and no failing one
const TIMED_OUT: &str = concat!(
    "--- run 9\n",
    "note: job `test-macos` was cancelled in step `Run cargo test --locked` after 20m27s: ",
    "its `timeout-minutes` ended it, so this is a slow job and not a failing test\n",
    "test-macos\tUNKNOWN STEP\ttest sheep::tests::finish_stops_the_sheep ... ok\n",
    "test-macos\tUNKNOWN STEP\t##[error]The operation was canceled.\n",
);

#[test]
fn a_head_whose_only_red_run_its_timeout_ended_counts_as_timed_out() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    rig.forge.set_ci_log_timed_out(TIMED_OUT, 1);
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".to_owned()]));
    rig.verdict(&runner);

    assert_eq!(counted(&rig, "timed-out"), Some(1.0));
    for other in ["runnable", "sandbox-only", "none"] {
        assert_eq!(counted(&rig, other), None, "{other}");
    }
    let kept = runner.lock().unwrap().state.work_items[0]
        .counts
        .ci_failures;
    assert_eq!(
        (
            kept.timed_out,
            kept.runnable,
            kept.sandbox_only,
            kept.no_tests
        ),
        (1, 0, 0, 0)
    );
}

// A run its timeout ended and a run that failed on a test a worker could run: the
// head is red on a test it could have caught, whatever else ended a run
#[test]
fn a_timed_out_run_that_names_a_failing_test_keeps_its_class() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    rig.forge
        .set_ci_log_timed_out(&format!("{TIMED_OUT}{UNLISTED_TEST}"), 1);
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".to_owned()]));
    rig.verdict(&runner);

    assert_eq!(counted(&rig, "runnable"), Some(1.0));
    assert_eq!(counted(&rig, "timed-out"), None);
}

// Kelpie read some of the failed runs only, so a timed-out run among them is not
// all that is red
#[test]
fn a_timed_out_run_among_runs_kelpie_did_not_read_counts_as_runnable() {
    let logs = |omitted| FailedLogs {
        text: TIMED_OUT.to_owned(),
        omitted,
        timed_out: 1,
    };

    assert_eq!(counted_as(&logs(0), &[]), CiTests::TimedOut);
    assert_eq!(counted_as(&logs(1), &[]), CiTests::Runnable);
}
