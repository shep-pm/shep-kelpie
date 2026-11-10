//! Holding every call on a source while its rate limit is used up
//!
//! A runner's forge and tickets go through [`RateHeld`]. A call the source
//! answers with its rate limit used up holds every later call until the limit
//! resets, read once from the source, or for [`FALLBACK`] when the source
//! cannot say. A held call fails at once with [`ForgeError::Held`], and each
//! hold is said once, as it begins, through [`RateHeld`]'s [`Say`].

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use super::Revalidated;
use super::{
    BranchRun, Clock, CommentMarks, FailedLogs, FolderEntry, Forge, ForgeError, HeadRun,
    InstallationToken, LocalPace, LogPart, NewLabel, OpenPull, Pace, PullRequest, QueueStanding,
    RateLimits, Rerun, Reviewed, Talk, ThreadComment, Tickets, Timestamp, Visibility, Workflow,
    pace,
};
use crate::board::OpenPullRequest;
use crate::forge_ledger::{Ended, Identity, Kind, Ledger};
use crate::review_bot::{Activity, Login};
use crate::settings::ForgeSlug;

mod app_reads;
pub use app_reads::{AppForges, AppReads, Reading};
use app_reads::{AppReader, Read};

mod tickets;

/// How long a hold lasts when the forge cannot say when its limit resets
pub const FALLBACK: u64 = 10 * 60;

/// What gh says when a limit is used up, lowercased: the REST and GraphQL
/// primary limits, the secondary limit, and an HTTP 429 answer
const USED_UP: [&str; 4] = [
    "api rate limit exceeded",
    "api rate limit already exceeded",
    "secondary rate limit",
    "http 429",
];

/// Whose limit a used-up answer is about
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// A login's or the App's own, which holds every call on the machine
    Login,
    /// The machine's IP, answered to a call that went out with no login: it
    /// holds the login's calls and no one else's
    Anonymous,
}

// What gh prints when a call went out with no login and was refused
const ANONYMOUS: &str = "authenticated requests get a higher rate limit";
const NO_LOGIN: &str = "requires authentication";

/// Whether `error` is the forge saying its rate limit is used up
fn used_up(error: &ForgeError) -> bool {
    limit_of(error).is_some()
}

fn limit_of(error: &ForgeError) -> Option<Limit> {
    match error {
        ForgeError::Limited { said, .. } => Some(says_used_up(said).unwrap_or(Limit::Login)),
        ForgeError::Failed(stderr) => says_used_up(stderr),
        _ => None,
    }
}

// Whether the login's call went out with no login: the anonymous limit, or a
// 401 that asks for authentication.
fn no_login(error: &ForgeError) -> bool {
    match error {
        ForgeError::Limited { said: text, .. } | ForgeError::Failed(text) => {
            let text = text.to_lowercase();
            text.contains(ANONYMOUS) || (text.contains("http 401") && text.contains(NO_LOGIN))
        }
        _ => false,
    }
}

/// Which limit a failed gh call's `stderr` says is used up, if it says so
pub fn says_used_up(stderr: &str) -> Option<Limit> {
    let stderr = stderr.to_lowercase();
    if !USED_UP.iter().any(|said| stderr.contains(said)) {
        return None;
    }
    Some(if stderr.contains(ANONYMOUS) {
        Limit::Anonymous
    } else {
        Limit::Login
    })
}

/// `at` as a UTC time, as `2026-10-10T05:31:54Z`
pub fn time_of(at: Timestamp) -> String {
    i64::try_from(at.0)
        .ok()
        .and_then(|s| jiff::Timestamp::from_second(s).ok())
        .map_or_else(|| at.0.to_string(), |at| at.to_string())
}

/// Until when the forge is held, shared by [`RateHeld`] and its runner, and
/// with every runner on the login when its [`Pace`] is the dog's
#[derive(Debug, Clone)]
pub struct ForgeHold(Arc<dyn Pace>);

impl Default for ForgeHold {
    /// A hold kept in this process alone
    fn default() -> Self {
        Self(Arc::new(LocalPace::new()))
    }
}

impl ForgeHold {
    /// The hold and pacer of `pace`
    pub fn shared(pace: Arc<dyn Pace>) -> Self {
        Self(pace)
    }

    /// Until when every forge call is held, while that is after `now`
    pub fn until(&self, now: Timestamp) -> Option<Timestamp> {
        self.0.held(now)
    }

    /// Holds the login until `until`
    pub fn hold(&self, until: Timestamp) {
        self.0.hold(until);
    }

