//! What the outside reviewers share on the forge: their review threads,
//! resolving one, and reading `gh api --jq` output

use serde::Deserialize;

use super::{gh, unreadable};
use crate::outside::Thread;
use crate::ports::{ForgeError, Timestamp};
use crate::settings::ForgeSlug;

// A pull request with more than 100 threads is not expected; one past it
// would read as unresolved-but-unseen, never as satisfied.
const THREADS: &str = "query($owner: String!, $name: String!, $number: Int!) { \
    repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
    reviewThreads(first: 100) { nodes { id isResolved path line \
    comments(first: 1) { nodes { author { login } body \
    pullRequestReview { databaseId } } } } } } } }";

const RESOLVE: &str = "mutation($id: ID!) { resolveReviewThread(input: {threadId: $id}) \
    { thread { isResolved } } }";

/// Every review thread on pull request `number` that `login` opened, as the
/// GraphQL API names its author
pub(super) fn threads(
    repo: &ForgeSlug,
    number: u64,
    login: &str,
) -> Result<Vec<Thread>, ForgeError> {
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
    parse_threads(&threads, login)
}

pub(super) fn resolve(thread: &str) -> Result<(), ForgeError> {
    let resolved = gh(&[
        "api",
        "graphql",
        "-f",
        &format!("query={RESOLVE}"),
        "-f",
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

pub(super) fn time(text: &str, stdout: &[u8]) -> Result<Timestamp, ForgeError> {
    let at: jiff::Timestamp = text.parse().map_err(|_| unreadable(stdout))?;
    let seconds = u64::try_from(at.as_second()).map_err(|_| unreadable(stdout))?;
    Ok(Timestamp(seconds))
}

// `--jq` prints one JSON object a line, across every page.
pub(super) fn lines<T: for<'de> Deserialize<'de>>(stdout: &[u8]) -> Result<Vec<T>, ForgeError> {
    stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.trim_ascii().is_empty())
        .map(|line| serde_json::from_slice(line).map_err(|_| unreadable(stdout)))
        .collect()
}

pub(crate) fn parse_threads(stdout: &[u8], login: &str) -> Result<Vec<Thread>, ForgeError> {
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
    #[serde(rename_all = "camelCase")]
    struct First {
        author: Option<Author>,
        body: String,
        #[serde(default)]
        pull_request_review: Option<Review>,
    }
    #[derive(Deserialize)]
    struct Author {
        login: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Review {
        database_id: Option<u64>,
    }
    let reply: Reply = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    let nodes = reply.data.repository.pull_request.review_threads.nodes;
    Ok(nodes
        .into_iter()
        .filter_map(|node| {
            let first = node.comments.nodes.into_iter().next()?;
            let by_bot = first.author.is_some_and(|a| a.login == login);
            by_bot.then_some(Thread {
                id: node.id,
                resolved: node.is_resolved,
                path: node.path,
                line: node.line,
                body: first.body,
                review: first.pull_request_review.and_then(|r| r.database_id),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_whose_first_comment_has_no_author_is_no_bots() {
        let reply = br#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
            {"id":"a","isResolved":false,"path":"x","line":1,
             "comments":{"nodes":[{"author":null,"body":"ghost"}]}}]}}}}}"#;
        assert_eq!(parse_threads(reply, "coderabbitai").unwrap(), []);
    }
}
