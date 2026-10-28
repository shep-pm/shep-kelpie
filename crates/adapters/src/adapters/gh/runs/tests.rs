use super::*;

// The shape `gh run list --json databaseId,conclusion,headSha,event`
// printed on this repo with gh 2.96, with conclusions and events varied.
const LISTED: &[u8] = br#"[
    {"conclusion":"failure","databaseId":4,"event":"merge_group","headSha":"queue"},
    {"conclusion":"failure","databaseId":3,"event":"pull_request","headSha":"abc"},
    {"conclusion":"success","databaseId":2,"event":"pull_request","headSha":"abc"},
    {"conclusion":"failure","databaseId":1,"event":"pull_request","headSha":"old"}
]"#;

fn ids(runs: Vec<FailedRun>) -> Vec<u64> {
    runs.into_iter().map(|r| r.id).collect()
}

#[test]
fn a_pull_requests_failed_runs_are_those_of_its_head() {
    assert_eq!(ids(failed_runs(LISTED, "abc", false).unwrap()), [3]);
}

#[test]
fn every_failed_run_of_a_head_is_listed_not_only_the_ones_whose_logs_are_read() {
    let listed = br#"[
        {"conclusion":"failure","databaseId":4,"event":"pull_request","headSha":"abc"},
        {"conclusion":"failure","databaseId":3,"event":"pull_request","headSha":"abc"},
        {"conclusion":"failure","databaseId":2,"event":"pull_request","headSha":"abc"},
        {"conclusion":"failure","databaseId":1,"event":"pull_request","headSha":"abc"}
    ]"#;
    assert_eq!(
        ids(failed_runs(listed, "abc", false).unwrap()),
        [4, 3, 2, 1]
    );
}

#[test]
fn the_queues_failed_runs_are_those_of_the_merge_group_event() {
    assert_eq!(ids(failed_runs(LISTED, "abc", true).unwrap()), [4]);
}

// A job `timeout-minutes` stops ends `cancelled`, and so does its run
// (`fixtures/gh-run-view-jobs-timed-out.json`), so a run is read for a log
// whichever of the three it concluded
#[test]
fn a_run_ended_by_its_timeout_is_listed_for_a_log() {
    let listed = br#"[
        {"conclusion":"success","databaseId":9,"event":"merge_group","headSha":"queue"},
        {"conclusion":"cancelled","databaseId":8,"event":"pull_request","headSha":"abc"},
        {"conclusion":"timed_out","databaseId":7,"event":"pull_request","headSha":"abc"},
        {"conclusion":"success","databaseId":6,"event":"pull_request","headSha":"abc"},
        {"conclusion":"failure","databaseId":5,"event":"pull_request","headSha":"abc"},
        {"conclusion":"cancelled","databaseId":4,"event":"pull_request","headSha":"old"},
        {"conclusion":"timed_out","databaseId":3,"event":"merge_group","headSha":"queue"},
        {"conclusion":"skipped","databaseId":2,"event":"pull_request","headSha":"abc"}
    ]"#;

    let of_head = failed_runs(listed, "abc", false).unwrap();
    assert_eq!(ids(of_head), [8, 7, 5]);
    let of_queue = failed_runs(listed, "abc", true).unwrap();
    assert_eq!(ids(of_queue), [3]);
}

#[test]
fn a_rerun_list_is_the_runs_that_failed_alone() {
    let listed = br#"[
        {"conclusion":"timed_out","databaseId":4,"event":"pull_request","headSha":"abc"},
        {"conclusion":"failure","databaseId":3,"event":"pull_request","headSha":"abc"},
        {"conclusion":"cancelled","databaseId":2,"event":"pull_request","headSha":"abc"}
    ]"#;
    assert_eq!(rerun_list(listed, "abc").unwrap(), [3]);

    // A rerun of a run its timeout ended would time out again
    let only_timed_out = br#"[
        {"conclusion":"timed_out","databaseId":4,"event":"pull_request","headSha":"abc"}
    ]"#;
    assert!(matches!(
        rerun_list(only_timed_out, "abc"),
        Err(ForgeError::Failed(_))
    ));
}

