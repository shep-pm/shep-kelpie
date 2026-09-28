//! The board's two lists: the ready issues, with what blocks each, and the
//! open pull requests, with the issues each closes

use serde::Deserialize;

use super::{Label, gh, unreadable};
use crate::board::{Blocker, OpenPullRequest, READY, ReadyIssue};
use crate::ports::ForgeError;
use crate::settings::ForgeSlug;

// `gh` lists newest first and stops at its limit, so a short limit would
// hide the oldest item. A list this long is not expected.
const LIST_LIMIT: &str = "1000";

pub(super) fn ready_issues(repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError> {
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
        "number,assignees,labels,blockedBy",
    ])?)
}

pub(super) fn open_pull_requests(repo: &ForgeSlug) -> Result<Vec<OpenPullRequest>, ForgeError> {
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

// `gh` reads an issue's first 50 blockers; more is not expected.
fn parse_ready_issues(stdout: &[u8]) -> Result<Vec<ReadyIssue>, ForgeError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Listed {
        number: u64,
        assignees: Vec<serde::de::IgnoredAny>,
        labels: Vec<Label>,
        blocked_by: BlockedBy,
    }
    #[derive(Deserialize)]
    struct BlockedBy {
        nodes: Vec<Blocking>,
    }
    #[derive(Deserialize)]
    struct Blocking {
        number: u64,
        state: State,
    }
    #[derive(Deserialize, PartialEq)]
    #[serde(rename_all = "UPPERCASE")]
    enum State {
        Open,
        Closed,
    }
    let listed: Vec<Listed> = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(listed
        .into_iter()
        .map(|i| ReadyIssue {
            number: i.number,
            assigned: !i.assignees.is_empty(),
            labels: i.labels.into_iter().map(|l| l.name).collect(),
            blocked_by: i
                .blocked_by
                .nodes
                .into_iter()
                .map(|b| Blocker {
                    number: b.number,
                    open: b.state == State::Open,
                })
                .collect(),
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

    // Recorded from gh 2.96 on this repo with the `ready_issues` arguments.
    const READY_LIST: &str = include_str!("../../../fixtures/gh-issue-list.json");

    // Recorded from gh 2.96 on this repo with the `open_pull_requests` arguments.
    const PR_LIST: &str = include_str!("../../../fixtures/gh-pr-list.json");

    fn blocker(number: u64, open: bool) -> Blocker {
        Blocker { number, open }
    }

    #[test]
    fn ready_issues_are_read() {
        let issues = parse_ready_issues(READY_LIST.as_bytes()).unwrap();
        let numbers: Vec<u64> = issues.iter().map(|i| i.number).collect();
        assert_eq!(numbers, [40, 38, 37, 33, 32, 30, 27, 19, 5]);
        assert!(issues.iter().all(|i| !i.assigned));
        assert_eq!(issues[0].labels, ["bug", "ready-for-agent"]);
    }

    #[test]
    fn each_ready_issue_carries_its_blockers_open_or_closed() {
        let issues = parse_ready_issues(READY_LIST.as_bytes()).unwrap();
        assert_eq!(issues[0].blocked_by, []);
        assert_eq!(issues[1].blocked_by, [blocker(12, false)]);
        assert_eq!(
            issues[6].blocked_by,
            [
                blocker(40, true),
                blocker(37, true),
                blocker(38, true),
                blocker(33, true),
                blocker(32, true),
                blocker(30, true),
                blocker(24, false),
                blocker(19, true),
            ]
        );
    }

    #[test]
    fn an_issue_with_an_assignee_is_assigned() {
        let listed = br#"[{"assignees":[{"id":"MDQ","login":"someone","name":""}],"labels":[],
            "number":3,"blockedBy":{"nodes":[],"totalCount":0}}]"#;
        assert!(parse_ready_issues(listed).unwrap()[0].assigned);
    }

    #[test]
    fn a_blocker_in_a_state_not_known_is_unreadable() {
        let listed = br#"[{"assignees":[],"labels":[],"number":3,
            "blockedBy":{"nodes":[{"number":2,"state":"MERGED"}],"totalCount":1}}]"#;
        assert!(matches!(
            parse_ready_issues(listed),
            Err(ForgeError::Unreadable(_))
        ));
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
