//! The rig's forge: issues, a board, and pull requests whose heads live on
//! the rig's bare origin

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::coderabbit::FakeCodeRabbit;
use crate::board::{OpenPullRequest, READY, ReadyIssue};
use crate::coderabbit::Activity;
use crate::ports::{Checks, Forge, ForgeError, Issue, PullRequest, PullRequestState, Visibility};
use crate::settings::ForgeSlug;

/// A forge whose repo is public and whose every issue exists, unless a
/// test says otherwise. Its board is empty until a test lists issues on it.
#[derive(Debug, Clone)]
pub(crate) struct FakeForge {
    visibility: Arc<Mutex<Visibility>>,
    missing: Arc<Mutex<HashSet<u64>>>,
    labels: Arc<Mutex<HashMap<u64, Vec<String>>>>,
    ready: Arc<Mutex<Vec<ReadyIssue>>>,
    open: Arc<Mutex<Vec<OpenPullRequest>>>,
    board_down: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    origin: PathBuf,
    pull_requests: Arc<Mutex<HashMap<u64, FakePullRequest>>>,
    checks: Arc<Mutex<HashMap<String, Checks>>>,
    comments: Arc<Mutex<Vec<(u64, String)>>>,
    comments_down: Arc<AtomicBool>,
    merges_down: Arc<AtomicBool>,
    lagging: Arc<Mutex<HashMap<u64, String>>>,
    readied: Arc<Mutex<Vec<u64>>>,
    merges: Arc<Mutex<Vec<(u64, String)>>>,
    /// Labels, and what CodeRabbit posts
    pub(crate) coderabbit: FakeCodeRabbit,
}

/// A pull request on the fake forge. Its head is its branch on the rig's
/// origin, so a push the runner makes moves it.
#[derive(Debug, Clone)]
struct FakePullRequest {
    branch: String,
    state: PullRequestState,
    draft: bool,
}

impl FakeForge {
    /// A public repo whose pull requests' branches live on `origin`, a bare repo
    pub(crate) fn new(origin: PathBuf) -> Self {
        Self {
            visibility: Arc::new(Mutex::new(Visibility::Public)),
            missing: Arc::default(),
            labels: Arc::default(),
            ready: Arc::default(),
            open: Arc::default(),
            board_down: Arc::default(),
            calls: Arc::default(),
            origin,
            pull_requests: Arc::default(),
            checks: Arc::default(),
            comments: Arc::default(),
            comments_down: Arc::default(),
            merges_down: Arc::default(),
            lagging: Arc::default(),
            readied: Arc::default(),
            merges: Arc::default(),
            coderabbit: FakeCodeRabbit::default(),
        }
    }

    pub(crate) fn set_visibility(&self, visibility: Visibility) {
        *self.visibility.lock().unwrap() = visibility;
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
            assigned,
            labels: Vec::new(),
        });
    }

    /// Opens draft pull request `number` from `head`, closing `closes`
    pub(crate) fn open_pull_request(&self, number: u64, head: &str, closes: &[u64]) {
        self.open.lock().unwrap().push(OpenPullRequest {
            number,
            head: head.to_owned(),
            closes: closes.to_vec(),
        });
        let pr = FakePullRequest {
            branch: head.to_owned(),
            state: PullRequestState::Open,
            draft: true,
        };
        self.pull_requests.lock().unwrap().insert(number, pr);
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

    /// Makes merging fail, or work again
    pub(crate) fn set_merges_down(&self, down: bool) {
        self.merges_down.store(down, Ordering::SeqCst);
    }

    /// Every comment posted, oldest first, with its pull request
    pub(crate) fn comments(&self) -> Vec<(u64, String)> {
        self.comments.lock().unwrap().clone()
    }

    /// Every pull request marked ready, in order
    pub(crate) fn readied(&self) -> Vec<u64> {
        self.readied.lock().unwrap().clone()
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

    fn board(&self) -> Result<(), ForgeError> {
        if self.board_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("the board is down".into()));
        }
        Ok(())
    }
}

impl Forge for FakeForge {
    fn visibility(&self, _repo: &ForgeSlug) -> Result<Visibility, ForgeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(*self.visibility.lock().unwrap())
    }

    fn issue(&self, _repo: &ForgeSlug, number: u64) -> Result<Issue, ForgeError> {
        if self.missing.lock().unwrap().contains(&number) {
            return Err(ForgeError::Failed(format!("no issue #{number}")));
        }
        Ok(Issue {
            title: format!("Title of #{number}"),
            body: format!("Body of #{number}.\n"),
            labels: self.labels_of(number),
        })
    }

    fn ready_issues(&self, _repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError> {
        self.board()?;
        let ready = self.ready.lock().unwrap().clone();
        Ok(ready
            .into_iter()
            .map(|i| ReadyIssue {
                labels: self.labels_of(i.number),
                ..i
            })
            .collect())
    }

    fn open_pull_requests(&self, _repo: &ForgeSlug) -> Result<Vec<OpenPullRequest>, ForgeError> {
        self.board()?;
        let prs = self.pull_requests.lock().unwrap();
        let open = self.open.lock().unwrap().clone();
        Ok(open
            .into_iter()
            .filter(|pr| prs[&pr.number].state == PullRequestState::Open)
            .collect())
    }

    fn pull_request(&self, _repo: &ForgeSlug, number: u64) -> Result<PullRequest, ForgeError> {
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
            draft: pr.draft,
            checks: checks.unwrap_or(Checks::Pending),
            head,
            labels: self.coderabbit.labels(number),
        })
    }

    fn comment(&self, _repo: &ForgeSlug, number: u64, body: &str) -> Result<(), ForgeError> {
        if self.comments_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("comments are down".into()));
        }
        self.opened(number)?;
        self.comments
            .lock()
            .unwrap()
            .push((number, body.to_owned()));
        Ok(())
    }

    fn mark_ready(&self, _repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
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
        self.opened(number)?;
        self.coderabbit.set_label(number, label, on);
        Ok(())
    }

    fn coderabbit(&self, _repo: &ForgeSlug, number: u64) -> Result<Activity, ForgeError> {
        self.opened(number)?;
        self.coderabbit.activity(number)
    }

    fn resolve_thread(&self, _repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError> {
        self.coderabbit.resolve(thread)
    }

    // Refuses what GitHub refuses: a draft, a pull request that is not open,
    // and a head that moved since the caller looked.
    fn merge(&self, repo: &ForgeSlug, number: u64, head: &str) -> Result<(), ForgeError> {
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
        self.set_state(number, PullRequestState::Merged);
        Ok(())
    }
}
