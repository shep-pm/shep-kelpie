//! A pull request's branch, what it closes and the maintainer's latest
//! review, for a rework or an adoption

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use super::board::{Closes, closed_here};
use super::{Label, gh, gh_as, pull_request_state, unreadable};
use crate::ports::{ForgeError, InstallationToken, MaintainerReview, ReviewComment, Reviewed};
use crate::settings::ForgeSlug;

// Reads the latest 100 reviews, threads, and comments a thread, so on a
// longer pull request the oldest fall out, never the latest review's.
const QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { \
    repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
    state title body isDraft baseRefName headRefName isCrossRepository maintainerCanModify headRepository { url owner { __typename } } \
    author { login } \
    closingIssuesReferences(first: 50) { nodes { number repository { name owner { login } } } } \
    labels(first: 100) { nodes { name } } \
    reviews(last: 100) { nodes { id state body authorAssociation author { __typename login } } } \
    reviewThreads(last: 100) { nodes { isResolved comments(last: 100) { nodes { \
    body path line pullRequestReview { id } } } } } } } }";

pub(super) fn reviewed(
    config: Option<&Path>,
    repo: &ForgeSlug,
    number: u64,
) -> Result<Reviewed, ForgeError> {
    let (owner, name) = repo.as_str().split_once('/').unwrap_or_default();
    parse_reviewed(
        &gh_as(
            config,
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
        )?,
        repo,
        |login| {
            gh_as(
                config,
                &[
                    "api",
                    &format!("repos/{}/collaborators/{login}/permission", repo.as_str()),
                ],
            )
        },
    )
}

pub(super) fn viewer() -> Result<String, ForgeError> {
    parse_viewer(&gh(&["api", "user", "--jq", ".login"])?)
}

pub(super) fn login_token() -> Result<InstallationToken, ForgeError> {
    login_token_with(gh)
}

// `gh auth token` prints a token gh already knows is dead, so the login is
// checked first. What `auth status` printed is dropped: it can hold part of a token.
fn login_token_with(
    run: impl Fn(&[&str]) -> Result<Vec<u8>, ForgeError>,
) -> Result<InstallationToken, ForgeError> {
    // Only the active account's token is the one `auth token` prints.
    match run(&["auth", "status", "--hostname", "github.com", "--active"]) {
        Err(spawn @ ForgeError::Spawn(_)) => return Err(spawn),
        Err(held @ ForgeError::Held(_)) => return Err(held),
        Err(ForgeError::Limited { cooldown, .. }) => {
            return Err(ForgeError::Limited {
                said: "`gh auth status` was rate limited".to_owned(),
                cooldown,
            });
        }
        Err(_) => {
            return Err(ForgeError::Failed(
                "`gh auth status` says the login is invalid".to_owned(),
            ));
        }
        Ok(_) => {}
    }
    parse_login_token(&run(&["auth", "token"])?)
}

// The token is one line. What failed to parse is not echoed: it may be a token.
fn parse_login_token(stdout: &[u8]) -> Result<InstallationToken, ForgeError> {
    let token = String::from_utf8_lossy(stdout).trim().to_owned();
    match token.is_empty() || token.contains(char::is_whitespace) {
        true => Err(ForgeError::Failed(
            "`gh auth token` printed no single token".to_owned(),
        )),
        false => Ok(InstallationToken::new(token)),
    }
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
    title: String,
    body: Option<String>,
    closing_issues_references: Nodes<Closes>,
    is_draft: bool,
    base_ref_name: String,
    head_ref_name: String,
    is_cross_repository: bool,
    #[serde(default)]
    maintainer_can_modify: bool,
    #[serde(default)]
    head_repository: Option<Head>,
    author: Option<Login>,
    labels: Nodes<Label>,
    reviews: Nodes<Review>,
    review_threads: Nodes<Thread>,
}

#[derive(Deserialize)]
struct Head {
    url: String,
    #[serde(default)]
    owner: Option<Owner>,
}

