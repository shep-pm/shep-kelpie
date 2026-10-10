//! The rig's forge: issues, a board, and pull requests whose heads live on
//! the rig's bare origin

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::coderabbit::FakeCodeRabbit;
use crate::board::{Blocker, OpenPullRequest, READY, ReadyIssue, SubIssues};
use crate::ports::{
    Checks, Forge, ForgeError, Issue, MaintainerReview, NewLabel, OpenIssue, PullRequest,
    PullRequestState, QueueStanding, Reviewed, Timestamp, Visibility,
};
use crate::review_bot::{Activity, Login};
use crate::settings::ForgeSlug;

/// A forge whose repo is public and whose every issue exists, unless a
/// test says otherwise. Its board is empty until a test lists issues on it.
#[derive(Debug, Clone)]
pub(crate) struct FakeForge {
    visibility: Arc<Mutex<Visibility>>,
    missing: Arc<Mutex<HashSet<u64>>>,
    labels: Arc<Mutex<HashMap<u64, Vec<String>>>>,
    ready: Arc<Mutex<Vec<ReadyIssue>>>,
    blockers: Arc<Mutex<HashMap<u64, Vec<u64>>>>,
    closed: Arc<Mutex<HashSet<u64>>>,
    // Each parent's sub-issues, in the order they were linked
    sub_issues: Arc<Mutex<HashMap<u64, Vec<u64>>>>,
    // The issues kelpie closed, with the comment each got
    closings: Arc<Mutex<Vec<(u64, String)>>>,
    // Whether closing an issue is refused
    closes_down: Arc<AtomicBool>,
    open: Arc<Mutex<Vec<OpenPullRequest>>>,
    board_down: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    origin: PathBuf,
    pull_requests: Arc<Mutex<HashMap<u64, FakePullRequest>>>,
    checks: Arc<Mutex<HashMap<String, Checks>>>,
    comments: Arc<Mutex<Vec<(u64, String)>>>,
    comments_down: Arc<AtomicBool>,
    merges_down: Arc<AtomicBool>,
    merge_answers_lost: Arc<AtomicBool>,
    // Whether the repo has a merge queue, and where each pull request stands in it
    queue_on: Arc<AtomicBool>,
    disarmed: Arc<Mutex<Vec<u64>>>,
    queue: Arc<Mutex<HashMap<u64, QueueStanding>>>,
    labels_down: Arc<AtomicBool>,
    unreadable: Arc<Mutex<HashSet<u64>>>,
    viewer_reads: Arc<AtomicUsize>,
    lagging: Arc<Mutex<HashMap<u64, String>>>,
    lagging_drafts: Arc<Mutex<HashSet<u64>>>,
    readied: Arc<Mutex<Vec<u64>>>,
    skipped: Arc<Mutex<Vec<u64>>>,
    merges: Arc<Mutex<Vec<(u64, String)>>>,
    issues: Arc<Mutex<Vec<OpenIssue>>>,
    created: Arc<Mutex<Vec<CreatedIssue>>>,
    // Why listing and opening issues fail, while they do
    issues_down: Arc<Mutex<Option<String>>>,
    // How many more issues may open before every later one fails
    creates_left: Arc<Mutex<Option<usize>>>,
    reviews: Arc<Mutex<HashMap<u64, MaintainerReview>>>,
    // A state file read as each comment is posted, and what it held then
    watched: Arc<Mutex<Option<PathBuf>>>,
    saved_at_comment: Arc<Mutex<Vec<serde_json::Value>>>,
    default_branch: Arc<Mutex<String>>,
    repo_labels: Arc<Mutex<Vec<String>>>,
    pushes: Arc<AtomicBool>,
    owner_is_user: Arc<AtomicBool>,
    bot_seen: Arc<AtomicBool>,
    viewer_down: Arc<Mutex<Option<ForgeError>>>,
    // Every call made, and the board's reads among them
    asked: Arc<AtomicUsize>,
    board_reads: Arc<AtomicUsize>,
    // Set while the forge is down
    down: Arc<Mutex<Option<down::Down>>>,
    /// Pull requests' labels, and what CodeRabbit posts
    pub(crate) coderabbit: FakeCodeRabbit,
}

