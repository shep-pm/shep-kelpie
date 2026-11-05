//! The fake forge's open issues, the ones kelpie opened and closed, and the issues' calls going down

use std::sync::atomic::Ordering;

use super::FakeForge;
use crate::ports::{
    AuthorAssociation, FinishedParent, ForgeError, IssueCloseReason, OpenIssue, OpenPull,
    PullRequestState, SubIssue, SubIssueState, Timestamp,
};

use crate::test::ticket;

/// An issue kelpie opened on the fake forge
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedIssue {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
}

fn plain_issue(number: u64, title: &str, body: &str) -> OpenIssue {
    OpenIssue {
        number,
        title: title.to_owned(),
        body: body.to_owned(),
        author: "the-maintainer".to_owned(),
        author_is_bot: false,
        author_association: Some(AuthorAssociation::Owner),
        labels: Vec::new(),
        created: Timestamp(0),
    }
}

impl FakeForge {
    /// Says the account `login` was created at `at`, which is how a test makes
    /// an author's account new. An account no test names is old.
    pub fn set_account_created(&self, login: &str, at: Timestamp) {
        self.accounts
            .lock()
            .unwrap()
            .insert(login.to_lowercase(), at);
    }

    /// Makes reading an account's age fail, or work again
    pub fn set_accounts_down(&self, down: bool) {
        self.accounts_down.store(down, Ordering::SeqCst);
    }

    /// How many times the forge was asked for `login`'s account, which the
    /// runner asks for once per author per start
    pub fn account_reads(&self, login: &str) -> usize {
        self.account_reads
            .lock()
            .unwrap()
            .get(&login.to_lowercase())
            .copied()
            .unwrap_or_default()
    }

    /// Makes issue `child` a sub-issue of issue `parent`, as someone else would
    pub fn link_sub_issue(&self, parent: u64, child: u64) {
        let mut links = self.sub_issues.lock().unwrap();
        links.entry(parent).or_default().push(child);
    }

    /// Closes issue `number` as not planned, as someone else would
    pub fn close_issue_as_not_planned(&self, number: u64) {
        self.not_planned.lock().unwrap().insert(number);
        self.close_issue(number);
    }

    /// Reopens closed issue `number`, as its author would. Its labels, and what
    /// kelpie closed it for, stay as they were.
    pub fn reopen_issue(&self, number: u64) {
        self.closed.lock().unwrap().remove(&number);
    }

    /// Makes listing a parent's sub-issues fail, or work again
    pub fn set_sub_issues_down(&self, down: bool) {
        self.sub_issues_down.store(down, Ordering::SeqCst);
    }

    /// Issue `parent`'s sub-issues, in the order they were linked
    pub fn sub_issues_of(&self, parent: u64) -> Vec<u64> {
        let links = self.sub_issues.lock().unwrap();
        links.get(&parent).cloned().unwrap_or_default()
    }

