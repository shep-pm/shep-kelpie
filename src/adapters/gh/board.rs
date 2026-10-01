//! The board's two lists: the ready issues, with what blocks each, and the
//! open pull requests, with the issues each closes

use serde::Deserialize;

use super::{Label, gh, unreadable};
use crate::board::{Blocker, OpenPullRequest, READY, ReadyIssue, SubIssues};
use crate::ports::ForgeError;
use crate::settings::ForgeSlug;

// `gh` lists newest first and stops at its limit, so a short limit would
// hide the oldest item. A list this long is not expected.
const LIST_LIMIT: &str = "1000";

pub(super) fn ready_issues(repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError> {
    parse_ready_issues(&gh(&ready_args(repo))?)
}

// `gh issue list` lists issues only, so a pull request labelled
// `ready-for-agent` to ask for a rework never reads as new work.
fn ready_args(repo: &ForgeSlug) -> [&str; 12] {
    [
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
        "number,assignees,labels,blockedBy,parent,subIssuesSummary",
    ]
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
            "number,headRefName,closingIssuesReferences,labels",
        ])?,
        repo,
    )
}

// `gh` lists an issue's first 50 blockers and counts them all.
fn parse_ready_issues(stdout: &[u8]) -> Result<Vec<ReadyIssue>, ForgeError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Listed {
        number: u64,
        assignees: Vec<serde::de::IgnoredAny>,
        labels: Vec<Label>,
        blocked_by: BlockedBy,
        parent: Option<Parent>,
        // Absent only from a listing made without asking for it.
        #[serde(default)]
        sub_issues_summary: Summary,
    }
    #[derive(Deserialize)]
    struct Parent {
        number: u64,
    }
    // `completed` counts the closed sub-issues, whatever they closed as.
    #[derive(Default, Deserialize)]
    struct Summary {
        total: u64,
        completed: u64,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct BlockedBy {
        nodes: Vec<Blocking>,
        total_count: u64,
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
            parent: i.parent.map(|p| p.number),
            sub_issues: SubIssues {
                total: i.sub_issues_summary.total,
                closed: i.sub_issues_summary.completed,
            },
            unlisted_blockers: i
                .blocked_by
                .total_count
                .saturating_sub(i.blocked_by.nodes.len() as u64),
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
        labels: Vec<Label>,
    }
    let listed: Vec<Listed> = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(listed
        .into_iter()
        .map(|pr| OpenPullRequest {
            number: pr.number,
            head: pr.head_ref_name,
            closes: closed_here(pr.closing_issues_references, repo),
            labels: pr.labels.into_iter().map(|l| l.name).collect(),
        })
        .collect())
}

/// An issue a pull request closes, on whichever repo it is
#[derive(Deserialize)]
pub(super) struct Closes {
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

// A pull request can close issues on other repos, which are not this board's.
pub(super) fn closed_here(closes: Vec<Closes>, repo: &ForgeSlug) -> Vec<u64> {
    let (owner, name) = repo.as_str().split_once('/').unwrap_or_default();
    closes
        .into_iter()
        .filter(|c| {
            let r = &c.repository;
            r.owner.login.eq_ignore_ascii_case(owner) && r.name.eq_ignore_ascii_case(name)
        })
        .map(|c| c.number)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded from gh 2.96 on this repo with the `ready_issues` arguments.
    const READY_LIST: &str = include_str!("../../../fixtures/gh-issue-list.json");

    #[test]
    fn the_board_lists_issues_so_a_labelled_pull_request_is_never_new_work() {
        let repo = ForgeSlug::try_from("shep-pm/shep".to_owned()).unwrap();
        let args = ready_args(&repo);
        assert_eq!(args[..2], ["issue", "list"]);
        assert!(
            !args
                .iter()
                .any(|a| a.contains("search") || a.contains("api"))
        );
    }

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

    // Recorded from gh 2.96 on cli/cli with the `ready_issues` fields, since
    // no repo of the maintainer's has sub-issues yet.
    const SUB_ISSUES: &str = include_str!("../../../fixtures/gh-issue-list-sub-issues.json");

    #[test]
    fn each_ready_issue_carries_its_parent_and_its_sub_issues() {
        let issues = parse_ready_issues(SUB_ISSUES.as_bytes()).unwrap();
        let read: Vec<(u64, Option<u64>, SubIssues)> = issues
            .iter()
            .map(|i| (i.number, i.parent, i.sub_issues))
            .collect();
        let count = |total, closed| SubIssues { total, closed };
        assert_eq!(
            read,
            [
                (14529, None, count(6, 0)),
                (14528, Some(14529), count(0, 0)),
                (12438, None, count(3, 2)),
            ]
        );
    }

    #[test]
    fn the_board_asks_for_each_issues_parent_and_sub_issues() {
        let repo = ForgeSlug::try_from("shep-pm/shep".to_owned()).unwrap();
        let args = ready_args(&repo);
        let json = args.iter().position(|&a| a == "--json").unwrap() + 1;
        let fields = args[json].split(',').collect::<Vec<_>>();
        assert!(fields.contains(&"parent") && fields.contains(&"subIssuesSummary"));
    }

    #[test]
    fn an_issue_with_an_assignee_is_assigned() {
        let listed = br#"[{"assignees":[{"id":"MDQ","login":"someone","name":""}],"labels":[],
            "number":3,"blockedBy":{"nodes":[],"totalCount":0}}]"#;
        assert!(parse_ready_issues(listed).unwrap()[0].assigned);
    }

    #[test]
    fn blockers_counted_past_the_listed_ones_are_unlisted() {
        let issues = parse_ready_issues(READY_LIST.as_bytes()).unwrap();
        assert!(issues.iter().all(|i| i.unlisted_blockers == 0));
        let listed = br#"[{"assignees":[],"labels":[],"number":3,
            "blockedBy":{"nodes":[{"number":2,"state":"CLOSED"}],"totalCount":51}}]"#;
        assert_eq!(parse_ready_issues(listed).unwrap()[0].unlisted_blockers, 50);
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
            [
                OpenPullRequest {
                    number: 81,
                    head: "worktree-kelpie-79".into(),
                    closes: vec![79],
                    labels: vec![],
                },
                OpenPullRequest {
                    number: 55,
                    head: "feat/54-see-the-ui".into(),
                    closes: vec![54],
                    labels: vec![],
                },
            ]
        );
    }

    // The recording carries no labels, and gh writes them as it does on an issue.
    #[test]
    fn an_open_pull_requests_labels_are_read_by_name() {
        let slug = ForgeSlug::try_from("shep-pm/shep".to_owned()).unwrap();
        let listed = br#"[{"closingIssuesReferences":[],"headRefName":"fix/x","number":614,
            "labels":[{"id":"LA_1","name":"ready-for-agent","description":"","color":"0e8a16"}]}]"#;
        let prs = parse_pull_requests(listed, &slug).unwrap();
        assert_eq!(prs[0].labels, [READY]);
    }

    #[test]
    fn an_issue_closed_on_another_repo_is_not_this_boards() {
        let slug = ForgeSlug::try_from("shep-pm/shep".to_owned()).unwrap();
        let prs = parse_pull_requests(PR_LIST.as_bytes(), &slug).unwrap();
        assert!(prs[0].closes.is_empty());
    }
}