// `gh run list --branch main --status completed --limit 50 --json
// databaseId,workflowName,conclusion,headSha,event,createdAt` on
// shep-pm/shep with gh 2.96, its first fourteen runs: several heads per
// workflow, one `test` failure and two cancelled `pages` runs.
const RECORDED: &[u8] = br#"[
    {"conclusion":"success","createdAt":"2026-10-13T10:58:41Z","databaseId":38387029879,"event":"push","headSha":"dc9b123d317baabe97afc3c22316f8a6723a9afe","workflowName":"file size"},
    {"conclusion":"success","createdAt":"2026-10-13T10:58:41Z","databaseId":38387029600,"event":"push","headSha":"dc9b123d317baabe97afc3c22316f8a6723a9afe","workflowName":"Release"},
    {"conclusion":"success","createdAt":"2026-10-13T10:57:51Z","databaseId":38386934120,"event":"push","headSha":"f0751a5563ba0a9c978af3330b05efa010e15172","workflowName":"file size"},
    {"conclusion":"success","createdAt":"2026-10-13T10:57:50Z","databaseId":38386933064,"event":"push","headSha":"f0751a5563ba0a9c978af3330b05efa010e15172","workflowName":"Release"},
    {"conclusion":"failure","createdAt":"2026-10-13T10:57:50Z","databaseId":38386933110,"event":"push","headSha":"f0751a5563ba0a9c978af3330b05efa010e15172","workflowName":"Release PR"},
    {"conclusion":"success","createdAt":"2026-10-13T10:43:27Z","databaseId":38385312112,"event":"push","headSha":"16d445f3b0a1b8ec446c7692f290d460f7837590","workflowName":"file size"},
    {"conclusion":"success","createdAt":"2026-10-13T10:43:27Z","databaseId":38385311813,"event":"push","headSha":"16d445f3b0a1b8ec446c7692f290d460f7837590","workflowName":"Release"},
    {"conclusion":"cancelled","createdAt":"2026-10-13T10:43:27Z","databaseId":38385311703,"event":"push","headSha":"16d445f3b0a1b8ec446c7692f290d460f7837590","workflowName":"pages"},
    {"conclusion":"failure","createdAt":"2026-10-13T10:43:26Z","databaseId":38385311310,"event":"push","headSha":"16d445f3b0a1b8ec446c7692f290d460f7837590","workflowName":"Release PR"},
    {"conclusion":"failure","createdAt":"2026-10-13T10:43:26Z","databaseId":38385311306,"event":"push","headSha":"16d445f3b0a1b8ec446c7692f290d460f7837590","workflowName":"test"},
    {"conclusion":"success","createdAt":"2026-10-13T10:28:04Z","databaseId":38383546750,"event":"push","headSha":"7665f5b080313743b4b569e4f93a6775328be708","workflowName":"file size"},
    {"conclusion":"success","createdAt":"2026-10-13T10:28:04Z","databaseId":38383546374,"event":"push","headSha":"7665f5b080313743b4b569e4f93a6775328be708","workflowName":"Release"},
    {"conclusion":"failure","createdAt":"2026-10-13T10:28:04Z","databaseId":38383546391,"event":"push","headSha":"7665f5b080313743b4b569e4f93a6775328be708","workflowName":"Release PR"},
    {"conclusion":"success","createdAt":"2026-10-13T10:28:04Z","databaseId":38383546340,"event":"push","headSha":"7665f5b080313743b4b569e4f93a6775328be708","workflowName":"test"},
    {"conclusion":"cancelled","createdAt":"2026-10-13T10:28:04Z","databaseId":38383546385,"event":"push","headSha":"7665f5b080313743b4b569e4f93a6775328be708","workflowName":"pages"}
]"#;

