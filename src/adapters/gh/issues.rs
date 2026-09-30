//! Issues kelpie files: the open ones it checks first, a new one, the
//! sub-issues and blockers a split links, and a parent it closes

use serde::Deserialize;

use super::{gh, unreadable};
use crate::ports::{ForgeError, OpenIssue};
use crate::settings::ForgeSlug;

// The same ceiling the board's lists use, since `gh` lists newest first and
// stops at its limit.
const LIST_LIMIT: &str = "1000";

pub(super) fn open_issues(repo: &ForgeSlug) -> Result<Vec<OpenIssue>, ForgeError> {
    let args = [
        "issue",
        "list",
        "--repo",
        repo.as_str(),
        "--state",
        "open",
        "--limit",
        LIST_LIMIT,
        "--json",
        "number,title,body",
    ];
    parse_open_issues(&gh(&args)?)
}

pub(super) fn create_issue(
    repo: &ForgeSlug,
    title: &str,
    body: &str,
    labels: &[&str],
) -> Result<u64, ForgeError> {
    let mut args = vec![
        "issue",
        "create",
        "--repo",
        repo.as_str(),
        "--title",
        title,
        "--body",
        body,
    ];
    for label in labels {
        args.extend(["--label", label]);
    }
    parse_created(&gh(&args)?)
}

fn parse_open_issues(stdout: &[u8]) -> Result<Vec<OpenIssue>, ForgeError> {
    #[derive(Deserialize)]
    struct Listed {
        number: u64,
        title: String,
        body: Option<String>,
    }
    let listed: Vec<Listed> = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(listed
        .into_iter()
        .map(|issue| OpenIssue {
            number: issue.number,
            title: issue.title,
            body: issue.body.unwrap_or_default(),
        })
        .collect())
}

// `gh issue create` prints the new issue's URL, which ends in its number.
pub(super) fn add_sub_issue(repo: &ForgeSlug, parent: u64, child: u64) -> Result<(), ForgeError> {
    let id = format!("sub_issue_id={}", database_id(repo, child)?);
    let path = format!("repos/{}/issues/{parent}/sub_issues", repo.as_str());
    gh(&["api", "--method", "POST", &path, "-F", &id]).map(drop)
}

pub(super) fn add_blocker(repo: &ForgeSlug, number: u64, by: u64) -> Result<(), ForgeError> {
    let id = format!("issue_id={}", database_id(repo, by)?);
    let path = format!(
        "repos/{}/issues/{number}/dependencies/blocked_by",
        repo.as_str()
    );
    gh(&["api", "--method", "POST", &path, "-F", &id]).map(drop)
}

pub(super) fn close_issue(repo: &ForgeSlug, number: u64, comment: &str) -> Result<(), ForgeError> {
    let number = number.to_string();
    let args = [
        "issue",
        "close",
        &number,
        "--repo",
        repo.as_str(),
        "--comment",
        comment,
    ];
    gh(&args).map(drop)
}

// The REST calls that link issues name the other issue by its database id,
// not its number.
fn database_id(repo: &ForgeSlug, number: u64) -> Result<u64, ForgeError> {
    let path = format!("repos/{}/issues/{number}", repo.as_str());
    parse_database_id(&gh(&["api", &path, "--jq", ".id"])?)
}

fn parse_database_id(stdout: &[u8]) -> Result<u64, ForgeError> {
    let text = String::from_utf8_lossy(stdout);
    text.trim().parse().map_err(|_| unreadable(stdout))
}

fn parse_created(stdout: &[u8]) -> Result<u64, ForgeError> {
    let text = String::from_utf8_lossy(stdout);
    text.trim()
        .rsplit('/')
        .next()
        .and_then(|number| number.parse().ok())
        .ok_or_else(|| unreadable(stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded from gh 2.96: `gh api repos/shep-pm/shep-kelpie/issues/139 --jq .id`.
    #[test]
    fn an_issues_database_id_is_read_from_the_api() {
        assert_eq!(parse_database_id(b"5634695475\n"), Ok(5_634_695_475));
        assert!(matches!(
            parse_database_id(b"null\n"),
            Err(ForgeError::Unreadable(_))
        ));
    }

    #[test]
    fn open_issues_are_read_with_a_body_that_may_be_null() {
        let out = br#"[{"number":4,"title":"a","body":"b"},{"number":5,"title":"c","body":null}]"#;
        let issues = parse_open_issues(out).unwrap();
        assert_eq!(
            issues,
            [
                OpenIssue {
                    number: 4,
                    title: "a".into(),
                    body: "b".into()
                },
                OpenIssue {
                    number: 5,
                    title: "c".into(),
                    body: String::new()
                },
            ]
        );
    }

    #[test]
    fn a_created_issues_number_is_the_end_of_the_url_gh_prints() {
        let out = b"https://github.com/shep-pm/shep-kelpie/issues/143\n";
        assert_eq!(parse_created(out).unwrap(), 143);
    }

    #[test]
    fn output_with_no_number_is_unreadable() {
        assert!(matches!(
            parse_created(b"created it\n"),
            Err(ForgeError::Unreadable(_))
        ));
    }
}
