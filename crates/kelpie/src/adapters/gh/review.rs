//! A pull request's branch and the maintainer's latest review, for a rework

use serde::Deserialize;

use super::{Label, gh, pull_request_state, unreadable};
use crate::ports::{ForgeError, MaintainerReview, ReviewComment, Reviewed};
use crate::settings::ForgeSlug;

// Reads the latest 100 reviews, threads, and comments a thread, so on a
// longer pull request the oldest fall out, never the latest review's.
const QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { \
    repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
    state isDraft headRefName isCrossRepository author { login } \
    labels(first: 100) { nodes { name } } \
    reviews(last: 100) { nodes { id state body author { __typename } } } \
    reviewThreads(last: 100) { nodes { isResolved comments(last: 100) { nodes { \
    body path line pullRequestReview { id } } } } } } } }";

pub(super) fn reviewed(repo: &ForgeSlug, number: u64) -> Result<Reviewed, ForgeError> {
    let (owner, name) = repo.as_str().split_once('/').unwrap_or_default();
    parse_reviewed(&gh(&[
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
    ])?)
}

pub(super) fn viewer() -> Result<String, ForgeError> {
    parse_viewer(&gh(&["api", "user", "--jq", ".login"])?)
}

fn parse_viewer(stdout: &[u8]) -> Result<String, ForgeError> {
    let login = String::from_utf8_lossy(stdout).trim().to_owned();
    let plain = !login.is_empty() && !login.contains(char::is_whitespace);
    plain.then_some(login).ok_or_else(|| unreadable(stdout))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pr {
    state: String,
    is_draft: bool,
    head_ref_name: String,
    is_cross_repository: bool,
    author: Option<Login>,
    labels: Nodes<Label>,
    reviews: Nodes<Review>,
    review_threads: Nodes<Thread>,
}

#[derive(Deserialize)]
struct Login {
    login: String,
}

#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
struct Review {
    id: String,
    state: String,
    body: Option<String>,
    author: Option<Author>,
}

#[derive(Deserialize)]
struct Author {
    #[serde(rename = "__typename")]
    kind: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Thread {
    is_resolved: bool,
    comments: Nodes<Comment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Comment {
    body: Option<String>,
    path: String,
    line: Option<u32>,
    pull_request_review: Option<ReviewId>,
}

#[derive(Deserialize)]
struct ReviewId {
    id: String,
}

// A pending review is a draft only its author sees, and a bot's is never
// the maintainer's.
pub(crate) fn parse_reviewed(stdout: &[u8]) -> Result<Reviewed, ForgeError> {
    #[derive(Deserialize)]
    struct Reply {
        data: Data,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Data {
        repository: Repository,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Repository {
        pull_request: Pr,
    }
    let reply: Reply = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    let pr = reply.data.repository.pull_request;
    let latest = pr
        .reviews
        .nodes
        .into_iter()
        .rev()
        .find(|r| r.state != "PENDING" && r.author.as_ref().is_some_and(|a| a.kind == "User"));
    let review = latest.map(|latest| {
        let comments = pr
            .review_threads
            .nodes
            .into_iter()
            .filter(|t| !t.is_resolved)
            .flat_map(|t| t.comments.nodes)
            .filter(|c| c.pull_request_review.as_ref().map(|r| &r.id) == Some(&latest.id))
            .map(|c| ReviewComment {
                file: c.path,
                line: c.line,
                body: c.body.unwrap_or_default(),
            })
            .collect();
        MaintainerReview {
            changes_requested: latest.state == "CHANGES_REQUESTED",
            body: latest.body.unwrap_or_default(),
            id: latest.id,
            comments,
        }
    });
    Ok(Reviewed {
        state: pull_request_state(&pr.state, stdout)?,
        branch: pr.head_ref_name,
        from_fork: pr.is_cross_repository,
        author: pr.author.map(|a| a.login).unwrap_or_default(),
        draft: pr.is_draft,
        labels: pr.labels.nodes.into_iter().map(|l| l.name).collect(),
        review,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::PullRequestState;

    // Recorded from gh 2.96: `gh api graphql` with `QUERY` on shep-pm/shep#617.
    // The maintainer's reviews there are replies with empty bodies, and
    // CodeRabbit's reviews come after them, with every thread resolved.
    const PR_617: &str = include_str!("../../../fixtures/gh-pr-reviewed-617.json");

    fn reply(pr: &str) -> String {
        format!(r#"{{"data":{{"repository":{{"pullRequest":{pr}}}}}}}"#)
    }

    #[test]
    fn the_viewer_is_the_plain_login_gh_prints() {
        assert_eq!(parse_viewer(b"TurtIeSocks\n"), Ok("TurtIeSocks".to_owned()));
        for bad in [&b""[..], b"\n", b"two words\n"] {
            assert!(matches!(parse_viewer(bad), Err(ForgeError::Unreadable(_))));
        }
    }

    #[test]
    fn the_query_reads_the_latest_reviews_threads_and_comments() {
        for latest in [
            "reviews(last: 100)",
            "reviewThreads(last: 100)",
            "comments(last: 100)",
        ] {
            assert!(QUERY.contains(latest), "{latest}");
        }
        assert!(!QUERY.contains("reviewThreads(first"));
        assert!(!QUERY.contains("comments(first"));
    }

    #[test]
    fn a_bots_later_review_and_resolved_threads_are_not_the_maintainers() {
        let pr = parse_reviewed(PR_617.as_bytes()).unwrap();
        assert_eq!(pr.author, "TurtIeSocks");
        assert_eq!(
            (pr.state, pr.branch.as_str(), pr.from_fork, pr.draft),
            (
                PullRequestState::Open,
                "c/stoic-dijkstra-31dgjs",
                false,
                false
            )
        );
        assert_eq!(
            pr.review,
            Some(MaintainerReview {
                id: "PRR_kwDOTytUD88AAAABPoL1yQ".into(),
                changes_requested: false,
                body: String::new(),
                comments: vec![],
            })
        );
    }

    #[test]
    fn the_latest_reviews_unresolved_comments_are_read_with_their_file_and_line() {
        let pr = reply(
            r#"{"state":"OPEN","isDraft":true,"headRefName":"kelpie/33",
            "isCrossRepository":false,"labels":{"nodes":[{"name":"review please"}]},
            "reviews":{"nodes":[
              {"id":"R1","state":"CHANGES_REQUESTED","body":"old","author":{"__typename":"User"}},
              {"id":"R2","state":"CHANGES_REQUESTED","body":"Redesign the timeline.","author":{"__typename":"User"}},
              {"id":"R3","state":"PENDING","body":"draft","author":{"__typename":"User"}}]},
            "reviewThreads":{"nodes":[
              {"isResolved":false,"comments":{"nodes":[
                {"body":"older","path":"a.ts","line":1,"pullRequestReview":{"id":"R1"}},
                {"body":"Use the grid here.","path":"src/Timeline.tsx","line":12,"pullRequestReview":{"id":"R2"}}]}},
              {"isResolved":true,"comments":{"nodes":[
                {"body":"settled","path":"b.ts","line":2,"pullRequestReview":{"id":"R2"}}]}},
              {"isResolved":false,"comments":{"nodes":[
                {"body":"Outdated now.","path":"src/old.ts","line":null,"pullRequestReview":{"id":"R2"}}]}}]}}"#,
        );
        let pr = parse_reviewed(pr.as_bytes()).unwrap();
        assert_eq!(pr.labels, ["review please"]);
        assert_eq!(
            pr.review,
            Some(MaintainerReview {
                id: "R2".into(),
                changes_requested: true,
                body: "Redesign the timeline.".into(),
                comments: vec![
                    ReviewComment {
                        file: "src/Timeline.tsx".into(),
                        line: Some(12),
                        body: "Use the grid here.".into(),
                    },
                    ReviewComment {
                        file: "src/old.ts".into(),
                        line: None,
                        body: "Outdated now.".into(),
                    },
                ],
            })
        );
    }

    #[test]
    fn a_pull_request_with_no_review_from_a_person_has_none() {
        let pr = reply(
            r#"{"state":"CLOSED","isDraft":false,"headRefName":"kelpie/3",
            "isCrossRepository":true,"labels":{"nodes":[]},
            "reviews":{"nodes":[{"id":"R1","state":"COMMENTED","body":"bot","author":{"__typename":"Bot"}},
              {"id":"R2","state":"COMMENTED","body":"ghost","author":null}]},
            "reviewThreads":{"nodes":[]}}"#,
        );
        let pr = parse_reviewed(pr.as_bytes()).unwrap();
        assert_eq!((pr.state, pr.from_fork), (PullRequestState::Closed, true));
        assert_eq!(pr.review, None);
    }

    #[test]
    #[ignore = "asks the real GitHub about shep-pm/shep#617 through gh, about 1 s"]
    fn the_real_gh_answers_the_query_in_the_recorded_shape() {
        let repo = ForgeSlug::try_from("shep-pm/shep".to_owned()).unwrap();
        let pr = reviewed(&repo, 617).unwrap();
        assert_eq!(pr.branch, "c/stoic-dijkstra-31dgjs");
        assert!(pr.review.is_some());
    }

    #[test]
    fn a_reply_without_a_pull_request_is_unreadable() {
        assert!(matches!(
            parse_reviewed(br#"{"data":{"repository":{"pullRequest":null}}}"#),
            Err(ForgeError::Unreadable(_))
        ));
    }
}
