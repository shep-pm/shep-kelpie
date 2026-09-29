//! CodeRabbit's comments, reviews and threads on a pull request, and the
//! label changes the project manager makes

use serde::Deserialize;

use super::gh;
use super::outside::{lines, threads, time};
use crate::coderabbit::{Activity, Comment, Review};
use crate::ports::ForgeError;
use crate::settings::ForgeSlug;

/// CodeRabbit's login on the REST API
const BOT: &str = "coderabbitai[bot]";

/// CodeRabbit's login on the GraphQL API, which drops the suffix
pub(crate) const BOT_GRAPHQL: &str = "coderabbitai";

pub(super) fn activity(repo: &ForgeSlug, number: u64) -> Result<Activity, ForgeError> {
    let comments = gh(&[
        "api",
        "--paginate",
        &format!(
            "repos/{}/issues/{number}/comments?per_page=100",
            repo.as_str()
        ),
        "--jq",
        &format!(".[] | select(.user.login == \"{BOT}\") | {{body, updated_at}}"),
    ])?;
    let reviews = gh(&[
        "api",
        "--paginate",
        &format!(
            "repos/{}/pulls/{number}/reviews?per_page=100",
            repo.as_str()
        ),
        "--jq",
        &format!(".[] | select(.user.login == \"{BOT}\") | {{commit_id, body, submitted_at}}"),
    ])?;
    Ok(Activity {
        comments: parse_comments(&comments)?,
        reviews: parse_reviews(&reviews)?,
        threads: threads(repo, number, BOT_GRAPHQL)?,
    })
}

pub(super) fn label(
    repo: &ForgeSlug,
    number: u64,
    label: &str,
    add: bool,
) -> Result<(), ForgeError> {
    let flag = if add { "--add-label" } else { "--remove-label" };
    let number = number.to_string();
    gh(&["pr", "edit", &number, "--repo", repo.as_str(), flag, label]).map(drop)
}

pub(crate) fn parse_comments(stdout: &[u8]) -> Result<Vec<Comment>, ForgeError> {
    #[derive(Deserialize)]
    struct Line {
        body: Option<String>,
        updated_at: String,
    }
    lines::<Line>(stdout)?
        .into_iter()
        .map(|c| {
            Ok(Comment {
                body: c.body.unwrap_or_default(),
                at: time(&c.updated_at, stdout)?,
            })
        })
        .collect()
}

pub(crate) fn parse_reviews(stdout: &[u8]) -> Result<Vec<Review>, ForgeError> {
    #[derive(Deserialize)]
    struct Line {
        commit_id: String,
        body: Option<String>,
        submitted_at: String,
    }
    lines::<Line>(stdout)?
        .into_iter()
        .map(|r| {
            Ok(Review {
                commit: r.commit_id,
                body: r.body.unwrap_or_default(),
                at: time(&r.submitted_at, stdout)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_of_nothing_is_no_comments() {
        assert_eq!(parse_comments(b"").unwrap(), []);
        assert_eq!(parse_reviews(b"\n").unwrap(), []);
    }

    #[test]
    fn a_line_that_is_not_a_comment_is_unreadable() {
        assert!(matches!(
            parse_comments(br#"{"body":"x","updated_at":"yesterday"}"#),
            Err(ForgeError::Unreadable(_))
        ));
        assert!(matches!(
            parse_reviews(b"{not json}"),
            Err(ForgeError::Unreadable(_))
        ));
    }
}
