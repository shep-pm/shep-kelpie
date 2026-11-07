//! Issues kelpie files: the open ones it checks first, a new one, and a
//! parent it closes

use serde::Deserialize;

use super::review_bot::time;
use super::{gh, unreadable};
use crate::ports::{
    AuthorAssociation, FinishedParent, ForgeError, IssueCloseReason, OpenIssue, OpenPull, SubIssue,
    SubIssueState, Timestamp,
};
use crate::settings::ForgeSlug;
use crate::ticket::TicketId;

// The same ceiling the board's lists use, since `gh` lists newest first and
// stops at its limit.
const LIST_LIMIT: &str = "1000";

// An item is an issue or a pull request, so pages are read until
// `LIST_ISSUES` issues are in hand or the endpoint runs out. `LIST_PAGES`
// bounds the cost: past it, open pull requests crowding the pages out lose the
// oldest issues.
const PER_PAGE: usize = 100;
const LIST_ISSUES: usize = 1000;
const LIST_PAGES: usize = 20;

// `gh issue list --json` has no `authorAssociation` in gh 2.96, which the
// triage limits read, so the open issues come from the REST endpoint. Its
// pull requests are left out, and each 100 more items cost one more call.
pub(super) fn open_issues(repo: &ForgeSlug) -> Result<Vec<OpenIssue>, ForgeError> {
    let mut open = Vec::new();
    for page in 1..=LIST_PAGES {
        let stdout = gh(&["api", &issue_page(repo, page)])?;
        let listed = parse_open_page(&stdout)?;
        let items = listed.len();
        // A full page can take the count past the ceiling, and the endpoint
        // lists newest first, so what does not fit is the oldest of it and goes.
        open.extend(listed.into_iter().flatten().take(room(open.len())));
        if !more_pages(items, open.len()) {
            break;
        }
    }
    Ok(open)
}

// How many more issues a listing has room for under its ceiling
fn room(issues: usize) -> usize {
    LIST_ISSUES.saturating_sub(issues)
}

// Whether another page is worth asking for: the endpoint has more items, and
// the issues in hand have not reached the ceiling. Pull requests are items and
// not issues, so a page of them fills neither end.
fn more_pages(items: usize, issues: usize) -> bool {
    items == PER_PAGE && issues < LIST_ISSUES
}

// One page of the endpoint's open issues, newest first
fn issue_page(repo: &ForgeSlug, page: usize) -> String {
    format!(
        "repos/{}/issues?state=open&per_page={PER_PAGE}&page={page}",
        repo.as_str()
    )
}

// When the account `login` was created, from `created_at`. The login is one a
// listing gave.
pub(super) fn account_created(login: &str) -> Result<Timestamp, ForgeError> {
    let path = format!("users/{login}");
    parse_account_created(&gh(&["api", &path])?)
}

fn parse_account_created(stdout: &[u8]) -> Result<Timestamp, ForgeError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct Account {
        created_at: String,
    }
    let account: Account = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    time(&account.created_at, stdout)
}

// `gh` counts an issue's sub-issues closed whatever they closed as, so this
// names the candidates and `sub_issues` tells which of them built something.
pub(super) fn finished_parents(repo: &ForgeSlug) -> Result<Vec<FinishedParent>, ForgeError> {
    let args = [
        "issue",
        "list",
        "--repo",
        repo.as_str(),
        "--state",
        "open",
        "--limit",
        LIST_LIMIT,
        "--json",
        "number,subIssuesSummary",
    ];
    parse_finished_parents(&gh(&args)?)
}

fn parse_finished_parents(stdout: &[u8]) -> Result<Vec<FinishedParent>, ForgeError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Listed {
        number: u64,
        sub_issues_summary: Summary,
    }
    #[derive(Deserialize)]
    struct Summary {
        total: u64,
        completed: u64,
    }
    let listed: Vec<Listed> = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(listed
        .into_iter()
        .filter(|i| i.sub_issues_summary.total > 0)
        .filter(|i| i.sub_issues_summary.completed >= i.sub_issues_summary.total)
        .filter_map(|i| {
            Some(FinishedParent {
                ticket: TicketId::from_issue(i.number)?,
                sub_issues: i.sub_issues_summary.total,
            })
        })
        .collect())
}