/// A pull request on the fake forge. Its head is its branch on the rig's
/// origin, so a push the runner makes moves it.
#[derive(Debug, Clone)]
struct FakePullRequest {
    base: String,
    branch: String,
    state: PullRequestState,
    draft: bool,
    from_fork: bool,
    author: String,
}

/// An issue kelpie opened on the fake forge
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreatedIssue {
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) labels: Vec<String>,
}

/// The account the fake forge says kelpie acts as, and opens pull requests as
pub(crate) const VIEWER: &str = "the-maintainer";

impl FakeForge {
    /// A public repo whose pull requests' branches live on `origin`, a bare repo
    pub(crate) fn new(origin: PathBuf) -> Self {
        Self {
            visibility: Arc::new(Mutex::new(Visibility::Public)),
            missing: Arc::default(),
            labels: Arc::default(),
            ready: Arc::default(),
            blockers: Arc::default(),
            closed: Arc::default(),
            sub_issues: Arc::default(),
            closings: Arc::default(),
            closes_down: Arc::default(),
            open: Arc::default(),
            board_down: Arc::default(),
            calls: Arc::default(),
            origin,
            pull_requests: Arc::default(),
            checks: Arc::default(),
            comments: Arc::default(),
            comments_down: Arc::default(),
            merges_down: Arc::default(),
            merge_answers_lost: Arc::default(),
            queue_on: Arc::default(),
            disarmed: Arc::default(),
            queue: Arc::default(),
            labels_down: Arc::default(),
            unreadable: Arc::default(),
            viewer_reads: Arc::default(),
            lagging: Arc::default(),
            lagging_drafts: Arc::default(),
            readied: Arc::default(),
            skipped: Arc::default(),
            merges: Arc::default(),
            issues: Arc::default(),
            created: Arc::default(),
            issues_down: Arc::default(),
            creates_left: Arc::default(),
            reviews: Arc::default(),
            watched: Arc::default(),
            saved_at_comment: Arc::default(),
            default_branch: Arc::new(Mutex::new("main".to_owned())),
            repo_labels: Arc::default(),
            pushes: Arc::new(AtomicBool::new(true)),
            owner_is_user: Arc::default(),
            bot_seen: Arc::new(AtomicBool::new(true)),
            viewer_down: Arc::default(),
            asked: Arc::default(),
            board_reads: Arc::default(),
            down: Arc::default(),
            coderabbit: FakeCodeRabbit::default(),
        }
    }

    /// Adds `label` to pull request `number`, as someone other than kelpie would
    pub(crate) fn label_pull_request(&self, number: u64, label: &str) {
        self.coderabbit.put_label(number, label, true);
    }

    /// Pull request `number`'s labels, in the order they went on
    pub(crate) fn pull_request_labels(&self, number: u64) -> Vec<String> {
        self.coderabbit.labels(number)
    }

    /// Removes `label` from pull request `number`, as someone other than kelpie would
    pub(crate) fn unlabel_pull_request(&self, number: u64, label: &str) {
        self.coderabbit.put_label(number, label, false);
    }

    pub(crate) fn set_visibility(&self, visibility: Visibility) {
        *self.visibility.lock().unwrap() = visibility;
    }

    /// Whether the account may push to the repo, which it may until a test says not
    pub(crate) fn set_can_push(&self, pushes: bool) {
        self.pushes.store(pushes, Ordering::SeqCst);
    }

    /// Whether the repo's owner is a user, which it is not until a test says so
    pub(crate) fn set_owner_is_user(&self, user: bool) {
        self.owner_is_user.store(user, Ordering::SeqCst);
    }

    /// Whether a review bot has ever commented on the repo, which it has until a test says not
    pub(crate) fn set_review_bot_seen(&self, seen: bool) {
        self.bot_seen.store(seen, Ordering::SeqCst);
    }

    /// Makes asking who the account is fail with `error`, as `gh` does when it is logged out
    pub(crate) fn set_viewer_down(&self, error: ForgeError) {
        *self.viewer_down.lock().unwrap() = Some(error);
    }

