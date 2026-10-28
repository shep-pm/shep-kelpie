//! Holding every call on a source while its rate limit is used up
//!
//! A runner's forge and tickets go through [`RateHeld`]. A call the source
//! answers with its rate limit used up holds every later call until the limit
//! resets, read once from the source, or for [`FALLBACK`] when the source
//! cannot say. A held call fails at once with [`ForgeError::Held`], and each
//! hold is said once, as it begins, through [`RateHeld`]'s [`Say`].

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use super::Revalidated;
use super::{
    BranchRun, Clock, CommentMarks, FailedLogs, FolderEntry, Forge, ForgeError, InstallationToken,
    LocalPace, NewLabel, OpenPull, Pace, PullRequest, QueueStanding, RateLimits, Rerun, Reviewed,
    Talk, ThreadComment, Tickets, Timestamp, Visibility, Workflow, pace,
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

/// Whether `error` is the forge saying its rate limit is used up
fn used_up(error: &ForgeError) -> bool {
    match error {
        ForgeError::Limited { .. } => true,
        ForgeError::Failed(stderr) => says_used_up(stderr),
        _ => false,
    }
}

/// Whether a failed gh call's `stderr` says a rate limit is used up
pub fn says_used_up(stderr: &str) -> bool {
    let stderr = stderr.to_lowercase();
    USED_UP.iter().any(|said| stderr.contains(said))
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
        let held = match &self.project {
            Some(project) => self.hold.admit(self.clock.as_ref(), project).err(),
            None => self.hold.until(self.clock.now()),
        };
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
        if let Err(error) = &result
            && ended == Ended::UsedUp
        {
            let until = self.held_until(error, now);
            self.hold.hold(until);
            (self.say)(format!(
                "the forge's rate limit is used up ({error}), so kelpie makes no forge call \
                 until {}",
                time_of(until)
            ));
        }
        result
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
        let held = match &self.project {
            Some(project) => self.hold.admit(self.clock.as_ref(), project).err(),
            None => self.hold.until(self.clock.now()),
        };
        if let Some(until) = held {
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
mod tests {
    use super::*;

    // The first is from the live runner's log; the rest are GitHub's
    // documented messages, as gh prints a failed call.
    #[test]
    fn each_of_ghs_used_up_answers_is_a_used_up_limit() {
        let said = [
            "GraphQL: API rate limit already exceeded for user ID 1.",
            "gh: API rate limit exceeded for user ID 1. (HTTP 403)",
            "gh: You have exceeded a secondary rate limit. Please wait a few minutes before you \
             try again. (HTTP 403)",
            "gh: Too Many Requests (HTTP 429)",
        ];
        for stderr in said {
            assert!(used_up(&ForgeError::Failed(stderr.into())), "{stderr}");
        }
    }

    #[test]
    fn any_other_failure_is_not() {
        let other = [
            ForgeError::Failed("gh: Not Found (HTTP 404)".into()),
            ForgeError::Failed("error connecting to api.github.com".into()),
            ForgeError::Unreadable("API rate limit exceeded".into()),
        ];
        for error in other {
            assert!(!used_up(&error), "{error}");
        }
    }

    fn bucket(used: u64, remaining: u64, reset: u64) -> crate::ports::RateBucket {
        crate::ports::RateBucket {
            used,
            remaining,
            reset: Timestamp(reset),
        }
    }

    fn held_over(forge: &crate::test::FakeForge) -> RateHeld {
        let source = Arc::new(forge.clone());
        RateHeld::new(
            Arc::clone(&source) as Arc<dyn Forge>,
            Arc::clone(&source) as Arc<dyn Tickets>,
            Arc::new(crate::test::FakeClock::at(1_000)),
            ForgeHold::default(),
            Box::new(|_| {}),
        )
    }

    #[test]
    fn rate_limit_reads_used_remaining_and_reset_for_core_and_graphql() {
        let home = tempfile::tempdir().unwrap();
        let forge = crate::test::FakeForge::new(home.path().join("origin"));
        let limits = RateLimits {
            core: bucket(120, 4_880, 1_800),
            graphql: bucket(7, 4_993, 1_700),
            search: bucket(3, 27, 1_600),
        };
        forge.set_rate_limits(limits);

        assert_eq!(held_over(&forge).rate_limit(), Ok(limits));
    }

    #[test]
    fn rate_limit_reset_still_answers_from_core_reset() {
        let home = tempfile::tempdir().unwrap();
        let forge = crate::test::FakeForge::new(home.path().join("origin"));
        let held = held_over(&forge);
        let spare = RateLimits {
            core: bucket(120, 4_880, 1_800),
            graphql: bucket(7, 4_993, 1_700),
            search: bucket(3, 27, 1_600),
        };
        forge.set_rate_limits(spare);
        assert_eq!(held.rate_limit_reset(), Ok(None), "none is used up");

        forge.set_rate_limits(RateLimits {
            core: bucket(5_000, 0, 1_800),
            ..spare
        });
        assert_eq!(held.rate_limit_reset(), Ok(Some(Timestamp(1_800))));

        forge.set_rate_limits(RateLimits {
            graphql: bucket(5_000, 0, 1_900),
            ..spare
        });
        assert_eq!(held.rate_limit_reset(), Ok(Some(Timestamp(1_900))));

        forge.set_rate_limits(RateLimits {
            search: bucket(30, 0, 1_650),
            ..spare
        });
        assert_eq!(
            held.rate_limit_reset(),
            Ok(Some(Timestamp(1_650))),
            "`gh search` spends from the search limit, so its reset holds too"
        );
    }

    mod cooldown {
        use super::*;
        use crate::ports::Cooldown;
        use crate::test::{FakeClock, FakeForge};

        fn repo() -> ForgeSlug {
            ForgeSlug::try_from("shep-pm/koji".to_owned()).unwrap()
        }

        fn held_on(forge: &FakeForge, clock: &FakeClock) -> RateHeld {
            let source = Arc::new(forge.clone());
            RateHeld::new(
                Arc::clone(&source) as Arc<dyn Forge>,
                Arc::clone(&source) as Arc<dyn Tickets>,
                Arc::new(clock.clone()),
                ForgeHold::default(),
                Box::new(|_| {}),
            )
        }

        #[test]
        fn a_retry_after_holds_for_that_long_and_not_for_the_reset_or_the_fallback() {
            let home = tempfile::tempdir().unwrap();
            let forge = FakeForge::new(home.path().join("origin"));
            let clock = FakeClock::at(1_000);
            let held = held_on(&forge, &clock);
            // The reset would hold for an hour, the fallback for ten minutes.
            forge.set_used_up(Some(Timestamp(4_600)));
            forge.set_limited(Cooldown {
                retry_after: Some(90),
                reset: None,
            });

            held.visibility(&repo()).unwrap_err();

            clock.advance(89);
            let held_for = held.viewer().unwrap_err();
            assert_eq!(held_for, ForgeError::Held(Timestamp(1_090)));
            forge.set_unlimited();
            forge.set_down(false);
            clock.advance(1);
            held.viewer().unwrap();
        }

        #[test]
        fn a_reset_in_the_answer_holds_until_then_without_asking_the_forge_for_it() {
            let home = tempfile::tempdir().unwrap();
            let forge = FakeForge::new(home.path().join("origin"));
            let clock = FakeClock::at(1_000);
            let held = held_on(&forge, &clock);
            forge.set_limited(Cooldown {
                retry_after: None,
                reset: Some(Timestamp(1_500)),
            });

            held.visibility(&repo()).unwrap_err();

            assert_eq!(forge.asked(), 1, "the reset was not asked for");
            assert_eq!(
                held.viewer().unwrap_err(),
                ForgeError::Held(Timestamp(1_500))
            );
        }

        #[test]
        fn an_answer_that_says_nothing_of_when_holds_for_the_fallback() {
            let home = tempfile::tempdir().unwrap();
            let forge = FakeForge::new(home.path().join("origin"));
            let clock = FakeClock::at(1_000);
            let held = held_on(&forge, &clock);
            forge.set_limited(Cooldown::default());

            held.visibility(&repo()).unwrap_err();

            assert_eq!(
                held.viewer().unwrap_err(),
                ForgeError::Held(Timestamp(1_000 + FALLBACK))
            );
        }
    }

    mod branch_runs {
        use super::*;
        use crate::test::{FakeClock, FakeForge};

        fn repo() -> ForgeSlug {
            ForgeSlug::try_from("shep-pm/koji".to_owned()).unwrap()
        }

        fn run(workflow: &str, id: u64, conclusion: &str) -> BranchRun {
            BranchRun {
                workflow: workflow.to_owned(),
                id,
                head: "abc".to_owned(),
                event: "push".to_owned(),
                conclusion: conclusion.to_owned(),
                created: Timestamp(1_000),
            }
        }

        fn held_on(forge: &FakeForge, clock: &FakeClock) -> RateHeld {
            let source = Arc::new(forge.clone());
            RateHeld::new(
                Arc::clone(&source) as Arc<dyn Forge>,
                Arc::clone(&source) as Arc<dyn Tickets>,
                Arc::new(clock.clone()),
                ForgeHold::default(),
                Box::new(|_| {}),
            )
        }

        #[test]
        fn each_branch_lists_the_runs_a_test_scripted_for_it_and_a_run_has_its_log_tail() {
            let home = tempfile::tempdir().unwrap();
            let forge = FakeForge::new(home.path().join("origin"));
            let held = held_on(&forge, &FakeClock::at(1_000));
            forge.set_branch_runs("kelpie/1", vec![run("test", 7, "failure")]);
            forge.set_branch_runs("kelpie/2", vec![run("test", 8, "cancelled")]);
            forge.set_run_log_tail(7, "error[E0308]");

            assert_eq!(
                held.branch_runs(&repo(), "kelpie/1").unwrap(),
                [run("test", 7, "failure")]
            );
            assert_eq!(
                held.branch_runs(&repo(), "kelpie/2").unwrap(),
                [run("test", 8, "cancelled")]
            );
            assert_eq!(held.branch_runs(&repo(), "kelpie/3").unwrap(), []);
            assert_eq!(held.run_log_tail(&repo(), 7).unwrap(), "error[E0308]");
            held.run_log_tail(&repo(), 8).unwrap_err();
        }

        #[test]
        fn a_branchs_runs_fail_with_the_error_a_test_scripted() {
            let home = tempfile::tempdir().unwrap();
            let forge = FakeForge::new(home.path().join("origin"));
            let held = held_on(&forge, &FakeClock::at(1_000));
            forge.set_branch_runs_error("gh: Not Found (HTTP 404)");

            assert_eq!(
                held.branch_runs(&repo(), "kelpie/1").unwrap_err(),
                ForgeError::Failed("gh: Not Found (HTTP 404)".into())
            );
        }

        #[test]
        fn listing_a_branchs_runs_is_held_while_the_rate_limit_is_used_up() {
            let home = tempfile::tempdir().unwrap();
            let forge = FakeForge::new(home.path().join("origin"));
            let held = held_on(&forge, &FakeClock::at(1_000));
            forge.set_limited(crate::ports::Cooldown::default());
            held.visibility(&repo()).unwrap_err();

            assert_eq!(
                held.branch_runs(&repo(), "kelpie/1").unwrap_err(),
                ForgeError::Held(Timestamp(1_000 + FALLBACK))
            );
            assert_eq!(
                held.run_log_tail(&repo(), 7).unwrap_err(),
                ForgeError::Held(Timestamp(1_000 + FALLBACK))
            );
        }
    }

    mod ledger {
        use super::*;
        use crate::forge_ledger::{self, Api, Line};
        use crate::test::{FakeClock, FakeForge};

        fn repo() -> ForgeSlug {
            ForgeSlug::try_from("shep-pm/koji".to_owned()).unwrap()
        }

        struct Rig {
            held: RateHeld,
            forge: FakeForge,
            clock: FakeClock,
            home: tempfile::TempDir,
        }

        impl Rig {
            fn new() -> Self {
                let home = tempfile::tempdir().unwrap();
                let forge = FakeForge::new(home.path().join("origin"));
                let clock = FakeClock::at(1_000);
                let source = Arc::new(forge.clone());
                let held = RateHeld::new(
                    Arc::clone(&source) as Arc<dyn Forge>,
                    Arc::clone(&source) as Arc<dyn Tickets>,
                    Arc::new(clock.clone()),
                    ForgeHold::default(),
                    Box::new(|_| {}),
                )
                .with_ledger(Arc::new(Ledger::in_home(home.path(), "koji")));
                Self {
                    held,
                    forge,
                    clock,
                    home,
                }
            }

            fn lines(&self) -> Vec<Line> {
                forge_ledger::read(&self.home.path().join(forge_ledger::FILE)).unwrap()
            }
        }

        #[test]
        fn each_call_adds_a_line_with_its_kind_and_how_it_ended() {
            let rig = Rig::new();
            rig.held.visibility(&repo()).unwrap();
            rig.clock.advance(30);
            rig.held.default_branch(&repo()).unwrap();
            rig.forge.set_down(true);
            rig.clock.advance(30);
            rig.held.repo_labels(&repo()).unwrap_err();

            let lines = rig.lines();
            let seen: Vec<_> = lines
                .iter()
                .map(|l| (l.at.0, l.project.as_str(), l.kind, l.api, l.ended))
                .collect();
            assert_eq!(
                seen,
                [
                    (1_000, "koji", Kind::Visibility, Api::GraphQl, Ended::Ok),
                    (1_030, "koji", Kind::DefaultBranch, Api::GraphQl, Ended::Ok),
                    (1_060, "koji", Kind::RepoLabels, Api::GraphQl, Ended::Failed),
                ]
            );
        }

        #[test]
        fn the_reads_that_came_back_304_are_put_on_the_line_of_the_call_they_were_made_in() {
            let revalidated = Revalidated::default();
            let rig = Rig::new();
            let held = rig.held.counting(revalidated.clone());

            held.viewer().unwrap();
            revalidated.note();
            revalidated.note();
            held.visibility(&repo()).unwrap();
            held.default_branch(&repo()).unwrap();

            let lines = forge_ledger::read(&rig.home.path().join(forge_ledger::FILE)).unwrap();
            let counts: Vec<_> = lines.iter().map(|l| l.not_modified).collect();
            assert_eq!(counts, [0, 2, 0]);
        }

        #[test]
        fn a_used_up_answer_is_recorded_and_the_calls_it_holds_are_recorded_as_held() {
            let rig = Rig::new();
            rig.forge.set_used_up(Some(Timestamp(1_600)));
            rig.held.visibility(&repo()).unwrap_err();
            rig.clock.advance(60);
            rig.held.viewer().unwrap_err();
            rig.forge.set_down(false);
            rig.clock.advance(600);
            rig.held.viewer().unwrap();

            let lines = rig.lines();
            let seen: Vec<_> = lines.iter().map(|l| (l.at.0, l.kind, l.ended)).collect();
            assert_eq!(
                seen,
                [
                    (1_000, Kind::Visibility, Ended::UsedUp),
                    (1_000, Kind::RateLimitReset, Ended::Ok),
                    (1_060, Kind::Viewer, Ended::Held),
                    (1_660, Kind::Viewer, Ended::Ok),
                ]
            );
            assert_eq!(lines[1].api, Api::Rest);
            assert_eq!(lines[1].millis, 0);
        }
    }
}
