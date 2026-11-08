//! The issue check the board runs on every open issue
//!
//! Each poll checks every open issue that is new or edited ([`check::key`]).
//! It does so whatever `triage.pick` says, and keeps the result in `checks`.
//! The maintainer's passing issue is labelled `ready-for-agent`. Anyone else's
//! waits at `needs-triage` for the maintainer. A failing issue is owed the
//! triager, where the pick rule reaches it. The board holds a ready one
//! meanwhile. Once its pass ends the issue is checked again. A rewrite that
//! still fails gets one comment naming what the check refused.

use std::collections::BTreeSet;

use super::super::Runner;
use super::super::contributions::post::forge_is_busy;
use super::decided;
use super::opened_by_us_or_people;
use super::ready::READY_LABEL;
use super::rewrite::new_kind_label;
use crate::board::{READY, ReadyIssue, Skip, TRIAGE, check};
use crate::ports::{ForgeError, OpenIssue, OpenPull};
use crate::settings::TriagePick;
use crate::state::{CheckTriage, IssueCheck, StateError, TriageStep};

// What a check asks of the forge, once its result is new
enum Act {
    // It passed: the maintainer's is made ready, anyone else's waits
    Passed,
    // Its triage's rewrite still fails: one comment names the failures
    Refused,
    // Nothing to write
    Kept,
}

/// The most closed issues whose `ready_seen` the check keeps
pub(super) const CLOSED_KEPT: usize = 200;

/// The most forge writes one poll's check makes; the rest wait for the next poll
pub(super) const CHECK_WRITES: usize = 20;

// What one poll's check may still write, and what it read once for all issues
struct Poll<'a> {
    // The writes left this poll
    writes: usize,
    // The forge said it is busy, so the check stops until the next poll
    busy: bool,
    // The labels made on the repo this poll
    made: BTreeSet<String>,
    // The maintainer's login, once read, if it could be
    maintainer: Option<Option<String>>,
    // The open pull requests the poll read
    pulls: Option<&'a Result<Vec<OpenPull>, ForgeError>>,
}

/// How many times the check tries a comment before it gives the comment up
pub(super) const CHECK_TRIES: u32 = 3;

// The marker that ends the check's comment on the edit `key`
fn check_marker(key: u64) -> String {
    format!("<!-- kelpie:check {key:x} -->")
}

fn carries(labels: &[String], label: &str) -> bool {
    labels.iter().any(|l| l == label)
}