    pub(crate) fn set_default_branch(&self, branch: &str) {
        *self.default_branch.lock().unwrap() = branch.to_owned();
    }

    /// The repo's labels, those it started with and those made since
    pub(crate) fn repo_labels_now(&self) -> Vec<String> {
        self.repo_labels.lock().unwrap().clone()
    }

    pub(crate) fn set_repo_labels(&self, labels: &[&str]) {
        *self.repo_labels.lock().unwrap() = labels.iter().map(|&l| l.to_owned()).collect();
    }

    pub(crate) fn remove_issue(&self, number: u64) {
        self.missing.lock().unwrap().insert(number);
    }

    /// Labels issue `number` with `label`, on the board and when viewed
    pub(crate) fn label(&self, number: u64, label: &str) {
        let mut labels = self.labels.lock().unwrap();
        labels.entry(number).or_default().push(label.to_owned());
    }

    /// Lists issue `number` as ready, with whether anyone is assigned
    pub(crate) fn list_ready(&self, number: u64, assigned: bool) {
        self.label(number, READY);
        self.ready.lock().unwrap().push(ReadyIssue {
            number,
            title: format!("Title of #{number}"),
            body: format!("Body of #{number}.\n"),
            assigned,
            labels: Vec::new(),
            blocked_by: Vec::new(),
            unlisted_blockers: 0,
            parent: None,
            sub_issues: SubIssues::default(),
        });
    }

    /// Gives ready issue `number` the body `body`, as its author wrote it
    pub(crate) fn set_ready_body(&self, number: u64, body: &str) {
        let mut ready = self.ready.lock().unwrap();
        let issue = ready.iter_mut().find(|i| i.number == number).unwrap();
        issue.body = body.to_owned();
    }

    /// Makes issue `child` a sub-issue of issue `parent`, as someone else would
    pub(crate) fn link_sub_issue(&self, parent: u64, child: u64) {
        let mut links = self.sub_issues.lock().unwrap();
        links.entry(parent).or_default().push(child);
    }

    /// Issue `parent`'s sub-issues, in the order they were linked
    pub(crate) fn sub_issues_of(&self, parent: u64) -> Vec<u64> {
        let links = self.sub_issues.lock().unwrap();
        links.get(&parent).cloned().unwrap_or_default()
    }

    /// The issues issue `number` is blocked by, open or closed
    pub(crate) fn blockers(&self, number: u64) -> Vec<u64> {
        self.blockers_of(number).iter().map(|b| b.number).collect()
    }

    /// Makes closing an issue fail, or work again
    pub(crate) fn set_closes_down(&self, down: bool) {
        self.closes_down.store(down, Ordering::SeqCst);
    }

    /// The issues kelpie closed, with the comment each got
    pub(crate) fn closings(&self) -> Vec<(u64, String)> {
        self.closings.lock().unwrap().clone()
    }

    /// Marks issue `number` blocked by issue `by`, which is open until closed
    pub(crate) fn block(&self, number: u64, by: u64) {
        let mut blockers = self.blockers.lock().unwrap();
        blockers.entry(number).or_default().push(by);
    }

    /// Closes issue `number`, as merging its pull request or someone else would
    pub(crate) fn close_issue(&self, number: u64) {
        self.closed.lock().unwrap().insert(number);
    }

    /// Opens draft pull request `number` from `head`, closing `closes`
    pub(crate) fn open_pull_request(&self, number: u64, head: &str, closes: &[u64]) {
        self.open.lock().unwrap().push(OpenPullRequest {
            number,
            head: head.to_owned(),
            closes: closes.to_vec(),
            labels: Vec::new(),
        });
        let pr = FakePullRequest {
            base: "main".to_owned(),
            branch: head.to_owned(),
            state: PullRequestState::Open,
            draft: true,
            from_fork: false,
            author: VIEWER.to_owned(),
        };
        self.pull_requests.lock().unwrap().insert(number, pr);
    }

    /// Makes pull request `number` one `login` opened, not kelpie's account
    pub(crate) fn set_author(&self, number: u64, login: &str) {
        let mut prs = self.pull_requests.lock().unwrap();
        prs.get_mut(&number).expect("an opened pull request").author = login.to_owned();
    }

