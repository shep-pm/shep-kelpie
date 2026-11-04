//! A stand-in GitHub for kelpie's App, and a signer that needs no key

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use super::FakeClock;
use super::{AppComments, FakeCodeRabbit};
use crate::github::tokens::Jwt;
use crate::github::{
    ApiError, AppTokens, Apps, Call, Conversion, GithubApi, Grant, Installation, IssuedToken, Pem,
    Signer, Verb,
};
use crate::ports::{Clock, InstallationToken, Timestamp};
use crate::settings::ForgeSlug;

/// A key no test would print by chance, so finding it anywhere else is a leak
pub const PEM: &str =
    "-----BEGIN RSA PRIVATE KEY-----\nkelpie-test-app-key-s3cr3t\n-----END RSA PRIVATE KEY-----\n";

/// What the stand-in was asked, in order
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    /// A conversion of a manifest code, and the code
    Convert(String),
    /// An installation lookup, and the repo it was asked about
    Installation(String),
    /// An installation's token, and the repo it was asked to reach
    AccessToken(u64, String),
    /// A write, and the token it was made with
    Write(String, Call),
}

#[derive(Debug, Default)]
struct Hub {
    installed: BTreeMap<String, u64>,
    // Whether the installations hold the `workflows` permission
    workflows: bool,
    // What the next calls answer in place of their success, in turn
    failures: VecDeque<ApiError>,
    // The owner the next conversion hands back in place of the stand-in's
    convert_owner: Option<String>,
    asked: Vec<Asked>,
    jwts: Vec<String>,
    minted: u32,
    issues: u64,
    comment_ids: u64,
    // What every new check run is answered with instead of its id alone
    check_run_answer: Option<String>,
    // The check runs GitHub took, in the order it numbered them
    check_runs: Vec<ShownCheck>,
    // Why every comment is refused, while it is
    comments_refused: Option<ApiError>,
    // Why every issue body edit is refused, while it is
    issue_edits_refused: Option<ApiError>,
    // Why every new issue is refused, while it is
    issue_creates_refused: Option<ApiError>,
    // Why every label taken off an issue is refused, while it is
    label_removals_refused: Option<ApiError>,
    // Whether a review that carries line comments is refused with 422, as
    // GitHub does one naming a line the diff does not show
    line_comments_refused: bool,
    reviews_refused: bool,
    // What every review is answered with instead of the recording
    review_answer: Option<String>,
    // Why every worker's token is refused, while it is
    workers_refused: Option<ApiError>,
    // A worker's token mint held open, with who reached it and who lets it go
    worker_hold: Option<Gate>,
    // The next comment the App posts, held open the same way
    comment_hold: Option<Gate>,
    // A permission GitHub grants a worker's token beyond the one asked for
    extra_grant: Option<(String, String)>,
    // Permissions the App was never given: a token asking for one is refused
    ungiven: Vec<String>,
    // The review threads a GraphQL write lands on
    threads: Option<FakeCodeRabbit>,
    // Each comment the App posted that GitHub took: its number, its id, what
    // it says and when, which is what the forge then lists beside anyone's
    posted: Vec<(u64, u64, String, jiff::Timestamp)>,
}

/// A check run as GitHub shows it after the App's writes
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShownCheck {
    /// Its id, numbered from 5000
    pub id: u64,
    /// The commit it is on
    pub head: String,
    /// Its name
    pub name: String,
    /// `queued`, `in_progress` or `completed`
    pub status: String,
    /// How it ended, once it has
    pub conclusion: Option<String>,
    /// The title it shows
    pub title: String,
    /// The summary under the title
    pub summary: String,
}

/// A pull request review the App sent
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentReview {
    /// The commit the review is of
    pub head: String,
    /// The event: comment, approve or request changes
    pub event: String,
    /// The review's own text
    pub body: String,
    /// The path, line and text of each line comment
    pub comments: Vec<(String, u64, String)>,
}

/// What GitHub answers a review: the shape its documentation gives
pub const REVIEW_ANSWER: &str = include_str!("../../../../fixtures/github-app-review.json");

/// The id `REVIEW_ANSWER` holds
pub const REVIEW_ID: u64 = 2_718_281_828;

// Who reached a held mint and who lets it go
#[derive(Debug)]
struct Gate {
    entered: Sender<()>,
    release: Receiver<()>,
}