fn run_of<'a>(runs: &'a [BranchRun], workflow: &str) -> &'a BranchRun {
    runs.iter().find(|r| r.workflow == workflow).unwrap()
}

#[test]
fn a_workflows_latest_run_is_the_only_one_kept() {
    let runs = latest_runs(RECORDED).unwrap();

    let mut workflows: Vec<_> = runs.iter().map(|r| r.workflow.as_str()).collect();
    workflows.sort_unstable();
    assert_eq!(
        workflows,
        ["Release", "Release PR", "file size", "pages", "test"]
    );
    assert_eq!(
        run_of(&runs, "file size"),
        &BranchRun {
            workflow: "file size".to_owned(),
            id: 38_387_029_879,
            head: "dc9b123d317baabe97afc3c22316f8a6723a9afe".to_owned(),
            event: "push".to_owned(),
            conclusion: "success".to_owned(),
            created: Timestamp(1_791_889_121),
        }
    );
}

#[test]
fn a_failed_run_is_kept_and_an_older_pass_of_its_workflow_is_not() {
    let runs = latest_runs(RECORDED).unwrap();

    let test = run_of(&runs, "test");
    assert_eq!(
        (test.id, test.conclusion.as_str()),
        (38_385_311_306, "failure")
    );
}

#[test]
fn a_cancelled_run_is_kept_with_its_conclusion() {
    let runs = latest_runs(RECORDED).unwrap();

    let pages = run_of(&runs, "pages");
    assert_eq!(
        (pages.id, pages.conclusion.as_str()),
        (38_385_311_703, "cancelled")
    );
}

#[test]
fn a_run_listed_before_a_newer_one_of_its_workflow_does_not_win_by_position() {
    let listed = br#"[
        {"conclusion":"failure","createdAt":"2026-10-13T10:00:00Z","databaseId":1,"event":"push","headSha":"a","workflowName":"test"},
        {"conclusion":"success","createdAt":"2026-10-13T11:00:00Z","databaseId":2,"event":"push","headSha":"b","workflowName":"test"}
    ]"#;

    let runs = latest_runs(listed).unwrap();

    assert_eq!(runs.iter().map(|r| r.id).collect::<Vec<_>>(), [2]);
}

#[test]
fn a_pull_request_run_does_not_hide_the_push_run_of_the_same_workflow() {
    let listed = br#"[
        {"conclusion":"failure","createdAt":"2026-10-13T11:00:00Z","databaseId":2,"event":"pull_request","headSha":"b","workflowName":"test"},
        {"conclusion":"success","createdAt":"2026-10-13T10:00:00Z","databaseId":1,"event":"push","headSha":"a","workflowName":"test"}
    ]"#;

    let runs = latest_runs(listed).unwrap();

    assert_eq!(runs.iter().map(|r| r.id).collect::<Vec<_>>(), [2, 1]);
}

#[test]
fn a_listing_gh_did_not_print_as_json_is_unreadable() {
    assert!(matches!(
        latest_runs(b"not json"),
        Err(ForgeError::Unreadable(_))
    ));
}

// What `gh workflow list --all --json name,path,state` printed on
// shep-pm/shep with gh 2.96, where every workflow is active.
const WORKFLOW_LIST: &[u8] = include_bytes!("../../../../../../fixtures/gh-workflow-list.json");

#[test]
fn the_recorded_workflow_list_reads_as_active_workflows_with_their_paths() {
    let workflows = parse_workflows(WORKFLOW_LIST).unwrap();

    assert_eq!(workflows.len(), 11);
    assert!(workflows.iter().all(|w| w.state == WorkflowState::Active));
    assert_eq!(
        workflows[7],
        Workflow {
            name: "test".to_owned(),
            path: ".github/workflows/test.yml".to_owned(),
            state: WorkflowState::Active,
        }
    );
}

