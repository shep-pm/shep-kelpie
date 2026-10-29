//! Taking a released contribution over
//!
//! A release makes the pull request kelpie's (ADR 0010). [`follow_adoption`]
//! queues it as a work item on its branch, for a release or a `rework` answer.
//! Where the pull request closes no issue, kelpie files one from its title and
//! body and adds `Resolves #<issue>` to the body, so the merge closes it.
//! The record keeps the filed issue, so a failed edit files no second one.
//! A fork kelpie cannot push to gets a ruling ([`awaits_edits`], [`ask_adopt`]).
//! A refused filing or edit is retried at the next poll; the pull request is
//! queued only once both went through.

use super::super::Runner;
use super::ruling::{raise_cannot_adopt, raise_no_edits};
use crate::ports::Reviewed;
use crate::state::{Adopting, Held, How, Ruling, RulingKind, StateError, Waiting};
use crate::ticket::TicketId;

impl Runner {
    // Whether the release stage stops for the pull request this poll: the
    // contribution is on a fork kelpie cannot push to, and its ruling is
    // raised now, or the forge could not say, so nothing is concluded. The
    // next poll looks again.
    pub(super) fn awaits_edits(&mut self, number: u64) -> Result<bool, StateError> {
        let Ok(pr) = self.ports.forge.reviewed(&self.remote, number) else {
            return Ok(true);
        };
        if pr.pushable() {
            return Ok(false);
        }
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        raise_no_edits(&mut next, number, now);
        self.save(next)?;
        Ok(true)
    }

    // Queues each released contribution for adoption, filing the issue for one
    // whose pull request closes none. The work item itself starts on the
    // board's step, which takes adopted pull requests before its own issues.
    pub(super) fn follow_adoption(&mut self) -> Result<(), StateError> {
        let numbers: Vec<u64> = (self.state.contributions.iter())
            .filter(|c| c.released.is_some())
            .map(|c| c.pull_request)
            .collect();
        for number in numbers {
            self.queue_adoption(number)?;
        }
        Ok(())
    }

    // Queues released pull request `number` for adoption, once it has the issue
    // its work item hangs on. One already queued, or whose work item is open,
    // is left as it is.
    fn queue_adoption(&mut self, number: u64) -> Result<(), StateError> {
        let Some(held) = (self.state.contributions.iter())
            .find(|c| c.pull_request == number)
            .cloned()
        else {
            return Ok(());
        };
        let queued = (self.state.adopted.iter()).any(|w| w.pull_request == number);
        let open = (self.state.work_items.iter()).any(|i| i.pull_request == Some(number));
        if queued || open {
            return Ok(());
        }
        let pr = match self.ports.forge.reviewed(&self.remote, number) {
            Ok(pr) => pr,
            Err(e) => {
                self.adopt_told(number, "read the pull request", &e);
                return Ok(());
            }
        };
        // A fork kelpie cannot push to is the maintainer's to decide on, whether
        // it was quiet or said yes: the ruling asks, and nothing is posted on
        // the pull request.
        if !pr.pushable() {
            if self.ruling_waits(number) {
                return Ok(());
            }
            let now = self.ports.clock.now();
            let mut next = self.state.clone();
            raise_no_edits(&mut next, number, now);
            self.save(next)?;
            return Ok(());
        }
        // A pull request that merges into another branch is no work item
        // kelpie can start: the board would refuse it every poll and post the
        // refusal on the contributor's pull request. Nothing is filed for it
        // while that stands.
        let base = self.settings.git.base();
        if pr.base != base {
            return self.ask_adopt(
                number,
                format!("it merges into `{}`, not `{base}`", pr.base),
            );
        }
        // The issue its work item hangs on: the one an earlier poll filed, or the
        // lowest the pull request closes. The filed issue is the one the body names,
        // so the merge closes it.
        let mut filed = held.adopting.map(|a| (a.issue, a.resolves));
        let issue = match (
            filed.map(|(issue, _)| issue),
            pr.closes.iter().min().copied(),
        ) {
            (Some(issue), _) => issue,
            (None, Some(issue)) => issue,
            (None, None) => {
                let how = held.released.as_ref().map(|r| r.how);
                match self.file_issue(number, &pr, how)? {
                    Some(issue) => {
                        filed = Some((issue, false));
                        issue
                    }
                    None => return Ok(()),
                }
            }
        };
        // Only the issue kelpie filed needs naming on the pull request: one it
        // already closes is named there.
        let to_link = filed.is_some_and(|(_, resolves)| !resolves);
        if to_link && !self.link_issue(number, &pr, issue)? {
            return Ok(());
        }
        // An issue labelled for an implementer the project may not use gives
        // the work item no agent, and the board would refuse it every poll.
        if let Some(why) = self.issue_label_refuses(issue) {
            return self.ask_adopt(number, why);
        }
        // The reason was read again and is gone: retargeting the pull request or
        // fixing the label takes the adoption up, and the ruling goes with it.
        let mut next = self.state.clone();
        next.rulings.retain(|r| !cannot_adopt_on(r, number));
        next.adopted.push(Waiting {
            pull_request: number,
            by_label: false,
        });
        self.save(next)
    }

