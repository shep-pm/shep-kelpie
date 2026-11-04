//! A review bot's comments, reviews, threads, head statuses and check runs
//! on a pull request, and the label and thread changes the project manager
//! makes

use std::path::Path;

use serde::Deserialize;

use super::conditional::{Etags, GhApi};
use super::{gh, gh_as, unreadable};
use crate::github::api::{REPLY_TO_THREAD, RESOLVE_THREAD};
use crate::ports::{ForgeError, Timestamp};
use crate::review_bot::{Activity, CheckRun, Comment, Login, Reaction, Review, Status, Thread};
use crate::settings::ForgeSlug;

// A pull request with more than 100 threads is not expected; one past it
// would read as unresolved-but-unseen, never as satisfied.
const THREADS: &str = "query($owner: String!, $name: String!, $number: Int!) { \
    repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
    reviewThreads(first: 100) { nodes { id isResolved isOutdated path line \
    comments(first: 1) { nodes { author { login } body } } } } } } }";

// Codex reacts on the pull request itself: a thumbs up when it has nothing to
// say, which it may leave with no comment.
const REACTIONS: &str = "query($owner: String!, $name: String!, $number: Int!) { \
    repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
    reactions(last: 50) { nodes { content createdAt user { login } } } } } }";

pub(super) fn activity(
    etags: &Etags,
    config: Option<&Path>,
    repo: &ForgeSlug,
    number: u64,
    login: Login<'_>,
) -> Result<Activity, ForgeError> {
    let bot = login.rest;
    let slug = repo.as_str();
    let list = |path: String| etags.pages(&GhApi, config, &path);
    let comments = made_by(
        &list(format!(
            "repos/{slug}/issues/{number}/comments?per_page=100"
        ))?,
        "user",
        bot,
        &["body", "updated_at"],
    )?;
    let reviews = made_by(
        &list(format!("repos/{slug}/pulls/{number}/reviews?per_page=100"))?,
        "user",
        bot,
        &["commit_id", "body", "submitted_at"],
    )?;
    let (owner, name) = repo.as_str().split_once('/').unwrap_or_default();
    let threads = gh_as(
        config,
        &[
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
        ],
    )?;
    let reactions = gh_as(
        config,
        &[
            "api",
            "graphql",
            "-f",
            &format!("query={REACTIONS}"),
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("name={name}"),
            "-F",
            &format!("number={number}"),
        ],
    )?;
    // The pull request's own ref, so the head needs no read of its own.
    let statuses = made_by(
        &list(format!(
            "repos/{slug}/commits/refs/pull/{number}/head/statuses?per_page=100"
        ))?,
        "creator",
        bot,
        &["url", "description", "created_at"],
    )?;
    let runs = match etags.pages_in(
        &GhApi,
        config,
        &format!("repos/{slug}/commits/refs/pull/{number}/head/check-runs?per_page=100"),
        "check_runs",
    ) {
        Err(e) if refused_or_missing(&e) => Vec::new(),
        pages => pages?,
    };
    Ok(Activity {
        comments: parse_comments(&comments)?,
        reviews: parse_reviews(&reviews)?,
        threads: parse_threads(&threads, login.graphql)?,
        statuses: parse_statuses(&statuses)?,
        checks: parse_checks(&runs, login.graphql)?,
        reactions: parse_reactions(&reactions, login.rest)?,
    })
}