impl Runner {
    // Checks each issue in `open` that is new, edited, or past its triage. Then
    // does what the result asks. One in `busy` was under a pass when the poll
    // began, and waits for a later listing. A write that fails keeps the old
    // result, so the next poll tries it again.
    pub(super) fn check_issues(
        &mut self,
        open: &[OpenIssue],
        pulls: Option<&Result<Vec<OpenPull>, ForgeError>>,
        busy: &BTreeSet<u64>,
    ) {
        // A closed issue's result is kept while it holds `ready_seen`, the newest
        // `CLOSED_KEPT` of them, so a reopened issue is not made ready again.
        let is_open = |issue: u64| open.iter().any(|i| i.number == issue);
        let mut closed: Vec<u64> = (self.state.checks.iter())
            .filter(|c| c.ready_seen && !is_open(c.issue))
            .map(|c| c.issue)
            .collect();
        closed.sort_unstable_by(|a, b| b.cmp(a));
        closed.truncate(CLOSED_KEPT);
        let mut kept: Vec<IssueCheck> = (self.state.checks.iter())
            .filter(|c| is_open(c.issue) || closed.contains(&c.issue))
            .cloned()
            .collect();
        let mut poll = Poll {
            writes: CHECK_WRITES,
            busy: false,
            made: BTreeSet::new(),
            maintainer: None,
            pulls,
        };
        let app = self.app_login();
        for issue in (open.iter()).filter(|i| opened_by_us_or_people(i, app.as_deref())) {
            if poll.busy {
                break;
            }
            // A pass under way writes its own body and labels. The next check
            // reads its end.
            if busy.contains(&issue.number) || self.triage_busy(issue.number, &issue.body) {
                continue;
            }
            let mut labels = issue.labels.clone();
            let mut key = check::key(&issue.title, &issue.body, &labels);
            let at = kept.iter().position(|c| c.issue == issue.number);
            let ran = at.is_some_and(|at| kept[at].triage == CheckTriage::Ran);
            // A listing that carries the label is the sight of it, whatever the
            // edit. Nothing is read from the forge for it.
            let mut ready_seen =
                carries(&labels, READY) || at.is_some_and(|at| kept[at].ready_seen);
            if let Some(at) = at {
                kept[at].ready_seen = ready_seen;
            }
            if at.is_some_and(|at| kept[at].key == key) && !ran {
                continue;
            }
            // The label goes on before the check reads, and into the key, so
            // the next listing, which carries it, is the same edit.
            match self.title_kind_label(issue, &mut poll) {
                Err(()) => continue,
                Ok(None) => {}
                Ok(Some(label)) => {
                    labels.push(label);
                    key = check::key(&issue.title, &issue.body, &labels);
                }
            }
            let same = at.is_some_and(|at| kept[at].key == key);
            let failures: Vec<String> = (check::check(&issue.title, &issue.body, &labels))
                .iter()
                .map(ToString::to_string)
                .collect();
            // An issue triaged before is owed a new pass for an edit. The check's
            // first read of it owes none.
            let triaged = self.triage_record(issue.number).is_some();
            // A pass that rewrote nothing the check reads said all it had in its
            // comment. Silence leaves it so. A triage label put on by hand is the
            // maintainer's word on it.
            let decided = decided(&issue.labels);
            let (triage, act) = match (failures.is_empty(), ran) {
                (true, _) => (CheckTriage::None, Act::Passed),
                (false, true) if same || decided => (CheckTriage::None, Act::Kept),
                (false, true) => (CheckTriage::None, Act::Refused),
                (false, false) if triaged && at.is_none() => (CheckTriage::None, Act::Kept),
                (false, false) if triaged => (CheckTriage::Edited, Act::Kept),
                (false, false) => (CheckTriage::Owed, Act::Kept),
            };
            let done = match act {
                Act::Passed => self.check_passed(issue, &mut ready_seen, &mut poll),
                Act::Refused => self.check_refused(issue, key, &failures, &mut poll),
                Act::Kept => true,
            };
            if !done {
                continue;
            }
            let result = IssueCheck {
                issue: issue.number,
                key,
                failures,
                triage,
                ready_seen,
            };
            match at {
                Some(at) => kept[at] = result,
                None => kept.push(result),
            }
        }
        if kept == self.state.checks {
            return;
        }
        let mut next = self.state.clone();
        next.checks = kept.clone();
        // The results can be read again from the forge. A save that fails keeps
        // them in memory and holds up nothing else.
        if let Err(e) = self.save(next) {
            self.notes
                .push(format!("cannot save the issue check's results: {e}"));
            self.state.checks = kept;
        }
    }

    // Puts `kind:<type>` on `issue` when its title starts with a kind and no
    // label is a kind label, even a wrong one. The label put on, if any. A
    // write that fails is noted and the issue is checked as it stands. `Err`
    // when the poll has no writes left or the forge is busy: the issue waits.
    fn title_kind_label(
        &mut self,
        issue: &OpenIssue,
        poll: &mut Poll,
    ) -> Result<Option<String>, ()> {
        let Some(kind) = check::title_kind(&issue.title) else {
            return Ok(None);
        };
        if issue.labels.iter().any(|l| check::kind_of(l).is_some()) {
            return Ok(None);
        }
        if poll.writes == 0 {
            return Err(());
        }
        let number = issue.number;
        let label = format!("{}{kind}", check::KIND);
        if !poll.made.contains(&label) {
            if let Err(why) = self.make_missing_labels([new_kind_label(&label)]) {
                self.notes
                    .push(format!("check #{number}: cannot make `{label}`: {why}"));
                return Ok(None);
            }
            poll.made.insert(label.clone());
        }
        poll.writes -= 1;
        match self.label_issue(number, &label, true) {
            Ok(()) => Ok(Some(label)),
            Err(e) => {
                self.notes
                    .push(format!("check #{number}: cannot label it `{label}`: {e}"));
                poll.busy = forge_is_busy(&e);
                match poll.busy {
                    true => Err(()),
                    false => Ok(None),
                }
            }
        }
    }

    // Whether `issue`, with `body`, is under a pass, or is a sub-issue of a split
    // whose links and labels are not all in
    pub(super) fn triage_busy(&self, issue: u64, body: &str) -> bool {
        let unfinished = self.triage_record(issue).is_some_and(|t| {
            !matches!(
                t.step,
                TriageStep::Done | TriageStep::Ready | TriageStep::Closed
            )
        });
        let unposted = (self.triage_tries.get(&issue))
            .is_some_and(|t| t.waiting.is_some() || t.body.is_some());
        let given_up = self.triage_given_up(issue);
        ((unfinished || unposted) && !given_up)
            || self.flights.triaging() == Some(issue)
            || self.split_child_pending(issue, body)
    }