// Hand-written: no repo of the maintainer's has a deleted or disabled
// workflow. The states are the ones GitHub documents for a workflow's
// `state` field in "List repository workflows".
#[test]
fn every_state_github_documents_for_a_workflow_is_read() {
    let listed = br#"[
        {"name":"a","path":".github/workflows/a.yml","state":"active"},
        {"name":"b","path":".github/workflows/b.yml","state":"deleted"},
        {"name":"c","path":".github/workflows/c.yml","state":"disabled_manually"},
        {"name":"d","path":".github/workflows/d.yml","state":"disabled_inactivity"},
        {"name":"e","path":".github/workflows/e.yml","state":"disabled_fork"}
    ]"#;

    let states: Vec<_> = parse_workflows(listed)
        .unwrap()
        .into_iter()
        .map(|w| w.state)
        .collect();

    assert_eq!(
        states,
        [
            WorkflowState::Active,
            WorkflowState::Deleted,
            WorkflowState::DisabledManually,
            WorkflowState::DisabledInactivity,
            WorkflowState::DisabledFork,
        ]
    );
}

#[test]
fn a_workflow_state_github_does_not_document_is_unreadable() {
    let listed = br#"[{"name":"a","path":"a.yml","state":"archived"}]"#;

    assert!(matches!(
        parse_workflows(listed),
        Err(ForgeError::Unreadable(_))
    ));
}

#[test]
fn a_workflow_list_gh_did_not_print_as_json_is_unreadable() {
    assert!(matches!(
        parse_workflows(b"not json"),
        Err(ForgeError::Unreadable(_))
    ));
}

// Shaped like the example GitHub documents for "List workflow runs for a
// repository", with `status` set to `action_required` and a fork's pull
// request as the event. Hand-written, not recorded: the real recording
// (shep-pm/shep-kelpie#636) replaces it.
const HELD: &[u8] = include_bytes!("../../../../../../fixtures/gh-api-action-required-runs.json");

#[test]
fn the_gh_adapter_parses_the_recorded_run_listing() {
    let head = "acb5820ced9479c074f688cc328bf03f341a511d";

    assert_eq!(held_runs(HELD, head).unwrap(), [30_433_642, 30_433_643]);
}

#[test]
fn a_run_of_another_head_is_never_listed_for_approval() {
    assert_eq!(
        held_runs(HELD, "f".repeat(40).as_str()).unwrap(),
        Vec::<u64>::new()
    );
}

#[test]
fn a_run_that_is_not_waiting_for_approval_is_never_listed() {
    let listed = br#"{"total_count":2,"workflow_runs":[
        {"id":1,"head_sha":"abc","status":"completed"},
        {"id":2,"head_sha":"abc","status":"action_required"}
    ]}"#;

    assert_eq!(held_runs(listed, "abc").unwrap(), [2]);
}

#[test]
fn a_run_held_as_completed_with_the_conclusion_action_required_is_listed() {
    let listed = br#"{"total_count":3,"workflow_runs":[
        {"id":1,"head_sha":"abc","status":"completed","conclusion":"action_required"},
        {"id":2,"head_sha":"abc","status":"completed","conclusion":"success"},
        {"id":3,"head_sha":"abc","status":"action_required","conclusion":null}
    ]}"#;

    assert_eq!(held_runs(listed, "abc").unwrap(), [1, 3]);
}

#[test]
fn a_listing_with_no_runs_is_an_empty_list() {
    let listed = br#"{"total_count":0,"workflow_runs":[]}"#;

    assert_eq!(held_runs(listed, "abc").unwrap(), Vec::<u64>::new());
}

#[test]
fn a_run_listing_gh_did_not_print_as_json_is_unreadable() {
    assert!(matches!(
        held_runs(b"not json", "abc"),
        Err(ForgeError::Unreadable(_))
    ));
}

const STAMP: &str = "test\tBuild\t2026-10-13T10:00:00.0000000Z";

