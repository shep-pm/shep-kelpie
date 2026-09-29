//! The rig's forge: issues, a board, and pull requests whose heads live on
//! the rig's bare origin

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::coderabbit::FakeCodeRabbit;
use crate::board::{Blocker, OpenPullRequest, READY, ReadyIssue};
use crate::coderabbit::Activity;
use crate::ports::{
    Checks, Forge, ForgeError, Issue, MaintainerReview, PullRequest, PullRequestState, Reviewed,
    Visibility,
};
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
    labels_down: Arc<AtomicBool>,
    unreadable: Arc<Mutex<HashSet<u64>>>,
    viewer_reads: Arc<AtomicUsize>,
    lagging: Arc<Mutex<HashMap<u64, String>>>,
    lagging_drafts: Arc<Mutex<HashSet<u64>>>,
    readied: Arc<Mutex<Vec<u64>>>,
    skipped: Arc<Mutex<Vec<u64>>>,
    merges: Arc<Mutex<Vec<(u64, String)>>>,
    reviews: Arc<Mutex<HashMap<u64, MaintainerReview>>>,
    /// Pull requests' labels, and what CodeRabbit posts
    pub(crate) coderabbit: FakeCodeRabbit,
}

/// A pull request on the fake forge. Its head is its branch on the rig's
/// origin, so a push the runner makes moves it.
#[derive(Debug, Clone)]
struct FakePullRequest {
    branch: String,
    state: PullRequestState,
    draft: bool,
    from_fork: bool,
    author: String,
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
            labels_down: Arc::default(),
            unreadable: Arc::default(),
            viewer_reads: Arc::default(),
            lagging: Arc::default(),
            lagging_drafts: Arc::default(),
            readied: Arc::default(),
            skipped: Arc::default(),
            merges: Arc::default(),
            reviews: Arc::default(),
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
            blocked_by: Vec::new(),
            unlisted_blockers: 0,
        });
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
        });
        let pr = FakePullRequest {
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

    /// Marks pull request `number` ready, as someone other than kelpie would
    pub(crate) fn ready_pull_request(&self, number: u64) {
        let mut prs = self.pull_requests.lock().unwrap();
        prs.get_mut(&number).expect("an opened pull request").draft = false;
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

    /// Makes changing a pull request's labels fail, or work again
    pub(crate) fn set_labels_down(&self, down: bool) {
        self.labels_down.store(down, Ordering::SeqCst);
    }

    /// Every comment posted, oldest first, with its pull request
    pub(crate) fn comments(&self) -> Vec<(u64, String)> {
        self.comments.lock().unwrap().clone()
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
                blocked_by: self.blockers_of(i.number),
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
            draft: pr.draft || self.lagging_drafts.lock().unwrap().contains(&number),
            checks: checks.unwrap_or(Checks::Pending),
            head,
            labels: self.coderabbit.labels(number),
        })
    }

    fn reviewed(&self, _repo: &ForgeSlug, number: u64) -> Result<Reviewed, ForgeError> {
        if self.unreadable.lock().unwrap().contains(&number) {
            return Err(ForgeError::Failed(format!("#{number} is unreadable")));
        }
        let pr = self.opened(number)?;
        Ok(Reviewed {
            state: pr.state,
            branch: pr.branch,
            from_fork: pr.from_fork,
            author: pr.author,
            draft: pr.draft,
            labels: self.coderabbit.labels(number),
            review: self.reviews.lock().unwrap().get(&number).cloned(),
        })
    }

    fn viewer(&self) -> Result<String, ForgeError> {
        self.viewer_reads.fetch_add(1, Ordering::SeqCst);
        Ok(VIEWER.to_owned())
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
        if self.merge_answers_lost.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("the answer was lost".into()));
        }
        Ok(())
    }
}