    // Says that `issue`'s `ready-for-agent` is not seen, because kelpie is taking it
    // back for triage and its coming off is not the maintainer's
    pub(super) fn check_ready_unseen(&mut self, issue: u64) -> Result<(), StateError> {
        let mut next = self.state.clone();
        let Some(kept) = next.checks.iter_mut().find(|c| c.issue == issue) else {
            return Ok(());
        };
        if !kept.ready_seen {
            return Ok(());
        }
        kept.ready_seen = false;
        self.save(next)
    }

    // Says that `issue` has carried `ready-for-agent`, which kelpie put on
    pub(super) fn check_ready_seen(&mut self, issue: u64) -> Result<(), StateError> {
        let mut next = self.state.clone();
        let Some(kept) = next.checks.iter_mut().find(|c| c.issue == issue) else {
            return Ok(());
        };
        if kept.ready_seen {
            return Ok(());
        }
        kept.ready_seen = true;
        self.save(next)
    }

    // Whether `issue` carried `ready-for-agent` once, by the check's last result
    pub(super) fn check_ready_was_seen(&self, issue: u64) -> bool {
        (self.state.checks.iter()).any(|c| c.issue == issue && c.ready_seen)
    }

    // Whether `issue` failed its last check and the triager has not run on it since
    pub(super) fn check_owed(&self, issue: u64) -> bool {
        (self.state.checks.iter()).any(|c| {
            c.issue == issue && matches!(c.triage, CheckTriage::Owed | CheckTriage::Edited)
        })
    }

    // Whether `issue` failed on an edit made after its triage ended
    pub(super) fn check_edited(&self, issue: u64) -> bool {
        (self.state.checks.iter()).any(|c| c.issue == issue && c.triage == CheckTriage::Edited)
    }

    // The ready issues the check refuses, or that need a package the poll found
    // unpublished, as the board's skips. A result kept
    // for the text an issue holds now is read. Any other issue is checked here,
    // whatever its triage did, so a hand fix is not held by the old failures.
    pub(in crate::runner) fn check_holds(&self, ready: &[ReadyIssue]) -> Vec<Skip> {
        (ready.iter())
            .filter_map(|issue| {
                let number = issue.ticket.github()?;
                let ended = (self.state.finished.iter())
                    .chain(&self.state.dropped)
                    .any(|t| *t == issue.ticket);
                if ended {
                    return None;
                }
                let key = check::key(&issue.title, &issue.body, &issue.labels);
                let kept = (self.state.checks.iter()).find(|c| c.issue == number);
                let failures = match kept {
                    Some(kept) if kept.key == key => kept.failures.clone(),
                    _ => (check::check(&issue.title, &issue.body, &issue.labels).iter())
                        .map(ToString::to_string)
                        .collect(),
                };
                if !failures.is_empty() {
                    return Some(Skip::Check {
                        issue: issue.ticket,
                        failures,
                    });
                }
                // The check reads no network: the poll asked the registry.
                let package = self.triage_run.registry.first_waiting(&issue.body)?;
                Some(Skip::Unpublished {
                    issue: issue.ticket,
                    package,
                })
            })
            .collect()
    }

    // Makes the maintainer's passing issue ready, `needs-triage` off, and sets
    // `ready_seen`. Puts `needs-triage` on anyone else's. Another triage label
    // stands, and so does a work item, an open pull request that closes it, or
    // the maintainer's take-off of a label it carried. Whether nothing is left.
    fn check_passed(&mut self, issue: &OpenIssue, ready_seen: &mut bool, poll: &mut Poll) -> bool {
        let number = issue.number;
        // A split parent is never ready: its sub-issues are what the board takes.
        if decided(&issue.labels) || self.has_work_item(number) || self.split_of(number).is_some() {
            return true;
        }
        let Some(maintainer) = self.poll_maintainer(poll) else {
            return false;
        };
        let wanted: &[(&str, bool)] = match self.is_own(&issue.author, &maintainer) {
            true if *ready_seen && !carries(&issue.labels, READY) => {
                self.notes.push(format!(
                    "check #{number}: the maintainer took `{READY}` off, so it stays off"
                ));
                &[]
            }
            true => match poll.pulls {
                Some(Ok(pulls)) if pulls.iter().any(|p| p.closes.contains(&number)) => {
                    return false;
                }
                // `ready-for-agent` goes on last, so no write can fail after it
                Some(Ok(_)) => &[(TRIAGE, false), (READY, true)],
                _ => return false,
            },
            false if carries(&issue.labels, READY) => &[],
            false if self.settings.triage.pick == TriagePick::Off => &[],
            false => &[(TRIAGE, true)],
        };
        for &(label, on) in wanted {
            if carries(&issue.labels, label) == on {
                continue;
            }
            if poll.writes == 0 {
                return false;
            }
            let waiting = label == TRIAGE && on;
            if on && !poll.made.contains(label) {
                let made = match waiting {
                    true => self.make_waiting_label(),
                    false => self.make_missing_labels([READY_LABEL]),
                };
                if let Err(why) = made {
                    self.notes
                        .push(format!("check #{number}: cannot make `{label}`: {why}"));
                    return false;
                }
                poll.made.insert(label.to_owned());
            }
            poll.writes -= 1;
            let labelled = match waiting {
                true => self.label_waiting(number),
                false => self.label_issue(number, label, on),
            };
            if let Err(e) = labelled {
                let how = if on { "label it" } else { "take off" };
                self.notes
                    .push(format!("check #{number}: cannot {how} `{label}`: {e}"));
                poll.busy = forge_is_busy(&e);
                return false;
            }
            if label == READY {
                *ready_seen = true;
                self.notes.push(format!(
                    "check #{number}: the maintainer's issue passes the issue check, so it is \
                     labelled `{READY}`"
                ));
            }
        }
        true
    }

