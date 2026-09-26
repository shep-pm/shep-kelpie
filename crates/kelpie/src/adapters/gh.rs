//! GitHub, through the `gh` command line

use std::process::{Command, Stdio};

use serde::Deserialize;

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
            "title,body",
        ])?)
    }
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
    }
    let view: View = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(Issue {
        title: view.title,
        body: view.body.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded from gh 2 on this repo: `gh issue view 6 --json title,body`.
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
    }

    #[test]
    fn an_issue_without_a_body_has_an_empty_one() {
        let issue = parse_issue(br#"{"title":"t","body":null}"#).unwrap();
        assert_eq!(issue.body, "");
    }

    #[test]
    fn an_issue_without_a_title_is_unreadable() {
        assert!(matches!(
            parse_issue(br#"{"body":"x"}"#),
            Err(ForgeError::Unreadable(_))
        ));
    }
}
