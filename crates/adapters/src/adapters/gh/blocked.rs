//! A blocked-by or sub-issue link added to a GitHub issue
//!
//! Each endpoint takes the linked issue's database id, which is not its
//! number, so that issue is read first. That read answers for a pull request
//! too, which the endpoint refuses, so a blocker that is one is left unlinked.

use serde::Deserialize;

use super::{gh, issues, unreadable};
use crate::ports::{ForgeError, SubIssue};
use crate::settings::ForgeSlug;

pub(super) fn add_blocked_by(
    repo: &ForgeSlug,
    number: u64,
    blocker: u64,
) -> Result<bool, ForgeError> {
    let path = format!("repos/{}/issues/{blocker}", repo.as_str());
    let Some(id) = parse_id(&gh(&["api", &path])?)? else {
        return Ok(false);
    };
    let args = link_args(repo, number, id);
    gh(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
    Ok(true)
}

pub(super) fn add_sub_issue(repo: &ForgeSlug, parent: u64, child: u64) -> Result<(), ForgeError> {
    if is_linked(&issues::sub_issues(repo, parent)?, child) {
        return Ok(());
    }
    let path = format!("repos/{}/issues/{child}", repo.as_str());
    let Some(id) = parse_id(&gh(&["api", &path])?)? else {
        return Err(ForgeError::Failed(format!(
            "#{child} is a pull request, which is no sub-issue"
        )));
    };
    let args = sub_issue_args(repo, parent, id);
    gh(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
    Ok(())
}

// Whether `subs` hold issue `child`
fn is_linked(subs: &[SubIssue], child: u64) -> bool {
    subs.iter().any(|sub| sub.ticket.github() == Some(child))
}

fn sub_issue_args(repo: &ForgeSlug, parent: u64, id: u64) -> Vec<String> {
    post_args(
        format!("repos/{}/issues/{parent}/sub_issues", repo.as_str()),
        format!("sub_issue_id={id}"),
    )
}

// `-F` sends the id as a number, which the endpoint wants.
fn link_args(repo: &ForgeSlug, number: u64, id: u64) -> Vec<String> {
    post_args(
        format!(
            "repos/{}/issues/{number}/dependencies/blocked_by",
            repo.as_str()
        ),
        format!("issue_id={id}"),
    )
}

fn post_args(path: String, field: String) -> Vec<String> {
    [
        "api",
        "--method",
        "POST",
        path.as_str(),
        "-F",
        field.as_str(),
    ]
    .map(str::to_owned)
    .to_vec()
}

// The database id of an issue, or None for a pull request, whose reply carries `pull_request`
fn parse_id(stdout: &[u8]) -> Result<Option<u64>, ForgeError> {
    #[derive(Deserialize)]
    struct Issue {
        id: u64,
        pull_request: Option<serde::de::IgnoredAny>,
    }
    let issue: Issue = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(issue.pull_request.is_none().then_some(issue.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> ForgeSlug {
        ForgeSlug::try_from("shep-pm/koji".to_owned()).unwrap()
    }

    #[test]
    fn the_blocker_is_linked_by_its_database_id_and_not_its_number() {
        // The head of `gh api repos/shep-pm/koji/issues/7`
        let out = br#"{"url":"https://api.github.com/repos/shep-pm/koji/issues/7",
            "id":3912345678,"node_id":"I_kwDOAbc","number":7,"title":"The blocker",
            "state":"open"}"#;
        let id = parse_id(out).unwrap().unwrap();
        assert_eq!(id, 3_912_345_678);
        assert_eq!(
            link_args(&repo(), 12, id),
            [
                "api",
                "--method",
                "POST",
                "repos/shep-pm/koji/issues/12/dependencies/blocked_by",
                "-F",
                "issue_id=3912345678",
            ]
        );
    }

    #[test]
    fn the_sub_issue_is_linked_by_its_database_id_and_not_its_number() {
        // The head of `gh api repos/shep-pm/koji/issues/13`
        let out = br#"{"url":"https://api.github.com/repos/shep-pm/koji/issues/13",
            "id":3912345999,"node_id":"I_kwDOAbd","number":13,"title":"The child",
            "state":"open"}"#;
        let id = parse_id(out).unwrap().unwrap();
        assert_eq!(id, 3_912_345_999);
        assert_eq!(
            sub_issue_args(&repo(), 12, id),
            [
                "api",
                "--method",
                "POST",
                "repos/shep-pm/koji/issues/12/sub_issues",
                "-F",
                "sub_issue_id=3912345999",
            ]
        );
    }

    #[test]
    fn a_child_already_linked_is_not_linked_twice() {
        // The head of `gh api repos/shep-pm/koji/issues/12/sub_issues?per_page=100`
        let out = br#"[{"id":3912345998,"number":11,"state":"closed","state_reason":"completed"},
            {"id":3912345999,"number":13,"state":"open","state_reason":null}]"#;
        let subs = issues::parse_sub_issues(out).unwrap();
        assert!(is_linked(&subs, 13));
        assert!(!is_linked(&subs, 14));
        assert!(!is_linked(&[], 13));
    }

    #[test]
    fn a_pull_request_has_no_id_to_link() {
        let out = br#"{"id":3912345679,"number":8,"title":"A change","state":"open",
            "pull_request":{"url":"https://api.github.com/repos/shep-pm/koji/pulls/8"}}"#;
        assert_eq!(parse_id(out).unwrap(), None);
    }

    #[test]
    fn a_reply_without_an_id_is_unreadable() {
        let error = parse_id(br#"{"message":"Not Found"}"#).unwrap_err();
        assert!(matches!(error, ForgeError::Unreadable(_)), "{error:?}");
    }
}