    // Says once on the maintainer's `issue` what the check still refuses after
    // its triage. Anyone else's was handed over by its triage comment. The
    // comment ends in a marker for `key`. A post whose answer was lost is
    // found by it and not made again.
    fn check_refused(
        &mut self,
        issue: &OpenIssue,
        key: u64,
        failures: &[String],
        poll: &mut Poll,
    ) -> bool {
        let number = issue.number;
        let Some(maintainer) = self.poll_maintainer(poll) else {
            return false;
        };
        if !self.is_own(&issue.author, &maintainer) {
            return true;
        }
        let marker = check_marker(key);
        match self.check_told(number, &marker) {
            Ok(true) => {
                self.triage_run.check_fails.remove(&number);
                return true;
            }
            Ok(false) => {}
            Err(e) => {
                let why = format!("cannot read its comments: {e}");
                return self.check_tell_failed(number, &why, forge_is_busy(&e), poll);
            }
        }
        if poll.writes == 0 {
            return false;
        }
        poll.writes -= 1;
        let listed: Vec<String> = failures.iter().map(|f| format!("- {f}")).collect();
        let text = format!(
            "Kelpie checked this issue again after its triage, and the issue check still \
             refuses it:\n\n{}\n\nIt waits at `{TRIAGE}`. Kelpie checks it again, and triages \
             it again if it still fails, once it is edited.\n\n{marker}",
            listed.join("\n")
        );
        match self.comment_on_issue(number, &text) {
            Ok(()) => {
                self.triage_run.check_fails.remove(&number);
                self.notes.push(format!(
                    "check #{number}: its triage's rewrite still fails the issue check, said on \
                     the issue"
                ));
                true
            }
            Err(e) => {
                let why = format!("cannot say what the check refused: {e}");
                self.check_tell_failed(number, &why, forge_is_busy(&e), poll)
            }
        }
    }

    // Whether `author` is the maintainer, `maintainer` read, or the App
    fn is_own(&self, author: &str, maintainer: &str) -> bool {
        author.eq_ignore_ascii_case(maintainer)
            || super::is_app(author, self.app_login().as_deref())
    }

    // The maintainer's login, read at most once a poll
    fn poll_maintainer(&self, poll: &mut Poll) -> Option<String> {
        let read = poll
            .maintainer
            .get_or_insert_with(|| self.maintainer().ok());
        read.clone()
    }

    // Whether a comment on `issue` ends in `marker`
    fn check_told(&self, issue: u64, marker: &str) -> Result<bool, ForgeError> {
        let comments = (self.ports.tickets).issue_comments(&self.remote, super::ticket(issue)?)?;
        Ok((comments.iter()).any(|c| c.body.trim_end().lines().last() == Some(marker)))
    }

    // Logs why telling `issue` failed. A busy forge stops the poll's check and
    // counts for nothing. After [`CHECK_TRIES`] other failures the comment is
    // given up. Whether the result is kept.
    fn check_tell_failed(&mut self, issue: u64, why: &str, busy: bool, poll: &mut Poll) -> bool {
        self.notes.push(format!("check #{issue}: {why}"));
        if busy {
            poll.busy = true;
            return false;
        }
        let fails = self.triage_run.check_fails.entry(issue).or_default();
        *fails += 1;
        if *fails < CHECK_TRIES {
            return false;
        }
        self.triage_run.check_fails.remove(&issue);
        self.notes.push(format!(
            "check #{issue}: gives up saying what the check refused after {CHECK_TRIES} tries"
        ));
        true
    }
}