#[derive(Deserialize)]
struct Owner {
    #[serde(rename = "__typename")]
    kind: String,
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
#[serde(rename_all = "camelCase")]
struct Review {
    id: String,
    state: String,
    author_association: String,
    body: Option<String>,
    author: Option<Author>,
}

#[derive(Deserialize)]
struct Author {
    #[serde(rename = "__typename")]
    kind: String,
    #[serde(default)]
    login: Option<String>,
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
// the maintainer's. On a public repo anyone can review, so only someone
// with a hand in the repo counts.
const TRUSTED: [&str; 3] = ["OWNER", "MEMBER", "COLLABORATOR"];

// The App reads a reviewer whose organization membership is private as a
// contributor, so a person the repo lets write also counts. `permission`
// reads one login's collaborator permission.
const WRITERS: [&str; 3] = ["write", "maintain", "admin"];

// A refusal or a reply it cannot read leaves the review uncounted. A tool
// that did not start and a rate limit are the caller's to see: an older
// review must not stand in for one that was never judged.
fn can_write(answer: Result<Vec<u8>, ForgeError>) -> Result<bool, ForgeError> {
    #[derive(Deserialize)]
    struct Permission {
        permission: String,
    }
    let body = match answer {
        Ok(body) => body,
        Err(
            reached @ (ForgeError::Spawn(_) | ForgeError::Held(_) | ForgeError::Limited { .. }),
        ) => return Err(reached),
        Err(_) => return Ok(false),
    };
    Ok(serde_json::from_slice::<Permission>(&body)
        .is_ok_and(|p| WRITERS.contains(&p.permission.as_str())))
}

pub(crate) fn parse_reviewed(
    stdout: &[u8],
    repo: &ForgeSlug,
    mut permission: impl FnMut(&str) -> Result<Vec<u8>, ForgeError>,
) -> Result<Reviewed, ForgeError> {
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
    let mut writers: HashMap<String, bool> = HashMap::new();
    let mut latest = None;
    for r in pr.reviews.nodes.into_iter().rev() {
        let Some(author) = r.author.as_ref().filter(|a| a.kind == "User") else {
            continue;
        };
        if r.state == "PENDING" {
            continue;
        }
        let counts = match (
            TRUSTED.contains(&r.author_association.as_str()),
            &author.login,
        ) {
            (true, _) => true,
            (false, None) => false,
            (false, Some(login)) => match writers.get(login) {
                Some(&known) => known,
                None => {
                    let known = can_write(permission(login))?;
                    writers.insert(login.clone(), known);
                    known
                }
            },
        };
        if counts {
            latest = Some(r);
            break;
        }
    }
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
        title: pr.title,
        body: pr.body.unwrap_or_default(),
        closes: closed_here(pr.closing_issues_references.nodes, repo),
        base: pr.base_ref_name,
        branch: pr.head_ref_name,
        from_fork: pr.is_cross_repository,
        maintainer_can_edit: pr.maintainer_can_modify,
        // GitHub names a repository by its page, which is also its clone URL
        // with `.git` after it.
        head_clone_url: pr
            .head_repository
            .as_ref()
            .map(|r| format!("{}.git", r.url)),
        head_owner_is_organization: pr
            .head_repository
            .and_then(|r| r.owner)
            .is_some_and(|o| o.kind == "Organization"),
        author: pr.author.map(|a| a.login).unwrap_or_default(),
        draft: pr.is_draft,
        labels: pr.labels.nodes.into_iter().map(|l| l.name).collect(),
        review,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{Cooldown, PullRequestState, Timestamp};

    // Recorded from gh 2.96: `gh api graphql` with `QUERY` on shep-pm/shep#617,
    // where the maintainer's reviews are empty replies and every thread is
    // resolved. `maintainerCanModify` and `headRepository` are added by hand
    // in the schema's shape; the ignored test below reads them from GitHub.
    const PR_617: &str = include_str!("../../../../../fixtures/gh-pr-reviewed-617.json");

    // The shape of #1241's reply, whose fork an organization owns, written by
    // hand from the schema; the ignored test below reads the real one.
    const ORG_FORK: &str = include_str!("../../../../../fixtures/gh-pr-reviewed-org-fork.json");

    fn shep() -> ForgeSlug {
        ForgeSlug::try_from("shep-pm/shep".to_owned()).unwrap()
    }

    // Recorded in the shape of `gh api repos/shep-pm/shep/collaborators/<login>/permission`.
    const ADMIN: &str =
        include_str!("../../../../../fixtures/gh-collaborator-permission-admin.json");

    fn unasked(login: &str) -> Result<Vec<u8>, ForgeError> {
        panic!("the permission of {login} was read")
    }

    // Two reviews by `maintainer`, whom the App reads as a contributor, the
    // newest after an older `MEMBER` review by `colleague`
    fn private_member_reviews() -> String {
        reply(
            r#"{"state":"OPEN","title":"t","body":"","isDraft":false,"baseRefName":"main",
            "headRefName":"fix/3","isCrossRepository":false,"author":{"login":"newcomer"},
            "closingIssuesReferences":{"nodes":[]},"labels":{"nodes":[]},
            "reviews":{"nodes":[
              {"id":"R1","state":"COMMENTED","body":"","authorAssociation":"MEMBER","author":{"__typename":"User","login":"colleague"}},
              {"id":"R2","state":"COMMENTED","body":"","authorAssociation":"CONTRIBUTOR","author":{"__typename":"User","login":"maintainer"}},
              {"id":"R3","state":"CHANGES_REQUESTED","body":"Fix it.","authorAssociation":"CONTRIBUTOR","author":{"__typename":"User","login":"maintainer"}}]},
            "reviewThreads":{"nodes":[]}}"#,
        )
    }

