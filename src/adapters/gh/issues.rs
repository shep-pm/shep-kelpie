//! Issues kelpie files: the open ones it checks first, and a new one

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

pub(super) fn set_label(
    repo: &ForgeSlug,
    number: u64,
    label: &str,
    add: bool,
) -> Result<(), ForgeError> {
    let flag = if add { "--add-label" } else { "--remove-label" };
    let number = number.to_string();
    gh(&[
        "issue",
        "edit",
        &number,
        "--repo",
        repo.as_str(),
        flag,
        label,
    ])
    .map(drop)
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