pub(super) fn create_issue(
    repo: &ForgeSlug,
    title: &str,
    body: &str,
    labels: &[&str],
) -> Result<TicketId, ForgeError> {
    let mut args = vec![
        "issue",
        "create",
        "--repo",
        repo.as_str(),
        "--title",
        title,
        "--body",
        body,
    ];
    for label in labels {
        args.extend(["--label", label]);
    }
    parse_created(&gh(&args)?)
}

pub(super) fn set_label(
    repo: &ForgeSlug,
    number: u64,
    label: &str,
    add: bool,
) -> Result<(), ForgeError> {
    let flag = if add { "--add-label" } else { "--remove-label" };
    let number = number.to_string();
    gh(&[
        "issue",
        "edit",
        &number,
        "--repo",
        repo.as_str(),
        flag,
        label,
    ])
    .map(drop)
}

// One page of the endpoint's answer: every item, with `None` for the pull
// requests it answers beside the issues
fn parse_open_page(stdout: &[u8]) -> Result<Vec<Option<OpenIssue>>, ForgeError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct Listed {
        number: u64,
        title: String,
        body: Option<String>,
        user: Option<User>,
        // Only a pull request carries it, and the endpoint lists them beside issues
        pull_request: Option<serde_json::Value>,
        labels: Vec<Label>,
        created_at: String,
        // Absent only from an answer that does not carry it, which is read as
        // the listing not saying how the author stands to the repo
        #[serde(default)]
        author_association: Option<String>,
    }
    #[derive(Deserialize)]
    struct User {
        login: String,
        // A bot account's type is `Bot`, a person's is `User`
        #[serde(rename = "type")]
        kind: String,
    }
    #[derive(Deserialize)]
    struct Label {
        name: String,
    }
    let listed: Vec<Listed> = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    listed
        .into_iter()
        .map(|issue| {
            // A pull request is not an issue, and the endpoint answers both
            if issue.pull_request.is_some() {
                return Ok(None);
            }
            Ok(Some(OpenIssue {
                number: issue.number,
                title: issue.title,
                body: issue.body.unwrap_or_default(),
                author_is_bot: issue.user.as_ref().is_some_and(|u| u.kind == "Bot"),
                author: issue.user.map(|u| u.login).unwrap_or_default(),
                author_association: (issue.author_association.as_deref())
                    .and_then(AuthorAssociation::from_word),
                labels: issue.labels.into_iter().map(|l| l.name).collect(),
                created: time(&issue.created_at, stdout)?,
            }))
        })
        .collect()
}

pub(super) fn close_issue(repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
    let number = number.to_string();
    gh(&["issue", "close", &number, "--repo", repo.as_str()]).map(drop)
}

// `gh issue close` has no flag for the reason an issue closed for, so the
// issue is closed through the API, which records `state_reason` on it.
fn close_as_args(repo: &ForgeSlug, number: u64, reason: IssueCloseReason) -> Vec<String> {
    let path = format!("repos/{}/issues/{number}", repo.as_str());
    let why = format!("state_reason={}", reason.as_str());
    [
        "api",
        "--method",
        "PATCH",
        path.as_str(),
        "-f",
        "state=closed",
        "-f",
        why.as_str(),
    ]
    .map(str::to_owned)
    .to_vec()
}

pub(super) fn close_issue_as(
    repo: &ForgeSlug,
    number: u64,
    reason: IssueCloseReason,
) -> Result<(), ForgeError> {
    let args = close_as_args(repo, number, reason);
    gh(&args.iter().map(String::as_str).collect::<Vec<_>>()).map(drop)
}