    /// Waits on `clock` until a call of `project`'s may go, or says until
    /// when the forge is held
    pub(super) fn admit(&self, clock: &dyn Clock, project: &str) -> Result<(), Timestamp> {
        pace::admit(self.0.as_ref(), clock, project)
    }
}

/// Where [`RateHeld`] says a hold began, once for each
pub type Say = Box<dyn Fn(String) + Send + Sync>;

/// Where [`RateHeld`] tells a warning that stands while its cause does
pub trait Warnings: Send + Sync {
    /// Tells `line`, unless the warning under `key`, which names its cause,
    /// stands already
    fn stands(&self, key: &str, line: String);

    /// The cause under `key` is gone, so the warning is told again if it returns
    fn ends(&self, key: &str);
}

/// The two ports of a source, whose calls are held while its rate limit is
/// used up
pub struct RateHeld {
    forge: Arc<dyn Forge>,
    tickets: Arc<dyn Tickets>,
    clock: Arc<dyn Clock>,
    hold: ForgeHold,
    say: Say,
    // Where a warning that stands is told, instead of `say`
    standing: Option<Arc<dyn Warnings>>,
    ledger: Option<Arc<Ledger>>,
    // The project whose calls these are, when they are spaced by the hold's pace
    project: Option<String>,
    // Who reads a repo, where the App can
    app: Option<AppReader>,
    // The reads the adapters sent a validator with and GitHub answered 304
    revalidated: Revalidated,
    // Until when the login's calls are held for going out with no login, in
    // this process alone: another runner's `gh` may be logged in. Zero is none.
    no_login_until: AtomicU64,
}

impl RateHeld {
    /// `forge` and `tickets`, the two ports of one source, holding its calls in
    /// `hold` by `clock`'s time, and telling `say` as each hold begins
    pub fn new(
        forge: Arc<dyn Forge>,
        tickets: Arc<dyn Tickets>,
        clock: Arc<dyn Clock>,
        hold: ForgeHold,
        say: Say,
    ) -> Self {
        Self {
            forge,
            tickets,
            clock,
            hold,
            say,
            standing: None,
            ledger: None,
            project: None,
            app: None,
            revalidated: Revalidated::default(),
            no_login_until: AtomicU64::new(0),
        }
    }

    /// Spaces each call as the hold's pace does for `project`: a call over
    /// the pace waits its turn on the clock
    #[must_use]
    pub fn paced_for(mut self, project: &str) -> Self {
        self.project = Some(project.to_owned());
        self
    }

    /// Tells a warning that stands while its cause does to `standing`, with
    /// its key, in place of `say`
    #[must_use]
    pub fn with_standing(mut self, standing: Arc<dyn Warnings>) -> Self {
        self.standing = Some(standing);
        self
    }

    /// Records each call in `ledger`, held ones included
    #[must_use]
    pub fn with_ledger(mut self, ledger: Arc<Ledger>) -> Self {
        self.ledger = Some(ledger);
        self
    }

    /// Puts each call's reads that came back 304 in the ledger, as `revalidated`
    /// counts them
    #[must_use]
    pub fn counting(mut self, revalidated: Revalidated) -> Self {
        self.revalidated = revalidated;
        self
    }

    fn call<T>(
        &self,
        kind: Kind,
        call: impl FnOnce(&dyn Forge) -> Result<T, ForgeError>,
    ) -> Result<T, ForgeError> {
        self.spent(kind, || call(self.forge.as_ref()))
    }

    // The same hold, pace and ledger around a call on the ticket port.
    fn call_tickets<T>(
        &self,
        kind: Kind,
        call: impl FnOnce(&dyn Tickets) -> Result<T, ForgeError>,
    ) -> Result<T, ForgeError> {
        self.spent(kind, || call(self.tickets.as_ref()))
    }

    fn spent<T>(
        &self,
        kind: Kind,
        call: impl FnOnce() -> Result<T, ForgeError>,
    ) -> Result<T, ForgeError> {
        let held = self.held();
        let now = self.clock.now();
        if let Some(until) = held {
            self.record(now, kind, 0, Ended::Held);
            return Err(ForgeError::Held(until));
        }
        let started = Instant::now();
        let result = call();
        let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let ended = match &result {
            Ok(_) => Ended::Ok,
            Err(error) if used_up(error) => Ended::UsedUp,
            Err(_) => Ended::Failed,
        };
        self.record(self.clock.now(), kind, millis, ended);
        if let Err(error) = &result {
            if no_login(error) {
                self.hold_no_login(error, now);
            } else if ended == Ended::UsedUp {
                let until = self.held_until(error, now);
                self.hold.hold(until);
                (self.say)(format!(
                    "the forge's rate limit is used up ({error}), so kelpie makes no forge call \
                     until {}",
                    time_of(until)
                ));
            }
        }
        result
    }

