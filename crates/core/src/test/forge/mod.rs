//! The rig's forge: issues, a board, and pull requests whose heads live on
//! the rig's bare origin

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::coderabbit::FakeCodeRabbit;
use crate::board::{OpenPullRequest, ReadyIssue, ReviewMarks};
use crate::ports::{
    BranchRun, Checks, CommentMarks, FailedLogs, FolderEntry, Forge, ForgeError, InstallationToken,
    IssueCloseReason, MaintainerReview, NewLabel, OpenIssue, OpenPull, PullRequest,
    PullRequestState, QueueStanding, RateLimits, Rerun, Reviewed, Talk, ThreadComment, Timestamp,
    Visibility, Workflow,
};
use crate::review_bot::{Activity, Login};
use crate::settings::ForgeSlug;

/// When the fake forge's review bot last reviewed, until a test sets it: 2025-10-09 08:53:20 UTC
pub const LAST_REVIEWED: u64 = 1_760_000_000;

/// A forge whose repo is public and whose every issue exists, unless a
/// test says otherwise. Its board is empty until a test lists issues on it.
#[derive(Debug, Clone)]
pub struct FakeForge {
    visibility: Arc<Mutex<Visibility>>,
    // What the repos a test named are, in place of `visibility`; keyed by lowercase slug
    visibilities: Arc<Mutex<HashMap<String, Visibility>>>,
    missing: Arc<Mutex<HashSet<u64>>>,
    unread: Arc<Mutex<BTreeMap<u64, usize>>>,
    labels: Arc<Mutex<HashMap<u64, Vec<String>>>>,
    ready: Arc<Mutex<Vec<ReadyIssue>>>,
    // The ready issues read with only the labels they carry, which need not pass the check
    unchecked: Arc<Mutex<HashSet<u64>>>,
    blockers: Arc<Mutex<HashMap<u64, Vec<u64>>>>,
    closed: Arc<Mutex<HashSet<u64>>>,
    // Each parent's sub-issues, in the order they were linked
    sub_issues: Arc<Mutex<HashMap<u64, Vec<u64>>>>,
    // The closed issues that were closed as not planned
    not_planned: Arc<Mutex<HashSet<u64>>>,
    // Whether listing a parent's sub-issues fails
    sub_issues_down: Arc<AtomicBool>,
    listed_finished: Arc<Mutex<Vec<u64>>>,
    // The issues kelpie closed
    closings: Arc<Mutex<Vec<u64>>>,
    // Why each issue kelpie closed was closed, as `close_issue_as` gave it
    closed_as: Arc<Mutex<HashMap<u64, IssueCloseReason>>>,
    // The pull requests kelpie closed unmerged
    pull_request_closings: Arc<Mutex<Vec<u64>>>,
    // Whether closing a pull request is refused
    pull_request_closes_down: Arc<AtomicBool>,
    pull_request_edit_down: Arc<AtomicBool>,
    // Whether closing an issue is refused
    closes_down: Arc<AtomicBool>,
    closes_refused: Arc<AtomicUsize>,
    open: Arc<Mutex<Vec<OpenPullRequest>>>,
    board_down: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    origin: PathBuf,
    pull_requests: Arc<Mutex<HashMap<u64, FakePullRequest>>>,
    checks: Arc<Mutex<HashMap<String, Checks>>>,
    comments: Arc<Mutex<Vec<(u64, String)>>>,
    comments_down: Arc<AtomicBool>,
    talk_down: Arc<AtomicBool>,
    thread_replies_down: Arc<AtomicBool>,
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
    // Each account's age, by lowercase login. One no test names is old.
    accounts: Arc<Mutex<HashMap<String, Timestamp>>>,
    // Whether reading an account's age fails
    accounts_down: Arc<AtomicBool>,
    // How many times each account's age was asked for, by lowercase login
    account_reads: Arc<Mutex<HashMap<String, usize>>>,
    created: Arc<Mutex<Vec<CreatedIssue>>>,
    // The failed CI runs: what their logs say, and the reruns asked of them
    ci: Arc<Mutex<handed::CiRuns>>,
    // The title a pull request is read with, where a test names one
    titles: Arc<Mutex<HashMap<u64, String>>>,
    // The title and body of each pull request kelpie opened or edited, by number
    texts: Arc<Mutex<HashMap<u64, (String, String)>>>,
    // Why listing and opening issues fail, while they do
    issues_down: Arc<Mutex<Option<String>>>,
    // How many more issues may open before every later one fails
    creates_left: Arc<Mutex<Option<usize>>>,
    reviews: Arc<Mutex<HashMap<u64, MaintainerReview>>>,
    // How many reviews each pull request was left, which its listing counts
    review_counts: Arc<Mutex<HashMap<u64, u64>>>,
    // A state file read as each comment is posted, and what it held then
    watched: Arc<Mutex<Option<PathBuf>>>,
    saved_at_comment: Arc<Mutex<Vec<serde_json::Value>>>,
    default_branch: Arc<Mutex<String>>,
    // Whether the repo requires a pull request's branch to be up to date with its base,
    // and whether reading that rule fails
    up_to_date_required: Arc<AtomicBool>,
    up_to_date_read_down: Arc<AtomicBool>,
    up_to_date_reads: Arc<AtomicUsize>,
    // Branches the forge has besides the default branch and those pushed to origin
    branches: Arc<Mutex<Vec<String>>>,
    // The files of the repos a test filled, by lowercase slug and then path
    files: Arc<Mutex<HashMap<String, BTreeMap<String, String>>>>,
    repo_labels: Arc<Mutex<Vec<String>>>,
    // Each write to the repo's labels or issues, such as `label made: in-progress`
    issue_writes: Arc<Mutex<Vec<String>>>,
    pushes: Arc<AtomicBool>,
    // When the review bot last reviewed on the repo, which it has until a test says not
    bot_reviewed: Arc<Mutex<Option<Timestamp>>>,
    bot_review_reads: Arc<AtomicUsize>,
    viewer_down: Arc<Mutex<Option<ForgeError>>>,
    login_token_down: Arc<Mutex<Option<ForgeError>>>,
    // Every call made, and the board's reads and open-issue listings among them
    asked: Arc<AtomicUsize>,
    board_reads: Arc<AtomicUsize>,
    issue_listings: Arc<AtomicUsize>,
    issue_reads: Arc<AtomicUsize>,
    // The reads of pull requests: the listing, one's view, and one's review
    pull_request_reads: Arc<reads::PullRequestReads>,
    // The heads whose held CI runs kelpie approved
    approvals: Arc<approvals::Approvals>,
    links: Arc<links::Links>,
    // Set while the forge is down
    down: Arc<Mutex<Option<down::Down>>>,
    // Set while the test holds the next call open
    hold: Arc<Mutex<Option<held::Gate>>>,
    limited: Arc<Mutex<Option<crate::ports::Cooldown>>>,
    // What reading the rate limits answers, and how often it was read
    rate: Arc<Mutex<down::Rate>>,
    /// Pull requests' labels, and what CodeRabbit posts
    pub coderabbit: FakeCodeRabbit,
    /// What people wrote on the threads, and who may write to the repo
    pub threads: thread::FakeThreads,
    /// The images the issues' links lead to
    pub images: images::FakeImages,
    // The App whose comments the forge then lists beside what people wrote,
    // which is what an App's own post looks like on a real repo
    app_comments: Arc<Mutex<Option<Arc<dyn AppComments>>>>,
}