    fn latest_id(permission: impl FnMut(&str) -> Result<Vec<u8>, ForgeError>) -> Option<String> {
        let pr = private_member_reviews();
        let pr = parse_reviewed(pr.as_bytes(), &shep(), permission).unwrap();
        pr.review.map(|r| r.id)
    }

    #[test]
    fn a_private_member_with_write_access_is_the_maintainer() {
        let id = latest_id(|_| Ok(ADMIN.as_bytes().to_vec()));
        assert_eq!(id.as_deref(), Some("R3"));
    }

    #[test]
    fn a_contributor_with_read_access_is_not_the_maintainer() {
        let read = ADMIN.replace(r#""permission": "admin""#, r#""permission": "read""#);
        let id = latest_id(|_| Ok(read.clone().into_bytes()));
        assert_eq!(id.as_deref(), Some("R1"));
    }

    #[test]
    fn a_failed_permission_read_leaves_the_review_uncounted() {
        for failed in [
            Err(ForgeError::Failed("HTTP 404".to_owned())),
            Ok(b"not json".to_vec()),
        ] {
            assert_eq!(latest_id(|_| failed.clone()).as_deref(), Some("R1"));
        }
    }

    #[test]
    fn a_permission_read_that_the_forge_held_back_is_the_callers_to_see() {
        let pr = private_member_reviews();
        for held in [
            ForgeError::Spawn("not found".to_owned()),
            ForgeError::Held(Timestamp(1_900_000_000)),
            ForgeError::Limited {
                said: "rate limit".to_owned(),
                cooldown: Cooldown::default(),
            },
        ] {
            let held = Err(held);
            let answer = parse_reviewed(pr.as_bytes(), &shep(), |_| held.clone());
            assert_eq!(answer.err().as_ref(), held.as_ref().err());
        }
    }

    #[test]
    fn the_permission_is_read_once_per_login() {
        let mut asked = Vec::new();
        let id = latest_id(|login| {
            asked.push(login.to_owned());
            Ok(ADMIN.as_bytes().to_vec())
        });
        assert_eq!(id.as_deref(), Some("R3"));
        assert_eq!(asked, ["maintainer"]);
        asked.clear();
        let read = ADMIN.replace(r#""permission": "admin""#, r#""permission": "none""#);
        latest_id(|login| {
            asked.push(login.to_owned());
            Ok(read.clone().into_bytes())
        });
        assert_eq!(asked, ["maintainer"], "a refusal is remembered too");
    }

    fn reply(pr: &str) -> String {
        format!(r#"{{"data":{{"repository":{{"pullRequest":{pr}}}}}}}"#)
    }

    #[test]
    fn the_login_token_is_the_one_line_gh_prints_and_nothing_else_is_echoed() {
        let token = parse_login_token(b"gho_abc123\r\n").unwrap();
        assert_eq!(token.expose(), "gho_abc123");
        for bad in [&b""[..], b"\n", b"gho_abc def\n"] {
            let error = parse_login_token(bad).unwrap_err().to_string();
            assert!(!error.contains("gho_abc"), "{error}");
        }
    }

    // gh 2.x's stderr for a login it knows is dead, with exit 1, written from
    // gh's format by hand since the sandbox cannot run `gh auth`.
    const INVALID_LOGIN: &str = include_str!("../../../../../fixtures/gh-auth-status-invalid.txt");

    // A gh that answers `auth status` with `status` and `auth token` with `token`
    fn gh_saying(
        status: Result<Vec<u8>, ForgeError>,
        token: &'static [u8],
    ) -> impl Fn(&[&str]) -> Result<Vec<u8>, ForgeError> {
        move |args| match args {
            ["auth", "status", "--hostname", "github.com", "--active"] => status.clone(),
            ["auth", "token"] => Ok(token.to_vec()),
            other => panic!("unexpected gh call {other:?}"),
        }
    }

    #[test]
    fn a_valid_login_gives_the_token_gh_prints() {
        let token = login_token_with(gh_saying(Ok(b"github.com\n".to_vec()), b"gho_abc123\n"));
        assert_eq!(token.unwrap().expose(), "gho_abc123");
    }

    #[test]
    fn an_invalid_login_is_refused_without_echoing_what_gh_status_printed() {
        let failed = gh_saying(
            Err(ForgeError::Failed(INVALID_LOGIN.to_owned())),
            b"gho_dead123\n",
        );
        let error = login_token_with(failed).unwrap_err().to_string();
        assert!(error.contains("invalid"), "{error}");
        for part in INVALID_LOGIN.lines().chain(["octocat", "gho_dead123"]) {
            assert!(!error.contains(part), "{error}");
        }
    }

    #[test]
    fn a_rate_limited_status_check_keeps_its_cooldown_and_none_of_what_gh_printed() {
        let cooldown = Cooldown::default();
        let limited = gh_saying(
            Err(ForgeError::Limited {
                said: format!("gh failed: {INVALID_LOGIN}"),
                cooldown,
            }),
            b"",
        );
        let error = login_token_with(limited).unwrap_err();
        assert!(matches!(&error, ForgeError::Limited { cooldown: c, .. } if *c == cooldown));
        assert!(!error.to_string().contains("octocat"), "{error}");
        assert!(!error.to_string().contains("invalid"), "{error}");
    }

    #[test]
    fn a_gh_that_cannot_start_is_reported_as_such() {
        let missing = gh_saying(Err(ForgeError::Spawn("not found".to_owned())), b"");
        assert_eq!(
            login_token_with(missing).unwrap_err(),
            ForgeError::Spawn("not found".to_owned())
        );
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
        assert!(QUERY.contains("author { __typename login }"));
    }

    #[test]
    fn a_bots_later_review_and_resolved_threads_are_not_the_maintainers() {
        let pr = parse_reviewed(PR_617.as_bytes(), &shep(), unasked).unwrap();
        assert_eq!(pr.author, "TurtIeSocks");
        assert_eq!(
            pr.title,
            "feat(bench): judge versus-pm2 runs against a committed baseline"
        );
        assert!(pr.body.contains("#367"), "{}", pr.body);
        assert_eq!(pr.closes, [367]);
        assert_eq!(pr.base, "main");
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
    fn the_gh_adapter_reads_maintainer_can_modify_and_the_head_repository_from_the_recording() {
        assert!(QUERY.contains("maintainerCanModify"));
        assert!(QUERY.contains("headRepository { url owner { __typename } }"));
        let pr = parse_reviewed(PR_617.as_bytes(), &shep(), unasked).unwrap();
        assert!(!pr.from_fork);
        assert!(!pr.maintainer_can_edit);
        assert_eq!(
            pr.head_clone_url.as_deref(),
            Some("https://github.com/shep-pm/shep.git")
        );
        assert!(pr.pushable(), "a branch on the repo itself needs no edits");

        let fork = |can_modify: bool, head: &str| {
            let pr = reply(&format!(
                r#"{{"state":"OPEN","title":"t","body":"","isDraft":false,"baseRefName":"main",
                "headRefName":"patch-1","isCrossRepository":true,"maintainerCanModify":{can_modify},
                "headRepository":{head},"author":{{"login":"newcomer"}},
                "closingIssuesReferences":{{"nodes":[]}},"labels":{{"nodes":[]}},
                "reviews":{{"nodes":[]}},"reviewThreads":{{"nodes":[]}}}}"#
            ));
            parse_reviewed(pr.as_bytes(), &shep(), unasked).unwrap()
        };
        let open = fork(true, r#"{"url":"https://github.com/newcomer/shep"}"#);
        assert!(open.from_fork && open.maintainer_can_edit);
        assert!(!open.head_owner_is_organization);
        assert_eq!(
            open.head_clone_url.as_deref(),
            Some("https://github.com/newcomer/shep.git")
        );
        assert!(open.pushable());
        let locked = fork(false, r#"{"url":"https://github.com/newcomer/shep"}"#);
        assert!(!locked.pushable(), "a fork without edits is not pushable");
        let deleted = fork(true, "null");
        assert_eq!(deleted.head_clone_url, None);
        assert!(!deleted.pushable(), "a deleted fork is not pushable");
    }

    #[test]
    fn an_organization_s_fork_is_read_as_not_pushable() {
        let pr = parse_reviewed(ORG_FORK.as_bytes(), &shep(), unasked).unwrap();
        assert!(pr.from_fork && pr.maintainer_can_edit);
        assert!(pr.head_owner_is_organization);
        assert_eq!(
            pr.head_clone_url.as_deref(),
            Some("https://github.com/Nitjsefnie-OSC/shep-kelpie.git")
        );
        assert!(
            !pr.pushable(),
            "GitHub refuses a push to an organization's fork"
        );
    }

    #[test]
    fn the_latest_reviews_unresolved_comments_are_read_with_their_file_and_line() {
        let pr = reply(
            r#"{"state":"OPEN","title":"t","body":"","isDraft":true,"baseRefName":"main","headRefName":"kelpie/33",
            "closingIssuesReferences":{"nodes":[]},
            "isCrossRepository":false,"labels":{"nodes":[{"name":"review please"}]},
            "reviews":{"nodes":[
              {"id":"R1","state":"CHANGES_REQUESTED","body":"old","authorAssociation":"OWNER","author":{"__typename":"User"}},
              {"id":"R2","state":"CHANGES_REQUESTED","body":"Redesign the timeline.","authorAssociation":"OWNER","author":{"__typename":"User"}},
              {"id":"R3","state":"PENDING","body":"draft","authorAssociation":"OWNER","author":{"__typename":"User"}}]},
            "reviewThreads":{"nodes":[
              {"isResolved":false,"comments":{"nodes":[
                {"body":"older","path":"a.ts","line":1,"pullRequestReview":{"id":"R1"}},
                {"body":"Use the grid here.","path":"src/Timeline.tsx","line":12,"pullRequestReview":{"id":"R2"}}]}},
              {"isResolved":true,"comments":{"nodes":[
                {"body":"settled","path":"b.ts","line":2,"pullRequestReview":{"id":"R2"}}]}},
              {"isResolved":false,"comments":{"nodes":[
                {"body":"Outdated now.","path":"src/old.ts","line":null,"pullRequestReview":{"id":"R2"}}]}}]}}"#,
        );
        let pr = parse_reviewed(pr.as_bytes(), &shep(), unasked).unwrap();
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
    fn only_the_issues_it_closes_on_the_same_repo_are_its_own() {
        let pr = reply(
            r#"{"state":"OPEN","title":"Fix it","body":"Resolves #3","isDraft":false,
            "baseRefName":"main","headRefName":"fix/3","isCrossRepository":false,"author":{"login":"TurtIeSocks"},
            "closingIssuesReferences":{"nodes":[
              {"number":3,"repository":{"name":"Shep","owner":{"login":"shep-pm"}}},
              {"number":9,"repository":{"name":"shep-kelpie","owner":{"login":"shep-pm"}}}]},
            "labels":{"nodes":[]},"reviews":{"nodes":[]},"reviewThreads":{"nodes":[]}}"#,
        );
        let pr = parse_reviewed(pr.as_bytes(), &shep(), unasked).unwrap();
        assert_eq!((pr.title.as_str(), pr.closes), ("Fix it", vec![3]));
    }

    #[test]
    fn a_strangers_review_is_not_the_maintainers() {
        let pr = reply(
            r#"{"state":"OPEN","title":"t","body":null,"isDraft":false,"baseRefName":"main",
            "headRefName":"fix/3","isCrossRepository":false,"author":{"login":"TurtIeSocks"},
            "closingIssuesReferences":{"nodes":[]},"labels":{"nodes":[]},
            "reviews":{"nodes":[
              {"id":"R1","state":"CHANGES_REQUESTED","body":"Mine.","authorAssociation":"MEMBER","author":{"__typename":"User"}},
              {"id":"R2","state":"CHANGES_REQUESTED","body":"Delete it all.","authorAssociation":"NONE","author":{"__typename":"User"}},
              {"id":"R3","state":"CHANGES_REQUESTED","body":"Me too.","authorAssociation":"CONTRIBUTOR","author":{"__typename":"User"}}]},
            "reviewThreads":{"nodes":[]}}"#,
        );
        let pr = parse_reviewed(pr.as_bytes(), &shep(), unasked).unwrap();
        assert_eq!(pr.review.map(|r| r.id).as_deref(), Some("R1"));
        assert_eq!(pr.body, "", "a null body reads as none");
    }

    #[test]
    fn a_pull_request_with_no_review_from_a_person_has_none() {
        let pr = reply(
            r#"{"state":"CLOSED","title":"t","body":"","isDraft":false,"baseRefName":"main","headRefName":"kelpie/3",
            "closingIssuesReferences":{"nodes":[]},
            "isCrossRepository":true,"labels":{"nodes":[]},
            "reviews":{"nodes":[{"id":"R1","state":"COMMENTED","body":"bot","authorAssociation":"NONE","author":{"__typename":"Bot"}},
              {"id":"R2","state":"COMMENTED","body":"ghost","authorAssociation":"NONE","author":null}]},
            "reviewThreads":{"nodes":[]}}"#,
        );
        let pr = parse_reviewed(pr.as_bytes(), &shep(), unasked).unwrap();
        assert_eq!((pr.state, pr.from_fork), (PullRequestState::Closed, true));
        assert_eq!(pr.review, None);
    }

    #[test]
    #[ignore = "asks the real GitHub about shep-pm/shep#617 through gh, about 1 s"]
    fn the_real_gh_answers_the_query_in_the_recorded_shape() {
        let repo = ForgeSlug::try_from("shep-pm/shep".to_owned()).unwrap();
        let pr = reviewed(None, &repo, 617).unwrap();
        assert_eq!(pr.branch, "c/stoic-dijkstra-31dgjs");
        assert_eq!(
            pr.head_clone_url.as_deref(),
            Some("https://github.com/shep-pm/shep.git")
        );
        assert!(pr.review.is_some());
    }

    #[test]
    fn a_reply_without_a_pull_request_is_unreadable() {
        assert!(matches!(
            parse_reviewed(
                br#"{"data":{"repository":{"pullRequest":null}}}"#,
                &shep(),
                unasked
            ),
            Err(ForgeError::Unreadable(_))
        ));
    }
}
