//! Reworking an open pull request kelpie opened, from its latest review
//!
//! A pull request asks for one with the `ready-for-agent` label or a review
//! requesting changes, seen on the board's poll, and `rework <pr>` asks by
//! hand. The worker starts on the pull request's branch as `origin` holds
//! it, and its first turn is the latest review, verbatim, in a file in its
//! build folder. The work item then runs every gate again. Its issue stays
//! finished, so the board never brings it back. Kelpie labels each pull
//! request it hands back `ready-for-human`, so its labels say whose turn it is.

use std::fmt;
use std::path::{Path, PathBuf};

use super::Runner;
use super::report::{Begin, ReworkBy, StepReport};
use super::trigger::{self, issue_list};
use super::turn;
use crate::board::{LabelError, OpenPullRequest, READY, Skip};
use crate::ports::{ForgeError, MaintainerReview, PullRequestState, Reviewed};
use crate::settings::AgentName;
use crate::state::StateError;
use crate::work_item::{Known, Phase, Review, WorkItem, new_session_id};
use crate::worktree;

/// The label on a pull request kelpie handed back to the maintainer
///
/// Kelpie never creates it: the maintainer makes it in the project's repo.
pub const HUMAN: &str = "ready-for-human";

/// The file in the build folder that carries the review
const REVIEW_FILE: &str = "maintainer-review.md";