    // Until when the login's calls are held, while that is after now: for
    // going out with no login, else as the shared hold says.
    fn held(&self) -> Option<Timestamp> {
        let until = Timestamp(self.no_login_until.load(Ordering::SeqCst));
        if until > self.clock.now() {
            return Some(until);
        }
        match &self.project {
            Some(project) => self.hold.admit(self.clock.as_ref(), project).err(),
            None => self.hold.until(self.clock.now()),
        }
    }

    // The login went out with no login. Only its own calls are held, in this
    // process, until the anonymous limit resets where that is what answered.
    fn hold_no_login(&self, error: &ForgeError, now: Timestamp) {
        let until = match limit_of(error) {
            Some(_) => self.held_until(error, now),
            None => fallback_from(now),
        };
        self.no_login_until.store(until.0, Ordering::SeqCst);
        (self.say)(format!(
            "gh answered without a login, so its calls are held until {}, and the App's calls go on",
            time_of(until)
        ));
    }

    // What the answer itself says of when to ask again comes first, then the
    // forge's reset, then the fallback.
    fn held_until(&self, error: &ForgeError, now: Timestamp) -> Timestamp {
        said_until(error, now)
            .or_else(|| match self.reset() {
                Ok(Some(reset)) if reset > now => Some(reset),
                _ => None,
            })
            .unwrap_or(fallback_from(now))
    }

    // The reset is a call to the forge too, so it takes its turn in the pace
    // and is recorded like the rest. A hold set meanwhile answers for it.
    fn reset(&self) -> Result<Option<Timestamp>, ForgeError> {
        if let Some(project) = &self.project
            && self.hold.admit(self.clock.as_ref(), project).is_err()
        {
            return Ok(None);
        }
        self.free_read(Kind::RateLimitReset, |f| f.rate_limit_reset())
    }

    fn free_read<T>(
        &self,
        kind: Kind,
        read: impl FnOnce(&dyn Forge) -> Result<T, ForgeError>,
    ) -> Result<T, ForgeError> {
        let started = Instant::now();
        let result = read(self.forge.as_ref());
        let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let ended = if result.is_ok() {
            Ended::Ok
        } else {
            Ended::Failed
        };
        self.record(self.clock.now(), kind, millis, ended);
        result
    }

    fn record(&self, at: Timestamp, kind: Kind, millis: u64, ended: Ended) {
        self.record_by(Identity::Login, at, kind, millis, ended);
    }

    fn record_by(&self, by: Identity, at: Timestamp, kind: Kind, millis: u64, ended: Ended) {
        if let Some(ledger) = &self.ledger {
            let not_modified = self.revalidated.take();
            ledger.record_with(by, at, kind, millis, ended, not_modified);
        }
    }
}

// What the answer itself says of when to ask again
fn said_until(error: &ForgeError, now: Timestamp) -> Option<Timestamp> {
    match error {
        ForgeError::Limited { cooldown, .. } => cooldown.until(now),
        _ => None,
    }
}

fn fallback_from(now: Timestamp) -> Timestamp {
    Timestamp(now.0.saturating_add(FALLBACK))
}

impl fmt::Debug for RateHeld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RateHeld")
            .field("hold", &self.hold)
            .finish_non_exhaustive()
    }
}

impl Forge for RateHeld {
    fn visibility(&self, repo: &ForgeSlug) -> Result<Visibility, ForgeError> {
        self.call(Kind::Visibility, |f| f.visibility(repo))
    }

    fn default_branch(&self, repo: &ForgeSlug) -> Result<String, ForgeError> {
        self.call(Kind::DefaultBranch, |f| f.default_branch(repo))
    }

    fn requires_up_to_date(&self, repo: &ForgeSlug, base: &str) -> Result<bool, ForgeError> {
        self.call(Kind::RequiresUpToDate, |f| {
            f.requires_up_to_date(repo, base)
        })
    }

    fn branch_exists(&self, repo: &ForgeSlug, branch: &str) -> Result<bool, ForgeError> {
        self.call(Kind::BranchExists, |f| f.branch_exists(repo, branch))
    }