    /// How many times the account kelpie acts as was asked
    pub(crate) fn viewer_reads(&self) -> usize {
        self.viewer_reads.load(Ordering::SeqCst)
    }

    /// Reports `checks` for commit `head`. A head with none reported is pending.
    pub(crate) fn set_checks(&self, head: &str, checks: Checks) {
        self.checks.lock().unwrap().insert(head.to_owned(), checks);
    }

    /// Merges or closes pull request `number` as someone other than kelpie would
    pub(crate) fn set_state(&self, number: u64, state: PullRequestState) {
        let mut prs = self.pull_requests.lock().unwrap();
        prs.get_mut(&number).expect("an opened pull request").state = state;
    }

    /// Whether pull request `number` is a draft
    pub(crate) fn draft(&self, number: u64) -> bool {
        let prs = self.pull_requests.lock().unwrap();
        prs.get(&number).expect("an opened pull request").draft
    }

    /// Marks pull request `number` ready, as someone other than kelpie would
    pub(crate) fn ready_pull_request(&self, number: u64) {
        let mut prs = self.pull_requests.lock().unwrap();
        prs.get_mut(&number).expect("an opened pull request").draft = false;
    }

    /// Makes pull request `number` merge into `base` rather than `main`
    pub(crate) fn set_base(&self, number: u64, base: &str) {
        let mut prs = self.pull_requests.lock().unwrap();
        prs.get_mut(&number).expect("an opened pull request").base = base.to_owned();
    }

    /// Makes pull request `number` come from a fork's branch
    pub(crate) fn set_from_fork(&self, number: u64) {
        let mut prs = self.pull_requests.lock().unwrap();
        prs.get_mut(&number)
            .expect("an opened pull request")
            .from_fork = true;
    }

    /// Makes reading pull request `number`'s review fail
    pub(crate) fn set_unreadable(&self, number: u64) {
        self.unreadable.lock().unwrap().insert(number);
    }

    /// Leaves `review` on pull request `number`, as the maintainer would
    pub(crate) fn review(&self, number: u64, review: MaintainerReview) {
        let prs = self.pull_requests.lock().unwrap();
        assert!(prs.contains_key(&number), "an opened pull request");
        self.reviews.lock().unwrap().insert(number, review);
    }

    /// Makes posting comments fail, or work again
    pub(crate) fn set_comments_down(&self, down: bool) {
        self.comments_down.store(down, Ordering::SeqCst);
    }

    /// Makes pull request `number` report `head` whatever origin holds, as
    /// GitHub does for a moment after a push, or stop doing so
    pub(crate) fn set_lagging(&self, number: u64, head: Option<&str>) {
        let mut lagging = self.lagging.lock().unwrap();
        match head {
            Some(head) => lagging.insert(number, head.to_owned()),
            None => lagging.remove(&number),
        };
    }

    /// Makes pull request `number` keep reading as a draft after it is marked
    /// ready, as GitHub does for a moment, or stop doing so
    pub(crate) fn set_lagging_draft(&self, number: u64, lagging: bool) {
        let mut drafts = self.lagging_drafts.lock().unwrap();
        if lagging {
            drafts.insert(number);
        } else {
            drafts.remove(&number);
        }
    }

    /// Makes merging fail, or work again
    pub(crate) fn set_merges_down(&self, down: bool) {
        self.merges_down.store(down, Ordering::SeqCst);
    }

    /// Makes a merge land but answer an error, as a timed-out call would
    pub(crate) fn set_merge_answers_lost(&self, lost: bool) {
        self.merge_answers_lost.store(lost, Ordering::SeqCst);
    }

    /// Issue `number`'s labels, in the order they went on
    pub(crate) fn issue_labels(&self, number: u64) -> Vec<String> {
        self.labels_of(number)
    }

    /// Makes changing a pull request's or an issue's labels fail, or work again
    pub(crate) fn set_labels_down(&self, down: bool) {
        self.labels_down.store(down, Ordering::SeqCst);
    }

