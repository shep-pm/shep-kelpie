//! A saved split, checked and filed as sub-issues of the issue it splits
//!
//! The plan lives in `TriageState::splits`, so a split cut short resumes after
//! a restart and files nothing twice. Every draft is checked first and a
//! failing one files none. Then each draft goes, in order: its labels made, the
//! ticket filed and saved, linked as a sub-issue, blocked by what it waits on,
//! and `ready-for-agent` last. The parent never gets `ready-for-agent`.
//!
//! The plan comes from the triager's `split`: [`Runner::judged_split`] checks
//! the parent's body and every draft, and a refusal files nothing.

use std::collections::BTreeSet;

use super::super::Runner;
use super::super::contributions::post::forge_is_busy;
use super::comment::{FALLBACK_PRIORITY, NEEDS_TRIAGE};
use super::ready::{READY_LABEL, Ready, ReadyLabels};
use super::research::{Drafted, Found};
use super::rewrite::{new_kind_label, priority_label, refusals};
use crate::board::check;
use crate::board::{AGENT_LABEL, PIN, READY, TRIAGE, agent_label, is_priority_label, priority};
use crate::local_paths::Surface;
use crate::ports::{ForgeError, NewLabel, OpenIssue};
use crate::state::{SplitDraft, StateError, TriageSplit, TriageState, TriageStep};
use crate::ticket::TicketId;

// The marker that ends a sub-issue's body: the parent and the draft's index.
// A body filed whose answer was lost is found by it and not filed again.
pub(super) fn marker(parent: u64, index: usize) -> String {
    format!("<!-- kelpie:split {parent} {index} -->")
}

// The parent a body's last line names as its split's, if it does
fn marked_parent(body: &str) -> Option<u64> {
    let last = body.trim_end().lines().last()?;
    let named = last.strip_prefix("<!-- kelpie:split ")?;
    let (parent, rest) = named.split_once(' ')?;
    let index = rest.strip_suffix(" -->")?;
    index.parse::<usize>().ok()?;
    parent.parse().ok()
}

// How a ticket is named in a `Blocked by:` line
fn named(ticket: TicketId) -> String {
    match ticket.github() {
        Some(number) => format!("#{number}"),
        None => ticket.to_string(),
    }
}

// The label as kelpie makes it on a repo that lacks it
fn new_label(name: &str) -> NewLabel<'_> {
    if name == READY {
        return READY_LABEL;
    }
    if name == TRIAGE {
        return NEEDS_TRIAGE;
    }
    if is_priority_label(name)
        && let Some(label) = name.strip_prefix("priority: ").and_then(priority_label)
    {
        return label;
    }
    if check::kind_of(name).is_some() {
        return new_kind_label(name);
    }
    // `AgentLabel::of` makes `agent:<name>` alone, and a draft is pinned
    if name.starts_with(AGENT_LABEL) && name.ends_with(PIN) {
        return NewLabel {
            name,
            color: "c5def5",
            description: "Pins a sub-issue to one of kelpie's agents",
        };
    }
    NewLabel {
        name,
        color: "ededed",
        description: "Put on a sub-issue by kelpie's split",
    }
}

// The labels draft `part` is filed with: its kind, which a conventional title
// already gives, its priority, which falls back on the one the issue carries
// (`seen`), then the triager's for it, and the implementer it is pinned to
fn draft_labels(found: &Found, seen: &[String], part: &Drafted) -> Vec<String> {
    let kind = part.kind.or_else(|| match check::title_kind(&part.title) {
        Some(_) => None,
        None => found.work,
    });
    let ranked = (part.priority)
        .or_else(|| priority(seen))
        .or(found.priority)
        .unwrap_or(FALLBACK_PRIORITY);
    let mut labels: Vec<String> = kind
        .map(|k| format!("{}{k}", check::KIND))
        .into_iter()
        .collect();
    labels.push(format!("priority: {ranked}"));
    labels.extend(
        part.agent
            .as_ref()
            .map(|a| format!("{AGENT_LABEL}{a}{PIN}")),
    );
    labels
}

