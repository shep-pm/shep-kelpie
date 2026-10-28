//! Keeping this machine's paths off the forge
//!
//! Everything kelpie posts to the forge goes through [`Guarded`], which
//! refuses a text [`LocalPaths`] finds something of this machine's in: its
//! folders or an address on a local network. A post to a public repo that
//! names a repo of the flock which GitHub marks private is refused too.
//!
//! The project's `publish` hooks judge the same text after that. A pull
//! request's title and body are the worker's text, handed over for kelpie to
//! post; every other text is kelpie's own.

use std::fmt;
use std::sync::Arc;

use super::{
    BranchRun, CommentMarks, FailedLogs, FolderEntry, Forge, ForgeError, InstallationToken,
    LineComment, NewLabel, OpenPull, PostedComment, PullRequest, QueueStanding, RateLimits, Rerun,
    Reviewed, Talk, ThreadComment, Tickets, Timestamp, Visibility, Voice, Workflow,
};
use crate::board::OpenPullRequest;
use crate::hooks::{Hooks, Role, SharedHooks};
use crate::local_paths::{Leak, LocalPaths, Surface, names_repo};
use crate::review_bot::{Activity, Login};
use crate::settings::ForgeSlug;

mod tickets;

/// `body` with each of `comments` added after it as a line naming its place,
/// for a review that GitHub refused with comments and is posted without them
pub fn with_comments_in_body(body: &str, comments: &[LineComment]) -> String {
    let mut text = body.to_owned();
    for c in comments {
        text.push_str(&format!("\n\n`{}:{}`: {}", c.path, c.line, c.body));
    }
    text
}

// The repos of the flock, and the forge to ask who can see one
struct Flock {
    repos: Vec<ForgeSlug>,
    forge: Arc<dyn Forge>,
}

impl Flock {
    fn new(forge: Arc<dyn Forge>) -> Self {
        Self {
            repos: Vec::new(),
            forge,
        }
    }

