//! The open pull requests whose commits a head carries
//!
//! A worker may merge another open pull request's branch into its own, and that pull request's
//! unresolved findings then reach `main` with no one asked. The merge ruling names them, under
//! `auto` too.

use super::super::Runner;
use super::super::report::{Begin, StepReport};
use super::super::ruling::park;
use crate::board::OpenPullRequest;
use crate::ports::{Finding, Severity};
use crate::state::{RulingKind, StateError};
use crate::work_item::WorkItem;

impl Runner {
    /// The notice for the pull requests in `open` whose head commit `head` carries and the base
    /// lacks, or none where it carries none, or the step's end: `Begin::Reads`, or the gate
    /// failing as git could not say
    ///
    /// A pull request of the work item's own, of a fork, or without a commit is not looked at: a
    /// fork's commits are not on `origin`.
    pub(super) fn borrowed(
        &self,
        open: &[OpenPullRequest],
        head: &str,
    ) -> Result<Option<String>, Box<Begin>> {
        let Some(item) = self.current() else {
            return Ok(None);
        };
        let others = |pr: &&OpenPullRequest| {
            Some(pr.number) != item.pull_request
                && pr.head != item.branch
                && !pr.from_fork
                && !pr.commit.is_empty()
        };
        let tips: Vec<String> = open
            .iter()
            .filter(others)
            .map(|pr| pr.commit.clone())
            .collect();
        if tips.is_empty() {
            return Ok(None);
        }
        let carried = self.tips_carried(head, tips)?;
        let lent: Vec<_> = open
            .iter()
            .filter(|pr| carried.contains(&pr.commit))
            .collect();
        if lent.is_empty() {
            return Ok(None);
        }
        let named: Vec<String> = lent.iter().map(|pr| self.lent_line(pr)).collect();
        Ok(Some(format!(
            "This head carries commits of other open pull requests, so a yes merges those into \
             main too. {}",
            named.join(" ")
        )))
    }

    // What kelpie knows of the findings on `pr`: its work item's `held`, which are the ones
    // kelpie sent its worker to fix. The state has no flag for a finding still open.
    fn lent_line(&self, pr: &OpenPullRequest) -> String {
        let kind = if pr.draft {
            "a draft"
        } else {
            "ready for review"
        };
        let named = format!("#{} ({kind})", pr.number);
        let kept = (self.state.work_items.iter()).find(|item| item.pull_request == Some(pr.number));
        match kept {
            None => format!(
                "{named}: kelpie holds no record of it. Read its review on the pull request."
            ),
            Some(item) if item.held.is_empty() => format!(
                "{named}: kelpie sent its worker no finding to fix. Read its review on the \
                 pull request."
            ),
            Some(WorkItem { held, .. }) => {
                let places: Vec<String> = held.iter().map(finding_place).collect();
                format!(
                    "{named}: kelpie sent its worker to fix {}. The review on the pull request \
                     may hold more.",
                    places.join(", ")
                )
            }
        }
    }

    /// Saves the merge ruling `kind` with `notice` after its question, parks the worker on it and
    /// posts it on pull request `number`, as `raise` does
    pub(super) fn raise_noticed(
        &mut self,
        number: u64,
        kind: RulingKind,
        notice: &str,
    ) -> Result<Begin, StateError> {
        let mut next = self.state.clone();
        let issue = self.current().expect("a ruling is about a work item").issue;
        let (id, mut question) = park(self.names(), &mut next, issue, Some(number), kind);
        question.push('\n');
        question.push_str(notice);
        if let Some(ruling) = next.rulings.iter_mut().rev().find(|r| r.id == id) {
            ruling.question.clone_from(&question);
        }
        self.save(next)?;
        self.word_effects(id)?;
        let comment_failed = self.post_ruling(Some(number), id);
        Ok(Begin::Report(StepReport::Ruling {
            issue,
            pull_request: number,
            id,
            question,
            comment_failed,
        }))
    }
}

// `HIGH src/lib.rs:12`, as the review names a finding
fn finding_place(finding: &Finding) -> String {
    let severity = match finding.severity {
        Severity::High => "HIGH",
        Severity::Medium => "MEDIUM",
        Severity::Low => "LOW",
    };
    match finding.line {
        0 => format!("{severity} {}", finding.file),
        line => format!("{severity} {}:{line}", finding.file),
    }
}
