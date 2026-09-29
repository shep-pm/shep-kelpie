//! GitHub, through the `gh` command line

mod board;
pub(crate) mod coderabbit;
mod review;

use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::board::{OpenPullRequest, ReadyIssue};
use crate::coderabbit::Activity;
use crate::ports::{
    Checks, Forge, ForgeError, Issue, PullRequest, PullRequestState, Reviewed, Visibility,
};
use crate::settings::ForgeSlug;

/// GitHub, through the `gh` command line
#[derive(Debug, Clone, Copy, Default)]
pub struct Gh;

impl Forge for Gh {
    fn visibility(&self, repo: &ForgeSlug) -> Result<Visibility, ForgeError> {
        parse_visibility(&gh(&[
            "repo",
            "view",
            repo.as_str(),
            "--json",
            "visibility",
        ])?)
    }

    fn issue(&self, repo: &ForgeSlug, number: u64) -> Result<Issue, ForgeError> {
        let number = number.to_string();
        parse_issue(&gh(&[
            "issue",
            "view",
            &number,
            "--repo",
            repo.as_str(),
            "--json",
            "title,body,labels",
        ])?)
    }

    fn ready_issues(&self, repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError> {
        board::ready_issues(repo)
    }

    fn open_pull_requests(&self, repo: &ForgeSlug) -> Result<Vec<OpenPullRequest>, ForgeError> {
        board::open_pull_requests(repo)
    }

    fn pull_request(&self, repo: &ForgeSlug, number: u64) -> Result<PullRequest, ForgeError> {
        let number = number.to_string();
        parse_pull_request(&gh(&[
            "pr",
            "view",
            &number,
            "--repo",
            repo.as_str(),
            "--json",
            "state,isDraft,headRefOid,statusCheckRollup,labels",
        ])?)
    }

    fn reviewed(&self, repo: &ForgeSlug, number: u64) -> Result<Reviewed, ForgeError> {
        review::reviewed(repo, number)
    }

    fn viewer(&self) -> Result<String, ForgeError> {
        review::viewer()
    }

    fn comment(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<(), ForgeError> {
        let number = number.to_string();
        let args = [
            "pr",
            "comment",
            &number,
            "--repo",
            repo.as_str(),
            "--body",
            body,
        ];
        gh(&args).map(drop)
    }

    fn post_comment(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<u64, ForgeError> {
        let path = format!("repos/{}/issues/{number}/comments", repo.as_str());
        let field = format!("body={body}");
        let args = [
            "api", "--method", "POST", &path, "-f", &field, "--jq", ".id",
        ];
        let out = gh(&args)?;
        let text = String::from_utf8_lossy(&out);
        text.trim()
            .parse()
            .map_err(|_| ForgeError::Unreadable(text.into_owned()))
    }

    fn edit_comment(&self, repo: &ForgeSlug, id: u64, body: &str) -> Result<(), ForgeError> {
        let path = format!("repos/{}/issues/comments/{id}", repo.as_str());
        let field = format!("body={body}");
        let args = ["api", "--method", "PATCH", &path, "-f", &field, "--silent"];
        gh(&args).map(drop)
    }

    fn mark_ready(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        let number = number.to_string();
        gh(&["pr", "ready", &number, "--repo", repo.as_str()]).map(drop)
    }

    fn set_label(
        &self,
        repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        coderabbit::label(repo, number, label, on)
    }

    fn coderabbit(&self, repo: &ForgeSlug, number: u64) -> Result<Activity, ForgeError> {
        coderabbit::activity(repo, number)
    }

    fn resolve_thread(&self, _repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError> {
        coderabbit::resolve(thread)
    }

    fn merge(&self, repo: &ForgeSlug, number: u64, head: &str) -> Result<(), ForgeError> {
        let number = number.to_string();
        gh(&merge_args(repo, &number, head)).map(drop)
    }
}

// A merge commit, and only of the head the ruling was about. No
// `--delete-branch`: gh would also switch branches in its working folder.
fn merge_args<'a>(repo: &'a ForgeSlug, number: &'a str, head: &'a str) -> [&'a str; 8] {
    [
        "pr",
        "merge",
        number,
        "--repo",
        repo.as_str(),
        "--merge",
        "--match-head-commit",
        head,
    ]
}

fn gh(args: &[&str]) -> Result<Vec<u8>, ForgeError> {
    let output = Command::new("gh")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| ForgeError::Spawn(e.to_string()))?;
    if !output.status.success() {
        return Err(ForgeError::Failed(
            String::from_utf8_lossy(&output.stderr).into(),
        ));
    }
    Ok(output.stdout)
}

fn unreadable(stdout: &[u8]) -> ForgeError {
    ForgeError::Unreadable(String::from_utf8_lossy(stdout).into())
}

fn parse_visibility(stdout: &[u8]) -> Result<Visibility, ForgeError> {
    #[derive(Deserialize)]
    struct View {
        visibility: String,
    }
    let view: View = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    match view.visibility.as_str() {
        "PUBLIC" => Ok(Visibility::Public),
        "PRIVATE" => Ok(Visibility::Private),
        "INTERNAL" => Ok(Visibility::Internal),
        _ => Err(unreadable(stdout)),
    }
}

fn parse_issue(stdout: &[u8]) -> Result<Issue, ForgeError> {
    #[derive(Deserialize)]
    struct View {
        title: String,
        body: Option<String>,
        labels: Vec<Label>,
    }
    let view: View = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(Issue {
        title: view.title,
        body: view.body.unwrap_or_default(),
        labels: view.labels.into_iter().map(|l| l.name).collect(),
    })
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

fn pull_request_state(state: &str, stdout: &[u8]) -> Result<PullRequestState, ForgeError> {
    match state {
        "OPEN" => Ok(PullRequestState::Open),
        "MERGED" => Ok(PullRequestState::Merged),
        "CLOSED" => Ok(PullRequestState::Closed),
        _ => Err(unreadable(stdout)),
    }
}

fn parse_pull_request(stdout: &[u8]) -> Result<PullRequest, ForgeError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct View {
        state: String,
        is_draft: bool,
        head_ref_oid: String,
        status_check_rollup: Vec<Check>,
        #[serde(default)]
        labels: Vec<Label>,
    }
    let view: View = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(PullRequest {
        state: pull_request_state(&view.state, stdout)?,
        draft: view.is_draft,
        head: view.head_ref_oid,
        checks: checks(&view.status_check_rollup),
        labels: view.labels.into_iter().map(|l| l.name).collect(),
    })
}

// GitHub has two kinds of check: an Actions-style check run, and a commit
// status posted by an outside service such as CodeRabbit.
#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum Check {
    CheckRun {
        name: String,
        status: Option<String>,
        conclusion: Option<String>,
    },
    StatusContext {
        context: String,
        state: Option<String>,
    },
}

enum Outcome {
    Pending,
    Passed,
    Failed,
}

impl Check {
    fn outcome(&self) -> (&str, Outcome) {
        match self {
            Self::CheckRun {
                name,
                status,
                conclusion,
            } => {
                let outcome = match (status.as_deref(), conclusion.as_deref()) {
                    (Some("COMPLETED"), Some("SUCCESS" | "NEUTRAL" | "SKIPPED")) => Outcome::Passed,
                    (Some("COMPLETED"), _) => Outcome::Failed,
                    _ => Outcome::Pending,
                };
                (name, outcome)
            }
            Self::StatusContext { context, state } => {
                let outcome = match state.as_deref() {
                    Some("SUCCESS") => Outcome::Passed,
                    Some("FAILURE" | "ERROR") => Outcome::Failed,
                    _ => Outcome::Pending,
                };
                (context, outcome)
            }
        }
    }
}

// The whole run is read before a verdict, so a red run names every failure.
fn checks(rollup: &[Check]) -> Checks {
    if rollup.is_empty() {
        return Checks::None;
    }
    let mut failed = Vec::new();
    for check in rollup {
        match check.outcome() {
            (_, Outcome::Pending) => return Checks::Pending,
            (name, Outcome::Failed) => failed.push(name.to_owned()),
            (_, Outcome::Passed) => {}
        }
    }
    if failed.is_empty() {
        Checks::Passed
    } else {
        Checks::Failed(failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded from gh 2.96 on this repo: `gh issue view 6 --json title,body,labels`.
    const ISSUE: &str = include_str!("../../fixtures/gh-issue-view.json");

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
    fn an_issue_is_read() {
        let issue = parse_issue(ISSUE.as_bytes()).unwrap();
        assert_eq!(issue.title, "Project runner skeleton with a status trigger");
        assert!(
            issue.body.starts_with("## Parent\n\n#5\n"),
            "{}",
            issue.body
        );
        assert_eq!(issue.labels, ["ready-for-agent"]);
    }

    #[test]
    fn an_issue_without_a_body_has_an_empty_one() {
        let issue = parse_issue(br#"{"title":"t","body":null,"labels":[]}"#).unwrap();
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
    const PR_GREEN: &str = include_str!("../../fixtures/gh-pr-view-green.json");

    // Recorded the same way from shep-pm/shep#625: skipped check runs and a
    // CodeRabbit commit status.
    const PR_SKIPPED: &str = include_str!("../../fixtures/gh-pr-view-skipped.json");

    // The check runs GitHub recorded on this repo's commit edc8526, a red
    // lint, read through GraphQL and set in `gh pr view`'s shape.
    const PR_RED: &str = include_str!("../../fixtures/gh-pr-view-red.json");

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
    const PR_LABELLED: &str = include_str!("../../fixtures/gh-pr-view-labelled.json");

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
    fn no_checks_is_not_green() {
        assert_eq!(rollup("[]"), Checks::None);
    }

    #[test]
    fn a_running_check_holds_the_verdict_even_beside_a_failure() {
        let failed = r#"{"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"FAILURE"}"#;
        for running in [
            r#"{"__typename":"CheckRun","name":"test","status":"IN_PROGRESS","conclusion":""}"#,
            r#"{"__typename":"CheckRun","name":"test","status":"QUEUED","conclusion":null}"#,
            r#"{"__typename":"StatusContext","context":"CodeRabbit","state":"PENDING"}"#,
        ] {
            assert_eq!(rollup(&format!("[{failed},{running}]")), Checks::Pending);
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
                "1a2b"
            ]
        );
        for never in ["--squash", "-s", "--rebase", "-r", "--auto", "--admin"] {
            assert!(!args.contains(&never), "{never}");
        }
    }
}
