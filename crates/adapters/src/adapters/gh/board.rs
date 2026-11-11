//! The board's two lists: the ready issues, with what blocks each, and the
//! open pull requests, with the issues each closes

use std::path::Path;

use serde::Deserialize;

use super::suites::{Suites, suites_fields};
use super::{Check, Label, checks, gh_as, unreadable};
use crate::board::{Blocker, OpenPullRequest, READY, ReadyIssue, ReviewMarks, SubIssues};
use crate::ports::ForgeError;
use crate::settings::ForgeSlug;
use crate::ticket::TicketId;

// `gh` lists newest first and stops at its limit, so a short limit would
// hide the oldest item. A list this long is not expected.
const LIST_LIMIT: &str = "1000";

pub(super) fn ready_issues(
    config: Option<&Path>,
    repo: &ForgeSlug,
) -> Result<Vec<ReadyIssue>, ForgeError> {
    parse_ready_issues(&gh_as(config, &ready_args(repo))?)
}

// `gh issue list` lists issues only, so a pull request labelled
// `ready-for-agent` to ask for a rework never reads as new work.
fn ready_args(repo: &ForgeSlug) -> [&str; 12] {
    [
        "issue",
        "list",
        "--repo",
        repo.as_str(),
        "--label",
        READY,
        "--state",
        "open",
        "--limit",
        LIST_LIMIT,
        "--json",
        "number,title,body,assignees,labels,blockedBy,parent,subIssuesSummary",
    ]
}

// One query lists every open pull request with what the board, rework, gate
// and merge steps read of it. Reviews and threads come as counts only:
// `reviewed` reads their words, for one pull request. A page holds 100, so
// each 100 more open pull requests cost one more query.
const OPEN_QUERY: &str = concat!(
    "query($owner: String!, $name: String!, $after: String) { \
    repository(owner: $owner, name: $name) { \
    pullRequests(states: OPEN, first: 100, after: $after, \
    orderBy: {field: CREATED_AT, direction: DESC}) { \
    pageInfo { hasNextPage endCursor } nodes { \
    number headRefName headRefOid isDraft isCrossRepository author { __typename login } \
    closingIssuesReferences(first: 50) { nodes { number repository { name owner { login } } } } \
    labels(first: 100) { nodes { name } } \
    reviews(first: 1) { totalCount } reviewThreads(first: 1) { totalCount } \
    commits(last: 1) { nodes { commit { statusCheckRollup { contexts(first: 100) { nodes { \
    __typename ... on CheckRun { name status conclusion completedAt detailsUrl startedAt workflowName checkSuite { id } } \
    ... on StatusContext { context state } } } } ",
    suites_fields!(),
    " } } } } } } }"
);

// As many pages as hold `LIST_LIMIT` pull requests
const OPEN_PAGES: usize = 10;