/// The account the fake forge says kelpie acts as, and opens pull requests as
pub const VIEWER: &str = "the-maintainer";

/// The token the fake forge says `gh auth token` prints
pub const LOGIN_TOKEN: &str = "gho_the_maintainers_login";

impl FakeForge {
    /// A public repo whose pull requests' branches live on `origin`, a bare repo
    pub fn new(origin: PathBuf) -> Self {
        Self {
            visibility: Arc::new(Mutex::new(Visibility::Public)),
            visibilities: Arc::default(),
            accounts: Arc::default(),
            accounts_down: Arc::default(),
            account_reads: Arc::default(),
            missing: Arc::default(),
            unread: Arc::default(),
            labels: Arc::default(),
            ready: Arc::default(),
            unchecked: Arc::default(),
            blockers: Arc::default(),
            closed: Arc::default(),
            sub_issues: Arc::default(),
            not_planned: Arc::default(),
            sub_issues_down: Arc::default(),
            listed_finished: Arc::default(),
            closings: Arc::default(),
            closed_as: Arc::default(),
            pull_request_closings: Arc::default(),
            pull_request_closes_down: Arc::default(),
            pull_request_edit_down: Arc::default(),
            closes_down: Arc::default(),
            closes_refused: Arc::default(),
            open: Arc::default(),
            board_down: Arc::default(),
            calls: Arc::default(),
            origin,
            pull_requests: Arc::default(),
            checks: Arc::default(),
            comments: Arc::default(),
            comments_down: Arc::default(),
            talk_down: Arc::default(),
            thread_replies_down: Arc::default(),
            merges_down: Arc::default(),
            merge_answers_lost: Arc::default(),
            queue_on: Arc::default(),
            disarmed: Arc::default(),
            queue: Arc::default(),
            labels_down: Arc::default(),
            unreadable: Arc::default(),
            titles: Arc::default(),
            viewer_reads: Arc::default(),
            lagging: Arc::default(),
            lagging_drafts: Arc::default(),
            readied: Arc::default(),
            skipped: Arc::default(),
            merges: Arc::default(),
            issues: Arc::default(),
            created: Arc::default(),
            texts: Arc::default(),
            ci: Arc::default(),
            issues_down: Arc::default(),
            creates_left: Arc::default(),
            reviews: Arc::default(),
            review_counts: Arc::default(),
            watched: Arc::default(),
            saved_at_comment: Arc::default(),
            default_branch: Arc::new(Mutex::new("main".to_owned())),
            up_to_date_required: Arc::new(AtomicBool::new(true)),
            up_to_date_read_down: Arc::new(AtomicBool::new(false)),
            up_to_date_reads: Arc::default(),
            branches: Arc::new(Mutex::new(Vec::new())),
            files: Arc::default(),
            repo_labels: Arc::default(),
            issue_writes: Arc::default(),
            pushes: Arc::new(AtomicBool::new(true)),
            bot_reviewed: Arc::new(Mutex::new(Some(Timestamp(LAST_REVIEWED)))),
            bot_review_reads: Arc::default(),
            viewer_down: Arc::default(),
            login_token_down: Arc::default(),
            asked: Arc::default(),
            board_reads: Arc::default(),
            issue_listings: Arc::default(),
            issue_reads: Arc::default(),
            pull_request_reads: Arc::default(),
            approvals: Arc::default(),
            links: Arc::default(),
            down: Arc::default(),
            hold: Arc::default(),
            limited: Arc::default(),
            rate: Arc::default(),
            coderabbit: FakeCodeRabbit::default(),
            threads: thread::FakeThreads::default(),
            images: images::FakeImages::default(),
            app_comments: Arc::default(),
        }
    }

