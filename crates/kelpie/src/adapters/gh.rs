//! GitHub, through the `gh` command line

use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::board::{OpenPullRequest, READY, ReadyIssue};
use crate::ports::{Forge, ForgeError, Issue, Visibility};
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
        parse_ready_issues(&gh(&[
            "issue",
            "list",
            "--repo",
            repo.as_str(),
            "--label",
            READY,
            "--state",
            "open",
            "--limit",
            LIST_LIMIT,
            "--json",
            "number,assignees,labels",
        ])?)
    }

    fn open_pull_requests(&self, repo: &ForgeSlug) -> Result<Vec<OpenPullRequest>, ForgeError> {
        parse_pull_requests(
            &gh(&[
                "pr",
                "list",
                "--repo",
                repo.as_str(),
                "--state",
                "open",
                "--limit",
                LIST_LIMIT,
                "--json",
                "number,headRefName,closingIssuesReferences",
            ])?,
            repo,
        )
    }
}

// `gh` lists newest first and stops at its limit, so a short limit would
// hide the oldest ready issue. A board this long is not expected.
const LIST_LIMIT: &str = "1000";

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

fn parse_ready_issues(stdout: &[u8]) -> Result<Vec<ReadyIssue>, ForgeError> {
    #[derive(Deserialize)]
    struct Listed {
        number: u64,
        assignees: Vec<serde::de::IgnoredAny>,
        labels: Vec<Label>,
    }
    let listed: Vec<Listed> = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(listed
        .into_iter()
        .map(|i| ReadyIssue {
            number: i.number,
            assigned: !i.assignees.is_empty(),
            labels: i.labels.into_iter().map(|l| l.name).collect(),
        })
        .collect())
}

// A pull request can close issues on other repos, which are not this board's.
fn parse_pull_requests(
    stdout: &[u8],
    repo: &ForgeSlug,
) -> Result<Vec<OpenPullRequest>, ForgeError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Listed {
        number: u64,
        head_ref_name: String,
        closing_issues_references: Vec<Closes>,
    }
    #[derive(Deserialize)]
    struct Closes {
        number: u64,
        repository: Repository,
    }
    #[derive(Deserialize)]
    struct Repository {
        name: String,
        owner: Owner,
    }
    #[derive(Deserialize)]
    struct Owner {
        login: String,
    }
    let listed: Vec<Listed> = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    let ours = |r: &Repository| {
        let (owner, name) = repo.as_str().split_once('/').unwrap_or_default();
        r.owner.login.eq_ignore_ascii_case(owner) && r.name.eq_ignore_ascii_case(name)
    };
    Ok(listed
        .into_iter()
        .map(|pr| OpenPullRequest {
            number: pr.number,
            head: pr.head_ref_name,
            closes: pr
                .closing_issues_references
                .into_iter()
                .filter(|c| ours(&c.repository))
                .map(|c| c.number)
                .collect(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded from gh 2.96 on this repo: `gh issue view 6 --json title,body,labels`.
    const ISSUE: &str = include_str!("../../fixtures/gh-issue-view.json");

    // Recorded from gh 2.96 on this repo with the `ready_issues` arguments,
    // `--limit 3`.
    const READY_LIST: &str = include_str!("../../fixtures/gh-issue-list.json");

    // Recorded from gh 2.96 on this repo with the `open_pull_requests` arguments.
    const PR_LIST: &str = include_str!("../../fixtures/gh-pr-list.json");

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

    #[test]
    fn ready_issues_are_read() {
        let issues = parse_ready_issues(READY_LIST.as_bytes()).unwrap();
        let numbers: Vec<u64> = issues.iter().map(|i| i.number).collect();
        assert_eq!(numbers, [16, 15, 14]);
        assert!(issues.iter().all(|i| !i.assigned));
        assert_eq!(issues[0].labels, ["ready-for-agent"]);
    }

    #[test]
    fn an_issue_with_an_assignee_is_assigned() {
        let listed =
            br#"[{"assignees":[{"id":"MDQ","login":"someone","name":""}],"labels":[],"number":3}]"#;
        assert!(parse_ready_issues(listed).unwrap()[0].assigned);
    }

    #[test]
    fn open_pull_requests_are_read_with_the_issues_they_close() {
        let slug = ForgeSlug::try_from("shep-pm/shep-kelpie".to_owned()).unwrap();
        let prs = parse_pull_requests(PR_LIST.as_bytes(), &slug).unwrap();
        assert_eq!(
            prs,
            [OpenPullRequest {
                number: 22,
                head: "feat/10-kelpie-dog-leases".into(),
                closes: vec![10],
            }]
        );
    }

    #[test]
    fn an_issue_closed_on_another_repo_is_not_this_boards() {
        let slug = ForgeSlug::try_from("shep-pm/shep".to_owned()).unwrap();
        let prs = parse_pull_requests(PR_LIST.as_bytes(), &slug).unwrap();
        assert!(prs[0].closes.is_empty());
    }
}
