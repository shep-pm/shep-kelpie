//! Gemini Code Assist's comments, reviews and threads on a pull request,
//! and the `/gemini review` summons

use serde::Deserialize;

use super::gh;
use super::outside::{lines, threads, time};
use crate::gemini::{Activity, Comment, Review, SUMMON};
use crate::ports::{ForgeError, Timestamp};
use crate::settings::ForgeSlug;

/// Gemini's login on the REST API
const BOT: &str = "gemini-code-assist[bot]";

/// Gemini's login on the GraphQL API, which drops the suffix
pub(crate) const BOT_GRAPHQL: &str = "gemini-code-assist";

pub(super) fn activity(repo: &ForgeSlug, number: u64) -> Result<Activity, ForgeError> {
    let comments = gh(&[
        "api",
        "--paginate",
        &format!(
            "repos/{}/issues/{number}/comments?per_page=100",
            repo.as_str()
        ),
        "--jq",
        &format!(
            ".[] | select(.user.login == \"{BOT}\" or (.body // \"\" | startswith(\"/gemini\"))) \
             | {{login: .user.login, body, created_at}}"
        ),
    ])?;
    let reviews = gh(&[
        "api",
        "--paginate",
        &format!(
            "repos/{}/pulls/{number}/reviews?per_page=100",
            repo.as_str()
        ),
        "--jq",
        &format!(".[] | select(.user.login == \"{BOT}\") | {{id, commit_id, body, submitted_at}}"),
    ])?;
    let (comments, summons) = parse_comments(&comments)?;
    Ok(Activity {
        comments,
        summons,
        reviews: parse_reviews(&reviews)?,
        threads: threads(repo, number, BOT_GRAPHQL)?,
    })
}

/// Gemini's own comments, and when each summon was posted
pub(crate) fn parse_comments(stdout: &[u8]) -> Result<(Vec<Comment>, Vec<Timestamp>), ForgeError> {
    #[derive(Deserialize)]
    struct Line {
        login: String,
        body: Option<String>,
        created_at: String,
    }
    let mut comments = Vec::new();
    let mut summons = Vec::new();
    for line in lines::<Line>(stdout)? {
        let at = time(&line.created_at, stdout)?;
        let body = line.body.unwrap_or_default();
        if line.login == BOT {
            comments.push(Comment { body, at });
        } else if body.trim() == SUMMON {
            summons.push(at);
        }
    }
    Ok((comments, summons))
}

pub(crate) fn parse_reviews(stdout: &[u8]) -> Result<Vec<Review>, ForgeError> {
    #[derive(Deserialize)]
    struct Line {
        id: u64,
        commit_id: String,
        submitted_at: String,
    }
    lines::<Line>(stdout)?
        .into_iter()
        .map(|r| {
            Ok(Review {
                id: r.id,
                commit: r.commit_id,
                at: time(&r.submitted_at, stdout)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_bare_summon_counts_as_one() {
        let stdout =
            br#"{"login":"maintainer","body":"/gemini review ","created_at":"2026-09-25T07:51:29Z"}
{"login":"maintainer","body":"/gemini summary","created_at":"2026-09-25T07:52:29Z"}
{"login":"gemini-code-assist[bot]","body":"/gemini review","created_at":"2026-09-25T07:53:29Z"}"#;
        let (comments, summons) = parse_comments(stdout).unwrap();
        assert_eq!(summons.len(), 1);
        assert_eq!(comments.len(), 1, "its own comment is never a summon");
    }

    #[test]
    fn a_review_without_an_id_is_unreadable() {
        assert!(matches!(
            parse_reviews(br#"{"commit_id":"c","submitted_at":"2026-09-25T07:51:29Z"}"#),
            Err(ForgeError::Unreadable(_))
        ));
    }
}