impl Runner {
    // The drafts of a reply that splits `issue`, checked with its parent's body,
    // or the lines that say why none is filed. Only the maintainer's own issue
    // is split, and one already split is not split again.
    pub(super) fn judged_split(&self, issue: u64, found: &Found) -> Ready {
        if self.split_of(issue).is_some() {
            return Ready::Not;
        }
        let mut failures = Vec::new();
        let body = found.body.clone().unwrap_or_default();
        match self.local.find(&body, Surface::Prose) {
            _ if body.is_empty() => failures.push("the issue's own body is missing".to_owned()),
            Some(leak) => failures.push(format!("the issue's own body names {leak}")),
            None => failures.extend(
                refusals(&self.title_now(issue), &body)
                    .into_iter()
                    .map(|why| format!("the issue's own body: {why}")),
            ),
        }
        let seen = self.triage_seen(issue);
        let drafts: Vec<SplitDraft> = (found.split.iter())
            .map(|part| SplitDraft {
                title: part.title.clone(),
                body: part.body.clone(),
                labels: draft_labels(found, &seen, part),
                after: part.after.clone(),
            })
            .collect();
        failures.extend(self.split_refusals(&drafts));
        match failures.is_empty() {
            true => Ready::Split { body, drafts },
            false => Ready::SplitRefused(failures),
        }
    }

    /// What stops `drafts` from being filed, one line for each draft that fails:
    /// the issue check, an `agent:` label that names no implementer of this
    /// project, and text that names a thing of this machine
    ///
    /// A draft is blocked only by an earlier draft, so an `after` that names
    /// itself or a later one is refused too.
    pub(super) fn split_refusals(&self, drafts: &[SplitDraft]) -> Vec<String> {
        let implementers = self.implementer_files();
        let mut lines = Vec::new();
        for (n, draft) in drafts.iter().enumerate() {
            let mut why: Vec<String> = (check::check(&draft.title, &draft.body, &draft.labels))
                .iter()
                .map(ToString::to_string)
                .collect();
            if let Err(e) = agent_label(&draft.labels, &implementers) {
                why.push(e.to_string());
            }
            let leak = (self.local.find(&draft.title, Surface::Prose))
                .or_else(|| self.local.find(&draft.body, Surface::Prose));
            why.extend(leak.map(|leak| format!("it names {leak}")));
            if let Some(late) = draft.after.iter().find(|&&after| after >= n) {
                why.push(format!(
                    "it is blocked by draft {}, which does not come before it",
                    late + 1
                ));
            }
            if !why.is_empty() {
                let title = draft.title.trim();
                lines.push(format!("draft {} (`{title}`): {}", n + 1, why.join("; ")));
            }
        }
        lines
    }
}

/// What the representative is told of a split kelpie files none of
pub(super) fn refused_facts(failures: &[String]) -> String {
    let listed: Vec<String> = failures.iter().map(|line| format!("- {line}")).collect();
    format!(
        "\nKelpie judged the issue too big for one work item and drafted a split, and files \
         none of it, since the issue check refuses what follows. Say so, and what is refused:\n{}\n",
        listed.join("\n")
    )
}

/// Saves `drafts` as the split of `issue`, in place of any plan it had
pub(super) fn plan(triage: &mut TriageState, issue: u64, drafts: Vec<SplitDraft>) {
    if drafts.is_empty() {
        return;
    }
    triage.splits.retain(|s| s.issue != issue);
    triage.splits.push(TriageSplit {
        issue,
        drafts,
        filed: Vec::new(),
        asked: None,
    });
}