    /// Every comment posted, oldest first, with its pull request
    pub(crate) fn comments(&self) -> Vec<(u64, String)> {
        self.comments.lock().unwrap().clone()
    }

    /// Reads the state file at `path` as each comment is posted
    pub(crate) fn watch_state(&self, path: PathBuf) {
        *self.watched.lock().unwrap() = Some(path);
    }

    /// The state file as each comment found it, oldest first
    pub(crate) fn saved_at_comment(&self) -> Vec<serde_json::Value> {
        self.saved_at_comment.lock().unwrap().clone()
    }

    /// Every pull request marked ready, in order
    pub(crate) fn readied(&self) -> Vec<u64> {
        self.readied.lock().unwrap().clone()
    }

    /// Every pull request the summon label went on while it was a draft, which
    /// CodeRabbit answers with "Draft PR not reviewed" and never reviews
    pub(crate) fn skipped_as_drafts(&self) -> Vec<u64> {
        self.skipped.lock().unwrap().clone()
    }

    /// Leaves issue `number` open, as someone other than kelpie would have,
    /// and shows this title and body when it is viewed
    pub(crate) fn open_issue(&self, number: u64, title: &str, body: &str) {
        self.issues.lock().unwrap().push(OpenIssue {
            number,
            title: title.to_owned(),
            body: body.to_owned(),
        });
    }

    /// Every issue kelpie opened, oldest first
    pub(crate) fn created(&self) -> Vec<CreatedIssue> {
        self.created.lock().unwrap().clone()
    }

    /// Makes listing and opening issues fail, or work again
    pub(crate) fn set_issues_down(&self, down: bool) {
        let why = down.then(|| "issues are down".to_owned());
        *self.issues_down.lock().unwrap() = why;
    }

    /// Makes listing and opening issues fail with `error`, verbatim
    pub(crate) fn set_issues_error(&self, error: &str) {
        *self.issues_down.lock().unwrap() = Some(error.to_owned());
    }

    /// Lets `n` more issues open, then makes every later one fail
    pub(crate) fn set_creates_left(&self, n: usize) {
        *self.creates_left.lock().unwrap() = Some(n);
    }

    /// Every merge made, with the head it was held to
    pub(crate) fn merges(&self) -> Vec<(u64, String)> {
        self.merges.lock().unwrap().clone()
    }

    /// The commit `branch` points at on origin, if it exists there
    pub(crate) fn head_of(&self, branch: &str) -> Option<String> {
        let output = Command::new("git")
            .arg("--git-dir")
            .arg(&self.origin)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("refs/heads/{branch}"))
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn opened(&self, number: u64) -> Result<FakePullRequest, ForgeError> {
        let prs = self.pull_requests.lock().unwrap();
        let pr = prs.get(&number).cloned();
        pr.ok_or_else(|| ForgeError::Failed(format!("no pull request #{number}")))
    }

    /// Makes listing the board fail, or work again
    pub(crate) fn set_board_down(&self, down: bool) {
        self.board_down.store(down, Ordering::SeqCst);
    }

    /// How many times the repo's visibility was asked
    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn labels_of(&self, number: u64) -> Vec<String> {
        let labels = self.labels.lock().unwrap();
        labels.get(&number).cloned().unwrap_or_default()
    }

    fn blockers_of(&self, number: u64) -> Vec<Blocker> {
        let blockers = self.blockers.lock().unwrap();
        let closed = self.closed.lock().unwrap();
        let by = blockers.get(&number).map(Vec::as_slice).unwrap_or_default();
        by.iter()
            .map(|&number| Blocker {
                number,
                open: !closed.contains(&number),
            })
            .collect()
    }

    fn parent_of(&self, number: u64) -> Option<u64> {
        let links = self.sub_issues.lock().unwrap();
        links
            .iter()
            .find(|(_, children)| children.contains(&number))
            .map(|(&parent, _)| parent)
    }

    fn count_sub_issues(&self, parent: u64) -> SubIssues {
        let children = self.sub_issues_of(parent);
        let closed = self.closed.lock().unwrap();
        SubIssues {
            total: children.len() as u64,
            closed: children.iter().filter(|c| closed.contains(c)).count() as u64,
        }
    }