    // Whether `text`, bound for `destination`, names a flock repo GitHub marks
    // private while `destination` is public. The forge is asked only when the
    // text names a repo, and a question it cannot answer refuses the post.
    fn names_private(&self, destination: &ForgeSlug, text: &str) -> Result<bool, ForgeError> {
        let named: Vec<&ForgeSlug> = (self.repos.iter())
            .filter(|repo| !repo.as_str().eq_ignore_ascii_case(destination.as_str()))
            .filter(|repo| {
                // A repo sharing the destination's bare name is the destination to
                // the reader, so only its full slug names it.
                let by_bare_name = !repo.name().eq_ignore_ascii_case(destination.name());
                names_repo(text, repo, by_bare_name)
            })
            .collect();
        if named.is_empty() || self.forge.visibility(destination)? != Visibility::Public {
            return Ok(false);
        }
        for repo in named {
            if self.forge.visibility(repo)? != Visibility::Public {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

// `what` names the field, so a refusal says which one to fix.
fn check(
    (local, flock, hooks): (&LocalPaths, Option<&Flock>, &Hooks),
    role: Role,
    repo: &ForgeSlug,
    what: &'static str,
    text: &str,
) -> Result<(), ForgeError> {
    if let Some(leak) = local.find(text, Surface::Prose) {
        return Err(ForgeError::LocalPath { what, leak });
    }
    if let Some(flock) = flock
        && flock.names_private(repo, text)?
    {
        let leak = Leak::Private;
        return Err(ForgeError::LocalPath { what, leak });
    }
    (hooks.publish(role, what, text)).map_err(|refusal| ForgeError::Hook { what, refusal })
}

/// The two ports of a forge, refusing to post any text naming something of
/// this machine's
pub struct Guarded {
    forge: Arc<dyn Forge>,
    tickets: Arc<dyn Tickets>,
    local: LocalPaths,
    flock: Flock,
    hooks: SharedHooks,
}

impl Guarded {
    /// `forge` and `tickets`, the two ports of one source, refusing posts that
    /// `local` finds something in
    pub fn new(forge: Arc<dyn Forge>, tickets: Arc<dyn Tickets>, local: LocalPaths) -> Self {
        Self {
            flock: Flock::new(Arc::clone(&forge)),
            forge,
            tickets,
            local,
            hooks: SharedHooks::default(),
        }
    }

    /// This forge, also refusing posts that one of `hooks`' publish hooks refuses,
    /// as they stand when the post is checked
    #[must_use]
    pub fn with_hooks(self, hooks: impl Into<SharedHooks>) -> Self {
        let hooks = hooks.into();
        Self { hooks, ..self }
    }

    /// This forge, also refusing a post to a public repo that names one of `repos`
    /// which GitHub marks private
    #[must_use]
    pub fn with_flock(mut self, repos: Vec<ForgeSlug>) -> Self {
        self.flock.repos = repos;
        self
    }

    fn check(&self, repo: &ForgeSlug, what: &'static str, text: &str) -> Result<(), ForgeError> {
        self.check_as(Role::Kelpie, repo, what, text)
    }

    fn check_as(
        &self,
        role: Role,
        repo: &ForgeSlug,
        what: &'static str,
        text: &str,
    ) -> Result<(), ForgeError> {
        let checks = (&self.local, Some(&self.flock), &*self.hooks.get());
        check(checks, role, repo, what, text)
    }
}

impl fmt::Debug for Guarded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Guarded").finish_non_exhaustive()
    }
}

impl Forge for Guarded {
    fn visibility(&self, repo: &ForgeSlug) -> Result<Visibility, ForgeError> {
        self.forge.visibility(repo)
    }

    fn default_branch(&self, repo: &ForgeSlug) -> Result<String, ForgeError> {
        self.forge.default_branch(repo)
    }

    fn requires_up_to_date(&self, repo: &ForgeSlug, base: &str) -> Result<bool, ForgeError> {
        self.forge.requires_up_to_date(repo, base)
    }

    fn branch_exists(&self, repo: &ForgeSlug, branch: &str) -> Result<bool, ForgeError> {
        self.forge.branch_exists(repo, branch)
    }

    fn folder(&self, repo: &ForgeSlug, path: &str) -> Result<Option<Vec<FolderEntry>>, ForgeError> {
        self.forge.folder(repo, path)
    }

    fn file_text(&self, repo: &ForgeSlug, path: &str) -> Result<String, ForgeError> {
        self.forge.file_text(repo, path)
    }

    fn repo_labels(&self, repo: &ForgeSlug) -> Result<Vec<String>, ForgeError> {
        self.forge.repo_labels(repo)
    }

    // A label's text is kelpie's own constants, which name no folder.
    fn create_label(&self, repo: &ForgeSlug, label: &NewLabel) -> Result<(), ForgeError> {
        self.forge.create_label(repo, label)
    }

    fn can_push(&self, repo: &ForgeSlug) -> Result<bool, ForgeError> {
        self.forge.can_push(repo)
    }

    fn last_bot_review(
        &self,
        repo: &ForgeSlug,
        login: Login<'_>,
    ) -> Result<Option<Timestamp>, ForgeError> {
        self.forge.last_bot_review(repo, login)
    }

    fn open_pull_requests(&self, repo: &ForgeSlug) -> Result<Vec<OpenPullRequest>, ForgeError> {
        self.forge.open_pull_requests(repo)
    }

    fn pull_request(&self, repo: &ForgeSlug, number: u64) -> Result<PullRequest, ForgeError> {
        self.forge.pull_request(repo, number)
    }

    fn reviewed(&self, repo: &ForgeSlug, number: u64) -> Result<Reviewed, ForgeError> {
        self.forge.reviewed(repo, number)
    }

    fn talk(&self, repo: &ForgeSlug, number: u64) -> Result<Talk, ForgeError> {
        self.forge.talk(repo, number)
    }

    fn viewer(&self) -> Result<String, ForgeError> {
        self.forge.viewer()
    }

    fn login_token(&self) -> Result<InstallationToken, ForgeError> {
        self.forge.login_token()
    }

    // A login is one a listing gave, and the read asks for no path of the repo.
    fn account_created(&self, login: &str) -> Result<Timestamp, ForgeError> {
        self.forge.account_created(login)
    }

    fn comments_since(
        &self,
        repo: &ForgeSlug,
        since: Timestamp,
    ) -> Result<Vec<ThreadComment>, ForgeError> {
        self.forge.comments_since(repo, since)
    }

    fn thread_replies_since(
        &self,
        repo: &ForgeSlug,
        number: u64,
        since: Timestamp,
    ) -> Result<Vec<ThreadComment>, ForgeError> {
        self.forge.thread_replies_since(repo, number, since)
    }

    fn can_write(&self, repo: &ForgeSlug, login: &str) -> Result<bool, ForgeError> {
        self.forge.can_write(repo, login)
    }

    fn comment_marks(&self, repo: &ForgeSlug, comment: u64) -> Result<CommentMarks, ForgeError> {
        self.forge.comment_marks(repo, comment)
    }

    fn comment(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<u64, ForgeError> {
        self.check(repo, "the comment", body)?;
        self.forge.comment(repo, number, body)
    }

    fn open_pulls(&self, repo: &ForgeSlug) -> Result<Vec<OpenPull>, ForgeError> {
        self.forge.open_pulls(repo)
    }

    fn close_pull_request(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        self.forge.close_pull_request(repo, number)
    }

    fn create_pull_request(
        &self,
        repo: &ForgeSlug,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<u64, ForgeError> {
        self.check_as(Role::Worker, repo, "the pull request's title", title)?;
        self.check_as(Role::Worker, repo, "the pull request's body", body)?;
        self.forge
            .create_pull_request(repo, head, base, title, body)
    }

    fn edit_pull_request(
        &self,
        repo: &ForgeSlug,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.check_as(Role::Worker, repo, "the pull request's title", title)?;
        self.check_as(Role::Worker, repo, "the pull request's body", body)?;
        self.forge.edit_pull_request(repo, number, title, body)
    }

    fn failed_run_logs(
        &self,
        repo: &ForgeSlug,
        head: &str,
        merge_group: bool,
    ) -> Result<FailedLogs, ForgeError> {
        self.forge.failed_run_logs(repo, head, merge_group)
    }

    fn branch_runs(&self, repo: &ForgeSlug, branch: &str) -> Result<Vec<BranchRun>, ForgeError> {
        self.forge.branch_runs(repo, branch)
    }

    fn workflows(&self, repo: &ForgeSlug) -> Result<Vec<Workflow>, ForgeError> {
        self.forge.workflows(repo)
    }

    fn run_log_tail(&self, repo: &ForgeSlug, run: u64) -> Result<String, ForgeError> {
        self.forge.run_log_tail(repo, run)
    }

    fn rerun_failed(&self, repo: &ForgeSlug, head: &str) -> Result<(), ForgeError> {
        self.forge.rerun_failed(repo, head)
    }

    fn rerun_run(&self, repo: &ForgeSlug, run: u64, rerun: Rerun) -> Result<(), ForgeError> {
        self.forge.rerun_run(repo, run, rerun)
    }

    fn approve_workflow_runs(&self, repo: &ForgeSlug, head: &str) -> Result<(), ForgeError> {
        self.forge.approve_workflow_runs(repo, head)
    }

    fn mark_ready(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        self.forge.mark_ready(repo, number)
    }

    fn set_label(
        &self,
        repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        self.forge.set_label(repo, number, label, on)
    }

    fn review_bot(
        &self,
        repo: &ForgeSlug,
        number: u64,
        login: Login<'_>,
    ) -> Result<Activity, ForgeError> {
        self.forge.review_bot(repo, number, login)
    }

    fn resolve_thread(&self, repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError> {
        self.forge.resolve_thread(repo, thread)
    }

    fn reply_to_thread(
        &self,
        repo: &ForgeSlug,
        thread: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.check(repo, "the reply", body)?;
        self.forge.reply_to_thread(repo, thread, body)
    }

    fn merge(&self, repo: &ForgeSlug, number: u64, head: &str) -> Result<(), ForgeError> {
        self.forge.merge(repo, number, head)
    }

    fn merge_queue(&self, repo: &ForgeSlug, number: u64) -> Result<QueueStanding, ForgeError> {
        self.forge.merge_queue(repo, number)
    }

    fn disable_auto_merge(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        self.forge.disable_auto_merge(repo, number)
    }

    fn rate_limit(&self) -> Result<RateLimits, ForgeError> {
        self.forge.rate_limit()
    }

    fn rate_limit_reset(&self) -> Result<Option<Timestamp>, ForgeError> {
        self.forge.rate_limit_reset()
    }
}

/// A voice that refuses to write any text naming something of this machine's
pub struct GuardedVoice {
    voice: Arc<dyn Voice>,
    local: LocalPaths,
    flock: Option<Flock>,
    hooks: SharedHooks,
}

impl GuardedVoice {
    /// `voice`, refusing writes that `local` finds something in
    pub fn new(voice: Arc<dyn Voice>, local: LocalPaths) -> Self {
        Self {
            voice,
            local,
            flock: None,
            hooks: SharedHooks::default(),
        }
    }

    /// This voice, also refusing writes that one of `hooks`' publish hooks refuses,
    /// as they stand when the write is checked
    #[must_use]
    pub fn with_hooks(self, hooks: impl Into<SharedHooks>) -> Self {
        let hooks = hooks.into();
        Self { hooks, ..self }
    }

    /// This voice, also refusing a write to a public repo that names one of `repos`
    /// which GitHub marks private, asking `forge` who can see each
    #[must_use]
    pub fn with_flock(self, repos: Vec<ForgeSlug>, forge: Arc<dyn Forge>) -> Self {
        let mut flock = Flock::new(forge);
        flock.repos = repos;
        Self {
            flock: Some(flock),
            ..self
        }
    }

    // Everything the App writes is kelpie's own text.
    fn check(&self, repo: &ForgeSlug, what: &'static str, text: &str) -> Result<(), ForgeError> {
        let checks = (&self.local, self.flock.as_ref(), &*self.hooks.get());
        check(checks, Role::Kelpie, repo, what, text)
    }
}

impl fmt::Debug for GuardedVoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuardedVoice").finish_non_exhaustive()
    }
}

impl Voice for GuardedVoice {
    fn covers(&self, repo: &ForgeSlug) -> bool {
        self.voice.covers(repo)
    }

    fn worker_token(&self, repo: &ForgeSlug) -> Result<Option<InstallationToken>, ForgeError> {
        self.voice.worker_token(repo)
    }

    fn comment(&self, repo: &ForgeSlug, thread: u64, body: &str) -> Result<(), ForgeError> {
        self.check(repo, "the comment", body)?;
        self.voice.comment(repo, thread, body)
    }

    fn post(&self, repo: &ForgeSlug, thread: u64, body: &str) -> Result<PostedComment, ForgeError> {
        self.check(repo, "the comment", body)?;
        self.voice.post(repo, thread, body)
    }

    fn review(
        &self,
        repo: &ForgeSlug,
        number: u64,
        head: &str,
        body: &str,
        comments: &[LineComment],
    ) -> Result<u64, ForgeError> {
        self.check(repo, "the review", body)?;
        for comment in comments {
            self.check(repo, "a line comment of the review", &comment.body)?;
        }
        // A forge that refuses the line comments gets the review once more
        // with them in its body, which is checked as it is sent.
        if !comments.is_empty() {
            let body = with_comments_in_body(body, comments);
            self.check(repo, "the review with its comments in its body", &body)?;
        }
        self.voice.review(repo, number, head, body, comments)
    }

    fn edit_comment(&self, repo: &ForgeSlug, comment: u64, body: &str) -> Result<(), ForgeError> {
        self.check(repo, "the comment", body)?;
        self.voice.edit_comment(repo, comment, body)
    }

    fn thumbs_up(&self, repo: &ForgeSlug, comment: u64) -> Result<(), ForgeError> {
        self.voice.thumbs_up(repo, comment)
    }

    fn create_issue(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<u64, ForgeError> {
        self.check(repo, "the issue's title", title)?;
        self.check(repo, "the issue's body", body)?;
        self.voice.create_issue(repo, title, body, labels)
    }

    fn edit_issue_body(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<(), ForgeError> {
        self.check(repo, "the issue's body", body)?;
        self.voice.edit_issue_body(repo, number, body)
    }

    fn resolve_thread(&self, repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError> {
        self.voice.resolve_thread(repo, thread)
    }

    fn reply_to_thread(
        &self,
        repo: &ForgeSlug,
        thread: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.check(repo, "the reply", body)?;
        self.voice.reply_to_thread(repo, thread, body)
    }

    // A label's text is kelpie's own constants, which name no folder.
    fn create_label(&self, repo: &ForgeSlug, label: &NewLabel) -> Result<(), ForgeError> {
        self.voice.create_label(repo, label)
    }

    fn set_issue_label(
        &self,
        repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        self.voice.set_issue_label(repo, number, label, on)
    }
}

#[cfg(test)]
mod tests;