// The body kelpie files for draft `index`: the slice line, one `Blocked by:`
// line naming the drafts it waits on and the issues its own text names, the
// draft's text, and the marker
fn child_body(parent: u64, index: usize, draft: &SplitDraft, filed: &[TicketId]) -> String {
    let mut blockers: Vec<TicketId> = draft
        .after
        .iter()
        .filter_map(|&a| filed.get(a))
        .copied()
        .collect();
    for own in check::blockers(&draft.body) {
        if !blockers.contains(&own) {
            blockers.push(own);
        }
    }
    let text: Vec<&str> = (draft.body.lines())
        .filter(|line| !line.trim().starts_with(check::BLOCKED_BY))
        .collect();
    let mut body = format!("Slice {} of #{parent}.\n", index + 1);
    if !blockers.is_empty() {
        let names: Vec<String> = blockers.into_iter().map(named).collect();
        body.push_str(&format!("{} {}\n", check::BLOCKED_BY, names.join(", ")));
    }
    body.push('\n');
    body.push_str(text.join("\n").trim());
    body.push_str(&format!("\n\n{}\n", marker(parent, index)));
    body
}

// What the notes say of splits that triage going off ended, as a local ticket
// source does: one line for each that was not fully filed
pub(in crate::runner) fn stopped(triage: Option<&TriageState>) -> Vec<String> {
    let splits = triage.map_or(&[][..], |t| &t.splits);
    (splits.iter())
        .filter(|s| s.filed.len() < s.drafts.len())
        .map(|s| {
            format!(
                "triage #{}: its split stopped with {} of {} sub-issues filed, since triage is \
                 off for this project now, as a local ticket source needs: the rest is not filed",
                s.issue,
                s.filed.len(),
                s.drafts.len()
            )
        })
        .collect()
}

// What one attempt at a split has read and done, so it asks the forge for each
// once
#[derive(Default)]
struct Attempt {
    // The open issues, read when the first draft is to be filed
    listing: Option<Vec<OpenIssue>>,
    // The labels it has made or found made
    made: BTreeSet<String>,
}

impl Runner {
    // Takes back the plan saved for `issue` when its pass ends with no body
    // written, since nothing the comment promised came to be: its drafts are
    // never filed, and a later reply may split the issue again. A plan with a
    // draft filed, or held, is part done and stays.
    pub(super) fn unplan(&mut self, issue: u64) -> Result<(), StateError> {
        let owed = self
            .split_of(issue)
            .is_some_and(|s| s.filed.is_empty() && s.asked.is_none());
        if !owed {
            return Ok(());
        }
        let mut next = self.state.clone();
        if let Some(triage) = next.triage.as_mut() {
            triage.splits.retain(|s| s.issue != issue);
        }
        self.save(next)
    }

    // The split saved for `issue`, if one is
    pub(super) fn split_of(&self, issue: u64) -> Option<&TriageSplit> {
        self.state
            .triage
            .as_ref()?
            .splits
            .iter()
            .find(|s| s.issue == issue)
    }

    // Whether `issue` is a sub-issue of a split whose parent's triage has not
    // ended: its links are not all in, so the check leaves it be
    pub(super) fn split_child_pending(&self, issue: u64, body: &str) -> bool {
        let Some(triage) = self.state.triage.as_ref() else {
            return false;
        };
        let unfinished = |parent: u64| {
            (self.triage_record(parent))
                .is_some_and(|t| !matches!(t.step, TriageStep::Done | TriageStep::Ready))
        };
        let filed = (triage.splits.iter())
            .any(|s| unfinished(s.issue) && s.filed.iter().any(|t| t.github() == Some(issue)));
        filed
            || marked_parent(body)
                .is_some_and(|parent| self.split_of(parent).is_some() && unfinished(parent))
    }

