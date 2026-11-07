//! What people wrote on a pull request: its conversation and its review threads

use serde::Deserialize;

use super::{gh_as, unreadable};
use crate::ports::{ForgeError, Remark, Standing, Talk, TalkThread};
use crate::settings::ForgeSlug;

// The latest 100 comments, and the first 100 threads with the first 100 of
// each one's comments. A pull request past those is not expected; what falls
// out is the oldest conversation, never a thread's root.
const QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { \
    repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
    comments(last: 100) { nodes { databaseId body authorAssociation \
    author { login __typename } } } \
    reviewThreads(first: 100) { nodes { id isResolved isOutdated path line \
    comments(first: 100) { nodes { databaseId body authorAssociation \
    author { login __typename } } } } } } } }";

pub(super) fn talk(repo: &ForgeSlug, number: u64) -> Result<Talk, ForgeError> {
    let (owner, name) = repo.as_str().split_once('/').unwrap_or_default();
    parse_talk(&gh_as(
        None,
        &[
            "api",
            "graphql",
            "-f",
            &format!("query={QUERY}"),
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("name={name}"),
            "-F",
            &format!("number={number}"),
        ],
    )?)
}

#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Comment {
    database_id: u64,
    body: Option<String>,
    author_association: String,
    author: Option<Author>,
}

#[derive(Deserialize)]
struct Author {
    login: String,
    #[serde(rename = "__typename")]
    kind: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Thread {
    id: String,
    is_resolved: bool,
    is_outdated: bool,
    path: String,
    line: Option<u32>,
    comments: Nodes<Comment>,
}

// Whoever has a hand in the repo can be asked for a change; a deleted
// account has no author, and a bot is no one's request.
const WRITERS: [&str; 3] = ["OWNER", "MEMBER", "COLLABORATOR"];

impl From<Comment> for Remark {
    fn from(comment: Comment) -> Self {
        let standing = match &comment.author {
            Some(a) if a.kind == "Bot" => Standing::Bot,
            Some(_) if WRITERS.contains(&comment.author_association.as_str()) => Standing::Writer,
            _ => Standing::Other,
        };
        Self {
            id: comment.database_id,
            author: comment.author.map(|a| a.login).unwrap_or_default(),
            standing,
            body: comment.body.unwrap_or_default(),
        }
    }
}

fn parse_talk(stdout: &[u8]) -> Result<Talk, ForgeError> {
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
        pull_request: Pr,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Pr {
        comments: Nodes<Comment>,
        review_threads: Nodes<Thread>,
    }
    let reply: Reply = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    let pr = reply.data.repository.pull_request;
    Ok(Talk {
        comments: pr.comments.nodes.into_iter().map(Remark::from).collect(),
        threads: (pr.review_threads.nodes.into_iter())
            .map(|t| TalkThread {
                id: t.id,
                path: t.path,
                line: t.line,
                resolved: t.is_resolved,
                outdated: t.is_outdated,
                remarks: t.comments.nodes.into_iter().map(Remark::from).collect(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Written by hand to the shape `QUERY` asks for: no recording of it
    // exists, since the worker's sandbox cannot call the API.
    const TALK: &str = include_str!("../../../../../fixtures/gh-pr-talk.json");

    #[test]
    fn a_conversation_is_read_with_each_authors_standing() {
        let talk = parse_talk(TALK.as_bytes()).unwrap();
        let seen: Vec<_> = talk
            .comments
            .iter()
            .map(|c| (c.id, c.author.as_str(), c.standing))
            .collect();
        assert_eq!(
            seen,
            [
                (9001, "octo-maintainer", Standing::Writer),
                (9002, "codecov", Standing::Bot),
                (9003, "passer-by", Standing::Other),
                (9004, "", Standing::Other),
            ]
        );
        assert_eq!(talk.comments[0].body, "Please rename the flag.");
    }

    #[test]
    fn a_review_thread_is_read_with_its_place_and_its_comments_oldest_first() {
        let talk = parse_talk(TALK.as_bytes()).unwrap();
        assert_eq!(talk.threads.len(), 2);
        let open = &talk.threads[0];
        assert_eq!(
            (
                open.id.as_str(),
                open.path.as_str(),
                open.line,
                open.resolved
            ),
            ("PRRT_open", "src/lib.rs", Some(12), false)
        );
        let ids: Vec<_> = open.remarks.iter().map(|r| r.id).collect();
        assert_eq!(ids, [7001, 7002]);
        assert!(talk.threads[1].resolved);
        assert_eq!(talk.threads[1].line, None);
    }

    #[test]
    fn a_threads_outdated_flag_is_read() {
        let talk = parse_talk(TALK.as_bytes()).unwrap();
        let flags: Vec<_> = talk.threads.iter().map(|t| t.outdated).collect();
        assert_eq!(flags, [false, true]);
    }

    #[test]
    fn an_answer_that_is_not_the_query_s_is_unreadable() {
        let err = parse_talk(b"{\"data\":{}}").unwrap_err();
        assert!(matches!(err, ForgeError::Unreadable(_)), "{err:?}");
    }
}
