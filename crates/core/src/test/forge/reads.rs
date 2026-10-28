//! The fake forge's reads of pull requests, and the count of each kind

use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicUsize;

use super::FakeForge;
use super::handed::FakePullRequest;
use crate::board::{OpenPullRequest, ReviewMarks};
use crate::ports::{Checks, ForgeError, PullRequest, PullRequestState};

/// How many times each kind of read of pull requests was made
#[derive(Debug, Default)]
pub struct PullRequestReads {
    /// The listing of every open pull request
    pub listed: AtomicUsize,
    /// The listing of every open pull request with the issues each closes, which
    /// the triage poll reads
    pub closing: AtomicUsize,
    /// One pull request's state, head and checks
    pub viewed: AtomicUsize,
    /// One pull request's review
    pub reviewed: AtomicUsize,
}

impl FakeForge {
    /// The reads of pull requests made so far
    pub fn pull_request_reads(&self) -> &PullRequestReads {
        &self.pull_request_reads
    }

    /// Makes pull request `number` a draft again, or not, as someone other than kelpie would
    pub fn set_draft(&self, number: u64, draft: bool) {
        let mut prs = self.pull_requests.lock().unwrap();
        prs.get_mut(&number).expect("an opened pull request").draft = draft;
    }

    /// Whether pull request `number` is a draft
    pub fn draft(&self, number: u64) -> bool {
        let prs = self.pull_requests.lock().unwrap();
        prs.get(&number).expect("an opened pull request").draft
    }

    /// Marks pull request `number` ready, as someone other than kelpie would
    pub fn ready_pull_request(&self, number: u64) {
        let mut prs = self.pull_requests.lock().unwrap();
        prs.get_mut(&number).expect("an opened pull request").draft = false;
    }

    /// Makes pull request `number` merge into `base` rather than `main`
    pub fn set_base(&self, number: u64, base: &str) {
        let mut prs = self.pull_requests.lock().unwrap();
        prs.get_mut(&number).expect("an opened pull request").base = base.to_owned();
    }

    /// The branch pull request `number` merges into
    pub fn base_of(&self, number: u64) -> String {
        let prs = self.pull_requests.lock().unwrap();
        prs.get(&number)
            .expect("an opened pull request")
            .base
            .clone()
    }

    /// Makes pull request `number` come from a fork's branch
    pub fn set_from_fork(&self, number: u64) {
        let mut prs = self.pull_requests.lock().unwrap();
        prs.get_mut(&number)
            .expect("an opened pull request")
            .from_fork = true;
    }

    /// Makes pull request `number` come from the fork cloned from `url`,
    /// whose owner lets the maintainers edit it or not
    pub fn set_fork(&self, number: u64, url: &str, maintainer_can_edit: bool) {
        let mut prs = self.pull_requests.lock().unwrap();
        let pr = prs.get_mut(&number).expect("an opened pull request");
        pr.from_fork = true;
        pr.fork = Some((url.to_owned(), maintainer_can_edit));
    }

    /// Makes an organization own the fork pull request `number` comes from
    pub fn set_fork_owned_by_organization(&self, number: u64) {
        let mut prs = self.pull_requests.lock().unwrap();
        let pr = prs.get_mut(&number).expect("an opened pull request");
        pr.fork_owned_by_organization = true;
    }

    /// Counts one more review read of `number`
    pub fn count_review(&self, number: u64) {
        let mut counts = self.review_counts.lock().unwrap();
        *counts.entry(number).or_default() += 1;
    }

    /// The commit `branch` points at on origin, if it exists there
    pub fn head_of(&self, branch: &str) -> Option<String> {
        Self::head_in(&self.origin, branch)
    }

    /// The commit `branch` points at in the bare repository `git_dir`
    pub fn head_in(git_dir: &Path, branch: &str) -> Option<String> {
        let output = Command::new("git")
            .arg("--git-dir")
            .arg(git_dir)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("refs/heads/{branch}"))
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    // The head of `pr`'s branch in the repository it lives in: the fork when
    // one was set that is a folder on disk, else origin
    fn head_in_its_repository(&self, pr: &FakePullRequest) -> Option<String> {
        match &pr.fork {
            Some((url, _)) if Path::new(url).is_dir() => Self::head_in(Path::new(url), &pr.branch),
            _ => self.head_of(&pr.branch),
        }
    }

    /// Pull request `number` as the forge shows it now, with no call counted
    pub fn read(&self, number: u64) -> Result<PullRequest, ForgeError> {
        let pr = self.opened(number)?;
        let lagging = self.lagging.lock().unwrap().get(&number).cloned();
        let head = match lagging {
            Some(head) => head,
            None => self
                .head_in_its_repository(&pr)
                .ok_or_else(|| ForgeError::Failed(format!("no branch {} on origin", pr.branch)))?,
        };
        let checks = self.checks.lock().unwrap().get(&head).cloned();
        Ok(PullRequest {
            state: pr.state,
            draft: pr.draft || self.lagging_drafts.lock().unwrap().contains(&number),
            checks: checks.unwrap_or(Checks::Pending(Vec::new())),
            head,
            labels: self.coderabbit.labels(number),
        })
    }

    /// Every open pull request as `read` shows it, one whose branch is not on
    /// origin with no head and no checks, with no call counted
    pub fn list_open(&self) -> Result<Vec<OpenPullRequest>, ForgeError> {
        let open = self.open.lock().unwrap().clone();
        let mut listed = Vec::new();
        for pr in open {
            let fake = self.opened(pr.number)?;
            if fake.state != PullRequestState::Open {
                continue;
            }
            let view = self.read(pr.number).ok();
            let reviews = self.review_counts.lock().unwrap().get(&pr.number).copied();
            listed.push(OpenPullRequest {
                labels: self.coderabbit.labels(pr.number),
                draft: view.as_ref().map_or(fake.draft, |v| v.draft),
                commit: view.as_ref().map(|v| v.head.clone()).unwrap_or_default(),
                checks: view.map_or(Checks::None, |v| v.checks),
                from_fork: fake.from_fork,
                author: fake.author,
                author_is_bot: fake.author_is_bot,
                marks: ReviewMarks {
                    reviews: reviews.unwrap_or_default(),
                    threads: 0,
                },
                ..pr
            });
        }
        Ok(listed)
    }

    /// Makes pull request `number` report `head` whatever origin holds, as
    /// GitHub does for a moment after a push, or stop doing so
    pub fn set_lagging(&self, number: u64, head: Option<&str>) {
        let mut lagging = self.lagging.lock().unwrap();
        match head {
            Some(head) => lagging.insert(number, head.to_owned()),
            None => lagging.remove(&number),
        };
    }

    /// Makes pull request `number` keep reading as a draft after it is marked
    /// ready, as GitHub does for a moment, or stop doing so
    pub fn set_lagging_draft(&self, number: u64, lagging: bool) {
        let mut drafts = self.lagging_drafts.lock().unwrap();
        if lagging {
            drafts.insert(number);
        } else {
            drafts.remove(&number);
        }
    }
}