    /// The sub-issues the stand-in holds under `parent`
    pub fn read_sub_issues(&self, parent: u64) -> Result<Vec<SubIssue>, ForgeError> {
        self.ask()?;
        if self.sub_issues_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("sub-issues are unreadable".into()));
        }
        let closed = self.closed.lock().unwrap();
        let not_planned = self.not_planned.lock().unwrap();
        Ok(self
            .sub_issues_of(parent)
            .into_iter()
            .map(|number| SubIssue {
                ticket: ticket(number),
                state: match (closed.contains(&number), not_planned.contains(&number)) {
                    (false, _) => SubIssueState::Open,
                    (true, true) => SubIssueState::NotPlanned,
                    (true, false) => SubIssueState::Completed,
                },
            })
            .collect())
    }

    /// Makes the listing of finished parents name `parent` from now on, as a
    /// stale listing would, whether or not its sub-issues are all closed
    pub fn list_finished_whatever_its_sub_issues(&self, parent: u64) {
        self.listed_finished.lock().unwrap().push(parent);
    }

    /// Shows closed issue `number` as completed, as someone else changing the
    /// reason it closed with would
    pub fn reclose_as_completed(&self, number: u64) {
        self.not_planned.lock().unwrap().remove(&number);
    }

    /// The parents the stand-in holds as finished
    pub fn read_finished_parents(&self) -> Result<Vec<FinishedParent>, ForgeError> {
        self.ask()?;
        let mut parents: Vec<_> = self.sub_issues.lock().unwrap().keys().copied().collect();
        parents.sort_unstable();
        let stale = self.listed_finished.lock().unwrap().clone();
        let closed = self.closed.lock().unwrap();
        Ok(parents
            .into_iter()
            .filter(|parent| !closed.contains(parent))
            .map(|number| (number, self.sub_issues_of(number)))
            .filter(|(n, subs)| stale.contains(n) || subs.iter().all(|s| closed.contains(s)))
            .map(|(number, subs)| FinishedParent {
                ticket: ticket(number),
                sub_issues: subs.len() as u64,
            })
            .collect())
    }

    /// Leaves issue `number` open, as someone other than kelpie would have,
    /// and shows this title and body when it is viewed
    pub fn open_issue(&self, number: u64, title: &str, body: &str) {
        self.list_open_issue(plain_issue(number, title, body));
    }

    /// Lists issue `number` as kelpie's own account opened it with `labels`
    pub fn list_created(&self, number: u64, title: &str, body: &str, labels: &[&str]) {
        self.list_open_issue(OpenIssue {
            labels: labels.iter().map(|&l| l.to_owned()).collect(),
            ..plain_issue(number, title, body)
        });
    }

    /// Leaves `issue` open, with the author, labels and time it carries, in
    /// place of any listed under its number
    pub fn list_open_issue(&self, issue: OpenIssue) {
        let mut listed = self.issues.lock().unwrap();
        listed.retain(|i| i.number != issue.number);
        listed.push(issue);
    }

    /// Takes issue `number` off the open listing, as closing it does
    pub fn unlist_open_issue(&self, number: u64) {
        self.issues.lock().unwrap().retain(|i| i.number != number);
    }

    /// Every issue kelpie opened, oldest first
    pub fn created(&self) -> Vec<CreatedIssue> {
        self.created.lock().unwrap().clone()
    }

    /// Takes `number` out of what the forge can read, as a repo whose issue is
    /// gone reads
    pub fn remove_issue(&self, number: u64) {
        self.missing.lock().unwrap().insert(number);
    }

    /// Lets `n` reads of `number` through and fails every one after, as a forge
    /// that begins to fail partway through what kelpie is doing, until
    /// [`Self::let_issue_read`]
    pub fn fail_issue_reads_after(&self, number: u64, n: usize) {
        self.unread.lock().unwrap().insert(number, n);
    }

    /// Lets reads of `number` through again after [`Self::fail_issue_reads_after`]
    /// or [`Self::set_issue_read_error`]
    pub fn let_issue_read(&self, number: u64) {
        self.unread.lock().unwrap().remove(&number);
        self.issue_errors.lock().unwrap().remove(&number);
    }

    /// Makes every read of `number` fail with `error`, as the forge answers it,
    /// until [`Self::let_issue_read`]
    pub fn set_issue_read_error(&self, number: u64, error: ForgeError) {
        self.issue_errors.lock().unwrap().insert(number, error);
    }

    /// Makes listing and opening issues fail, or work again
    pub fn set_issues_down(&self, down: bool) {
        let why = down.then(|| "issues are down".to_owned());
        *self.issues_down.lock().unwrap() = why;
    }

    /// Makes listing and opening issues fail with `error`, verbatim
    pub fn set_issues_error(&self, error: &str) {
        *self.issues_down.lock().unwrap() = Some(error.to_owned());
    }

    /// Lets `n` more issues open, then makes every later one fail
    pub fn set_creates_left(&self, n: usize) {
        *self.creates_left.lock().unwrap() = Some(n);
    }

    /// Makes closing an issue fail, or work again
    pub fn set_closes_down(&self, down: bool) {
        self.closes_down.store(down, Ordering::SeqCst);
    }

    /// How many closes the forge refused while closing was down
    pub fn closes_refused(&self) -> usize {
        self.closes_refused.load(Ordering::SeqCst)
    }

    /// The issues kelpie closed
    pub fn closings(&self) -> Vec<u64> {
        self.closings.lock().unwrap().clone()
    }

    /// The pull requests kelpie closed unmerged
    pub fn pull_request_closings(&self) -> Vec<u64> {
        self.pull_request_closings.lock().unwrap().clone()
    }

    /// Makes closing a pull request fail, or work again
    pub fn set_pull_request_closes_down(&self, down: bool) {
        self.pull_request_closes_down.store(down, Ordering::SeqCst);
    }

    /// Makes editing a pull request's title or body fail, or work again
    pub fn set_pull_request_edit_down(&self, down: bool) {
        self.pull_request_edit_down.store(down, Ordering::SeqCst);
    }

    /// The body pull request `number` has on the forge: what an edit left,
    /// or the body its listing gave it
    pub fn pull_request_body(&self, number: u64) -> String {
        (self.texts.lock().unwrap().get(&number).map(|t| t.1.clone()))
            .unwrap_or_else(|| format!("Body of pull request #{number}.\n"))
    }

    /// Lists open pull request `number`, titled `title` and closing `closes`,
    /// as someone other than kelpie opening it would
    pub fn list_open_pull(&self, number: u64, title: &str, closes: &[u64]) {
        self.open_pull_request(number, &format!("someone/{number}"), closes);
        self.titles.lock().unwrap().insert(number, title.to_owned());
    }

    /// The reason kelpie closed issue `number` for, once it has closed it
    pub fn closing_reason(&self, number: u64) -> Option<IssueCloseReason> {
        self.closed_as.lock().unwrap().get(&number).copied()
    }

    /// Every open pull request, each with the title a test named it and the
    /// issues its listing says it closes
    pub fn read_open_pulls(&self) -> Result<Vec<OpenPull>, ForgeError> {
        let states: Vec<(u64, PullRequestState)> = self
            .pull_requests
            .lock()
            .unwrap()
            .iter()
            .map(|(number, pr)| (*number, pr.state))
            .collect();
        let titles = self.titles.lock().unwrap();
        let texts = self.texts.lock().unwrap();
        let open = self.open.lock().unwrap();
        Ok(open
            .iter()
            .filter(|pull| {
                states
                    .iter()
                    .find(|(number, _)| *number == pull.number)
                    .is_none_or(|(_, state)| *state == PullRequestState::Open)
            })
            .map(|pull| OpenPull {
                number: pull.number,
                title: titles
                    .get(&pull.number)
                    .cloned()
                    .or_else(|| texts.get(&pull.number).map(|(title, _)| title.clone()))
                    .unwrap_or_else(|| format!("pull {}", pull.number)),
                closes: pull.closes.clone(),
            })
            .collect())
    }

    /// Closes the pull request open on `number`
    pub fn close_open_pull_request(&self, number: u64) -> Result<(), ForgeError> {
        if self.pull_request_closes_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("closing is refused".into()));
        }
        let now = self.read(number)?;
        if now.state != PullRequestState::Open {
            return Err(ForgeError::Failed(format!(
                "cannot close #{number}: it is {:?}",
                now.state
            )));
        }
        self.set_state(number, PullRequestState::Closed);
        self.pull_request_closings.lock().unwrap().push(number);
        Ok(())
    }
}