    /// Opens draft pull request `number` from `head`, closing `closes`
    pub fn open_pull_request(&self, number: u64, head: &str, closes: &[u64]) {
        self.open.lock().unwrap().push(OpenPullRequest {
            number,
            head: head.to_owned(),
            closes: closes.to_vec(),
            labels: Vec::new(),
            draft: true,
            commit: String::new(),
            checks: Checks::None,
            from_fork: false,
            author: VIEWER.to_owned(),
            author_is_bot: false,
            marks: ReviewMarks::default(),
        });
        let pr = FakePullRequest {
            base: "main".to_owned(),
            branch: head.to_owned(),
            state: PullRequestState::Open,
            draft: true,
            from_fork: false,
            fork: None,
            fork_owned_by_organization: false,
            author: VIEWER.to_owned(),
            author_is_bot: false,
        };
        self.pull_requests.lock().unwrap().insert(number, pr);
    }

    /// How many times the account kelpie acts as was asked
    pub fn viewer_reads(&self) -> usize {
        self.viewer_reads.load(Ordering::SeqCst)
    }

    /// Reports `checks` for commit `head`. A head with none reported is pending.
    pub fn set_checks(&self, head: &str, checks: Checks) {
        self.checks.lock().unwrap().insert(head.to_owned(), checks);
    }

    /// Merges or closes pull request `number` as someone other than kelpie would
    pub fn set_state(&self, number: u64, state: PullRequestState) {
        let mut prs = self.pull_requests.lock().unwrap();
        prs.get_mut(&number).expect("an opened pull request").state = state;
    }