#[test]
fn a_failed_logs_tail_has_no_cargo_progress_lines() {
    let mut log = String::new();
    for n in 0..400 {
        log.push_str(&format!(
            "{STAMP}    Compiling crate{n} v0.1.0\n\
             {STAMP}   Downloaded dep{n} v1.0.0\n\
             {STAMP}  Downloading crates ...\n\
             {STAMP}     Checking crate{n} v0.1.0\n\
             {STAMP}        Fresh crate{n} v0.1.0\n"
        ));
    }
    log.push_str(&format!("{STAMP} error[E0308]: mismatched types\n"));
    log.push_str(&format!("{STAMP}  --> src/lib.rs:1:1\n"));

    let kept = failure_tail(&log, LOG_BYTES);

    assert_eq!(
        kept,
        "test\tBuild\terror[E0308]: mismatched types\ntest\tBuild\t --> src/lib.rs:1:1"
    );
}

#[test]
fn coloured_progress_lines_are_dropped_and_colour_is_stripped_from_the_rest() {
    let log = format!(
        "{STAMP} \x1b[1m\x1b[92m   Compiling\x1b[0m foo v0.1.0\n\
         {STAMP} \x1b[1m\x1b[91merror\x1b[0m: could not compile `foo`\n"
    );

    assert_eq!(
        failure_tail(&log, LOG_BYTES),
        "test\tBuild\terror: could not compile `foo`"
    );
}

#[test]
fn a_line_with_a_tab_in_its_text_loses_its_timestamp() {
    let log = format!("{STAMP} ---- a stdout ----\tx\n");

    assert_eq!(
        failure_tail(&log, LOG_BYTES),
        "test\tBuild\t---- a stdout ----\tx"
    );
}

#[test]
fn a_failure_line_that_starts_with_a_progress_word_stays() {
    let log = format!(
        "{STAMP} Checking invariants after merge: left != right\n\
         {STAMP} Checking formatting...\n\
         {STAMP}   Downloading v1 of the fixture failed\n"
    );

    let kept = failure_tail(&log, LOG_BYTES);

    assert_eq!(kept.lines().count(), 3, "{kept}");
}

#[test]
fn a_log_keeps_its_end_from_a_line_start_and_never_splits_a_character() {
    assert_eq!(tail("a\nb\n", 100), "a\nb");
    let log = "first line\nsecond line\nthird é line\n";
    assert_eq!(tail(log, 16), "third é line");
    assert_eq!(tail("é".repeat(10).as_str(), 5), "éé");
}

// Run 38275563600 of shep-pm/shep-kelpie, read with gh 2.96: its `test-macos` job
// (`timeout-minutes: 20`) was stopped in `Run cargo test --locked`, and its
// `--log-failed` answer is empty. The log is the first 3 and last 60 lines, with
// the runner folder written `/home/me`.
const JOBS: &[u8] = include_bytes!("../../../../../../fixtures/gh-run-view-jobs-timed-out.json");
const JOB_LOG: &str = include_str!("../../../../../../fixtures/gh-run-log-timed-out.txt");

const RUN: u64 = 38_275_563_600;
const NOTE: &str = "note: job `test-macos` was cancelled in step `Run cargo test --locked` \
                    after 20m27s: its `timeout-minutes` ended it, so this is a slow job and \
                    not a failing test";

// What `gh` answered for run 38275563600 to `args`, `failed` being its `--log-failed`
fn answer(args: &[&str], failed: &str, job_log: &str) -> Result<Vec<u8>, ForgeError> {
    let id = RUN.to_string();
    let repo = "shep-pm/shep-kelpie";
    let reply = match args {
        ["run", "view", run, "--repo", r, "--log-failed"] if (*run, *r) == (&id, repo) => failed,
        ["run", "view", run, "--json", "jobs", "--repo", r] if (*run, *r) == (&id, repo) => {
            return Ok(JOBS.to_vec());
        }
        [
            "run",
            "view",
            run,
            "--job",
            "114900834180",
            "--log",
            "--repo",
            r,
        ] if (*run, *r) == (&id, repo) => job_log,
        other => panic!("gh was asked {other:?}"),
    };
    Ok(reply.as_bytes().to_vec())
}

