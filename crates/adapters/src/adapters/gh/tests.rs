use super::*;
use crate::test::ticket;

// Recorded from gh 2.96 on this repo: `gh issue view 6 --json title,body,labels,state`.
const ISSUE: &str = include_str!("../../../../../fixtures/gh-issue-view.json");

// Recorded from gh 2.96 on cli/cli:
// `gh issue view 14528 --json title,body,labels,state,parent,blockedBy`.
const SUB_ISSUE: &str = include_str!("../../../../../fixtures/gh-issue-view-sub-issue.json");

#[test]
fn an_issue_is_read_with_its_parent_and_blockers() {
    let issue = parse_issue(SUB_ISSUE.as_bytes()).unwrap();
    assert_eq!(
        (issue.parent, issue.blocked_by),
        (Some(ticket(14529)), vec![])
    );
    let blocked = br#"{"title":"t","body":"","labels":[],"state":"OPEN","parent":null,
        "blockedBy":{"nodes":[{"number":3,"state":"OPEN"}],"totalCount":1}}"#;
    let issue = parse_issue(blocked).unwrap();
    assert_eq!((issue.parent, issue.blocked_by), (None, vec![ticket(3)]));
}

#[test]
fn visibility_is_read() {
    let read = |s: &str| parse_visibility(s.as_bytes());
    assert_eq!(read(r#"{"visibility":"PUBLIC"}"#), Ok(Visibility::Public));
    assert_eq!(read(r#"{"visibility":"PRIVATE"}"#), Ok(Visibility::Private));
    assert_eq!(
        read(r#"{"visibility":"INTERNAL"}"#),
        Ok(Visibility::Internal)
    );
    assert!(matches!(
        read(r#"{"visibility":"SECRET"}"#),
        Err(ForgeError::Unreadable(_))
    ));
}

#[test]
fn a_repo_s_label_names_are_read() {
    let read = |s: &str| parse_label_names(s.as_bytes());
    assert_eq!(
        read(r#"[{"name":"bug"},{"name":"review please"}]"#),
        Ok(vec!["bug".to_owned(), "review please".to_owned()])
    );
    assert!(matches!(read("{}"), Err(ForgeError::Unreadable(_))));
}

#[test]
fn the_default_branch_is_read() {
    let read = |s: &str| parse_default_branch(s.as_bytes());
    assert_eq!(
        read(r#"{"defaultBranchRef":{"name":"trunk"}}"#),
        Ok("trunk".to_owned())
    );
    assert!(matches!(read("{}"), Err(ForgeError::Unreadable(_))));
}

#[test]
fn an_issue_is_read() {
    let issue = parse_issue(ISSUE.as_bytes()).unwrap();
    assert_eq!(issue.title, "Project runner skeleton with a status trigger");
    assert!(
        issue.body.starts_with("## Parent\n\n#5\n"),
        "{}",
        issue.body
    );
    assert_eq!(issue.labels, ["ready-for-agent"]);
    assert!(!issue.open);
}

#[test]
fn an_open_issue_is_read_as_open() {
    let issue = parse_issue(br#"{"title":"t","body":"","labels":[],"state":"OPEN"}"#).unwrap();
    assert!(issue.open);
}

#[test]
fn an_issue_without_a_body_has_an_empty_one() {
    let issue = parse_issue(br#"{"title":"t","body":null,"labels":[],"state":"OPEN"}"#).unwrap();
    assert_eq!(issue.body, "");
}

#[test]
fn an_issue_without_a_title_is_unreadable() {
    assert!(matches!(
        parse_issue(br#"{"body":"x"}"#),
        Err(ForgeError::Unreadable(_))
    ));
}

// Recorded from gh 2.96: `gh pr view 23 --repo shep-pm/shep-kelpie` with
// the `pull_request` arguments.
const PR_GREEN: &str = include_str!("../../../../../fixtures/gh-pr-view-green.json");

// Recorded the same way from shep-pm/shep#625: skipped check runs and a
// CodeRabbit commit status.
const PR_SKIPPED: &str = include_str!("../../../../../fixtures/gh-pr-view-skipped.json");

// The check runs GitHub recorded on this repo's commit edc8526, a red
// lint, read through GraphQL and set in `gh pr view`'s shape.
const PR_RED: &str = include_str!("../../../../../fixtures/gh-pr-view-red.json");

fn rollup(checks: &str) -> Checks {
    let view = format!(
        r#"{{"headRefOid":"abc","isDraft":true,"state":"OPEN","statusCheckRollup":{checks}}}"#
    );
    parse_pull_request(view.as_bytes()).unwrap().checks
}

#[test]
fn a_merged_pull_request_with_every_check_green_is_read() {
    let pr = parse_pull_request(PR_GREEN.as_bytes()).unwrap();
    assert_eq!(
        pr,
        PullRequest {
            state: PullRequestState::Merged,
            draft: false,
            head: "baea925a2ed5358932b3506e99ecb9f20cba5e2c".into(),
            checks: Checks::Passed,
            labels: vec![],
        }
    );
}

// Recorded the same way, labels included, from shep-pm/shep#598.
const PR_LABELLED: &str = include_str!("../../../../../fixtures/gh-pr-view-labelled.json");

#[test]
fn a_pull_request_is_read_with_its_labels() {
    let pr = parse_pull_request(PR_LABELLED.as_bytes()).unwrap();
    assert_eq!(pr.labels, ["review please"]);
}

#[test]
fn a_pull_requests_labels_are_read() {
    let view = br#"{"headRefOid":"abc","isDraft":false,"state":"OPEN",
        "statusCheckRollup":[],"labels":[{"name":"review please"},{"name":"bug"}]}"#;
    let pr = parse_pull_request(view).unwrap();
    assert_eq!(pr.labels, ["review please", "bug"]);
}

#[test]
fn a_pull_request_recorded_before_labels_were_read_has_none() {
    let view = br#"{"headRefOid":"abc","isDraft":false,"state":"OPEN","statusCheckRollup":[]}"#;
    assert_eq!(
        parse_pull_request(view).unwrap().labels,
        Vec::<String>::new()
    );
}

#[test]
fn skipped_runs_and_a_green_commit_status_pass() {
    let pr = parse_pull_request(PR_SKIPPED.as_bytes()).unwrap();
    assert_eq!(pr.checks, Checks::Passed);
}

// Recorded the same way from #1653: CI green, and kelpie's own `kelpie` and
// `kelpie / review` check runs still in progress on the head.
const PR_GATES: &str = include_str!("../../../../../fixtures/gh-pr-view-kelpie-gates.json");

#[test]
fn kelpies_own_check_runs_are_not_ci() {
    let pr = parse_pull_request(PR_GATES.as_bytes()).unwrap();
    assert_eq!(pr.checks, Checks::Passed);
}

#[test]
fn a_finished_kelpie_check_run_is_not_ci_either() {
    let failed = r#"[{"__typename":"CheckRun","name":"kelpie / ruling","status":"COMPLETED",
        "conclusion":"FAILURE","detailsUrl":"https://github.com/shep-pm/shep-kelpie"},
        {"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"SUCCESS"}]"#;
    assert_eq!(rollup(failed), Checks::Passed);
}

#[test]
fn a_head_with_only_kelpies_check_runs_has_no_ci() {
    let only = r#"[{"__typename":"CheckRun","name":"kelpie","status":"IN_PROGRESS",
        "conclusion":"","detailsUrl":"https://github.com/shep-pm/shep-kelpie"}]"#;
    assert_eq!(rollup(only), Checks::None);
}

#[test]
fn a_workflow_job_named_like_a_kelpie_check_is_ci() {
    let job = r#"[{"__typename":"CheckRun","name":"kelpie","status":"IN_PROGRESS",
        "conclusion":"","detailsUrl":"https://github.com/o/r/actions/runs/7/job/8"}]"#;
    assert_eq!(
        rollup(job),
        Checks::Pending(vec![Queued {
            name: "kelpie".into(),
            run: Some(7),
            running: true,
        }])
    );
}

// The shape `gh pr view --json state,isDraft,headRefOid,statusCheckRollup,labels`
// gives shep-pm/shep#982, where the `windows-gnu` job of a finished run
// stayed queued.
const PR_QUEUED: &str = include_str!("../../../../../fixtures/gh-pr-view-queued.json");

#[test]
fn a_queued_check_run_is_read_with_its_workflow_run() {
    let pr = parse_pull_request(PR_QUEUED.as_bytes()).unwrap();
    assert_eq!(
        pr.checks,
        Checks::Pending(vec![
            Queued {
                name: "windows-gnu".into(),
                run: Some(39_697_676_985),
                running: false,
            },
            // Still running, and listed as the gate cannot tell it from a stuck one
            Queued {
                name: "docs".into(),
                run: Some(39_697_679_104),
                running: true,
            },
        ])
    );
}

#[test]
fn a_queued_check_run_whose_link_names_no_run_has_none() {
    let queued = r#"{"__typename":"CheckRun","name":"deploy","status":"QUEUED","conclusion":"","detailsUrl":"https://ci.example.com/jobs/4"}"#;
    let unlinked = r#"{"__typename":"CheckRun","name":"build","status":"QUEUED","conclusion":""}"#;
    assert_eq!(
        rollup(&format!("[{queued},{unlinked}]")),
        Checks::Pending(vec![
            Queued {
                name: "deploy".into(),
                run: None,
                running: false,
            },
            Queued {
                name: "build".into(),
                run: None,
                running: false,
            },
        ])
    );
}

// The same shape for shep-pm/shep#1050, where the `typos` job of a finished
// run kept `IN_PROGRESS` beside a `completedAt` and a `SUCCESS`.
const PR_IN_PROGRESS: &str = include_str!("../../../../../fixtures/gh-pr-view-in-progress.json");

#[test]
fn a_check_run_left_in_progress_with_a_conclusion_is_read_by_its_conclusion() {
    let pr = parse_pull_request(PR_IN_PROGRESS.as_bytes()).unwrap();
    assert_eq!(pr.checks, Checks::Passed);
}

#[test]
fn a_finished_check_run_is_read_by_its_conclusion_whatever_its_status() {
    let failed = r#"{"__typename":"CheckRun","name":"lint","status":"IN_PROGRESS","conclusion":"FAILURE","completedAt":"2026-10-23T05:56:37Z"}"#;
    let skipped = r#"{"__typename":"CheckRun","name":"test","status":"QUEUED","conclusion":"SKIPPED","completedAt":"2026-10-23T05:56:37Z"}"#;
    assert_eq!(
        rollup(&format!("[{failed}]")),
        Checks::Failed(vec!["lint".into()])
    );
    assert_eq!(rollup(&format!("[{skipped}]")), Checks::Skipped);
}

#[test]
fn a_running_check_run_is_pending_whatever_its_other_fields_say() {
    // GitHub fills an unfinished run's `completedAt` with the zero time
    for fields in [
        r#""completedAt":"0001-01-01T00:00:00Z","conclusion":"""#,
        r#""completedAt":"0001-01-01T00:00:00Z","conclusion":"SUCCESS""#,
        r#""completedAt":"2026-10-23T05:56:37Z","conclusion":"""#,
        r#""conclusion":"SUCCESS""#,
        r#""conclusion":null"#,
    ] {
        let run = format!(
            r#"[{{"__typename":"CheckRun","name":"typos","status":"IN_PROGRESS",{fields}}}]"#
        );
        assert!(matches!(rollup(&run), Checks::Pending(_)), "{run}");
    }
}

#[test]
fn a_check_run_still_in_progress_is_listed_with_its_workflow_run() {
    let running = r#"{"__typename":"CheckRun","name":"docs","status":"IN_PROGRESS","conclusion":"","completedAt":"0001-01-01T00:00:00Z","detailsUrl":"https://github.com/shep-pm/shep/actions/runs/39697679104/job/117210601377"}"#;
    let done = r#"{"__typename":"CheckRun","name":"typos","status":"IN_PROGRESS","conclusion":"SUCCESS","completedAt":"2026-10-23T05:56:37Z","detailsUrl":"https://github.com/shep-pm/shep/actions/runs/39908548395/job/117840336197"}"#;
    assert_eq!(
        rollup(&format!("[{done},{running}]")),
        Checks::Pending(vec![Queued {
            name: "docs".into(),
            run: Some(39_697_679_104),
            running: true,
        }])
    );
}

#[test]
fn a_red_run_names_its_failed_checks() {
    let pr = parse_pull_request(PR_RED.as_bytes()).unwrap();
    assert_eq!(
        (pr.state, pr.draft, pr.checks),
        (
            PullRequestState::Open,
            true,
            Checks::Failed(vec!["lint".into()])
        )
    );
}

#[test]
fn skipped_runs_pass_beside_a_run_that_ran_and_are_skipped_alone() {
    let skipped =
        r#"{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SKIPPED"}"#;
    let passed =
        r#"{"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"SUCCESS"}"#;
    assert_eq!(rollup(&format!("[{skipped},{skipped}]")), Checks::Skipped);
    assert_eq!(rollup(&format!("[{skipped},{passed}]")), Checks::Passed);
}

#[test]
fn no_checks_is_not_green() {
    assert_eq!(rollup("[]"), Checks::None);
}

#[test]
fn a_running_check_holds_the_verdict_even_beside_a_failure() {
    let failed =
        r#"{"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"FAILURE"}"#;
    for running in [
        r#"{"__typename":"CheckRun","name":"test","status":"IN_PROGRESS","conclusion":""}"#,
        r#"{"__typename":"CheckRun","name":"test","status":"QUEUED","conclusion":null}"#,
        r#"{"__typename":"StatusContext","context":"CodeRabbit","state":"PENDING"}"#,
    ] {
        assert!(matches!(
            rollup(&format!("[{failed},{running}]")),
            Checks::Pending(_)
        ));
    }
}

#[test]
fn a_cancelled_run_and_an_errored_status_fail() {
    let checks = r#"[
        {"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"CANCELLED"},
        {"__typename":"StatusContext","context":"ci/other","state":"ERROR"}
    ]"#;
    assert_eq!(
        rollup(checks),
        Checks::Failed(vec!["test".into(), "ci/other".into()])
    );
}

#[test]
fn a_state_gh_does_not_name_is_unreadable() {
    let view = br#"{"headRefOid":"a","isDraft":false,"state":"LOCKED","statusCheckRollup":[]}"#;
    assert!(matches!(
        parse_pull_request(view),
        Err(ForgeError::Unreadable(_))
    ));
}

#[test]
fn a_rerun_of_failed_jobs_passes_failed_and_a_rerun_of_all_does_not() {
    let slug = ForgeSlug::try_from("shep-pm/shep".to_owned()).unwrap();
    assert_eq!(
        runs::rerun_args(&slug, "7", Rerun::Failed),
        ["run", "rerun", "7", "--repo", "shep-pm/shep", "--failed"]
    );
    assert_eq!(
        runs::rerun_args(&slug, "7", Rerun::All),
        ["run", "rerun", "7", "--repo", "shep-pm/shep"]
    );
}

#[test]
fn a_merge_is_a_merge_commit_of_the_ruled_head_only() {
    let slug = ForgeSlug::try_from("shep-pm/shep".to_owned()).unwrap();
    let args = merge_args(&slug, "30", "1a2b");
    assert_eq!(
        args,
        [
            "pr",
            "merge",
            "30",
            "--repo",
            "shep-pm/shep",
            "--merge",
            "--match-head-commit",
            "1a2b",
            "--body",
            ""
        ]
    );
    for never in ["--squash", "-s", "--rebase", "-r", "--auto", "--admin"] {
        assert!(!args.contains(&never), "{never}");
    }
}
