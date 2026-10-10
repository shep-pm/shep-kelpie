//! What the logged-in account may do on a repo, and whether a review bot works there

use serde::Deserialize;

use super::{gh, unreadable};
use crate::ports::ForgeError;
use crate::review_bot::Login;
use crate::settings::ForgeSlug;

pub(super) fn can_push(repo: &ForgeSlug) -> Result<bool, ForgeError> {
    parse_permission(&gh(&[
        "repo",
        "view",
        repo.as_str(),
        "--json",
        "viewerPermission",
    ])?)
}

pub(super) fn owner_is_user(repo: &ForgeSlug) -> Result<bool, ForgeError> {
    let path = format!("users/{}", repo.owner());
    parse_owner_type(&gh(&["api", &path, "--jq", ".type"])?)
}

fn parse_owner_type(stdout: &[u8]) -> Result<bool, ForgeError> {
    match String::from_utf8_lossy(stdout).trim() {
        "User" => Ok(true),
        "Organization" => Ok(false),
        _ => Err(unreadable(stdout)),
    }
}

// GitHub's search finds a bot's comments by its app name, which is the
// GraphQL login.
pub(super) fn review_bot_seen(repo: &ForgeSlug, login: Login<'_>) -> Result<bool, ForgeError> {
    let commenter = format!("app/{}", login.graphql);
    parse_found(&gh(&[
        "search",
        "prs",
        "--repo",
        repo.as_str(),
        "--commenter",
        &commenter,
        "--limit",
        "1",
        "--json",
        "number",
    ])?)
}

fn parse_permission(stdout: &[u8]) -> Result<bool, ForgeError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct View {
        viewer_permission: String,
    }
    let view: View = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    match view.viewer_permission.as_str() {
        "ADMIN" | "MAINTAIN" | "WRITE" => Ok(true),
        "TRIAGE" | "READ" => Ok(false),
        _ => Err(unreadable(stdout)),
    }
}

fn parse_found(stdout: &[u8]) -> Result<bool, ForgeError> {
    let found: Vec<serde_json::Value> =
        serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(!found.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    // `gh api users/<owner> --jq .type` prints the type and a newline.
    #[test]
    fn an_owner_is_a_user_or_an_organization_and_nothing_else() {
        assert!(parse_owner_type(b"User\n").unwrap());
        assert!(!parse_owner_type(b"Organization\n").unwrap());
        assert!(parse_owner_type(b"Bot\n").is_err());
    }

    // Recorded from gh 2.96: `gh repo view <repo> --json viewerPermission`
    // on this repo, as its maintainer, and on cli/cli, as a stranger.
    const ADMIN: &str = r#"{"viewerPermission":"ADMIN"}"#;
    const READ: &str = r#"{"viewerPermission":"READ"}"#;

    // Recorded from gh 2.96: `gh search prs --repo <repo> --commenter
    // app/coderabbitai --limit 1 --json number` on shep-pm/shep, which
    // CodeRabbit reviews, and on cli/cli, which it does not.
    const REVIEWED: &str = r#"[{"number":649}]"#;
    const UNREVIEWED: &str = "[]";

    #[test]
    fn only_a_writer_may_push() {
        assert_eq!(parse_permission(ADMIN.as_bytes()), Ok(true));
        assert_eq!(parse_permission(READ.as_bytes()), Ok(false));
        for (permission, pushes) in [("MAINTAIN", true), ("WRITE", true), ("TRIAGE", false)] {
            let text = format!(r#"{{"viewerPermission":"{permission}"}}"#);
            assert_eq!(parse_permission(text.as_bytes()), Ok(pushes), "{text}");
        }
    }

    #[test]
    fn a_permission_it_does_not_know_is_unreadable() {
        for text in [r#"{"viewerPermission":"OWNER"}"#, "{}", ""] {
            assert!(
                matches!(
                    parse_permission(text.as_bytes()),
                    Err(ForgeError::Unreadable(_))
                ),
                "{text}"
            );
        }
    }

    #[test]
    fn a_bot_is_seen_when_the_search_finds_a_pull_request() {
        assert_eq!(parse_found(REVIEWED.as_bytes()), Ok(true));
        assert_eq!(parse_found(UNREVIEWED.as_bytes()), Ok(false));
        assert!(matches!(parse_found(b"{}"), Err(ForgeError::Unreadable(_))));
    }
}