/// Why `rework` was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReworkError {
    /// No slot is free under `concurrency.active_items`, counting the items waiting for
    /// one, or one for this issue is open already: the issues of those in
    /// the way
    InFlight(Vec<u64>),
    /// The forge could not show the pull request
    PullRequest(u64, ForgeError),
    /// The pull request is merged or closed, as named
    NotOpen(u64, &'static str),
    /// The pull request is not from a `kelpie/<issue>` branch on the repo
    /// itself, opened by the account kelpie acts as
    NotKelpies(u64),
    /// The forge could not say which account kelpie acts as
    Viewer(ForgeError),
    /// The latest review has no body and no unresolved comment
    NothingToRework(u64),
    /// The forge could not show the pull request's issue
    Issue(u64, ForgeError),
    /// The issue's `agent:` label cannot be used
    Label(LabelError),
    /// No random session id could be drawn, with the OS's reason
    Session(String),
    /// The review could not be written for the worker, with the reason
    ReviewFile(String),
    /// A label could not be taken off the pull request
    Unlabel(u64, &'static str, ForgeError),
    /// The forge could not show a review bot's reviews of the pull request
    ReviewBot(&'static str, u64, ForgeError),
    /// The branch's head on `origin` could not be read, with the reason
    Head(String, String),
    /// `finish` holds the board back, so nothing new is taken on
    Finishing,
    /// The work item could not be saved
    State(StateError),
}

impl fmt::Display for ReworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InFlight(issues) if issues.len() == 1 => {
                write!(f, "the work item for {} is in flight", issue_list(issues))
            }
            Self::InFlight(issues) => {
                write!(f, "the work items for {} are in flight", issue_list(issues))
            }
            Self::PullRequest(number, e) => write!(f, "cannot read pull request #{number}: {e}"),
            Self::NotOpen(number, state) => write!(f, "pull request #{number} is {state}"),
            Self::NotKelpies(number) => {
                write!(f, "pull request #{number} is not one kelpie opened")
            }
            Self::NothingToRework(number) => write!(
                f,
                "nothing to rework: the latest review of #{number} has no body \
                 and no unresolved comment"
            ),
            Self::Viewer(e) => write!(f, "cannot read the account kelpie acts as: {e}"),
            Self::Issue(issue, e) => write!(f, "cannot read issue #{issue}: {e}"),
            Self::Label(e) => e.fmt(f),
            Self::Session(e) => write!(f, "cannot draw a session id: {e}"),
            Self::ReviewFile(e) => f.write_str(e),
            Self::Unlabel(number, label, e) => {
                write!(f, "cannot take the `{label}` label off #{number}: {e}")
            }
            Self::ReviewBot(bot, number, e) => {
                write!(f, "cannot read {bot}'s reviews of #{number}: {e}")
            }
            Self::Head(branch, e) => write!(f, "cannot read the head of `{branch}`: {e}"),
            Self::Finishing => f.write_str(super::finishing::NOTHING_NEW),
            Self::State(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for ReworkError {}

impl ReworkError {
    // Whether asking again, with nothing changed on the pull request, is
    // refused the same way
    fn settled(&self) -> bool {
        matches!(
            self,
            Self::NotOpen(..) | Self::NotKelpies(_) | Self::NothingToRework(_) | Self::Label(_)
        )
    }
}

impl Runner {
    /// Opens a work item reworking open pull request `number`, which kelpie
    /// opened, and returns the implementer its worker runs on
    ///
    /// Its first turn runs on a later pass.
    ///
    /// # Errors
    ///
    /// [`ReworkError`] naming why the pull request cannot be reworked, or
    /// the change cannot be saved. A refusal changes nothing. A label or
    /// save that fails after the triage labels began coming off leaves them off.
    pub fn rework(&mut self, number: u64) -> Result<AgentName, ReworkError> {
        if !self.picks() {
            return Err(ReworkError::Finishing);
        }
        if !self.slot_free() {
            return Err(ReworkError::InFlight(self.slot_issues()));
        }
        let pr = self
            .ports
            .forge
            .reviewed(&self.remote, number)
            .map_err(|e| ReworkError::PullRequest(number, e))?;
        self.start_rework(number, pr)
    }

    // Starts the rework one of `open` asks for, lowest number first, and
    // returns each pull request that asked but could not start this poll. A
    // refusal that asking again would not change takes the label off,
    // records the review and goes to the pull request as a comment.
    pub(super) fn rework_asked(
        &mut self,
        open: &[OpenPullRequest],
    ) -> Result<(Option<Begin>, Vec<Skip>), StateError> {
        let mut skipped = Vec::new();
        // A second ask waits for the work item in flight on its issue to end.
        let mut ours: Vec<(u64, u64)> = open
            .iter()
            .filter_map(|pr| {
                let issue = pr.head.strip_prefix("kelpie/").and_then(trigger::number)?;
                Some((pr.number, issue))
            })
            .filter(|&(_, issue)| self.state.item(issue).is_none())
            .collect();
        ours.sort_unstable();
        // One pull request that fails holds up none of the others, nor the board.
        let skip = |issue, pull_request, error: ReworkError| Skip::Rework {
            issue,
            pull_request,
            error: error.to_string(),
        };
        for (number, issue) in ours {
            let pr = match self.ports.forge.reviewed(&self.remote, number) {
                Ok(pr) => pr,
                Err(e) => {
                    skipped.push(skip(issue, number, ReworkError::PullRequest(number, e)));
                    continue;
                }
            };
            // A fork's branch, or a collaborator's, can take kelpie's name,
            // and is none of its business.
            if pr.from_fork {
                continue;
            }
            let labelled = pr.labels.iter().any(|l| l == READY);
            let asked =
                |r: &MaintainerReview| r.changes_requested && !self.state.reworked.contains(&r.id);
            let by = if labelled {
                ReworkBy::Label
            } else if pr.review.as_ref().is_some_and(asked) {
                ReworkBy::Review
            } else {
                continue;
            };
            match self.viewer() {
                Ok(me) if pr.author == me => {}
                Ok(_) => continue,
                Err(e) => {
                    skipped.push(skip(issue, number, ReworkError::Viewer(e)));
                    continue;
                }
            }
            if let Some(held) = self.pace_dispatch()?.holds() {
                return Ok((Some(held), skipped));
            }
            let review = pr.review.as_ref().map(|r| r.id.clone());
            let begin = match self.start_rework(number, pr) {
                Ok(agent) => Begin::Report(StepReport::Reworked {
                    issue,
                    pull_request: number,
                    agent,
                    by,
                }),
                Err(ReworkError::State(e)) => return Err(e),
                Err(e) if e.settled() => match self.refuse_rework(number, labelled, review, &e)? {
                    Ok(begin) => begin,
                    Err(e) => {
                        skipped.push(skip(issue, number, e));
                        continue;
                    }
                },
                Err(e) => {
                    skipped.push(skip(issue, number, e));
                    continue;
                }
            };
            return Ok((Some(begin), skipped));
        }
        Ok((None, skipped))
    }

    // The outer error is a save that failed; the inner, a label that would
    // not come off, which leaves the refusal to the next poll.
    fn refuse_rework(
        &mut self,
        number: u64,
        labelled: bool,
        review: Option<String>,
        refused: &ReworkError,
    ) -> Result<Result<Begin, ReworkError>, StateError> {
        let repo = &self.remote;
        if labelled && let Err(e) = self.ports.forge.set_label(repo, number, READY, false) {
            return Ok(Err(ReworkError::Unlabel(number, READY, e)));
        }
        if let Some(review) = review.filter(|r| !self.state.reworked.contains(r)) {
            let mut next = self.state.clone();
            next.reworked.push(review);
            self.save(next)?;
        }
        let reason = refused.to_string();
        let comment = format!("Kelpie cannot rework this pull request: {reason}.");
        let comment_failed = self
            .ports
            .forge
            .comment(&self.remote, number, &comment)
            .err()
            .map(|e| e.to_string());
        Ok(Ok(Begin::Report(StepReport::ReworkRefused {
            pull_request: number,
            reason,
            comment_failed,
        })))
    }

    // The login kelpie opens pull requests as, asked once a run
    pub(super) fn viewer(&mut self) -> Result<String, ForgeError> {
        if let Some(me) = &self.viewer {
            return Ok(me.clone());
        }
        let me = self.ports.forge.viewer()?;
        self.viewer = Some(me.clone());
        Ok(me)
    }

    // Puts `ready-for-human` on pull request `number` and takes
    // `ready-for-agent` off, as its labels on the forge stand now.
    pub(super) fn hand_back(&self, number: u64) -> Result<(), String> {
        let repo = &self.remote;
        let failed = |e: ForgeError| format!("cannot hand #{number} back: {e}");
        let labels = self
            .ports
            .forge
            .pull_request(repo, number)
            .map_err(failed)?
            .labels;
        if !labels.iter().any(|l| l == HUMAN) {
            self.ports
                .forge
                .set_label(repo, number, HUMAN, true)
                .map_err(failed)?;
        }
        if labels.iter().any(|l| l == READY) {
            self.ports
                .forge
                .set_label(repo, number, READY, false)
                .map_err(failed)?;
        }
        Ok(())
    }

    // Checks `pr` can be reworked, then writes its review for the worker,
    // takes the triage labels off and saves the work item, in that order.
    fn start_rework(&mut self, number: u64, pr: Reviewed) -> Result<AgentName, ReworkError> {
        match pr.state {
            PullRequestState::Open => {}
            PullRequestState::Merged => return Err(ReworkError::NotOpen(number, "merged")),
            PullRequestState::Closed => return Err(ReworkError::NotOpen(number, "closed")),
        }
        let me = self.viewer().map_err(ReworkError::Viewer)?;
        let repo = &self.remote;
        let issue = pr
            .branch
            .strip_prefix("kelpie/")
            .and_then(trigger::number)
            .filter(|_| !pr.from_fork && pr.author == me)
            .ok_or(ReworkError::NotKelpies(number))?;
        if self.state.item(issue).is_some() {
            return Err(ReworkError::InFlight(vec![issue]));
        }
        let review = pr
            .review
            .filter(|r| !r.body.trim().is_empty() || !r.comments.is_empty())
            .ok_or(ReworkError::NothingToRework(number))?;
        let found = self
            .ports
            .forge
            .issue(repo, issue)
            .map_err(|e| ReworkError::Issue(issue, e))?;
        let (agent, note) = self
            .labelled_agent(issue, &found.labels)
            .map_err(ReworkError::Label)?;
        let session = new_session_id().map_err(|e| ReworkError::Session(e.to_string()))?;
        let fresh = self.fresh(issue, found.title, agent.clone(), session);
        // A rework stays on its pull request, so a listed bot's `rounds`
        // counts the reviews it gave it before. As an adoption does, it
        // leaves out a review of the current head, which the bot's round
        // counts once when it finds it.
        let mut bot_reads = std::collections::BTreeMap::new();
        // The head the turn starts from, so a turn that pushes nothing is told apart.
        let head = worktree::origin_head(&self.settings.git.checkout, &pr.branch)
            .map_err(|e| ReworkError::Head(pr.branch.clone(), e.to_string()))?;
        for bot in self
            .listed_bots()
            .into_iter()
            .filter(|b| b.rounds.is_some())
        {
            let bot = self.profile(bot.bot);
            let activity = self
                .ports
                .forge
                .review_bot(repo, number, bot.login())
                .map_err(|e| ReworkError::ReviewBot(bot.bot().name(), number, e))?;
            let reads = bot.reviewed_besides(&activity, &head);
            if reads > 0 {
                bot_reads.insert(bot.bot(), reads);
            }
        }
        let text = review_text(number, &review);
        turn::write(&fresh.build, &review_path(&fresh.build), &text)
            .map_err(ReworkError::ReviewFile)?;
        let mut labels = pr.labels;
        for label in [READY, HUMAN] {
            if labels.iter().any(|l| l == label) {
                self.ports
                    .forge
                    .set_label(repo, number, label, false)
                    .map_err(|e| ReworkError::Unlabel(number, label, e))?;
                labels.retain(|l| l != label);
            }
        }
        let mut next = self.state.clone();
        if !next.reworked.contains(&review.id) {
            next.reworked.push(review.id);
        }
        next.work_items.push(WorkItem {
            branch: pr.branch,
            rework: true,
            bot_reads,
            pull_request: Some(number),
            // The fix is new code, so a pass of the review runs before CI.
            resume: Some(Phase::Review(Review::first())),
            known: Known {
                labels,
                ready: !pr.draft,
                head: Some(head),
            },
            ..fresh
        });
        self.save(next).map_err(ReworkError::State)?;
        self.notes.extend(note);
        self.mark_held(issue, true);
        Ok(agent)
    }
}

// The worker can read its build folder, and a commit never carries it.
fn review_path(build: &Path) -> PathBuf {
    build.join(REVIEW_FILE)
}

// The reviewer's words go in as written: nothing here rewords them.
pub(super) fn review_text(number: u64, review: &MaintainerReview) -> String {
    let mut text = format!("# The latest review of pull request #{number}\n");
    if !review.body.trim().is_empty() {
        text.push('\n');
        push_verbatim(&mut text, &review.body);
    }
    for comment in &review.comments {
        let at = match comment.line {
            Some(line) => format!("`{}` line {line}", comment.file),
            None => format!("`{}`", comment.file),
        };
        text.push_str(&format!("\n## On {at}\n\n"));
        push_verbatim(&mut text, &comment.body);
    }
    text
}

fn push_verbatim(text: &mut String, words: &str) {
    text.push_str(words);
    if !words.ends_with('\n') {
        text.push('\n');
    }
}

/// The first turn of a rework: the review, in the file it was written to
pub(super) fn first_prompt(item: &WorkItem) -> String {
    let number = item.pull_request.unwrap_or_default();
    format!(
        "Your work item reworks your pull request #{number} for issue #{}: {}\n\n\
         Its latest review asks for changes, and is in {}. \
         Make the changes it asks for, then commit and push with `git push origin HEAD`. \
         Your branch is the pull request's as `origin` holds it, with any commits the \
         maintainer pushed. The pull request is already open, so do not open another.\n",
        item.issue,
        item.title,
        review_path(&item.build).display()
    )
}

#[cfg(test)]
mod tests;