    fn issues_up(&self) -> Result<(), ForgeError> {
        match &*self.issues_down.lock().unwrap() {
            Some(why) => Err(ForgeError::Failed(why.clone())),
            None => Ok(()),
        }
    }

    fn board(&self) -> Result<(), ForgeError> {
        if self.board_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("the board is down".into()));
        }
        Ok(())
    }
}

impl Forge for FakeForge {
    fn visibility(&self, _repo: &ForgeSlug) -> Result<Visibility, ForgeError> {
        self.ask()?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(*self.visibility.lock().unwrap())
    }

    fn default_branch(&self, _repo: &ForgeSlug) -> Result<String, ForgeError> {
        self.ask()?;
        Ok(self.default_branch.lock().unwrap().clone())
    }

    fn repo_labels(&self, _repo: &ForgeSlug) -> Result<Vec<String>, ForgeError> {
        self.ask()?;
        Ok(self.repo_labels.lock().unwrap().clone())
    }

    fn create_label(&self, _repo: &ForgeSlug, label: &NewLabel) -> Result<(), ForgeError> {
        self.ask()?;
        let mut labels = self.repo_labels.lock().unwrap();
        if labels.iter().any(|l| l == label.name) {
            return Err(ForgeError::Failed(format!(
                "label with name \"{}\" already exists",
                label.name
            )));
        }
        labels.push(label.name.to_owned());
        Ok(())
    }

    fn can_push(&self, _repo: &ForgeSlug) -> Result<bool, ForgeError> {
        self.ask()?;
        Ok(self.pushes.load(Ordering::SeqCst))
    }

    fn owner_is_user(&self, _repo: &ForgeSlug) -> Result<bool, ForgeError> {
        self.ask()?;
        Ok(self.owner_is_user.load(Ordering::SeqCst))
    }

    fn review_bot_seen(&self, _repo: &ForgeSlug, _login: Login<'_>) -> Result<bool, ForgeError> {
        self.ask()?;
        Ok(self.bot_seen.load(Ordering::SeqCst))
    }

    fn issue(&self, _repo: &ForgeSlug, number: u64) -> Result<Issue, ForgeError> {
        self.ask()?;
        if self.missing.lock().unwrap().contains(&number) {
            return Err(ForgeError::Failed(format!("no issue #{number}")));
        }
        // Each read takes its own lock, so none may be held across another.
        let open = !self.closed.lock().unwrap().contains(&number);
        let filed = (self.issues.lock().unwrap().iter())
            .find(|i| i.number == number)
            .map(|i| (i.title.clone(), i.body.clone()));
        let (title, body) = filed.unwrap_or_else(|| {
            (
                format!("Title of #{number}"),
                format!("Body of #{number}.\n"),
            )
        });
        Ok(Issue {
            title,
            body,
            labels: self.labels_of(number),
            open,
            parent: self.parent_of(number),
            blocked_by: self.blockers(number),
        })
    }