pub(super) fn open_pulls(repo: &ForgeSlug) -> Result<Vec<OpenPull>, ForgeError> {
    let args = [
        "pr",
        "list",
        "--repo",
        repo.as_str(),
        "--state",
        "open",
        "--limit",
        LIST_LIMIT,
        "--json",
        "number,title,body",
    ];
    parse_open_pulls(&gh(&args)?)
}

fn parse_open_pulls(stdout: &[u8]) -> Result<Vec<OpenPull>, ForgeError> {
    #[derive(Deserialize)]
    struct Listed {
        number: u64,
        title: String,
        body: Option<String>,
    }
    let listed: Vec<Listed> = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(listed
        .into_iter()
        .map(|pull| OpenPull {
            number: pull.number,
            title: pull.title,
            closes: closes_named_in(pull.body.as_deref().unwrap_or_default()),
        })
        .collect())
}

// The issues a pull request's body names with `Resolves`, `Fixes` or `Closes`,
// in any case, in order. As GitHub reads them, the keyword is a word of its
// own, with an optional colon after it and markup before it (`**Fixes #12**`).
// A number ends at whitespace or punctuation: `#12abc` names nothing.
fn closes_named_in(body: &str) -> Vec<u64> {
    const KEYWORDS: [&str; 3] = ["resolves", "fixes", "closes"];
    let words: Vec<&str> = body.split_whitespace().collect();
    let mut named = Vec::new();
    for (at, word) in words.iter().enumerate() {
        let word = word.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '_');
        let (keyword, tail) = match word.split_once(':') {
            Some((keyword, tail)) => (keyword, Some(tail)),
            None => (word, None),
        };
        if !KEYWORDS.iter().any(|k| keyword.eq_ignore_ascii_case(k)) {
            continue;
        }
        // The number is what the colon left in the same word, or the next word
        let tail = tail.filter(|t| !t.is_empty());
        let rest = match tail {
            Some(tail) => tail,
            None => words.get(at + 1).map_or("", |next| next),
        };
        let Some(rest) = rest.strip_prefix('#') else {
            continue;
        };
        let digits: String = (rest.chars())
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        let ends = match rest.chars().nth(digits.len()) {
            None => true,
            Some(next) => !(next.is_ascii_alphanumeric() || matches!(next, '_' | '-')),
        };
        let Ok(number) = digits.parse::<u64>() else {
            continue;
        };
        if !ends || digits.is_empty() {
            continue;
        }
        if !named.contains(&number) {
            named.push(number);
        }
    }
    named
}

pub(super) fn close_pull_request(repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
    let number = number.to_string();
    gh(&["pr", "close", &number, "--repo", repo.as_str()]).map(drop)
}

// GitHub allows a parent 100 sub-issues, so one page lists them all.
pub(super) fn sub_issues(repo: &ForgeSlug, parent: u64) -> Result<Vec<SubIssue>, ForgeError> {
    let path = format!(
        "repos/{}/issues/{parent}/sub_issues?per_page=100",
        repo.as_str()
    );
    parse_sub_issues(&gh(&["api", &path])?)
}

fn parse_sub_issues(stdout: &[u8]) -> Result<Vec<SubIssue>, ForgeError> {
    #[derive(Deserialize)]
    struct Listed {
        number: u64,
        state: String,
        state_reason: Option<String>,
    }
    let listed: Vec<Listed> = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(listed
        .into_iter()
        .filter_map(|sub| {
            Some(SubIssue {
                ticket: TicketId::from_issue(sub.number)?,
                state: match (sub.state.as_str(), sub.state_reason.as_deref()) {
                    ("closed", Some("not_planned" | "duplicate")) => SubIssueState::NotPlanned,
                    ("closed", _) => SubIssueState::Completed,
                    _ => SubIssueState::Open,
                },
            })
        })
        .collect())
}