    fn folder(&self, repo: &ForgeSlug, path: &str) -> Result<Option<Vec<FolderEntry>>, ForgeError> {
        self.call(Kind::Folder, |f| f.folder(repo, path))
    }

    fn file_text(&self, repo: &ForgeSlug, path: &str) -> Result<String, ForgeError> {
        self.call(Kind::FileText, |f| f.file_text(repo, path))
    }

    fn repo_labels(&self, repo: &ForgeSlug) -> Result<Vec<String>, ForgeError> {
        self.call(Kind::RepoLabels, |f| f.repo_labels(repo))
    }

    fn create_label(&self, repo: &ForgeSlug, label: &NewLabel) -> Result<(), ForgeError> {
        self.call(Kind::CreateLabel, |f| f.create_label(repo, label))
    }

    fn rename_label(&self, repo: &ForgeSlug, from: &str, to: &str) -> Result<(), ForgeError> {
        self.call(Kind::CreateLabel, |f| f.rename_label(repo, from, to))
    }

    fn can_push(&self, repo: &ForgeSlug) -> Result<bool, ForgeError> {
        self.call(Kind::CanPush, |f| f.can_push(repo))
    }

    fn last_bot_review(
        &self,
        repo: &ForgeSlug,
        login: Login<'_>,
    ) -> Result<Option<Timestamp>, ForgeError> {
        self.call(Kind::ReviewBotSeen, |f| f.last_bot_review(repo, login))
    }

    fn open_pull_requests(&self, repo: &ForgeSlug) -> Result<Vec<OpenPullRequest>, ForgeError> {
        self.read(
            repo,
            Read::OpenPullRequests,
            |a| a.open_pull_requests(repo),
            |f| f.open_pull_requests(repo),
        )
    }

    fn pull_request(&self, repo: &ForgeSlug, number: u64) -> Result<PullRequest, ForgeError> {
        self.read(
            repo,
            Read::PullRequest,
            |a| a.pull_request(repo, number),
            |f| f.pull_request(repo, number),
        )
    }

    fn reviewed(&self, repo: &ForgeSlug, number: u64) -> Result<Reviewed, ForgeError> {
        self.read(
            repo,
            Read::Reviewed,
            |a| a.reviewed(repo, number),
            |f| f.reviewed(repo, number),
        )
    }

    fn talk(&self, repo: &ForgeSlug, number: u64) -> Result<Talk, ForgeError> {
        self.call(Kind::Talk, |f| f.talk(repo, number))
    }

    fn viewer(&self) -> Result<String, ForgeError> {
        self.call(Kind::Viewer, |f| f.viewer())
    }

    // `gh auth token` makes no API call, so no hold or budget applies.
    fn login_token(&self) -> Result<InstallationToken, ForgeError> {
        self.forge.login_token()
    }

    fn account_created(&self, login: &str) -> Result<Timestamp, ForgeError> {
        self.call(Kind::AccountCreated, |f| f.account_created(login))
    }

    fn comments_since(
        &self,
        repo: &ForgeSlug,
        since: Timestamp,
    ) -> Result<Vec<ThreadComment>, ForgeError> {
        self.call(Kind::CommentsSince, |f| f.comments_since(repo, since))
    }

    fn thread_replies_since(
        &self,
        repo: &ForgeSlug,
        number: u64,
        since: Timestamp,
    ) -> Result<Vec<ThreadComment>, ForgeError> {
        self.call(Kind::ThreadReplies, |f| {
            f.thread_replies_since(repo, number, since)
        })
    }

    fn can_write(&self, repo: &ForgeSlug, login: &str) -> Result<bool, ForgeError> {
        self.call(Kind::CanWrite, |f| f.can_write(repo, login))
    }

    fn comment_marks(&self, repo: &ForgeSlug, comment: u64) -> Result<CommentMarks, ForgeError> {
        self.call(Kind::CommentMarks, |f| f.comment_marks(repo, comment))
    }

