//! A blocked-by link added to a GitHub issue
//!
//! The endpoint takes the blocker's database id, which is not its number, so
//! the blocker is read first.

use serde::Deserialize;

use super::{gh, unreadable};
use crate::ports::ForgeError;
use crate::settings::ForgeSlug;

pub(super) fn add_blocked_by(
    repo: &ForgeSlug,
    number: u64,
    blocker: u64,
) -> Result<(), ForgeError> {
    let path = format!("repos/{}/issues/{blocker}", repo.as_str());
    let id = parse_id(&gh(&["api", &path])?)?;
    let args = link_args(repo, number, id);
    gh(&args.iter().map(String::as_str).collect::<Vec<_>>()).map(drop)
}

// `-F` sends the id as a number, which the endpoint wants.
fn link_args(repo: &ForgeSlug, number: u64, id: u64) -> Vec<String> {
    let path = format!(
        "repos/{}/issues/{number}/dependencies/blocked_by",
        repo.as_str()
    );
    let field = format!("issue_id={id}");
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

fn parse_id(stdout: &[u8]) -> Result<u64, ForgeError> {
    #[derive(Deserialize)]
    struct Issue {
        id: u64,
    }
    let issue: Issue = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(issue.id)
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
        let id = parse_id(out).unwrap();
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
    fn a_reply_without_an_id_is_unreadable() {
        let error = parse_id(br#"{"message":"Not Found"}"#).unwrap_err();
        assert!(matches!(error, ForgeError::Unreadable(_)), "{error:?}");
    }
}