    /// Makes pull request `number` close `closes` when kelpie merges it, in
    /// place of the issues it was opened to close
    pub fn set_closes(&self, number: u64, closes: &[u64]) {
        let mut open = self.open.lock().unwrap();
        for listed in open.iter_mut().filter(|l| l.number == number) {
            listed.closes = closes.to_vec();
        }
    }

    /// Makes reading pull request `number`'s review fail
    pub fn set_unreadable(&self, number: u64) {
        self.unreadable.lock().unwrap().insert(number);
    }

    /// Leaves `review` on pull request `number`, as the maintainer would
    pub fn review(&self, number: u64, review: MaintainerReview) {
        let prs = self.pull_requests.lock().unwrap();
        assert!(prs.contains_key(&number), "an opened pull request");
        self.reviews.lock().unwrap().insert(number, review);
        self.count_review(number);
    }

    /// Makes posting comments fail, or work again
    pub fn set_comments_down(&self, down: bool) {
        self.comments_down.store(down, Ordering::SeqCst);
    }

    /// Makes reading what people wrote on a pull request fail, or work again
    pub fn set_talk_down(&self, down: bool) {
        self.talk_down.store(down, Ordering::SeqCst);
    }

    /// Makes merging fail, or work again
    pub fn set_merges_down(&self, down: bool) {
        self.merges_down.store(down, Ordering::SeqCst);
    }

    /// Makes a merge land but answer an error, as a timed-out call would
    pub fn set_merge_answers_lost(&self, lost: bool) {
        self.merge_answers_lost.store(lost, Ordering::SeqCst);
    }

    /// Issue `number`'s labels, in the order they went on
    /// Makes changing a pull request's or an issue's labels fail, or work again
    pub fn set_labels_down(&self, down: bool) {
        self.labels_down.store(down, Ordering::SeqCst);
    }

    /// Every write to the repo's labels or issues, oldest first: a label made,
    /// an issue opened, closed or labelled
    pub fn issue_writes(&self) -> Vec<String> {
        self.issue_writes.lock().unwrap().clone()
    }

    /// Records `write` as one more write made through the forge
    pub fn wrote(&self, write: String) {
        self.issue_writes.lock().unwrap().push(write);
    }

    /// Every comment posted, oldest first, with its pull request
    pub fn comments(&self) -> Vec<(u64, String)> {
        self.comments.lock().unwrap().clone()
    }

    /// Reads the state file at `path` as each comment is posted
    pub fn watch_state(&self, path: PathBuf) {
        *self.watched.lock().unwrap() = Some(path);
    }

    /// The state file as each comment found it, oldest first
    pub fn saved_at_comment(&self) -> Vec<serde_json::Value> {
        self.saved_at_comment.lock().unwrap().clone()
    }

    /// Every pull request marked ready, in order
    pub fn readied(&self) -> Vec<u64> {
        self.readied.lock().unwrap().clone()
    }

    /// Every pull request the summon label went on while it was a draft, which
    /// CodeRabbit answers with "Draft PR not reviewed" and never reviews
    pub fn skipped_as_drafts(&self) -> Vec<u64> {
        self.skipped.lock().unwrap().clone()
    }

    /// Every merge made, with the head it was held to
    pub fn merges(&self) -> Vec<(u64, String)> {
        self.merges.lock().unwrap().clone()
    }

    fn opened(&self, number: u64) -> Result<FakePullRequest, ForgeError> {
        let prs = self.pull_requests.lock().unwrap();
        let pr = prs.get(&number).cloned();
        pr.ok_or_else(|| ForgeError::Failed(format!("no pull request #{number}")))
    }

    /// Makes reading the review threads' comments fail, or work again
    pub fn set_thread_replies_down(&self, down: bool) {
        self.thread_replies_down.store(down, Ordering::SeqCst);
    }

    /// Makes listing the board fail, or work again
    pub fn set_board_down(&self, down: bool) {
        self.board_down.store(down, Ordering::SeqCst);
    }

    /// How many times the repo's visibility was asked
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
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
    fn visibility(&self, repo: &ForgeSlug) -> Result<Visibility, ForgeError> {
        self.ask()?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let key = repo.as_str().to_lowercase();
        let own = self.visibilities.lock().unwrap().get(&key).copied();
        Ok(own.unwrap_or_else(|| *self.visibility.lock().unwrap()))
    }