/// A GitHub that holds its next worker token mint or comment open until this is dropped
///
/// Preparing a worker turn mints its token with the runner's lock held, so a
/// mint held here is a pass that holds the lock. A test takes one to ask a
/// trigger of a runner in the middle of preparing a turn, or of posting a comment.
#[derive(Debug)]
pub struct HeldToken {
    entered: Receiver<()>,
    // Let go of this and the mint goes through
    release: Option<Sender<()>>,
}

impl HeldToken {
    fn gate() -> (Gate, Self) {
        let (entered, heard) = mpsc::channel();
        let (release, waited) = mpsc::channel();
        let gate = Gate {
            entered,
            release: waited,
        };
        let held = Self {
            entered: heard,
            release: Some(release),
        };
        (gate, held)
    }

    /// Whether the mint reached GitHub and waits here, within `within`
    pub fn waiting(&self, within: Duration) -> bool {
        self.entered.recv_timeout(within).is_ok()
    }

    /// Lets the mint through
    pub fn release(&mut self) {
        self.release = None;
    }
}

/// GitHub as kelpie's App sees it: one code converts to an App owned by
/// `owner`, and each token lasts an hour from the clock's now
#[derive(Debug, Clone)]
pub struct FakeGithub {
    hub: Arc<Mutex<Hub>>,
    clock: FakeClock,
    owner: String,
}

impl FakeGithub {
    /// How long a token it mints lasts, as GitHub's do
    pub const LIFETIME: u64 = 3600;

    /// A stand-in GitHub for `owner`, with its tokens' lifetimes on `clock`
    pub fn new(clock: FakeClock, owner: &str) -> Self {
        Self {
            hub: Arc::default(),
            clock,
            owner: owner.to_owned(),
        }
    }

    /// The App the stand-in hands back for a code
    pub fn conversion(&self) -> Conversion {
        Conversion {
            id: 42,
            slug: format!("kelpie-{}", self.owner.to_ascii_lowercase()),
            client_id: "Iv23liTEST".to_owned(),
            owner: self.owner.clone(),
            pem: Pem::new(PEM.to_owned()),
        }
    }

    /// Installs the App on `repo` as installation `id`
    pub fn install(&self, repo: &str, id: u64) {
        let repo = repo.to_ascii_lowercase();
        self.hub.lock().unwrap().installed.insert(repo, id);
    }

    /// Gives the App the `workflows` permission, accepted on every installation
    pub fn hold_workflows(&self) {
        self.hub.lock().unwrap().workflows = true;
    }

    /// Makes the next call answer `error`, after any failures already set
    pub fn fail_next(&self, error: ApiError) {
        self.hub.lock().unwrap().failures.push_back(error);
    }

    /// Makes every comment answer `error` from now on, whatever else is asked
    pub fn refuse_comments(&self, error: ApiError) {
        self.hub.lock().unwrap().comments_refused = Some(error);
    }

    /// Makes every issue body edit answer `error` from now on
    pub fn refuse_issue_edits(&self, error: ApiError) {
        self.hub.lock().unwrap().issue_edits_refused = Some(error);
    }

    /// Takes issue body edits again after [`Self::refuse_issue_edits`]
    pub fn accept_issue_edits(&self) {
        self.hub.lock().unwrap().issue_edits_refused = None;
    }

    /// Makes every new issue answer `error` from now on
    pub fn refuse_issue_creates(&self, error: ApiError) {
        self.hub.lock().unwrap().issue_creates_refused = Some(error);
    }

    /// Makes every label taken off an issue answer `error` from now on
    pub fn refuse_label_removals(&self, error: ApiError) {
        self.hub.lock().unwrap().label_removals_refused = Some(error);
    }

    /// Takes labels off issues again after [`Self::refuse_label_removals`]
    pub fn accept_label_removals(&self) {
        self.hub.lock().unwrap().label_removals_refused = None;
    }

    /// Takes new issues again after [`Self::refuse_issue_creates`]
    pub fn accept_issue_creates(&self) {
        self.hub.lock().unwrap().issue_creates_refused = None;
    }

    /// How many issues the App opened
    pub fn issues_created(&self) -> u64 {
        self.hub.lock().unwrap().issues
    }

