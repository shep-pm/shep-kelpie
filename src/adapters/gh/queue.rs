//! Where a pull request stands in the merge queue

use serde::Deserialize;

use super::{gh, unreadable};
use crate::ports::{ForgeError, QueueStanding};
use crate::settings::ForgeSlug;

// `gh pr view --json` has no merge queue field. The removals are timeline
// events, and the latest one's reason is all the forge says about why. The
// filtered `totalCount` counts every timeline item, so the nodes are counted.
const QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { \
    repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
    isInMergeQueue \
    timelineItems(itemTypes: [REMOVED_FROM_MERGE_QUEUE_EVENT], last: 100) { \
    nodes { ... on RemovedFromMergeQueueEvent { reason } } } } } }";

pub(super) fn standing(repo: &ForgeSlug, number: u64) -> Result<QueueStanding, ForgeError> {
    let (owner, name) = repo.as_str().split_once('/').unwrap_or_default();
    parse_standing(&gh(&[
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

fn parse_standing(stdout: &[u8]) -> Result<QueueStanding, ForgeError> {
    #[derive(Deserialize)]
    struct Reply {
        data: Data,
    }
    #[derive(Deserialize)]
    struct Data {
        repository: Repository,
    }
    #[derive(Deserialize)]
    struct Repository {
        #[serde(rename = "pullRequest")]
        pull_request: Option<Pr>,
    }
    #[derive(Deserialize)]
    struct Pr {
        #[serde(rename = "isInMergeQueue")]
        queued: bool,
        #[serde(rename = "timelineItems")]
        removals: Removals,
    }
    #[derive(Deserialize)]
    struct Removals {
        nodes: Vec<Removal>,
    }
    #[derive(Deserialize)]
    struct Removal {
        reason: Option<String>,
    }
    let reply: Reply = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    let pr = reply
        .data
        .repository
        .pull_request
        .ok_or_else(|| unreadable(stdout))?;
    Ok(QueueStanding {
        queued: pr.queued,
        removals: u32::try_from(pr.removals.nodes.len()).unwrap_or(u32::MAX),
        reason: pr.removals.nodes.into_iter().last().and_then(|r| r.reason),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(pr: &str) -> String {
        format!(r#"{{"data":{{"repository":{{"pullRequest":{pr}}}}}}}"#)
    }

    #[test]
    fn a_queued_pull_request_with_no_removal_reads_as_queued() {
        let pr = reply(r#"{"isInMergeQueue":true,"timelineItems":{"nodes":[]}}"#);
        assert_eq!(
            parse_standing(pr.as_bytes()).unwrap(),
            QueueStanding {
                queued: true,
                removals: 0,
                reason: None
            }
        );
    }

    #[test]
    fn a_removed_pull_request_carries_the_latest_reason_and_the_count() {
        let pr = reply(
            r#"{"isInMergeQueue":false,"timelineItems":{"nodes":[
            {"reason":"older"},{"reason":"Required status check \"test\" failed."}]}}"#,
        );
        assert_eq!(
            parse_standing(pr.as_bytes()).unwrap(),
            QueueStanding {
                queued: false,
                removals: 2,
                reason: Some("Required status check \"test\" failed.".into()),
            }
        );
    }

    #[test]
    fn a_reply_without_a_pull_request_is_unreadable() {
        let pr = reply("null");
        assert!(matches!(
            parse_standing(pr.as_bytes()),
            Err(ForgeError::Unreadable(_))
        ));
    }
}