    fn default_branch(&self, _repo: &ForgeSlug) -> Result<String, ForgeError> {
        self.ask()?;
        Ok(self.default_branch.lock().unwrap().clone())
    }

    fn requires_up_to_date(&self, repo: &ForgeSlug, base: &str) -> Result<bool, ForgeError> {
        self.up_to_date_rule(repo, base)
    }

    fn branch_exists(&self, _repo: &ForgeSlug, branch: &str) -> Result<bool, ForgeError> {
        self.ask()?;
        let default = self.default_branch.lock().unwrap().clone() == branch;
        let named = self.branches.lock().unwrap().iter().any(|b| b == branch);
        Ok(default || named || self.head_of(branch).is_some())
    }

    fn folder(&self, repo: &ForgeSlug, path: &str) -> Result<Option<Vec<FolderEntry>>, ForgeError> {
        self.ask()?;
        Ok(self.folder_of(repo, path))
    }

    fn file_text(&self, repo: &ForgeSlug, path: &str) -> Result<String, ForgeError> {
        self.ask()?;
        let files = self.files.lock().unwrap();
        let found = files
            .get(&repo.as_str().to_lowercase())
            .and_then(|f| f.get(path));
        found
            .cloned()
            .ok_or_else(|| ForgeError::Failed("gh: Not Found (HTTP 404)".into()))
    }

    fn repo_labels(&self, _repo: &ForgeSlug) -> Result<Vec<String>, ForgeError> {
        self.ask()?;
        Ok(self.repo_labels.lock().unwrap().clone())
    }

    fn create_label(&self, _repo: &ForgeSlug, label: &NewLabel) -> Result<(), ForgeError> {
        self.ask()?;
        self.wrote(format!("label made: {}", label.name));
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

    fn last_bot_review(
        &self,
        _repo: &ForgeSlug,
        _login: Login<'_>,
    ) -> Result<Option<Timestamp>, ForgeError> {
        self.bot_review_reads.fetch_add(1, Ordering::SeqCst);
        self.ask()?;
        Ok(*self.bot_reviewed.lock().unwrap())
    }

    fn open_pull_requests(&self, _repo: &ForgeSlug) -> Result<Vec<OpenPullRequest>, ForgeError> {
        self.pull_request_reads
            .listed
            .fetch_add(1, Ordering::SeqCst);
        self.ask()?;
        self.wait_if_held(held::At::OpenPullRequests);
        self.board()?;
        self.list_open()
    }

    fn pull_request(&self, _repo: &ForgeSlug, number: u64) -> Result<PullRequest, ForgeError> {
        self.pull_request_reads
            .viewed
            .fetch_add(1, Ordering::SeqCst);
        self.ask()?;
        self.wait_if_held(held::At::PullRequest);
        self.read(number)
    }

    fn reviewed(&self, _repo: &ForgeSlug, number: u64) -> Result<Reviewed, ForgeError> {
        self.pull_request_reads
            .reviewed
            .fetch_add(1, Ordering::SeqCst);
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
            title: (self.titles.lock().unwrap().get(&number).cloned())
                .unwrap_or_else(|| format!("Title of pull request #{number}")),
            body: (self.texts.lock().unwrap().get(&number).map(|t| t.1.clone()))
                .unwrap_or_else(|| format!("Body of pull request #{number}.\n")),
            closes,
            base: pr.base,
            branch: pr.branch,
            from_fork: pr.from_fork,
            maintainer_can_edit: pr.fork.as_ref().is_some_and(|(_, edits)| *edits),
            head_owner_is_organization: pr.fork_owned_by_organization,
            head_clone_url: match pr.fork {
                Some((url, _)) => Some(url),
                // A branch of the repo itself is cloned from origin.
                None if !pr.from_fork => Some("origin".to_owned()),
                None => None,
            },
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

    fn login_token(&self) -> Result<InstallationToken, ForgeError> {
        self.ask()?;
        match &*self.login_token_down.lock().unwrap() {
            Some(error) => Err(error.clone()),
            None => Ok(InstallationToken::new(LOGIN_TOKEN.to_owned())),
        }
    }

    fn account_created(&self, login: &str) -> Result<Timestamp, ForgeError> {
        self.ask()?;
        let key = login.to_lowercase();
        let mut reads = self.account_reads.lock().unwrap();
        *reads.entry(key.clone()).or_default() += 1;
        drop(reads);
        if self.accounts_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("the account is unreadable".into()));
        }
        Ok(self
            .accounts
            .lock()
            .unwrap()
            .get(&key)
            .copied()
            .unwrap_or(Timestamp(0)))
    }