    /// Makes GitHub answer 422 to every review that carries a line comment
    /// from now on, as it does one naming a line the diff does not show
    pub fn refuse_line_comments(&self) {
        self.hub.lock().unwrap().line_comments_refused = true;
    }

    /// Makes every review answer 403 from now on, line comments or not
    pub fn refuse_reviews(&self) {
        self.hub.lock().unwrap().reviews_refused = true;
    }

    /// Makes every review answer `text` from now on, whatever GitHub says
    pub fn answer_reviews_with(&self, text: &str) {
        self.hub.lock().unwrap().review_answer = Some(text.to_owned());
    }

    /// Takes reviews again after [`Self::refuse_reviews`]
    pub fn accept_reviews(&self) {
        self.hub.lock().unwrap().reviews_refused = false;
    }

    /// Makes every worker's token answer `error` from now on, as an App that
    /// was never given the Contents permission does
    pub fn refuse_workers(&self, error: ApiError) {
        self.hub.lock().unwrap().workers_refused = Some(error);
    }

    /// Every comment the App posted on issue or pull request `number` that
    /// GitHub took, as the forge lists it: its own login, a bot's, oldest first
    pub fn posted_on(&self, number: u64) -> Vec<crate::ports::ThreadComment> {
        let hub = self.hub.lock().unwrap();
        let slug = format!("kelpie-{}[bot]", self.owner.to_ascii_lowercase());
        (hub.posted.iter())
            .filter(|(thread, _, _, _)| *thread == number)
            .map(|(thread, id, body, created)| crate::ports::ThreadComment {
                id: *id,
                thread: *thread,
                author: slug.clone(),
                bot: true,
                body: body.clone(),
                created: Timestamp(created.as_second() as u64),
            })
            .collect()
    }

    /// Mints workers' tokens again after [`Self::refuse_workers`]
    pub fn accept_workers(&self) {
        self.hub.lock().unwrap().workers_refused = None;
    }

    /// Holds the next worker token mint open, so a test can ask a trigger of a
    /// runner whose pass is in the middle of preparing the turn it mints for
    pub fn hold_next_worker_token(&self) -> HeldToken {
        let (gate, held) = HeldToken::gate();
        self.hub.lock().unwrap().worker_hold = Some(gate);
        held
    }

    /// Holds the next comment the App posts open, so a test can ask a trigger
    /// of a runner whose comment is in flight
    pub fn hold_next_comment(&self) -> HeldToken {
        let (gate, held) = HeldToken::gate();
        self.hub.lock().unwrap().comment_hold = Some(gate);
        held
    }

    /// Makes GitHub grant every worker's token `permission` at `level` besides
    /// what was asked for
    pub fn grants_too(&self, permission: &str, level: &str) {
        let grant = (permission.to_owned(), level.to_owned());
        self.hub.lock().unwrap().extra_grant = Some(grant);
    }

    /// Makes `permissions` ones the App was never given: GitHub answers 422 to
    /// a token that asks for one
    pub fn never_given(&self, permissions: &[&str]) {
        let ungiven = permissions.iter().map(|&p| p.to_owned());
        self.hub.lock().unwrap().ungiven.extend(ungiven);
    }

    /// Makes the App's GraphQL writes on review threads land in `threads`
    pub fn use_threads(&self, threads: FakeCodeRabbit) {
        self.hub.lock().unwrap().threads = Some(threads);
    }