// Reads run 38275563600 as `gh` answers with `failed` and `job_log`, and returns
// what was read with each call `gh` was asked
fn read(failed: &str, job_log: &str) -> (RunLog, Vec<String>) {
    let repo = ForgeSlug::try_from("shep-pm/shep-kelpie".to_owned()).unwrap();
    let mut asked = Vec::new();
    let read = read_run(&repo, RUN, &mut |args: &[&str]| {
        asked.push(args.join(" "));
        answer(args, failed, job_log)
    })
    .unwrap();
    (read, asked)
}

#[test]
fn a_run_that_failed_keeps_reading_log_failed() {
    let failed = "test\tRun cargo test\t2026-10-13T10:00:00.0000000Z error: boom";

    let (read, asked) = read(failed, JOB_LOG);

    assert_eq!(read.text, "test\tRun cargo test\terror: boom");
    assert!(!read.timed_out);
    assert_eq!(
        asked,
        [format!(
            "run view {RUN} --repo shep-pm/shep-kelpie --log-failed"
        )]
    );
}

#[test]
fn an_empty_failed_log_falls_back_to_the_cancelled_jobs_log() {
    let (read, asked) = read("", JOB_LOG);

    assert!(read.timed_out);
    assert_eq!(
        asked,
        [
            format!("run view {RUN} --repo shep-pm/shep-kelpie --log-failed"),
            format!("run view {RUN} --json jobs --repo shep-pm/shep-kelpie"),
            format!("run view {RUN} --job 114900834180 --log --repo shep-pm/shep-kelpie"),
        ]
    );
}

#[test]
fn a_timed_out_tail_names_the_job_the_step_and_its_elapsed_time() {
    let (read, _) = read("", JOB_LOG);

    let mut lines = read.text.lines();
    assert_eq!(lines.next(), Some(NOTE));
    let kept: Vec<&str> = lines.collect();
    assert!(!kept.is_empty());
    assert!(
        kept.iter()
            .all(|l| l.starts_with("test-macos\tUNKNOWN STEP\t"))
    );
    assert!(
        kept.contains(&"test-macos\tUNKNOWN STEP\t##[error]The operation was canceled."),
        "{kept:?}"
    );
}

#[test]
fn a_timed_out_tail_is_cut_to_the_same_bytes_as_a_failed_one() {
    let line = "test-macos\tUNKNOWN STEP\t2026-10-12T16:33:08.5367760Z test é::a_test ... ok\n";
    let long = line.repeat(1000);

    let (read, _) = read("", &long);

    let kept = read.text.strip_prefix(&format!("{NOTE}\n")).unwrap();
    assert!(kept.len() <= LOG_BYTES, "{}", kept.len());
    assert_eq!(kept, failure_tail(&long, LOG_BYTES));
    assert!(kept.starts_with("test-macos\tUNKNOWN STEP\ttest é::a_test"));
}

#[test]
fn a_run_cancelled_in_no_job_has_no_log_to_read() {
    let repo = ForgeSlug::try_from("shep-pm/shep-kelpie".to_owned()).unwrap();
    let no_jobs = br#"{"jobs":[{"databaseId":1,"name":"lint","conclusion":"success","startedAt":"2026-10-12T16:12:47Z","completedAt":"2026-10-12T16:13:44Z","steps":[]}]}"#;

    let read = read_run(&repo, 5, &mut |args: &[&str]| match args {
        [.., "--log-failed"] => Ok(Vec::new()),
        _ => Ok(no_jobs.to_vec()),
    })
    .unwrap();

    assert_eq!(read.text, "");
    assert!(!read.timed_out);
}