    // Asks the maintainer what to do with a released contribution kelpie cannot
    // adopt. While its ruling waits nothing is queued, nothing is raised again,
    // and nothing is posted on the contributor's pull request.
    fn ask_adopt(&mut self, number: u64, why: String) -> Result<(), StateError> {
        if self.ruling_waits(number) {
            return Ok(());
        }
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        raise_cannot_adopt(&mut next, number, why, now);
        self.save(next)
    }

    // Why issue `issue` gives a work item no agent to run on, where it does: an
    // `agent:` label naming no implementer the project may use, or more than
    // one. A forge that cannot show the issue is no answer, and the board tries
    // the adoption and skips it, as it does now.
    fn issue_label_refuses(&self, issue: u64) -> Option<String> {
        let found = self
            .ports
            .tickets
            .issue(&self.remote, TicketId::from_issue(issue)?)
            .ok()?;
        self.label_refuses(&found.labels).map(|why| why.to_string())
    }

    // Files the issue `number`'s work item hangs on, from the pull request's
    // title and body, with no `agent:` label, so the agent board picks its
    // implementer. It is saved as filed before the body is touched, so a body
    // the forge will not let kelpie edit costs no second issue.
    fn file_issue(
        &mut self,
        number: u64,
        pr: &Reviewed,
        how: Option<How>,
    ) -> Result<Option<u64>, StateError> {
        let issue = match self.open_issue(&pr.title, &filed_body(number, pr, how), &[]) {
            Ok(issue) => issue,
            Err(e) => {
                self.adopt_told(number, "file its issue", &e);
                return Ok(None);
            }
        };
        self.change(number, |held| {
            held.adopting = Some(Adopting {
                issue,
                resolves: false,
            });
        })?;
        Ok(Some(issue))
    }

    // Adds `Resolves #<issue>` to the pull request's body, so the merge closes
    // the issue kelpie filed. Whether the forge took the edit, and the next
    // poll's retry when it did not.
    fn link_issue(&mut self, number: u64, pr: &Reviewed, issue: u64) -> Result<bool, StateError> {
        let body = format!("{}\n\nResolves #{issue}", pr.body.trim_end());
        match self
            .ports
            .forge
            .edit_pull_request(&self.remote, number, &pr.title, &body)
        {
            Ok(()) => {
                self.change(number, |held| {
                    if let Some(adopting) = held.adopting.as_mut() {
                        adopting.resolves = true;
                    }
                })?;
                Ok(true)
            }
            Err(e) => {
                let what = format!("add `Resolves #{issue}` to its body");
                self.adopt_told(number, &what, &e);
                Ok(false)
            }
        }
    }

    // Says that the forge would not do what adopting this contribution needs,
    // and that the next poll tries again.
    fn adopt_told(&self, number: u64, what: &str, why: &dyn std::fmt::Display) {
        self.notes.push(format!(
            "contribution #{number}: cannot {what}: {why}; it is tried at the next poll"
        ));
    }
}

// Whether `ruling` is the one that asks about a contribution kelpie cannot
// adopt
fn cannot_adopt_on(ruling: &Ruling, number: u64) -> bool {
    ruling.id == TicketId::from_pull_request(number)
        && matches!(
            ruling.kind,
            RulingKind::Contribution(Held::CannotAdopt { .. })
        )
}

// What the filed issue says: where it came from and who handed the pull
// request over, then its body as its contributor wrote it. A release by the
// maintainer's ruling says so, and does not claim the contributor released it.
fn filed_body(number: u64, pr: &Reviewed, how: Option<How>) -> String {
    let taken = match how {
        Some(How::Yes | How::ThumbsUp | How::Silence) => {
            "which its contributor released and kelpie took over"
        }
        Some(How::Maintainer | How::Rework) | None => {
            "which the maintainer's ruling had kelpie take over"
        }
    };
    format!(
        "Kelpie filed this issue from pull request #{number}, {taken}.\n\n{}\n",
        pr.body.trim_end()
    )
}