    /// Every GraphQL write the App made on a review thread, in order, with
    /// the token it was made with and the query it sent
    pub fn thread_writes(&self) -> Vec<(String, String)> {
        (self.writes().into_iter())
            .filter(|(_, call)| call.path == "/graphql")
            .map(|(token, call)| {
                (
                    token,
                    graphql(&call)["query"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                )
            })
            .collect()
    }

    /// Makes every new check run answer `text` from now on, whatever GitHub says
    pub fn answer_check_runs_with(&self, text: &str) {
        self.hub.lock().unwrap().check_run_answer = Some(text.to_owned());
    }

    /// The check runs GitHub took from the App, in creation order, each as
    /// its latest write left it
    pub fn check_runs(&self) -> Vec<ShownCheck> {
        self.hub.lock().unwrap().check_runs.clone()
    }

    /// How many writes to check runs the App made, those GitHub refused included
    pub fn check_run_writes(&self) -> usize {
        (self.every_write().into_iter())
            .filter(|(_, call)| call.path.contains("/check-runs"))
            .count()
    }

    // A check run answers with its id, numbered from 5000, and the hub keeps
    // what the write left it showing.
    fn check_run_answer(hub: &mut Hub, call: &Call) -> String {
        let sent: serde_json::Value =
            serde_json::from_str(call.body.as_deref().unwrap_or("{}")).unwrap_or_default();
        let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_owned();
        if call.verb == Verb::Post {
            let id = 5000 + hub.check_runs.len() as u64;
            hub.check_runs.push(ShownCheck {
                id,
                head: text(&sent["head_sha"]),
                name: text(&sent["name"]),
                status: String::new(),
                conclusion: None,
                title: String::new(),
                summary: String::new(),
            });
            Self::show(hub, id, &sent);
            return (hub.check_run_answer.clone()).unwrap_or_else(|| format!(r#"{{"id":{id}}}"#));
        }
        let id = call.path.rsplit('/').next().and_then(|n| n.parse().ok());
        Self::show(hub, id.unwrap_or_default(), &sent);
        "{}".to_owned()
    }

    fn show(hub: &mut Hub, id: u64, sent: &serde_json::Value) {
        let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_owned();
        if let Some(run) = hub.check_runs.iter_mut().find(|run| run.id == id) {
            run.status = text(&sent["status"]);
            run.conclusion = sent["conclusion"].as_str().map(str::to_owned);
            run.title = text(&sent["output"]["title"]);
            run.summary = text(&sent["output"]["summary"]);
        }
    }

    /// Takes comments again after [`Self::refuse_comments`]
    pub fn accept_comments(&self) {
        self.hub.lock().unwrap().comments_refused = None;
    }

    /// Makes the next conversion hand back an App owned by `owner`
    pub fn converts_for(&self, owner: &str) {
        self.hub.lock().unwrap().convert_owner = Some(owner.to_owned());
    }

    /// What it was asked, in order
    pub fn asked(&self) -> Vec<Asked> {
        self.hub.lock().unwrap().asked.clone()
    }

    /// Every write made as the App but to a check run (see
    /// [`Self::check_run_writes`]), in order, with the token it was made with
    pub fn writes(&self) -> Vec<(String, Call)> {
        (self.every_write().into_iter())
            .filter(|(_, call)| !call.path.contains("/check-runs"))
            .collect()
    }

    fn every_write(&self) -> Vec<(String, Call)> {
        (self.asked().into_iter())
            .filter_map(|asked| match asked {
                Asked::Write(token, call) => Some((token, call)),
                _ => None,
            })
            .collect()
    }

    /// The text of each comment the App posted, with the issue or pull
    /// request it went to, in order
    pub fn comments(&self) -> Vec<(u64, String)> {
        (self.writes().into_iter())
            .filter_map(|(_, call)| {
                let thread = call.path.strip_suffix("/comments")?.rsplit('/').next()?;
                let body: serde_json::Value = serde_json::from_str(call.body.as_deref()?).ok()?;
                Some((thread.parse().ok()?, body["body"].as_str()?.to_owned()))
            })
            .collect()
    }

    /// Each review the App posted, in order, as the pull request it went to
    /// and what was sent
    pub fn reviews(&self) -> Vec<(u64, SentReview)> {
        (self.writes().into_iter())
            .filter_map(|(_, call)| {
                let number = call.path.strip_suffix("/reviews")?.rsplit('/').next()?;
                let sent: serde_json::Value = serde_json::from_str(call.body.as_deref()?).ok()?;
                let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_owned();
                let comments = (sent["comments"].as_array().into_iter().flatten())
                    .map(|c| {
                        (
                            text(&c["path"]),
                            c["line"].as_u64().unwrap_or(0),
                            text(&c["body"]),
                        )
                    })
                    .collect();
                let review = SentReview {
                    head: text(&sent["commit_id"]),
                    event: text(&sent["event"]),
                    body: text(&sent["body"]),
                    comments,
                };
                Some((number.parse().ok()?, review))
            })
            .collect()
    }

    /// The id and new text of each comment the App edited, in order
    pub fn edits(&self) -> Vec<(u64, String)> {
        (self.writes().into_iter())
            .filter(|(_, call)| call.verb == Verb::Patch && call.path.contains("/issues/comments/"))
            .filter_map(|(_, call)| {
                let id = call.path.rsplit('/').next()?.parse().ok()?;
                let body: serde_json::Value = serde_json::from_str(call.body.as_deref()?).ok()?;
                Some((id, body["body"].as_str()?.to_owned()))
            })
            .collect()
    }

    /// The number and new body of each issue whose body the App replaced,
    /// in order
    pub fn issue_edits(&self) -> Vec<(u64, String)> {
        (self.writes().into_iter())
            .filter(|(_, call)| is_issue_edit(call))
            .filter_map(|(_, call)| {
                let number = call.path.rsplit('/').next()?;
                let body: serde_json::Value = serde_json::from_str(call.body.as_deref()?).ok()?;
                Some((number.parse().ok()?, body["body"].as_str()?.to_owned()))
            })
            .collect()
    }

    /// Each issue label the App took off, in order, as the issue it came off
    /// and the label's name, with the path's percent-encoding undone.
    pub fn label_takes(&self) -> Vec<(u64, String)> {
        (self.writes().into_iter())
            .filter(|(_, call)| call.verb == Verb::Delete)
            .filter_map(|(_, call)| {
                let mut parts = call.path.rsplit('/');
                let name = parts.next()?;
                if parts.next() != Some("labels") {
                    return None;
                }
                Some((parts.next()?.parse().ok()?, unescaped(name)))
            })
            .collect()
    }

    /// The id of each comment the App reacted to, in order
    pub fn reactions(&self) -> Vec<u64> {
        (self.writes().into_iter())
            .filter_map(|(_, call)| {
                let path = call.path.strip_suffix("/reactions")?;
                path.rsplit('/').next()?.parse().ok()
            })
            .collect()
    }

    /// Every JWT a call was made with
    pub fn jwts(&self) -> Vec<String> {
        self.hub.lock().unwrap().jwts.clone()
    }

    /// Kelpie's App tokens over this stand-in and [`FakeSigner`], kept in `kelpie_home`
    pub fn tokens(&self, kelpie_home: &Path) -> AppTokens {
        AppTokens::new(
            Apps::under(kelpie_home),
            Box::new(self.clone()),
            Box::new(FakeSigner),
            Box::new(self.clock.clone()),
        )
    }

    /// Registers the App in `kelpie_home` as setup would, and installs it on `repo`
    pub fn registered(&self, kelpie_home: &Path, repo: &str) {
        Apps::under(kelpie_home).save(&self.conversion()).unwrap();
        self.install(repo, 7);
    }
}

impl AppComments for FakeGithub {
    fn posted_on(&self, number: u64) -> Vec<crate::ports::ThreadComment> {
        FakeGithub::posted_on(self, number)
    }
}

impl GithubApi for FakeGithub {
    fn convert(&self, code: &str) -> Result<Conversion, ApiError> {
        let mut hub = self.hub.lock().unwrap();
        hub.asked.push(Asked::Convert(code.to_owned()));
        if let Some(error) = hub.failures.pop_front() {
            return Err(error);
        }
        let owner = hub
            .convert_owner
            .take()
            .unwrap_or_else(|| self.owner.clone());
        Ok(Conversion {
            owner,
            ..self.conversion()
        })
    }

    fn installation(&self, jwt: &Jwt, repo: &ForgeSlug) -> Result<Option<Installation>, ApiError> {
        let mut hub = self.hub.lock().unwrap();
        hub.asked
            .push(Asked::Installation(repo.as_str().to_owned()));
        hub.jwts.push(jwt.expose().to_owned());
        if let Some(error) = hub.failures.pop_front() {
            return Err(error);
        }
        let repo = repo.as_str().to_ascii_lowercase();
        let workflows = hub.workflows;
        Ok(hub
            .installed
            .get(&repo)
            .map(|&id| Installation { id, workflows }))
    }

    fn access_token(
        &self,
        jwt: &Jwt,
        installation: u64,
        repo: &ForgeSlug,
        grant: Grant,
    ) -> Result<IssuedToken, ApiError> {
        let body = crate::github::api::token_request(repo, grant);
        let held = {
            let mut hub = self.hub.lock().unwrap();
            hub.asked
                .push(Asked::AccessToken(installation, body.clone()));
            hub.jwts.push(jwt.expose().to_owned());
            (matches!(grant, Grant::Worker { .. }))
                .then(|| hub.worker_hold.take())
                .flatten()
        };
        // The hub is let go while it waits, so nothing else is held up behind it.
        if let Some(gate) = held {
            let _ = gate.entered.send(());
            let _ = gate.release.recv();
        }
        let mut hub = self.hub.lock().unwrap();
        if let Some(error) = hub.failures.pop_front() {
            return Err(error);
        }
        if let Some(error) = hub
            .workers_refused
            .clone()
            .filter(|_| matches!(grant, Grant::Worker { .. }))
        {
            return Err(error);
        }
        let asks_ungiven = hub
            .ungiven
            .iter()
            .any(|p| body.contains(&format!("\"{p}\":")));
        if asks_ungiven {
            return Err(ApiError::Refused(422));
        }
        hub.minted += 1;
        let ask = |pairs: &[(&str, &str)]| {
            (pairs.iter())
                .map(|&(name, level)| (name.to_owned(), level.to_owned()))
                .collect::<BTreeMap<_, _>>()
        };
        let mut permissions = match grant {
            Grant::Kelpie => ask(&[
                ("issues", "write"),
                ("metadata", "read"),
                ("pull_requests", "write"),
            ]),
            Grant::Worker { workflows: false } => {
                ask(&[("contents", "write"), ("metadata", "read")])
            }
            Grant::Worker { workflows: true } => ask(&[
                ("contents", "write"),
                ("metadata", "read"),
                ("workflows", "write"),
            ]),
            Grant::Threads => ask(&[
                ("contents", "write"),
                ("metadata", "read"),
                ("pull_requests", "write"),
            ]),
            Grant::Checks => ask(&[("checks", "write"), ("metadata", "read")]),
            Grant::Reader => ask(&[
                ("checks", "read"),
                ("issues", "read"),
                ("metadata", "read"),
                ("pull_requests", "read"),
                ("statuses", "read"),
            ]),
            Grant::ReaderCore => ask(&[
                ("issues", "read"),
                ("metadata", "read"),
                ("pull_requests", "read"),
            ]),
        };
        let extra = (hub.extra_grant.clone()).filter(|_| matches!(grant, Grant::Worker { .. }));
        if let Some((name, level)) = extra {
            permissions.insert(name, level);
        }
        Ok(IssuedToken {
            permissions,
            token: InstallationToken::new(format!("ghs_test{}", hub.minted)),
            expires_at: Timestamp(self.clock.now().0 + Self::LIFETIME),
        })
    }

    // A new issue answers with the next number from 900, clear of any a test opens by hand.
    fn write(&self, token: &InstallationToken, call: &Call) -> Result<String, ApiError> {
        let mut hub = self.hub.lock().unwrap();
        hub.asked
            .push(Asked::Write(token.expose().to_owned(), call.clone()));
        let held = (call.verb == Verb::Post && call.path.ends_with("/comments"))
            .then(|| hub.comment_hold.take())
            .flatten();
        if let Some(gate) = held {
            // The hub is let go while it waits, so nothing else is held up behind it.
            drop(hub);
            let _ = gate.entered.send(());
            let _ = gate.release.recv();
            hub = self.hub.lock().unwrap();
        }
        if let Some(error) = hub.failures.pop_front() {
            return Err(error);
        }
        if let Some(error) = hub
            .comments_refused
            .clone()
            .filter(|_| call.path.ends_with("/comments"))
        {
            return Err(error);
        }
        if let Some(error) = hub
            .issue_edits_refused
            .clone()
            .filter(|_| is_issue_edit(call))
        {
            return Err(error);
        }
        if let Some(error) =
            (hub.label_removals_refused.clone()).filter(|_| call.verb == Verb::Delete)
        {
            return Err(error);
        }
        if call.path == "/graphql" {
            let threads = hub.threads.clone();
            drop(hub);
            return Ok(Self::graphql_answer(threads, call));
        }
        if call.path.ends_with("/reviews") {
            let lines = call
                .body
                .as_deref()
                .unwrap_or_default()
                .contains("\"line\":");
            if hub.reviews_refused {
                return Err(ApiError::Refused(403));
            }
            if hub.line_comments_refused && lines {
                return Err(ApiError::Refused(422));
            }
            return Ok(hub
                .review_answer
                .clone()
                .unwrap_or_else(|| REVIEW_ANSWER.to_owned()));
        }
        // A check run answers with its id, numbered from 5000.
        if call.path.contains("/check-runs") {
            return Ok(Self::check_run_answer(&mut hub, call));
        }
        // A comment answers with its id, numbered from 100 and so below any a
        // person writes on the fake forge, and the time the clock says.
        if call.path.ends_with("/comments") {
            hub.comment_ids += 1;
            let created = jiff::Timestamp::from_second(self.clock.now().0 as i64).unwrap();
            let id = 99 + hub.comment_ids;
            let body =
                serde_json::from_str::<serde_json::Value>(call.body.as_deref().unwrap_or("{}"))
                    .ok()
                    .and_then(|sent| sent["body"].as_str().map(str::to_owned))
                    .unwrap_or_default();
            if let Some(thread) = call
                .path
                .strip_suffix("/comments")
                .and_then(|p| p.rsplit('/').next())
                .and_then(|n| n.parse::<u64>().ok())
            {
                hub.posted.push((thread, id, body, created));
            }
            return Ok(format!(r#"{{"id":{id},"created_at":"{created}"}}"#));
        }
        if let Some(error) = hub
            .issue_creates_refused
            .clone()
            .filter(|_| call.path.ends_with("/issues"))
        {
            return Err(error);
        }
        if call.path.ends_with("/issues") {
            hub.issues += 1;
            return Ok(format!(r#"{{"number":{}}}"#, 899 + hub.issues));
        }
        Ok("{}".to_owned())
    }
}

// A `PATCH /repos/{owner}/{repo}/issues/{number}`: an issue body edit.
fn is_issue_edit(call: &Call) -> bool {
    call.verb == Verb::Patch && call.path.rsplit('/').nth(1) == Some("issues")
}

// A GraphQL call's JSON body.
fn graphql(call: &Call) -> serde_json::Value {
    serde_json::from_str(call.body.as_deref().unwrap_or("{}")).unwrap()
}

impl FakeGithub {
    // What GraphQL answers a write on a review thread, which lands in the
    // review bot store where the rig has one. GraphQL answers HTTP 200 with
    // its errors in the body, as GitHub does.
    fn graphql_answer(threads: Option<FakeCodeRabbit>, call: &Call) -> String {
        let sent = graphql(call);
        let (query, id) = (
            sent["query"].as_str().unwrap_or_default(),
            &sent["variables"],
        );
        let id = id["id"].as_str().unwrap_or_default();
        let done = |result: Result<(), crate::ports::ForgeError>| match result {
            Ok(()) => None,
            Err(e) => Some(e.to_string()),
        };
        let (field, answer, failed) = if query.contains("resolveReviewThread") {
            let failed = threads.and_then(|t| done(t.resolve(id)));
            (
                "resolveReviewThread",
                r#"{"thread":{"isResolved":true}}"#,
                failed,
            )
        } else {
            let body = sent["variables"]["body"].as_str().unwrap_or_default();
            let failed = threads.and_then(|t| done(t.reply(id, body)));
            (
                "addPullRequestReviewThreadReply",
                r#"{"comment":{"id":"c"}}"#,
                failed,
            )
        };
        match failed {
            Some(why) => serde_json::json!({ "errors": [{ "message": why }] }).to_string(),
            None => format!(r#"{{"data":{{"{field}":{answer}}}}}"#),
        }
    }
}

/// Signs anything with one fixed signature
#[derive(Debug, Clone, Copy, Default)]
pub struct FakeSigner;

impl FakeSigner {
    /// What every signature is
    pub const SIGNATURE: &[u8] = b"signed";
}

impl Signer for FakeSigner {
    fn sign(&self, _key: &Path, _input: &[u8]) -> Result<Vec<u8>, String> {
        Ok(Self::SIGNATURE.to_vec())
    }
}

// A path segment with its percent-escapes undone
fn unescaped(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        let hex = segment.get(at + 1..at + 3);
        match (byte, hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(decoded)) => {
                out.push(decoded);
                at += 3;
            }
            _ => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