    fn ready_issues(&self, _repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError> {
        self.board_reads.fetch_add(1, Ordering::SeqCst);
        self.ask()?;
        self.board()?;
        let ready = self.ready.lock().unwrap().clone();
        let closed = self.closed.lock().unwrap().clone();
        Ok(ready
            .into_iter()
            .filter(|i| !closed.contains(&i.number))
            .map(|i| ReadyIssue {
                labels: self.labels_of(i.number),
                blocked_by: self.blockers_of(i.number),
                parent: self.parent_of(i.number),
                sub_issues: self.count_sub_issues(i.number),
                ..i
            })
            .collect())
    }

    fn open_pull_requests(&self, _repo: &ForgeSlug) -> Result<Vec<OpenPullRequest>, ForgeError> {
        self.ask()?;
        self.board()?;
        let prs = self.pull_requests.lock().unwrap();
        let open = self.open.lock().unwrap().clone();
        Ok(open
            .into_iter()
            .filter(|pr| prs[&pr.number].state == PullRequestState::Open)
            .map(|pr| OpenPullRequest {
                labels: self.coderabbit.labels(pr.number),
                ..pr
            })
            .collect())
    }

    fn pull_request(&self, _repo: &ForgeSlug, number: u64) -> Result<PullRequest, ForgeError> {
        self.ask()?;
        let pr = self.opened(number)?;
        let lagging = self.lagging.lock().unwrap().get(&number).cloned();
        let head = match lagging {
            Some(head) => head,
            None => self
                .head_of(&pr.branch)
                .ok_or_else(|| ForgeError::Failed(format!("no branch {} on origin", pr.branch)))?,
        };
        let checks = self.checks.lock().unwrap().get(&head).cloned();
        Ok(PullRequest {
            state: pr.state,
            draft: pr.draft || self.lagging_drafts.lock().unwrap().contains(&number),
            checks: checks.unwrap_or(Checks::Pending),
            head,
            labels: self.coderabbit.labels(number),
        })
    }

    fn reviewed(&self, _repo: &ForgeSlug, number: u64) -> Result<Reviewed, ForgeError> {
        self.ask()?;
        if self.unreadable.lock().unwrap().contains(&number) {
            return Err(ForgeError::Failed(format!("#{number} is unreadable")));
        }
        let pr = self.opened(number)?;
        let closes = {
            let open = self.open.lock().unwrap();
            // Opened again, a pull request's latest listing is the one that counts.
            let listed = open.iter().rfind(|l| l.number == number);
            listed.map(|l| l.closes.clone()).unwrap_or_default()
        };
        Ok(Reviewed {
            state: pr.state,
            title: format!("Title of pull request #{number}"),
            body: format!("Body of pull request #{number}.\n"),
            closes,
            base: pr.base,
            branch: pr.branch,
            from_fork: pr.from_fork,
            author: pr.author,
            draft: pr.draft,
            labels: self.coderabbit.labels(number),
            review: self.reviews.lock().unwrap().get(&number).cloned(),
        })
    }

    fn viewer(&self) -> Result<String, ForgeError> {
        self.ask()?;
        self.viewer_reads.fetch_add(1, Ordering::SeqCst);
        match &*self.viewer_down.lock().unwrap() {
            Some(error) => Err(error.clone()),
            None => Ok(VIEWER.to_owned()),
        }
    }

    fn comment(&self, _repo: &ForgeSlug, number: u64, body: &str) -> Result<(), ForgeError> {
        self.ask()?;
        if self.comments_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("comments are down".into()));
        }
        let is_issue = self
            .issues
            .lock()
            .unwrap()
            .iter()
            .any(|i| i.number == number)
            || self
                .ready
                .lock()
                .unwrap()
                .iter()
                .any(|i| i.number == number);
        if !is_issue {
            self.opened(number)?;
        }
        if let Some(path) = self.watched.lock().unwrap().as_ref() {
            let saved = std::fs::read_to_string(path).unwrap();
            let saved = serde_json::from_str(&saved).unwrap();
            self.saved_at_comment.lock().unwrap().push(saved);
        }
        self.comments
            .lock()
            .unwrap()
            .push((number, body.to_owned()));
        Ok(())
    }

    fn post_comment(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<u64, ForgeError> {
        self.ask()?;
        self.comment(repo, number, body)?;
        let at = self.comments.lock().unwrap().len() - 1;
        Ok(9000 + at as u64)
    }

    fn open_issues(&self, _repo: &ForgeSlug) -> Result<Vec<OpenIssue>, ForgeError> {
        self.ask()?;
        if let Some(why) = &*self.issues_down.lock().unwrap() {
            return Err(ForgeError::Failed(why.clone()));
        }
        Ok(self.issues.lock().unwrap().clone())
    }

    // Numbers start at 900, clear of every issue a test opens by hand.
    fn create_issue(
        &self,
        _repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<u64, ForgeError> {
        self.ask()?;
        self.issues_up()?;
        if let Some(left) = self.creates_left.lock().unwrap().as_mut() {
            if *left == 0 {
                return Err(ForgeError::Failed("issues are down".into()));
            }
            *left -= 1;
        }
        let mut created = self.created.lock().unwrap();
        let number = 900 + created.len() as u64;
        created.push(CreatedIssue {
            number,
            title: title.to_owned(),
            body: body.to_owned(),
            labels: labels.iter().map(|&l| l.to_owned()).collect(),
        });
        self.issues.lock().unwrap().push(OpenIssue {
            number,
            title: title.to_owned(),
            body: body.to_owned(),
        });
        drop(created);
        if labels.contains(&READY) {
            self.list_ready(number, false);
        }
        for label in labels.iter().filter(|&&l| l != READY) {
            self.label(number, label);
        }
        Ok(number)
    }

    fn close_issue(&self, _repo: &ForgeSlug, number: u64, comment: &str) -> Result<(), ForgeError> {
        self.ask()?;
        self.issues_up()?;
        if self.closes_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("closing is refused".into()));
        }
        self.closed.lock().unwrap().insert(number);
        self.closings
            .lock()
            .unwrap()
            .push((number, comment.to_owned()));
        Ok(())
    }

    fn mark_ready(&self, _repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        self.ask()?;
        self.opened(number)?;
        let mut prs = self.pull_requests.lock().unwrap();
        prs.get_mut(&number).expect("checked above").draft = false;
        self.readied.lock().unwrap().push(number);
        Ok(())
    }

    fn set_label(
        &self,
        _repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        self.ask()?;
        if self.labels_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("labels are down".into()));
        }
        self.opened(number)?;
        let draft = self.pull_requests.lock().unwrap()[&number].draft;
        if on && draft && label == "review please" {
            self.skipped.lock().unwrap().push(number);
        }
        self.coderabbit.set_label(number, label, on);
        Ok(())
    }

    fn set_issue_label(
        &self,
        _repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        self.ask()?;
        if self.labels_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("labels are down".into()));
        }
        let mut labels = self.labels.lock().unwrap();
        let on_issue = labels.entry(number).or_default();
        on_issue.retain(|l| l != label);
        if on {
            on_issue.push(label.to_owned());
        }
        Ok(())
    }

    // One store for every bot, since a test runs one; the login is recorded.
    fn review_bot(
        &self,
        _repo: &ForgeSlug,
        number: u64,
        login: Login<'_>,
    ) -> Result<Activity, ForgeError> {
        self.ask()?;
        self.opened(number)?;
        self.coderabbit.activity(number, login.rest)
    }

    fn resolve_thread(&self, _repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError> {
        self.ask()?;
        self.coderabbit.resolve(thread)
    }

    // Refuses what GitHub refuses: a draft, a pull request that is not open,
    // and a head that moved since the caller looked.
    fn merge(&self, repo: &ForgeSlug, number: u64, head: &str) -> Result<(), ForgeError> {
        self.ask()?;
        if self.merges_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("merges are down".into()));
        }
        let now = self.pull_request(repo, number)?;
        let refused = if now.draft {
            Some("it is a draft".to_owned())
        } else if now.state != PullRequestState::Open {
            Some(format!("it is {:?}", now.state))
        } else if now.head != head {
            Some(format!("its head is {}, not {head}", now.head))
        } else {
            None
        };
        if let Some(why) = refused {
            return Err(ForgeError::Failed(format!("cannot merge #{number}: {why}")));
        }
        self.merges.lock().unwrap().push((number, head.to_owned()));
        if self.queue_on.load(Ordering::SeqCst) {
            self.enters_queue(number);
            return Ok(());
        }
        self.set_state(number, PullRequestState::Merged);
        if self.merge_answers_lost.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("the answer was lost".into()));
        }
        Ok(())
    }

    fn merge_queue(&self, _repo: &ForgeSlug, number: u64) -> Result<QueueStanding, ForgeError> {
        self.ask()?;
        self.opened(number)?;
        Ok(self.standing(number))
    }

    fn disable_auto_merge(&self, _repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        self.ask()?;
        self.opened(number)?;
        self.disarm(number);
        Ok(())
    }

    fn rate_limit_reset(&self) -> Result<Option<Timestamp>, ForgeError> {
        Ok(self.reset())
    }
}

mod down;
mod queue;