// `gh issue create` prints the new issue's URL, which ends in its number.
fn parse_created(stdout: &[u8]) -> Result<TicketId, ForgeError> {
    let text = String::from_utf8_lossy(stdout);
    text.trim()
        .rsplit('/')
        .next()
        .and_then(|number| number.parse().ok())
        .and_then(TicketId::from_issue)
        .ok_or_else(|| unreadable(stdout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::Timestamp;
    use crate::test::ticket;

    // One page of the listing's answer, as the runner would have it: the issues,
    // the pull requests left out
    fn page(stdout: &[u8]) -> Vec<OpenIssue> {
        parse_open_page(stdout)
            .unwrap()
            .into_iter()
            .flatten()
            .collect()
    }

    #[test]
    fn an_issue_closes_with_the_reason_the_forge_records_it_under() {
        let repo = ForgeSlug::try_from("shep-pm/koji".to_owned()).unwrap();
        for (reason, named) in [
            (IssueCloseReason::Duplicate, "duplicate"),
            (IssueCloseReason::NotPlanned, "not_planned"),
            (IssueCloseReason::Completed, "completed"),
        ] {
            let why = format!("state_reason={named}");
            assert_eq!(
                close_as_args(&repo, 12, reason),
                [
                    "api",
                    "--method",
                    "PATCH",
                    "repos/shep-pm/koji/issues/12",
                    "-f",
                    "state=closed",
                    "-f",
                    why.as_str(),
                ],
                "{reason:?}"
            );
        }
    }

    #[test]
    fn open_pulls_are_read_with_the_issues_their_bodies_close() {
        let out = br#"[
            {"number":41,"title":"Fix the crash","body":"One line.\n\nResolves #30\nfixes #31, and Closes #30 again."},
            {"number":42,"title":"Rework the docs","body":"No closing line here."},
            {"number":43,"title":"No body","body":null},
            {"number":44,"title":"Stop the crash","body":"Closes: #30, and (fixes #31)."}
        ]"#;
        let pulls = parse_open_pulls(out).unwrap();
        assert_eq!(
            pulls,
            [
                OpenPull {
                    number: 41,
                    title: "Fix the crash".into(),
                    closes: vec![30, 31],
                },
                OpenPull {
                    number: 42,
                    title: "Rework the docs".into(),
                    closes: Vec::new(),
                },
                OpenPull {
                    number: 43,
                    title: "No body".into(),
                    closes: Vec::new(),
                },
                OpenPull {
                    number: 44,
                    title: "Stop the crash".into(),
                    closes: vec![30, 31],
                },
            ]
        );
    }

    #[test]
    fn a_closing_keyword_names_nothing_without_a_number_after_it() {
        for body in [
            "Resolves #<issue>",
            "Resolves the crash",
            "fixes issue 12",
            "Closes#12",
            "Fixes:: #12",
            "Fixes::#12",
            "Fixes. #12",
            "unfixes #12",
            "Resolves #12abc",
            "Resolves #12_3",
            "Resolves #12-13",
            "Resolves #1x",
            "Resolves #",
            "Resolves:",
        ] {
            assert_eq!(closes_named_in(body), Vec::<u64>::new(), "{body}");
        }
        assert_eq!(closes_named_in("Fixes #12.\n"), [12]);
        assert_eq!(closes_named_in("Resolves #12; fixes #13."), [12, 13]);
    }

    // GitHub reads the keyword at a word boundary and takes a colon between it
    // and the number, with or without a space, so a body written either way
    // names the issue, and markup around the keyword hides nothing.
    #[test]
    fn a_closing_keyword_names_the_number_after_a_colon_or_under_punctuation() {
        for body in [
            "Fixes: #12",
            "Fixes:#12",
            "Closes: #12",
            "(Resolves #12)",
            "- Fixes: #12",
            "`Closes #12`",
            "**Closes #12**",
        ] {
            assert_eq!(closes_named_in(body), [12], "{body}");
        }
        assert_eq!(closes_named_in("Resolves: #12 and (fixes #13)"), [12, 13]);
    }

    #[test]
    fn finished_parents_are_the_issues_whose_sub_issues_are_all_closed() {
        let out = br#"[
            {"number":1,"subIssuesSummary":{"total":0,"completed":0,"percentCompleted":0}},
            {"number":2,"subIssuesSummary":{"total":2,"completed":1,"percentCompleted":50}},
            {"number":3,"subIssuesSummary":{"total":2,"completed":2,"percentCompleted":100}}
        ]"#;
        assert_eq!(
            parse_finished_parents(out).unwrap(),
            [FinishedParent {
                ticket: ticket(3),
                sub_issues: 2
            }]
        );
    }

    // The shape of one page of `gh api repos/{repo}/issues?state=open`, written
    // from GitHub's REST docs: `gh api` is not one a worker's sandbox runs, so
    // it is not a recording.
    #[test]
    fn open_issues_are_read_with_a_body_that_may_be_null() {
        let out = br#"[
            {"number":4,"title":"a","body":"b","created_at":"2026-10-14T08:00:00Z",
             "user":{"login":"rin","id":"U_1","type":"User"},
             "author_association":"OWNER",
             "labels":[{"name":"bug","id":"L_1"},{"name":"needs-info","id":"L_2"}]},
            {"number":5,"title":"c","body":null,"created_at":"2026-10-14T09:30:00Z",
             "user":{"login":"kelpie-app[bot]","id":"B_1","type":"Bot"},
             "author_association":"NONE","labels":[]},
            {"number":6,"title":"d","body":"","created_at":"2026-10-14T09:30:00Z",
             "user":null,"labels":[]}
        ]"#;
        let issues = page(out);
        assert_eq!(
            issues,
            [
                OpenIssue {
                    number: 4,
                    title: "a".into(),
                    body: "b".into(),
                    author: "rin".into(),
                    author_is_bot: false,
                    author_association: Some(AuthorAssociation::Owner),
                    labels: vec!["bug".into(), "needs-info".into()],
                    created: Timestamp(1_791_964_800),
                },
                OpenIssue {
                    number: 5,
                    title: "c".into(),
                    body: String::new(),
                    author: "kelpie-app[bot]".into(),
                    author_is_bot: true,
                    author_association: Some(AuthorAssociation::None),
                    labels: Vec::new(),
                    created: Timestamp(1_791_970_200),
                },
                OpenIssue {
                    number: 6,
                    title: "d".into(),
                    body: String::new(),
                    author: String::new(),
                    author_is_bot: false,
                    author_association: None,
                    labels: Vec::new(),
                    created: Timestamp(1_791_970_200),
                },
            ]
        );
    }

    // The endpoint answers pull requests beside issues, and a pull request is
    // no issue: it is not triaged, not labelled and not listed as due.
    #[test]
    fn a_pull_request_in_the_issue_listing_is_no_issue() {
        let out = br#"[
            {"number":41,"title":"a pull","body":"","created_at":"2026-10-14T08:00:00Z",
             "user":{"login":"someone","type":"User"},"author_association":"CONTRIBUTOR",
             "labels":[],"pull_request":{"url":"https://github.com/o/r/pull/41"}},
            {"number":42,"title":"an issue","body":"","created_at":"2026-10-14T08:00:00Z",
             "user":{"login":"someone","type":"User"},"author_association":"CONTRIBUTOR",
             "labels":[]}
        ]"#;
        let page = parse_open_page(out).unwrap();
        assert_eq!(page.len(), 2, "the page holds both items");
        assert_eq!(page[0], None, "the pull request is left out");
        assert_eq!(page[1].as_ref().map(|i| i.number), Some(42));
    }

    #[test]
    fn an_author_association_kelpie_does_not_know_is_no_association() {
        for (word, want) in [
            ("NONE", Some(AuthorAssociation::None)),
            ("FIRST_TIMER", Some(AuthorAssociation::FirstTimer)),
            (
                "FIRST_TIME_CONTRIBUTOR",
                Some(AuthorAssociation::FirstTimeContributor),
            ),
            ("CONTRIBUTOR", Some(AuthorAssociation::Contributor)),
            ("MEMBER", Some(AuthorAssociation::Member)),
            ("COLLABORATOR", Some(AuthorAssociation::Collaborator)),
            ("OWNER", Some(AuthorAssociation::Owner)),
            ("SOMETHING_NEW", None),
        ] {
            let out = format!(
                r#"[{{"number":4,"title":"a","body":"b","created_at":"2026-10-14T08:00:00Z","user":null,"labels":[],"author_association":"{word}"}}]"#
            );
            let issues = page(out.as_bytes());
            assert_eq!(issues[0].author_association, want, "{word}");
        }
    }

    #[test]
    fn an_account_is_read_by_the_time_its_answer_gives_it() {
        let out = br#"{"login":"newcomer","created_at":"2026-10-14T08:00:00Z"}"#;
        assert_eq!(
            parse_account_created(out).unwrap(),
            Timestamp(1_791_964_800)
        );
        let bad = br#"{"login":"newcomer"}"#;
        assert!(matches!(
            parse_account_created(bad),
            Err(ForgeError::Unreadable(_))
        ));
    }

    #[test]
    fn the_open_issue_listing_is_the_rest_endpoint_and_not_a_gh_list() {
        let repo = ForgeSlug::try_from("shep-pm/koji".to_owned()).unwrap();
        assert_eq!(
            issue_page(&repo, 2),
            "repos/shep-pm/koji/issues?state=open&per_page=100&page=2"
        );
        assert_eq!(LIST_ISSUES, 1000, "as long a list as gh stops at");
        assert_eq!(
            LIST_PAGES * PER_PAGE,
            2_000,
            "twice the issues ceiling, so a page full of pull requests cannot fill it"
        );
    }

    #[test]
    fn pull_requests_do_not_fill_the_open_issue_ceiling() {
        assert!(
            more_pages(PER_PAGE, 0),
            "a full page of pull requests is neither end"
        );
        assert!(more_pages(PER_PAGE, LIST_ISSUES - 1));
        assert!(!more_pages(PER_PAGE, LIST_ISSUES), "the ceiling is issues");
        assert!(
            !more_pages(40, 12),
            "a short page is the end of the listing"
        );
        assert_eq!(room(0), LIST_ISSUES, "a fresh listing has room");
        assert_eq!(room(LIST_ISSUES - 1), 1, "one issue of the last page");
        assert_eq!(room(LIST_ISSUES), 0, "nothing past the ceiling");
        assert_eq!(room(LIST_ISSUES + 500), 0, "and nothing to make up for it");
    }

    #[test]
    fn sub_issues_are_read_with_the_reason_each_closed_for() {
        let out = br#"[
            {"number":1,"state":"open","state_reason":"reopened"},
            {"number":2,"state":"closed","state_reason":"completed"},
            {"number":3,"state":"closed","state_reason":"not_planned"},
            {"number":4,"state":"closed","state_reason":"duplicate"},
            {"number":5,"state":"closed","state_reason":null}
        ]"#;
        let states: Vec<_> = parse_sub_issues(out)
            .unwrap()
            .iter()
            .map(|s| s.state)
            .collect();
        assert_eq!(
            states,
            [
                SubIssueState::Open,
                SubIssueState::Completed,
                SubIssueState::NotPlanned,
                SubIssueState::NotPlanned,
                SubIssueState::Completed,
            ]
        );
    }

    #[test]
    fn a_created_issues_number_is_the_end_of_the_url_gh_prints() {
        let out = b"https://github.com/shep-pm/shep-kelpie/issues/143\n";
        assert_eq!(parse_created(out).unwrap().github(), Some(143));
    }

    #[test]
    fn output_with_no_number_is_unreadable() {
        assert!(matches!(
            parse_created(b"created it\n"),
            Err(ForgeError::Unreadable(_))
        ));
    }
}