    fn talk(&self, _repo: &ForgeSlug, number: u64) -> Result<Talk, ForgeError> {
        self.ask()?;
        self.wait_if_held(held::At::Talk);
        if self.talk_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("comments are down".into()));
        }
        self.opened(number)?;
        let mut talk = self.coderabbit.talk_of(number);
        talk.comments.extend(self.threads.remarks(number));
        talk.comments.sort_by_key(|r| r.id);
        Ok(talk)
    }

    fn comments_since(
        &self,
        _repo: &ForgeSlug,
        since: Timestamp,
    ) -> Result<Vec<ThreadComment>, ForgeError> {
        self.ask()?;
        Ok(self.threads.read(since))
    }

    fn thread_replies_since(
        &self,
        _repo: &ForgeSlug,
        number: u64,
        since: Timestamp,
    ) -> Result<Vec<ThreadComment>, ForgeError> {
        self.ask()?;
        self.opened(number)?;
        if self.thread_replies_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("review threads are down".into()));
        }
        Ok(self.threads.read_reviews(number, since))
    }

    fn can_write(&self, _repo: &ForgeSlug, login: &str) -> Result<bool, ForgeError> {
        self.ask()?;
        Ok(self.threads.may_write(login))
    }

    fn comment_marks(&self, _repo: &ForgeSlug, comment: u64) -> Result<CommentMarks, ForgeError> {
        self.ask()?;
        (self.threads.marks_of(comment))
            .ok_or_else(|| ForgeError::Failed(format!("comment {comment} is unreadable")))
    }

    fn comment(&self, _repo: &ForgeSlug, number: u64, body: &str) -> Result<u64, ForgeError> {
        self.ask()?;
        self.wait_if_held(held::At::Comment);
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
                .any(|i| i.ticket.github() == Some(number));
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
        // Kelpie's login is a writer: the comment is on the thread like any other.
        Ok(self.threads.write_now(number, VIEWER, body))
    }

    fn open_pulls(&self, _repo: &ForgeSlug) -> Result<Vec<OpenPull>, ForgeError> {
        self.pull_request_reads
            .closing
            .fetch_add(1, Ordering::SeqCst);
        self.ask()?;
        self.read_open_pulls()
    }

    fn close_pull_request(&self, _repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        self.ask()?;
        self.close_open_pull_request(number)
    }

    // Numbers start at 500, clear of every pull request a test opens by hand.
    fn create_pull_request(
        &self,
        _repo: &ForgeSlug,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<u64, ForgeError> {
        self.ask()?;
        let mut texts = self.texts.lock().unwrap();
        let number = 500 + texts.len() as u64;
        let closes = (body.lines())
            .filter_map(|l| l.strip_prefix("Resolves #")?.trim().parse().ok())
            .collect::<Vec<u64>>();
        self.open_pull_request(number, head, &closes);
        self.set_base(number, base);
        texts.insert(number, (title.to_owned(), body.to_owned()));
        Ok(number)
    }

    fn edit_pull_request(
        &self,
        _repo: &ForgeSlug,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.ask()?;
        self.opened(number)?;
        if self.pull_request_edit_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed(
                "editing a pull request is refused".into(),
            ));
        }
        let text = (title.to_owned(), body.to_owned());
        self.texts.lock().unwrap().insert(number, text);
        Ok(())
    }

    fn failed_run_logs(
        &self,
        _repo: &ForgeSlug,
        _head: &str,
        _merge_group: bool,
    ) -> Result<FailedLogs, ForgeError> {
        self.ask()?;
        self.wait_if_held(held::At::FailedLogs);
        self.ci.lock().unwrap().log().map_err(ForgeError::Failed)
    }

    fn branch_runs(&self, _repo: &ForgeSlug, branch: &str) -> Result<Vec<BranchRun>, ForgeError> {
        self.ask()?;
        self.wait_if_held(held::At::MainRuns);
        self.branch_runs_of(branch).map_err(ForgeError::Failed)
    }

    fn workflows(&self, _repo: &ForgeSlug) -> Result<Vec<Workflow>, ForgeError> {
        self.ask()?;
        self.workflows_listed().map_err(ForgeError::Failed)
    }

    fn run_log_tail(&self, _repo: &ForgeSlug, run: u64) -> Result<String, ForgeError> {
        self.ask()?;
        self.wait_if_held(held::At::RunLogTail);
        self.run_log_tail_of(run).map_err(ForgeError::Failed)
    }

    fn rerun_failed(&self, _repo: &ForgeSlug, head: &str) -> Result<(), ForgeError> {
        self.ask()?;
        self.ci
            .lock()
            .unwrap()
            .rerun(head)
            .map_err(ForgeError::Failed)
    }

    fn rerun_run(&self, _repo: &ForgeSlug, run: u64, rerun: Rerun) -> Result<(), ForgeError> {
        self.ask()?;
        self.wait_if_held(held::At::Rerun);
        let mut ci = self.ci.lock().unwrap();
        ci.rerun_of_run(run, rerun).map_err(ForgeError::Failed)
    }

    fn approve_workflow_runs(&self, _repo: &ForgeSlug, head: &str) -> Result<(), ForgeError> {
        self.approve(head)
    }

    fn mark_ready(&self, _repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        self.ask()?;
        self.wait_if_held(held::At::MarkReady);
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

    // One store for every bot, since a test runs one; the login is recorded.
    fn review_bot(
        &self,
        _repo: &ForgeSlug,
        number: u64,
        login: Login<'_>,
    ) -> Result<Activity, ForgeError> {
        // Held before the forge is asked, so a test can bring it down meanwhile
        self.wait_if_held(held::At::Activity);
        self.ask()?;
        self.opened(number)?;
        self.coderabbit.activity(number, login.rest)
    }

    fn resolve_thread(&self, _repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError> {
        self.ask()?;
        self.coderabbit.made_by_gh();
        self.coderabbit.resolve(thread)
    }

    fn reply_to_thread(
        &self,
        _repo: &ForgeSlug,
        thread: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.ask()?;
        self.coderabbit.made_by_gh();
        self.coderabbit.reply(thread, body)
    }

    // Refuses what GitHub refuses: a draft, a pull request that is not open,
    // and a head that moved since the caller looked.
    fn merge(&self, _repo: &ForgeSlug, number: u64, head: &str) -> Result<(), ForgeError> {
        self.ask()?;
        self.wait_if_held(held::At::Merge);
        if self.merges_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("merges are down".into()));
        }
        // The forge checks the head itself, which is no read of the runner's
        let now = self.read(number)?;
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
        // The forge closes the issues the pull request names as it merges.
        let closes = (self.open.lock().unwrap().iter())
            .rfind(|l| l.number == number)
            .map(|l| l.closes.clone())
            .unwrap_or_default();
        self.closed.lock().unwrap().extend(closes);
        if self.merge_answers_lost.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("the answer was lost".into()));
        }
        Ok(())
    }

    fn merge_queue(&self, _repo: &ForgeSlug, number: u64) -> Result<QueueStanding, ForgeError> {
        self.ask()?;
        self.wait_if_held(held::At::MergeQueue);
        self.opened(number)?;
        Ok(self.standing(number))
    }

    fn disable_auto_merge(&self, _repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        self.ask()?;
        self.opened(number)?;
        self.disarm(number);
        Ok(())
    }

    fn rate_limit(&self) -> Result<RateLimits, ForgeError> {
        self.read_rate_limit()
    }
}

pub use comments::AppComments;
pub use held::HeldCall;
pub(crate) use held::{At, Gate};
pub use issues::CreatedIssue;

use handed::FakePullRequest;

mod app;
mod approvals;
mod authors;
mod comments;
mod down;
mod handed;
mod held;
mod images;
mod issues;
mod links;
mod lookup;
mod pulls;
mod queue;
mod reads;
mod repo;
mod rules;
mod thread;
mod tickets;