pub(super) fn open_pull_requests(
    config: Option<&Path>,
    repo: &ForgeSlug,
) -> Result<Vec<OpenPullRequest>, ForgeError> {
    let (owner, name) = repo.as_str().split_once('/').unwrap_or_default();
    let mut open = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..OPEN_PAGES {
        let query = format!("query={OPEN_QUERY}");
        let (owner, name) = (format!("owner={owner}"), format!("name={name}"));
        let mut args = vec!["api", "graphql", "-f", &query, "-f", &owner, "-f", &name];
        let cursor = after.as_ref().map(|c| format!("after={c}"));
        if let Some(cursor) = &cursor {
            args.extend(["-f", cursor]);
        }
        let page = parse_pull_requests(&gh_as(config, &args)?, repo)?;
        open.extend(page.pull_requests);
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    Ok(open)
}

// `gh` lists an issue's first 50 blockers and counts them all.
fn parse_ready_issues(stdout: &[u8]) -> Result<Vec<ReadyIssue>, ForgeError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Listed {
        number: u64,
        // Absent only from a listing made without asking for them.
        #[serde(default)]
        title: String,
        #[serde(default)]
        body: String,
        assignees: Vec<serde::de::IgnoredAny>,
        labels: Vec<Label>,
        blocked_by: BlockedBy,
        parent: Option<Parent>,
        // Absent only from a listing made without asking for it.
        #[serde(default)]
        sub_issues_summary: Summary,
    }
    #[derive(Deserialize)]
    struct Parent {
        number: u64,
    }
    // `completed` counts the closed sub-issues, whatever they closed as.
    #[derive(Default, Deserialize)]
    struct Summary {
        total: u64,
        completed: u64,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct BlockedBy {
        nodes: Vec<Blocking>,
        total_count: u64,
    }
    #[derive(Deserialize)]
    struct Blocking {
        number: u64,
        state: State,
    }
    #[derive(Deserialize, PartialEq)]
    #[serde(rename_all = "UPPERCASE")]
    enum State {
        Open,
        Closed,
    }
    let listed: Vec<Listed> = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(listed
        .into_iter()
        .filter_map(|i| {
            Some(ReadyIssue {
                ticket: TicketId::from_issue(i.number)?,
                title: i.title,
                body: i.body,
                assigned: !i.assignees.is_empty(),
                labels: i.labels.into_iter().map(|l| l.name).collect(),
                parent: i.parent.and_then(|p| TicketId::from_issue(p.number)),
                sub_issues: SubIssues {
                    total: i.sub_issues_summary.total,
                    closed: i.sub_issues_summary.completed,
                },
                unlisted_blockers: i
                    .blocked_by
                    .total_count
                    .saturating_sub(i.blocked_by.nodes.len() as u64),
                blocked_by: i
                    .blocked_by
                    .nodes
                    .into_iter()
                    .filter_map(|b| {
                        Some(Blocker {
                            ticket: TicketId::from_issue(b.number)?,
                            open: b.state == State::Open,
                        })
                    })
                    .collect(),
            })
        })
        .collect())
}

// One page of the listing, and the cursor of the next when there is one
struct Page {
    pull_requests: Vec<OpenPullRequest>,
    next: Option<String>,
}

fn parse_pull_requests(stdout: &[u8], repo: &ForgeSlug) -> Result<Page, ForgeError> {
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
        pull_requests: Connection,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Connection {
        page_info: PageInfo,
        nodes: Vec<Listed>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PageInfo {
        has_next_page: bool,
        end_cursor: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Listed {
        number: u64,
        head_ref_name: String,
        head_ref_oid: String,
        is_draft: bool,
        is_cross_repository: bool,
        author: Option<Author>,
        closing_issues_references: Nodes<Closes>,
        labels: Nodes<Label>,
        reviews: Count,
        review_threads: Count,
        commits: Nodes<Tip>,
    }
    // A bot account's `__typename` is `Bot`; a person's is `User`.
    #[derive(Deserialize)]
    struct Author {
        #[serde(rename = "__typename")]
        kind: String,
        login: String,
    }
    #[derive(Deserialize)]
    struct Nodes<T> {
        nodes: Vec<T>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Count {
        total_count: u64,
    }
    #[derive(Deserialize)]
    struct Tip {
        commit: Commit,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Commit {
        status_check_rollup: Option<Rollup>,
        #[serde(default)]
        check_suites: Suites,
    }
    #[derive(Deserialize)]
    struct Rollup {
        contexts: Nodes<Check>,
    }
    let reply: Reply = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    let listed = reply.data.repository.pull_requests;
    let next = listed
        .page_info
        .has_next_page
        .then_some(listed.page_info.end_cursor)
        .flatten();
    let pull_requests = listed
        .nodes
        .into_iter()
        .map(|pr| {
            let tip = pr.commits.nodes.into_iter().next().map(|tip| tip.commit);
            let (rollup, suites) = tip
                .map(|c| {
                    let rollup = c.status_check_rollup.map(|r| r.contexts.nodes);
                    (rollup.unwrap_or_default(), c.check_suites)
                })
                .unwrap_or_default();
            OpenPullRequest {
                number: pr.number,
                head: pr.head_ref_name,
                closes: closed_here(pr.closing_issues_references.nodes, repo),
                labels: pr.labels.nodes.into_iter().map(|l| l.name).collect(),
                draft: pr.is_draft,
                commit: pr.head_ref_oid,
                checks: suites
                    .counted(rollup.iter().filter_map(Check::suite))
                    .fold(checks(&rollup)),
                from_fork: pr.is_cross_repository,
                author_is_bot: pr.author.as_ref().is_some_and(|a| a.kind == "Bot"),
                author: pr.author.map(|a| a.login).unwrap_or_default(),
                marks: ReviewMarks {
                    reviews: pr.reviews.total_count,
                    threads: pr.review_threads.total_count,
                },
            }
        })
        .collect();
    Ok(Page {
        pull_requests,
        next,
    })
}

/// An issue a pull request closes, on whichever repo it is
#[derive(Deserialize)]
pub(super) struct Closes {
    number: u64,
    repository: Repository,
}

#[derive(Deserialize)]
struct Repository {
    name: String,
    owner: Owner,
}

#[derive(Deserialize)]
struct Owner {
    login: String,
}

// A pull request can close issues on other repos, which are not this board's.
pub(super) fn closed_here(closes: Vec<Closes>, repo: &ForgeSlug) -> Vec<u64> {
    let (owner, name) = repo.as_str().split_once('/').unwrap_or_default();
    closes
        .into_iter()
        .filter(|c| {
            let r = &c.repository;
            r.owner.login.eq_ignore_ascii_case(owner) && r.name.eq_ignore_ascii_case(name)
        })
        .map(|c| c.number)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::is_bump;
    use crate::ports::{Checks, Queued};
    use crate::settings::Dependencies;
    use crate::test::ticket;

    // Recorded from gh 2.96 on this repo with the `ready_issues` arguments.
    const READY_LIST: &str = include_str!("../../../../../fixtures/gh-issue-list.json");

    #[test]
    fn the_board_lists_issues_so_a_labelled_pull_request_is_never_new_work() {
        let repo = ForgeSlug::try_from("shep-pm/shep".to_owned()).unwrap();
        let args = ready_args(&repo);
        assert_eq!(args[..2], ["issue", "list"]);
        assert!(
            !args
                .iter()
                .any(|a| a.contains("search") || a.contains("api"))
        );
    }

    // The shape `OPEN_QUERY` asks for, written from GitHub's schema: `gh api`
    // is not one a worker's sandbox runs, so it is not a recording.
    const OPEN_LIST: &str =
        include_str!("../../../../../fixtures/gh-graphql-open-pull-requests.json");

    fn blocker(number: u64, open: bool) -> Blocker {
        Blocker {
            ticket: ticket(number),
            open,
        }
    }

    #[test]
    fn ready_issues_are_read() {
        let issues = parse_ready_issues(READY_LIST.as_bytes()).unwrap();
        let ids: Vec<TicketId> = issues.iter().map(|i| i.ticket).collect();
        assert_eq!(ids, [40, 38, 37, 33, 32, 30, 27, 19, 5].map(ticket));
        assert!(issues.iter().all(|i| !i.assigned));
        assert_eq!(issues[0].labels, ["bug", "ready-for-agent"]);
    }

    #[test]
    fn each_ready_issue_carries_its_blockers_open_or_closed() {
        let issues = parse_ready_issues(READY_LIST.as_bytes()).unwrap();
        assert_eq!(issues[0].blocked_by, []);
        assert_eq!(issues[1].blocked_by, [blocker(12, false)]);
        assert_eq!(
            issues[6].blocked_by,
            [
                blocker(40, true),
                blocker(37, true),
                blocker(38, true),
                blocker(33, true),
                blocker(32, true),
                blocker(30, true),
                blocker(24, false),
                blocker(19, true),
            ]
        );
    }

    // Recorded from gh 2.96 on cli/cli with the `ready_issues` fields, since
    // no repo of the maintainer's has sub-issues yet.
    const SUB_ISSUES: &str = include_str!("../../../../../fixtures/gh-issue-list-sub-issues.json");

    #[test]
    fn each_ready_issue_carries_its_parent_and_its_sub_issues() {
        let issues = parse_ready_issues(SUB_ISSUES.as_bytes()).unwrap();
        let read: Vec<(TicketId, Option<TicketId>, SubIssues)> = issues
            .iter()
            .map(|i| (i.ticket, i.parent, i.sub_issues))
            .collect();
        let count = |total, closed| SubIssues { total, closed };
        assert_eq!(
            read,
            [
                (ticket(14529), None, count(6, 0)),
                (ticket(14528), Some(ticket(14529)), count(0, 0)),
                (ticket(12438), None, count(3, 2)),
            ]
        );
    }

    #[test]
    fn the_board_asks_for_each_issues_parent_and_sub_issues() {
        let repo = ForgeSlug::try_from("shep-pm/shep".to_owned()).unwrap();
        let args = ready_args(&repo);
        let json = args.iter().position(|&a| a == "--json").unwrap() + 1;
        let fields = args[json].split(',').collect::<Vec<_>>();
        assert!(fields.contains(&"parent") && fields.contains(&"subIssuesSummary"));
    }

    #[test]
    fn an_issue_with_an_assignee_is_assigned() {
        let listed = br#"[{"assignees":[{"id":"MDQ","login":"someone","name":""}],"labels":[],
            "number":3,"blockedBy":{"nodes":[],"totalCount":0}}]"#;
        assert!(parse_ready_issues(listed).unwrap()[0].assigned);
    }

    #[test]
    fn blockers_counted_past_the_listed_ones_are_unlisted() {
        let issues = parse_ready_issues(READY_LIST.as_bytes()).unwrap();
        assert!(issues.iter().all(|i| i.unlisted_blockers == 0));
        let listed = br#"[{"assignees":[],"labels":[],"number":3,
            "blockedBy":{"nodes":[{"number":2,"state":"CLOSED"}],"totalCount":51}}]"#;
        assert_eq!(parse_ready_issues(listed).unwrap()[0].unlisted_blockers, 50);
    }

    #[test]
    fn a_blocker_in_a_state_not_known_is_unreadable() {
        let listed = br#"[{"assignees":[],"labels":[],"number":3,
            "blockedBy":{"nodes":[{"number":2,"state":"MERGED"}],"totalCount":1}}]"#;
        assert!(matches!(
            parse_ready_issues(listed),
            Err(ForgeError::Unreadable(_))
        ));
    }

    fn kelpie() -> ForgeSlug {
        ForgeSlug::try_from("shep-pm/shep-kelpie".to_owned()).unwrap()
    }

    #[test]
    fn the_one_listing_reads_what_the_steps_after_the_board_read() {
        let page = parse_pull_requests(OPEN_LIST.as_bytes(), &kelpie()).unwrap();
        assert_eq!(page.next, None);
        assert_eq!(
            page.pull_requests,
            [
                OpenPullRequest {
                    number: 81,
                    head: "worktree-kelpie-79".into(),
                    closes: vec![79],
                    labels: vec![],
                    draft: true,
                    commit: "edc85265d286c48bd84dd729764c5043c7dd5625".into(),
                    checks: Checks::Failed(vec!["lint".into()]),
                    from_fork: false,
                    author: "the-maintainer".into(),
                    author_is_bot: false,
                    marks: ReviewMarks::default(),
                },
                OpenPullRequest {
                    number: 55,
                    head: "feat/54-see-the-ui".into(),
                    closes: vec![],
                    labels: vec![READY.into()],
                    draft: false,
                    commit: "7a1c0f3b9d2e4a58c6b1d0e9f8a7b6c5d4e3f201".into(),
                    checks: Checks::None,
                    from_fork: true,
                    author: String::new(),
                    author_is_bot: false,
                    marks: ReviewMarks {
                        reviews: 3,
                        threads: 2
                    },
                },
            ]
        );
    }

    // The shape `OPEN_QUERY` asks for, written from GitHub's schema, of pull
    // requests from other people: a person, a bot, a fork and a draft.
    const CONTRIBUTIONS: &str =
        include_str!("../../../../../fixtures/gh-pr-list-contributions.json");

    #[test]
    fn the_listing_reads_who_opened_each_pull_request_and_whether_a_bot_did() {
        let page = parse_pull_requests(CONTRIBUTIONS.as_bytes(), &kelpie()).unwrap();
        let read: Vec<_> = (page.pull_requests.iter())
            .map(|pr| {
                let head = pr.commit.get(..7).unwrap_or_default();
                let (author, bot, fork) = (pr.author.as_str(), pr.author_is_bot, pr.from_fork);
                (pr.number, author, bot, fork, pr.draft, head)
            })
            .collect();
        assert_eq!(
            read,
            [
                (104, "octocat", false, false, true, "4d1c0a9"),
                (103, "dependabot", true, false, false, "3b9e2f7"),
                (102, "newcomer", false, true, false, "2a8f1e6"),
                (101, "Octocat", false, false, false, "1f7e0d5"),
            ]
        );
        assert_eq!(page.pull_requests[2].head, "kelpie/5");
    }

    #[test]
    fn the_recorded_listing_has_one_bump_for_the_default_bots() {
        let page = parse_pull_requests(CONTRIBUTIONS.as_bytes(), &kelpie()).unwrap();
        let bumps: Vec<_> = (page.pull_requests.iter())
            .filter(|pr| is_bump(&Dependencies::default(), pr))
            .map(|pr| (pr.number, pr.head.as_str()))
            .collect();
        assert_eq!(bumps, [(103, "dependabot/cargo/serde-1.0.230")]);
    }

    #[test]
    fn the_listing_asks_for_the_authors_type() {
        assert!(OPEN_QUERY.contains("author { __typename login }"));
    }

    #[test]
    fn a_page_with_more_after_it_names_where_the_next_begins() {
        let more = br#"{"data":{"repository":{"pullRequests":{
            "pageInfo":{"hasNextPage":true,"endCursor":"abc"},"nodes":[]}}}}"#;
        assert_eq!(
            parse_pull_requests(more, &kelpie())
                .unwrap()
                .next
                .as_deref(),
            Some("abc")
        );
    }

    #[test]
    fn the_listing_is_one_query_that_reads_counts_of_reviews_and_threads_not_their_words() {
        for field in ["states: OPEN", "statusCheckRollup", "headRefOid", "isDraft"] {
            assert!(OPEN_QUERY.contains(field), "{field}");
        }
        assert!(OPEN_QUERY.contains("reviews(first: 1) { totalCount }"));
        assert!(OPEN_QUERY.contains("reviewThreads(first: 1) { totalCount }"));
        assert!(!OPEN_QUERY.contains("comments("));
    }

    // A count under each of 50 suites of 100 pull requests made the query
    // cost 57 points against 6, which spent a login's hourly 5000 in two runners.
    #[test]
    fn the_listing_counts_no_check_runs_per_suite() {
        assert!(!OPEN_QUERY.contains("checkRuns("));
        assert!(OPEN_QUERY.contains("checkSuite { id }"));
    }

    // A pull request whose rollup holds `check_run`, beside `suites`
    fn listed_check(check_run: &str, suites: &str) -> Checks {
        let page = r#"{"data":{"repository":{"pullRequests":{
            "pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{
            "number":1,"headRefName":"a","headRefOid":"b","isDraft":false,
            "isCrossRepository":false,"author":null,"closingIssuesReferences":{"nodes":[]},
            "labels":{"nodes":[]},"reviews":{"totalCount":0},"reviewThreads":{"totalCount":0},
            "commits":{"nodes":[{"commit":{"statusCheckRollup":{"contexts":{"nodes":[
                CHECK_RUN]}},
            "checkSuites":{"nodes":[SUITES]}}}]}}]}}}}"#
            .replace("CHECK_RUN", check_run)
            .replace("SUITES", suites);
        let mut page = parse_pull_requests(page.as_bytes(), &kelpie()).unwrap();
        page.pull_requests.remove(0).checks
    }

    // A pull request whose rollup holds one green check run of suite `S1`,
    // beside `suites`
    fn listed_with(suites: &str) -> Checks {
        let green = r#"{"__typename":"CheckRun","name":"test","status":"COMPLETED",
            "conclusion":"SUCCESS","checkSuite":{"id":"S1"}}"#;
        listed_check(green, suites)
    }

    // The listing asks for the time a check run completed and its link, for
    // a run GitHub left at `IN_PROGRESS` after it finished (shep-pm/shep#1050).
    #[test]
    fn the_listing_asks_for_what_reads_a_check_run_left_in_progress() {
        assert!(OPEN_QUERY.contains("completedAt detailsUrl"));
    }

    // The listing asks for the workflow a check run belongs to and the time it
    // started, for a run a later run of the same check replaced.
    #[test]
    fn the_listing_asks_for_what_names_a_check_run_its_workflow_ran_again() {
        assert!(OPEN_QUERY.contains("startedAt workflowName"));
    }

    #[test]
    fn a_listed_cancelled_run_its_check_ran_again_is_dropped() {
        let cancelled = r#"{"__typename":"CheckRun","name":"test","status":"COMPLETED",
            "conclusion":"CANCELLED","completedAt":"2026-11-07T20:30:00Z",
            "startedAt":"2026-11-07T20:25:54Z","workflowName":"test","checkSuite":{"id":"S1"}}"#;
        let passed = r#"{"__typename":"CheckRun","name":"test","status":"COMPLETED",
            "conclusion":"SUCCESS","completedAt":"2026-11-07T20:52:00Z",
            "startedAt":"2026-11-07T20:46:52Z","workflowName":"test","checkSuite":{"id":"S2"}}"#;
        assert_eq!(
            listed_check(&format!("{cancelled},{passed}"), ""),
            Checks::Passed
        );
    }

    #[test]
    fn a_listed_check_run_left_in_progress_with_a_conclusion_is_read_by_it() {
        let done = r#"{"__typename":"CheckRun","name":"typos","status":"IN_PROGRESS",
            "conclusion":"SUCCESS","completedAt":"2026-10-23T05:56:37Z",
            "detailsUrl":"https://github.com/shep-pm/shep/actions/runs/39908548395/job/1",
            "checkSuite":{"id":"S1"}}"#;
        assert_eq!(listed_check(done, ""), Checks::Passed);
    }

    #[test]
    fn a_listed_check_run_still_in_progress_names_its_workflow_run() {
        let running = r#"{"__typename":"CheckRun","name":"docs","status":"IN_PROGRESS",
            "conclusion":null,"completedAt":null,
            "detailsUrl":"https://github.com/shep-pm/shep/actions/runs/39697679104/job/2",
            "checkSuite":{"id":"S1"}}"#;
        assert_eq!(
            listed_check(running, ""),
            Checks::Pending(vec![Queued {
                name: "docs".into(),
                run: Some(39_697_679_104),
                running: true,
                held: false,
            }])
        );
    }

    #[test]
    fn a_failed_workflow_no_listed_check_run_names_started_no_job() {
        let suites = r#"{"id":"S1","conclusion":"SUCCESS","workflowRun":{"file":{"path":"test.yml"}}},
            {"id":"S2","conclusion":"FAILURE","workflowRun":{"file":{"path":"lint.yml"}}}"#;
        assert_eq!(
            listed_with(suites),
            Checks::Failed(vec!["lint.yml: started no job".into()])
        );
    }

    #[test]
    fn a_failed_workflow_a_listed_check_run_names_is_the_rollups_to_judge() {
        let suites =
            r#"{"id":"S1","conclusion":"FAILURE","workflowRun":{"file":{"path":"test.yml"}}}"#;
        assert_eq!(listed_with(suites), Checks::Passed);
    }

    #[test]
    #[ignore = "asks the real GitHub about shep-pm/shep-kelpie through gh, about 1 s"]
    fn the_real_gh_answers_the_listing_in_the_shape_parsed() {
        open_pull_requests(None, &kelpie()).unwrap();
    }

    #[test]
    fn an_issue_closed_on_another_repo_is_not_this_boards() {
        let page = parse_pull_requests(OPEN_LIST.as_bytes(), &kelpie()).unwrap();
        assert_eq!(page.pull_requests[0].closes, [79]);
        assert!(page.pull_requests[1].closes.is_empty());
    }
}