// An App installed before it could read Checks, and a head with no runs, are
// GitHub's 403 and 404: there is nothing to read beside the rest.
fn refused_or_missing(error: &ForgeError) -> bool {
    matches!(error, ForgeError::Failed(said)
        if said.contains("HTTP 403") || said.contains("HTTP 404"))
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

// GitHub answers an already resolved thread as resolved, and one it no
// longer has with a not-found error, which leaves nothing to resolve.
pub(super) fn resolve(thread: &str) -> Result<(), ForgeError> {
    let resolved = match gh(&[
        "api",
        "graphql",
        "-f",
        &format!("query={RESOLVE_THREAD}"),
        "-f",
        &format!("id={thread}"),
    ]) {
        Err(e) if gone(&e) => return Ok(()),
        reply => reply?,
    };
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

// GitHub answers a thread it no longer has with a not-found error, which
// leaves nothing to reply to.
pub(super) fn reply(thread: &str, body: &str) -> Result<(), ForgeError> {
    match gh(&[
        "api",
        "graphql",
        "-f",
        &format!("query={REPLY_TO_THREAD}"),
        "-f",
        &format!("id={thread}"),
        "-f",
        &format!("body={body}"),
    ]) {
        Err(e) if gone(&e) => Ok(()),
        reply => reply.map(drop),
    }
}

// Whether `error` is GitHub's answer for a node id it does not have.
fn gone(error: &ForgeError) -> bool {
    matches!(error, ForgeError::Failed(stderr)
        if stderr.contains("Could not resolve to a node with the global id"))
}

pub(super) fn time(text: &str, stdout: &[u8]) -> Result<Timestamp, ForgeError> {
    let at: jiff::Timestamp = text.parse().map_err(|_| unreadable(stdout))?;
    let seconds = u64::try_from(at.as_second()).map_err(|_| unreadable(stdout))?;
    Ok(Timestamp(seconds))
}

// The `fields` of each item `who` made, as `by` names it, one JSON object a
// line across the pages
fn made_by(pages: &[Vec<u8>], by: &str, who: &str, fields: &[&str]) -> Result<Vec<u8>, ForgeError> {
    let mut out = Vec::new();
    for page in pages {
        let items: Vec<serde_json::Value> =
            serde_json::from_slice(page).map_err(|_| unreadable(page))?;
        for item in items {
            if item[by]["login"].as_str() != Some(who) {
                continue;
            }
            let kept: serde_json::Map<_, _> = (fields.iter())
                .map(|f| ((*f).to_owned(), item[*f].clone()))
                .collect();
            serde_json::to_writer(&mut out, &kept).map_err(|_| unreadable(page))?;
            out.push(b'\n');
        }
    }
    Ok(out)
}

// One JSON object a line, across every page.
fn lines<T: for<'de> Deserialize<'de>>(stdout: &[u8]) -> Result<Vec<T>, ForgeError> {
    stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.trim_ascii().is_empty())
        .map(|line| serde_json::from_slice(line).map_err(|_| unreadable(stdout)))
        .collect()
}

/// The comments in the lines of `gh api` JSON
///
/// # Errors
///
/// When `gh` printed what is not the expected JSON.
pub fn parse_comments(stdout: &[u8]) -> Result<Vec<Comment>, ForgeError> {
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

/// The reviews in the lines of `gh api` JSON
///
/// # Errors
///
/// When `gh` printed what is not the expected JSON.
pub fn parse_reviews(stdout: &[u8]) -> Result<Vec<Review>, ForgeError> {
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

/// The statuses in the lines of `gh api` JSON
///
/// A status names its commit only as the last part of its URL.
///
/// # Errors
///
/// When `gh` printed what is not the expected JSON.
pub fn parse_statuses(stdout: &[u8]) -> Result<Vec<Status>, ForgeError> {
    #[derive(Deserialize)]
    struct Line {
        url: String,
        description: Option<String>,
        created_at: String,
    }
    lines::<Line>(stdout)?
        .into_iter()
        .map(|s| {
            let commit = match s.url.rsplit_once('/') {
                Some((_, commit)) if !commit.is_empty() => commit,
                _ => return Err(unreadable(stdout)),
            };
            Ok(Status {
                commit: commit.to_owned(),
                description: s.description.unwrap_or_default(),
                at: time(&s.created_at, stdout)?,
            })
        })
        .collect()
}

/// The check runs the bot's App completed on the head, newest first
///
/// A run another App posted is left out, as is one still running: only a
/// finished one says what the bot has to say.
///
/// # Errors
///
/// When `gh` printed what is not the expected JSON.
pub fn parse_checks(pages: &[Vec<u8>], bot: &str) -> Result<Vec<CheckRun>, ForgeError> {
    #[derive(Deserialize)]
    struct Page {
        check_runs: Vec<Run>,
    }
    #[derive(Deserialize)]
    struct Run {
        app: App,
        status: String,
        conclusion: Option<String>,
        completed_at: String,
        output: Output,
    }
    #[derive(Deserialize)]
    struct App {
        slug: String,
    }
    #[derive(Deserialize)]
    struct Output {
        title: Option<String>,
        summary: Option<String>,
    }
    let mut runs: Vec<CheckRun> = Vec::new();
    for body in pages {
        let page: Page = serde_json::from_slice(body).map_err(|_| unreadable(body))?;
        for run in page.check_runs {
            if run.app.slug != bot || run.status != "completed" {
                continue;
            }
            runs.push(CheckRun {
                title: run.output.title.unwrap_or_default(),
                summary: run.output.summary.unwrap_or_default(),
                conclusion: run.conclusion.unwrap_or_default(),
                at: time(&run.completed_at, body)?,
            });
        }
    }
    runs.sort_by_key(|run| std::cmp::Reverse(run.at));
    Ok(runs)
}

/// The reactions in the lines of `gh api` JSON
///
/// Only the reactions `bot` left, by its REST login: a reaction's user comes
/// with the `[bot]` suffix, which another account of the same name lacks.
///
/// # Errors
///
/// When `gh` printed what is not the expected JSON.
pub fn parse_reactions(stdout: &[u8], bot: &str) -> Result<Vec<Reaction>, ForgeError> {
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
    struct PullRequest {
        reactions: Nodes,
    }
    #[derive(Deserialize)]
    struct Nodes {
        nodes: Vec<Node>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Node {
        content: String,
        created_at: String,
        user: Option<User>,
    }
    #[derive(Deserialize)]
    struct User {
        login: String,
    }
    let reply: Reply = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    reply
        .data
        .repository
        .pull_request
        .reactions
        .nodes
        .into_iter()
        .filter(|node| node.user.as_ref().is_some_and(|u| u.login == bot))
        .map(|node| {
            Ok(Reaction {
                content: node.content,
                at: time(&node.created_at, stdout)?,
            })
        })
        .collect()
}

/// The review threads in `gh api graphql`'s answer
///
/// Only the threads whose first comment is by `bot`, its GraphQL login.
///
/// # Errors
///
/// When `gh` printed what is not the expected JSON.
pub fn parse_threads(stdout: &[u8], bot: &str) -> Result<Vec<Thread>, ForgeError> {
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
        // Fixtures recorded before kelpie asked for it have none.
        #[serde(default)]
        is_outdated: bool,
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
            let by_bot = first.author.is_some_and(|a| a.login == bot);
            by_bot.then_some(Thread {
                id: node.id,
                resolved: node.is_resolved,
                outdated: node.is_outdated,
                path: node.path,
                line: node.line,
                body: first.body,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_thread_github_no_longer_has_is_gone_and_any_other_refusal_is_not() {
        let missing = "gh: Could not resolve to a node with the global id of 'PRRT_kwDOx'.\n";
        assert!(super::gone(&ForgeError::Failed(missing.into())));
        let scope = "gh: Resource not accessible by integration (HTTP 403)\n";
        assert!(!super::gone(&ForgeError::Failed(scope.into())));
        assert!(!super::gone(&ForgeError::Spawn("no gh".into())));
    }

    use super::*;

    #[test]
    fn only_what_the_bot_made_is_kept_across_pages_and_read_as_the_jq_lines_were() {
        let pages = [
            br#"[{"id":1,"user":{"login":"coderabbitai[bot]"},"body":"a","updated_at":"2026-10-10T05:31:54Z"},
                 {"id":2,"user":{"login":"octocat"},"body":"b","updated_at":"2026-10-10T05:31:55Z"}]"#
                .to_vec(),
            br#"[{"id":3,"user":null,"body":"c","updated_at":"2026-10-10T05:31:56Z"},
                 {"id":4,"user":{"login":"coderabbitai[bot]"},"body":"d","updated_at":"2026-10-10T05:31:57Z"}]"#
                .to_vec(),
        ];

        let kept = made_by(&pages, "user", "coderabbitai[bot]", &["body", "updated_at"]).unwrap();

        let comments = parse_comments(&kept).unwrap();
        let bodies: Vec<_> = comments.iter().map(|c| c.body.as_str()).collect();
        assert_eq!(bodies, ["a", "d"]);
    }

    #[test]
    fn a_status_is_kept_by_its_creator_not_its_user() {
        let page = br#"[{"url":"https://api.github.com/repos/o/r/statuses/abc123","description":"d","created_at":"2026-10-10T05:31:54Z",
                         "creator":{"login":"cubic-dev-ai[bot]"}}]"#
            .to_vec();

        let kept = made_by(
            &[page],
            "creator",
            "cubic-dev-ai[bot]",
            &["url", "description", "created_at"],
        )
        .unwrap();

        assert_eq!(parse_statuses(&kept).unwrap().len(), 1);
    }

    #[test]
    fn a_page_that_is_not_a_list_is_unreadable() {
        let kept = made_by(&[b"{}".to_vec()], "user", "bot", &["body"]);

        assert!(matches!(kept, Err(ForgeError::Unreadable(_))));
    }

    #[test]
    fn a_page_of_nothing_is_no_comments() {
        assert_eq!(parse_comments(b"").unwrap(), []);
        assert_eq!(parse_reviews(b"\n").unwrap(), []);
        assert_eq!(parse_statuses(b"").unwrap(), []);
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
    fn a_status_takes_its_commit_from_its_url() {
        let line = br#"{"created_at":"2026-09-29T05:53:45Z","description":"Review completed","url":"https://api.github.com/repos/shep-pm/shep/statuses/7d30d0f6f8d03314fe9f060b4627643bb32db8cc"}"#;
        let parsed = parse_statuses(line).unwrap();
        assert_eq!(parsed[0].commit, "7d30d0f6f8d03314fe9f060b4627643bb32db8cc");
        assert_eq!(parsed[0].description, "Review completed");
        let at = r#""created_at":"2026-09-29T05:53:45Z""#;
        for url in ["x", "https://api.github.com/repos/o/r/statuses/"] {
            let line = format!(r#"{{"url":"{url}",{at}}}"#);
            assert!(
                matches!(
                    parse_statuses(line.as_bytes()),
                    Err(ForgeError::Unreadable(_))
                ),
                "{url}"
            );
        }
    }

    // Recorded from shep-pm/shep-kelpie#234: Codex's thumbs up at 04:43:06Z.
    const REACTIONS_234: &str = include_str!("../../../../../fixtures/codex-reactions-234.json");

    #[test]
    fn a_reaction_is_read_by_the_bots_exact_login() {
        let codex = crate::codex::LOGIN.rest;
        let seen = parse_reactions(REACTIONS_234.as_bytes(), codex).unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].content, "THUMBS_UP");
        assert_eq!(seen[0].at, Timestamp(1_790_829_786));
        let cubic = crate::cubic::LOGIN.rest;
        assert_eq!(
            parse_reactions(REACTIONS_234.as_bytes(), cubic).unwrap(),
            []
        );
        // The same name with no suffix is an account, not the app.
        let account = REACTIONS_234.replace("connector[bot]", "connector");
        assert_eq!(parse_reactions(account.as_bytes(), codex).unwrap(), []);
        assert!(matches!(
            parse_reactions(b"{}", codex),
            Err(ForgeError::Unreadable(_))
        ));
    }

    // Recorded from shep-pm/shep#1544's head, where cubic posted no comment
    // and only this run, on 2026-11-04.
    const CHECK_RUNS_1544: &str =
        include_str!("../../../../../fixtures/gh-cubic-check-runs-limit.json");

    #[test]
    fn cubics_check_runs_are_read_from_the_recorded_page() {
        let seen = parse_checks(&[CHECK_RUNS_1544.as_bytes().to_vec()], "cubic-dev-ai").unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].title, "AI review line limit reached");
        assert_eq!(seen[0].conclusion, "neutral");
        assert_eq!(seen[0].at, Timestamp(1_793_796_444));
        assert!(
            seen[0]
                .summary
                .contains("Reviews resume on 29 November 2026 (in 25 days)"),
            "{}",
            seen[0].summary
        );
        assert_eq!(
            parse_checks(&[CHECK_RUNS_1544.as_bytes().to_vec()], "coderabbitai").unwrap(),
            [],
            "the run is cubic's App, not another bot's"
        );
    }

    #[test]
    fn another_apps_check_runs_are_left_out() {
        let page = br#"{"total_count":3,"check_runs":[
            {"app":{"slug":"github-actions"},"status":"completed","conclusion":"success",
             "completed_at":"2026-11-04T12:00:00Z","output":{"title":"Build","summary":"ok"}},
            {"app":{"slug":"cubic-dev-ai"},"status":"in_progress","conclusion":null,
             "completed_at":"1970-01-01T00:00:00Z","output":{"title":null,"summary":null}},
            {"app":{"slug":"cubic-dev-ai"},"status":"completed","conclusion":"neutral",
             "completed_at":"2026-11-04T12:47:24Z","output":{"title":"AI review line limit reached",
             "summary":"Reviews resume on 29 November 2026."}}]}"#
            .to_vec();

        let seen = parse_checks(&[page], "cubic-dev-ai").unwrap();

        let titles: Vec<&str> = seen.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(
            titles,
            ["AI review line limit reached"],
            "its running run is not one"
        );
    }

    #[test]
    fn runs_are_newest_first_across_pages() {
        let older = br#"{"check_runs":[{"app":{"slug":"cubic-dev-ai"},"status":"completed",
            "conclusion":"neutral","completed_at":"2026-11-04T12:00:00Z",
            "output":{"title":"AI review completed","summary":"clean"}}]}"#
            .to_vec();
        let newer = br#"{"check_runs":[{"app":{"slug":"cubic-dev-ai"},"status":"completed",
            "conclusion":"neutral","completed_at":"2026-11-04T12:47:24Z",
            "output":{"title":"AI review line limit reached","summary":"out of lines"}}]}"#
            .to_vec();

        let seen = parse_checks(&[older, newer], "cubic-dev-ai").unwrap();

        assert_eq!(seen[0].title, "AI review line limit reached");
        assert_eq!(seen[1].title, "AI review completed");
    }

    #[test]
    fn a_head_whose_check_runs_the_app_cannot_read_is_no_checks() {
        let scope = "gh: Resource not accessible by integration (HTTP 403)\n";
        assert!(refused_or_missing(&ForgeError::Failed(scope.into())));
        let missing = "gh: Not Found (HTTP 404)\n";
        assert!(refused_or_missing(&ForgeError::Failed(missing.into())));
        let limited = ForgeError::Limited {
            said: "gh: API rate limit exceeded (HTTP 403)".into(),
            cooldown: crate::ports::Cooldown::default(),
        };
        assert!(
            !refused_or_missing(&limited),
            "a limit is asked about again"
        );
        assert!(!refused_or_missing(&ForgeError::Spawn("no gh".into())));
    }

    #[test]
    fn a_thread_whose_first_comment_has_no_author_is_not_coderabbits() {
        let reply = br#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
            {"id":"a","isResolved":false,"path":"x","line":1,
             "comments":{"nodes":[{"author":null,"body":"ghost"}]}}]}}}}}"#;
        let coderabbit = crate::coderabbit::LOGIN.graphql;
        assert_eq!(parse_threads(reply, coderabbit).unwrap(), []);
    }

    #[test]
    fn an_outdated_thread_is_read_so_and_is_not_open() {
        let reply = br#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
            {"id":"a","isResolved":false,"isOutdated":true,"path":"x","line":null,
             "comments":{"nodes":[{"author":{"login":"cubic-dev-ai"},"body":"P2: old"}]}},
            {"id":"b","isResolved":false,"isOutdated":false,"path":"x","line":3,
             "comments":{"nodes":[{"author":{"login":"cubic-dev-ai"},"body":"P2: new"}]}}]}}}}}"#;
        let threads = parse_threads(reply, crate::cubic::LOGIN.graphql).unwrap();
        let outdated: Vec<bool> = threads.iter().map(|t| t.outdated).collect();
        assert_eq!(outdated, [true, false]);
        let activity = crate::review_bot::Activity {
            threads,
            ..Default::default()
        };
        let open: Vec<&str> = activity.open_threads().map(|t| t.id.as_str()).collect();
        assert_eq!(open, ["b"]);
    }
}