    fn comment(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<u64, ForgeError> {
        self.call(Kind::Comment, |f| f.comment(repo, number, body))
    }

    fn open_pulls(&self, repo: &ForgeSlug) -> Result<Vec<OpenPull>, ForgeError> {
        self.call(Kind::OpenPulls, |f| f.open_pulls(repo))
    }

    fn close_pull_request(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        self.call(Kind::ClosePullRequest, |f| {
            f.close_pull_request(repo, number)
        })
    }

    fn create_pull_request(
        &self,
        repo: &ForgeSlug,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<u64, ForgeError> {
        self.call(Kind::CreatePullRequest, |f| {
            f.create_pull_request(repo, head, base, title, body)
        })
    }

    fn edit_pull_request(
        &self,
        repo: &ForgeSlug,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.call(Kind::EditPullRequest, |f| {
            f.edit_pull_request(repo, number, title, body)
        })
    }

    fn failed_run_logs(
        &self,
        repo: &ForgeSlug,
        head: &str,
        merge_group: bool,
    ) -> Result<FailedLogs, ForgeError> {
        self.call(Kind::FailedRunLogs, |f| {
            f.failed_run_logs(repo, head, merge_group)
        })
    }

    fn branch_runs(&self, repo: &ForgeSlug, branch: &str) -> Result<Vec<BranchRun>, ForgeError> {
        self.call(Kind::BranchRuns, |f| f.branch_runs(repo, branch))
    }

    fn workflows(&self, repo: &ForgeSlug) -> Result<Vec<Workflow>, ForgeError> {
        self.call(Kind::Workflows, |f| f.workflows(repo))
    }

    fn run_log_tail(&self, repo: &ForgeSlug, run: u64) -> Result<String, ForgeError> {
        self.call(Kind::RunLogTail, |f| f.run_log_tail(repo, run))
    }

    fn head_runs(&self, repo: &ForgeSlug, head: &str) -> Result<Vec<HeadRun>, ForgeError> {
        self.call(Kind::HeadRuns, |f| f.head_runs(repo, head))
    }

    fn run_log(&self, repo: &ForgeSlug, run: u64, part: LogPart) -> Result<String, ForgeError> {
        self.call(Kind::RunLog, |f| f.run_log(repo, run, part))
    }

    fn rerun_failed(&self, repo: &ForgeSlug, head: &str) -> Result<(), ForgeError> {
        self.call(Kind::RerunFailed, |f| f.rerun_failed(repo, head))
    }

    fn rerun_run(&self, repo: &ForgeSlug, run: u64, rerun: Rerun) -> Result<(), ForgeError> {
        self.call(Kind::RerunRun, |f| f.rerun_run(repo, run, rerun))
    }

    fn approve_workflow_runs(&self, repo: &ForgeSlug, head: &str) -> Result<(), ForgeError> {
        self.call(Kind::ApproveWorkflowRuns, |f| {
            f.approve_workflow_runs(repo, head)
        })
    }

    fn mark_ready(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        self.call(Kind::MarkReady, |f| f.mark_ready(repo, number))
    }

    fn set_label(
        &self,
        repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        self.call(Kind::SetLabel, |f| f.set_label(repo, number, label, on))
    }

    fn review_bot(
        &self,
        repo: &ForgeSlug,
        number: u64,
        login: Login<'_>,
    ) -> Result<Activity, ForgeError> {
        self.read(
            repo,
            Read::ReviewBot,
            |a| a.review_bot(repo, number, login),
            |f| f.review_bot(repo, number, login),
        )
    }

    fn resolve_thread(&self, repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError> {
        self.call(Kind::ResolveThread, |f| f.resolve_thread(repo, thread))
    }

    fn reply_to_thread(
        &self,
        repo: &ForgeSlug,
        thread: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.call(Kind::ReplyToThread, |f| {
            f.reply_to_thread(repo, thread, body)
        })
    }

    fn merge(&self, repo: &ForgeSlug, number: u64, head: &str) -> Result<(), ForgeError> {
        self.call(Kind::Merge, |f| f.merge(repo, number, head))
    }

    fn merge_queue(&self, repo: &ForgeSlug, number: u64) -> Result<QueueStanding, ForgeError> {
        self.call(Kind::MergeQueue, |f| f.merge_queue(repo, number))
    }

    fn disable_auto_merge(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        self.call(Kind::DisableAutoMerge, |f| {
            f.disable_auto_merge(repo, number)
        })
    }

    // The read costs none of the limits, but it is still a call to the forge:
    // it takes its turn in the pace, and fails unrecorded while the forge is held.
    fn rate_limit(&self) -> Result<RateLimits, ForgeError> {
        if let Some(until) = self.held() {
            return Err(ForgeError::Held(until));
        }
        self.free_read(Kind::RateLimit, |f| f.rate_limit())
    }

    // Free of the limits, and asked only once a call finds them used up.
    fn rate_limit_reset(&self) -> Result<Option<Timestamp>, ForgeError> {
        self.reset()
    }
}

#[cfg(test)]
mod shared_tests;

#[cfg(test)]
mod tests;