    // The issues a split is saved for
    fn split_issues(&self) -> impl Iterator<Item = u64> + '_ {
        (self.state.triage.iter()).flat_map(|t| t.splits.iter().map(|s| s.issue))
    }

    // Drops the splits whose issue is no longer in `open`, and ends the pass
    // each left pending, which would otherwise label the issue ready
    pub(super) fn settle_splits(&mut self, open: &[OpenIssue]) -> Result<(), StateError> {
        let listed = |issue: u64| open.iter().any(|i| i.number == issue);
        let Some(triage) = self.state.triage.as_ref() else {
            return Ok(());
        };
        if triage.splits.iter().all(|s| listed(s.issue)) {
            return Ok(());
        }
        let mut next = self.state.clone();
        if let Some(triage) = next.triage.as_mut() {
            triage.splits.retain(|s| listed(s.issue));
            let gone: Vec<u64> = (self.split_issues()).filter(|&n| !listed(n)).collect();
            for held in triage.issues.iter_mut().filter(|t| gone.contains(&t.issue)) {
                if !matches!(
                    held.step,
                    TriageStep::Done | TriageStep::Ready | TriageStep::Closed
                ) {
                    held.step = TriageStep::Done;
                    held.labels.clear();
                    held.ready_body = None;
                }
            }
        }
        self.save(next)
    }

    // Files the split saved for `issue`, which carries `carried`, or says why
    // it did not. The caller ruled out a hand triage label. The parent comes off
    // `ready-for-agent` first, and `needs-triage` before the first draft goes.
    pub(super) fn label_split(
        &mut self,
        issue: u64,
        carried: &[String],
    ) -> Result<ReadyLabels, StateError> {
        let Some(split) = self.split_of(issue).cloned() else {
            return Ok(ReadyLabels::Held);
        };
        // A parent is never ready, whether its split files, is refused or waits.
        if carried.iter().any(|l| l == READY)
            && let Err(e) = self.label_issue(issue, READY, false)
        {
            return Ok(self.split_failed(issue, &format!("cannot take `{READY}` off"), &e));
        }
        let refused = self.split_refusals(&split.drafts);
        if !refused.is_empty() {
            self.notes.push(format!(
                "triage #{issue}: its split files nothing, since {} of its {} drafts fail the \
                 issue check",
                refused.len(),
                split.drafts.len()
            ));
            let listed: Vec<String> = refused.iter().map(|line| format!("- {line}")).collect();
            self.notice_on(
                issue,
                &format!(
                    "Kelpie did not file this issue's split: a draft fails the issue check, so \
                     none is filed.\n\n{}\n\nThe issue stays as it is, and the maintainer \
                     triages it next.",
                    listed.join("\n")
                ),
            );
            self.triage_stepped(issue, TriageStep::Done)?;
            return Ok(ReadyLabels::Stood);
        }
        if split.asked.is_some() {
            return Ok(ReadyLabels::Held);
        }
        if carried.iter().any(|l| l == TRIAGE)
            && let Err(e) = self.label_issue(issue, TRIAGE, false)
        {
            return Ok(self.split_failed(issue, &format!("cannot take `{TRIAGE}` off"), &e));
        }
        let started = split.filed.len();
        let mut filed = split.filed.clone();
        let mut attempt = Attempt::default();
        for (n, draft) in split.drafts.iter().enumerate() {
            // A child before the last one saved had every link and its label
            // before the next was filed.
            if n + 1 < started {
                continue;
            }
            let child = match filed.get(n) {
                Some(&child) => child,
                None => match self.file_draft(issue, n, draft, &filed, &mut attempt)? {
                    Some(child) => {
                        filed.push(child);
                        child
                    }
                    None => return Ok(ReadyLabels::Held),
                },
            };
            if !self.link_draft(issue, draft, child, &filed, &mut attempt) {
                return Ok(ReadyLabels::Held);
            }
        }
        self.triage_stepped(issue, TriageStep::Done)?;
        self.notes.push(format!(
            "triage #{issue}: its split is filed as {} sub-issues",
            split.drafts.len()
        ));
        Ok(ReadyLabels::Stood)
    }

    // Files draft `index`, or finds the body a lost answer left, and saves it as
    // filed. `None` when it could not be done, which is already said.
    fn file_draft(
        &mut self,
        issue: u64,
        index: usize,
        draft: &SplitDraft,
        filed: &[TicketId],
        attempt: &mut Attempt,
    ) -> Result<Option<TicketId>, StateError> {
        let open = match &attempt.listing {
            Some(open) => open,
            None => match (self.ports.tickets).open_issues(&self.remote) {
                Ok(open) => attempt.listing.insert(open),
                Err(e) => {
                    self.split_failed(issue, "cannot read the open issues", &e);
                    return Ok(None);
                }
            },
        };
        let wanted = marker(issue, index);
        // The marker is public, so only an issue its own author could have filed
        // counts: the App's, or the maintainer's where `gh` files.
        let found = (open.iter())
            .filter(|i| self.own_author(&i.author))
            .find(|i| i.body.trim_end().lines().last() == Some(wanted.as_str()))
            .and_then(|i| TicketId::from_issue(i.number));
        let child = match found {
            Some(child) => child,
            None => {
                let labels: Vec<&str> = (draft.labels.iter())
                    .map(String::as_str)
                    .filter(|l| *l != READY)
                    .collect();
                if !self.make_split_labels(issue, &labels, attempt) {
                    return Ok(None);
                }
                let body = child_body(issue, index, draft, filed);
                match self.file_ticket(&draft.title, &body, &labels) {
                    Ok(child) => child,
                    Err(e) => {
                        let what = format!("cannot file draft {}", index + 1);
                        self.split_failed(issue, &what, &e);
                        return Ok(None);
                    }
                }
            }
        };
        let mut next = self.state.clone();
        if let Some(split) = next
            .triage
            .as_mut()
            .and_then(|t| t.splits.iter_mut().find(|s| s.issue == issue))
        {
            split.filed.push(child);
        }
        self.save(next)?;
        Ok(Some(child))
    }

    // Links `child` under the parent and under what it is blocked by, then makes
    // it ready, last. False when a write failed, which is already said.
    fn link_draft(
        &mut self,
        issue: u64,
        draft: &SplitDraft,
        child: TicketId,
        filed: &[TicketId],
        attempt: &mut Attempt,
    ) -> bool {
        let Ok(parent) = super::ticket(issue) else {
            return false;
        };
        if let Err(e) = (self.ports.tickets).add_sub_issue(&self.remote, parent, child) {
            self.split_failed(
                issue,
                &format!("cannot link {} as its sub-issue", named(child)),
                &e,
            );
            return false;
        }
        let mut blockers: Vec<TicketId> = draft
            .after
            .iter()
            .filter_map(|&a| filed.get(a))
            .copied()
            .collect();
        blockers.extend(check::blockers(&draft.body));
        for blocker in blockers {
            if let Err(e) = (self.ports.tickets).add_blocked_by(&self.remote, child, blocker) {
                let what = format!("cannot block {} by {}", named(child), named(blocker));
                self.split_failed(issue, &what, &e);
                return false;
            }
        }
        let Some(number) = child.github() else {
            self.triage_failed(issue, &format!("{} is no issue number", named(child)), true);
            return false;
        };
        if !self.make_split_labels(issue, &[READY], attempt) {
            return false;
        }
        if let Err(e) = self.label_issue(number, READY, true) {
            let what = format!("cannot label {} `{READY}`", named(child));
            self.split_failed(issue, &what, &e);
            return false;
        }
        true
    }

    // Makes each of `labels` the repo lacks, once an attempt. False when that
    // failed, which is already said.
    fn make_split_labels(&mut self, issue: u64, labels: &[&str], attempt: &mut Attempt) -> bool {
        let wanted: Vec<&str> = (labels.iter().copied())
            .filter(|l| !attempt.made.contains(*l))
            .collect();
        if let Err(why) = self.make_missing_labels(wanted.iter().map(|l| new_label(l))) {
            self.triage_failed(issue, &format!("cannot make its labels: {why}"), true);
            return false;
        }
        attempt.made.extend(wanted.into_iter().map(str::to_owned));
        true
    }

    // Says that the split of `issue` could not go on, to be tried at the next
    // poll
    fn split_failed(&mut self, issue: u64, what: &str, e: &ForgeError) -> ReadyLabels {
        let why = format!("{what}: {e}");
        self.triage_failed(issue, &why, !forge_is_busy(e));
        ReadyLabels::Held
    }
}
