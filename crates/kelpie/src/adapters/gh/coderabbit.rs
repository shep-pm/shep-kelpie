//! CodeRabbit's comments, reviews and threads on a pull request, and the
//! label and thread changes the project manager makes

use serde::Deserialize;

use super::{gh, unreadable};
use crate::coderabbit::{Activity, Comment, Review, Thread};
use crate::ports::{ForgeError, Timestamp};
use crate::settings::ForgeSlug;

/// CodeRabbit's login on the REST API
const BOT: &str = "coderabbitai[bot]";

/// CodeRabbit's login on the GraphQL API, which drops the suffix
const BOT_GRAPHQL: &str = "coderabbitai";

// A pull request with more than 100 threads is not expected; one past it
// would read as unresolved-but-unseen, never as satisfied.
const THREADS: &str = "query($owner: String!, $name: String!, $number: Int!) { \
    repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
    reviewThreads(first: 100) { nodes { id isResolved path line \
    comments(first: 1) { nodes { author { login } body } } } } } } }";

const RESOLVE: &str = "mutation($id: ID!) { resolveReviewThread(input: {threadId: $id}) \
    { thread { isResolved } } }";

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
    let (owner, name) = repo.as_str().split_once('/').unwrap_or_default();
    let threads = gh(&[
        "api",
        "graphql",
        "-f",
        &format!("query={THREADS}"),
        "-f",
        &format!("owner={owner}"),
        "-f",
        &format!("name={name}"),
        "-F",
        &format!("number={number}"),
    ])?;
    Ok(Activity {
        comments: parse_comments(&comments)?,
        reviews: parse_reviews(&reviews)?,
        threads: parse_threads(&threads)?,
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

pub(super) fn resolve(thread: &str) -> Result<(), ForgeError> {
    let resolved = gh(&[
        "api",
        "graphql",
        "-f",
        &format!("query={RESOLVE}"),
        "-F",
        &format!("id={thread}"),
    ])?;
    #[derive(Deserialize)]
    struct Reply {
        data: Data,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Data {
        resolve_review_thread: Resolved,
    }
    #[derive(Deserialize)]
    struct Resolved {
        thread: State,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct State {
        is_resolved: bool,
    }
    let reply: Reply = serde_json::from_slice(&resolved).map_err(|_| unreadable(&resolved))?;
    if reply.data.resolve_review_thread.thread.is_resolved {
        Ok(())
    } else {
        Err(unreadable(&resolved))
    }
}

fn time(text: &str, stdout: &[u8]) -> Result<Timestamp, ForgeError> {
    let at: jiff::Timestamp = text.parse().map_err(|_| unreadable(stdout))?;
    let seconds = u64::try_from(at.as_second()).map_err(|_| unreadable(stdout))?;
    Ok(Timestamp(seconds))
}

// `--jq` prints one JSON object a line, across every page.
fn lines<T: for<'de> Deserialize<'de>>(stdout: &[u8]) -> Result<Vec<T>, ForgeError> {
    stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.trim_ascii().is_empty())
        .map(|line| serde_json::from_slice(line).map_err(|_| unreadable(stdout)))
        .collect()
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

pub(crate) fn parse_threads(stdout: &[u8]) -> Result<Vec<Thread>, ForgeError> {
    #[derive(Deserialize)]
    struct Reply {
        data: Data,
    }
    #[derive(Deserialize)]
    struct Data {
        repository: Repository,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Repository {
        pull_request: PullRequest,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PullRequest {
        review_threads: Nodes<Node>,
    }
    #[derive(Deserialize)]
    struct Nodes<T> {
        nodes: Vec<T>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Node {
        id: String,
        is_resolved: bool,
        path: String,
        line: Option<u32>,
        comments: Nodes<First>,
    }
    #[derive(Deserialize)]
    struct First {
        author: Option<Author>,
        body: String,
    }
    #[derive(Deserialize)]
    struct Author {
        login: String,
    }
    let reply: Reply = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    let nodes = reply.data.repository.pull_request.review_threads.nodes;
    Ok(nodes
        .into_iter()
        .filter_map(|node| {
            let first = node.comments.nodes.into_iter().next()?;
            let by_bot = first.author.is_some_and(|a| a.login == BOT_GRAPHQL);
            by_bot.then_some(Thread {
                id: node.id,
                resolved: node.is_resolved,
                path: node.path,
                line: node.line,
                body: first.body,
            })
        })
        .collect())
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

    #[test]
    fn a_thread_whose_first_comment_has_no_author_is_not_coderabbits() {
        let reply = br#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
            {"id":"a","isResolved":false,"path":"x","line":1,
             "comments":{"nodes":[{"author":null,"body":"ghost"}]}}]}}}}}"#;
        assert_eq!(parse_threads(reply).unwrap(), []);
    }
}
