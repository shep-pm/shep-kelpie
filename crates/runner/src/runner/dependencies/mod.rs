//! Merging the bumps bots open, and closing the ones that break
//! Under `dependencies.pick = "on"`, once a board poll
//! the runner records each open pull request from a listed bot, dropping one
//! that left the board. A bump behind `main` is asked to rebase, once per head,
//! only when it conflicts or the repo's rules require up to date; one only
//! behind is proved as it stands. A proved head waits
//! [`CHECKS_SETTLE`](super::CHECKS_SETTLE), is proved by its checks and, for a
//! `dependencies.web` path, by an e2e run ([`flight::proving`](super::flight)),
//! then merges with a merge commit. It is red when a check or the run failed,
//! or no check reported after `dependencies.wait_hours`; it is then closed with an issue filed.

use super::Runner;
use super::dev_server::Browsed;
use super::flight::Verdict;
use super::gate::{settled, short};
use crate::board::{OpenPullRequest, is_bump};
use crate::ports::{Checks, E2eOutcome, NewLabel, PullRequestState, Timestamp};
use crate::state::{Bump, E2eResult, E2eState, Red, RedStep, StateError};
use crate::worktree::Base;
use acts::{Act, Done, Made};
use read::{Proof, Reads};

/// Made on a repo that lacks it before the first issue for a red bump
const DEPENDENCIES_LABEL: NewLabel<'static> = NewLabel {
    name: "dependencies",
    color: "0366d6",
    description: "A bump a bot opened that kelpie closed because it broke",
};

// What `status` says of a bump whose poll did not read what it needed, which the next one does
const UNREAD: &str = "waiting for the next poll to read it";

// What `status` says of a web bump the dog has no host for
const NO_HOST: &str = "a web bump, and the project has no e2e host, so it is not merged";

// What `status` and the log say of a bump behind `main` that its rule lets through as it stands
const BEHIND_ALLOWED: &str = "behind main, which does not require it to be up to date";

// Why a bump behind `main` is asked to rebase
enum Why {
    /// Its head conflicts with `main`, whatever its rules say
    Conflict,
    /// `main`'s rules require a pull request's branch to be up to date
    Strict,
    /// Those rules could not be read, so kelpie asks as it always did
    Unknown(String),
}

impl Why {
    fn standing(&self) -> String {
        match self {
            Self::Conflict => "behind main and conflicts with it".into(),
            Self::Strict => "behind main, which requires it to be up to date".into(),
            Self::Unknown(why) => format!("behind main, and cannot tell its rule ({why})"),
        }
    }

    fn note(&self) -> String {
        match self {
            Self::Conflict => "it conflicts with main".into(),
            Self::Strict => "main requires it to be up to date".into(),
            Self::Unknown(why) => format!("its rule cannot be read: {why}"),
        }
    }
}

// Where one poll left a bump
enum Next {
    // Not yet: this is why
    Waiting(String),
    // Behind `main`: ask its bot to rebase it, for this reason
    Rebase(Why),
    // Proved: merge it. `behind` names the rule that let a behind bump through,
    // which the merge's log line says
    Merge { behind: Option<&'static str> },
    // Broken: this is what failed
    Red(Vec<String>),
}

// What to do with a bump behind `main`: ask it to rebase, for this reason, or
// prove it on its head as it stands
enum Behind {
    Ask(Why),
    Prove,
}

fn behind_rule(read: &Proof, up_to_date: Option<Result<bool, String>>) -> Behind {
    match &read.conflicts {
        Some(Ok(true)) => return Behind::Ask(Why::Conflict),
        Some(Err(why)) => {
            return Behind::Ask(Why::Unknown(format!(
                "git cannot say whether it conflicts: {why}"
            )));
        }
        Some(Ok(false)) => {}
        None => return Behind::Ask(Why::Unknown("git has not said whether it conflicts".into())),
    }
    match up_to_date {
        Some(Ok(false)) => Behind::Prove,
        Some(Ok(true)) => Behind::Ask(Why::Strict),
        Some(Err(why)) => Behind::Ask(Why::Unknown(why)),
        None => Behind::Ask(Why::Unknown("the repo's rule has not been read".into())),
    }
}

// Whether a bump behind `main` is to be asked to rebase, for the reason `why` gives, which it is
// once for each head: else where it stands
fn rebase_due(bump: &Bump, why: Why) -> Result<Why, String> {
    match bump.rebase_asked.as_deref() == Some(&bump.head) {
        true => Err(format!(
            "{}: asked to rebase, and has not pushed yet",
            why.standing()
        )),
        false => Ok(why),
    }
}

// Says which rule let a behind bump through in the line it waits on
fn stood(behind: Option<&'static str>, next: Next) -> Next {
    match (behind, next) {
        (Some(rule), Next::Waiting(why)) => Next::Waiting(format!("{rule}: {why}")),
        (_, next) => next,
    }
}

impl Runner {
    // The bumps as `status` lists them, each with where it stands
    pub(super) fn bumps_status(&self) -> Vec<BumpStatus> {
        (self.state.bumps.iter())
            .map(|bump| BumpStatus {
                pull_request: bump.pull_request,
                author: bump.author.clone(),
                head: bump.head.clone(),
                standing: standing_of(bump),
            })
            .collect()
    }

    fn forget_bumps(&mut self) -> Result<(), StateError> {
        if self.state.bumps.is_empty() {
            return Ok(());
        }
        let mut next = self.state.clone();
        next.follow_bumps(Vec::new());
        self.save(next)
    }

    fn record_bumps(&mut self, open: &[OpenPullRequest], now: Timestamp) -> Result<(), StateError> {
        let mut followed: Vec<Bump> = Vec::new();
        for pr in open
            .iter()
            .filter(|pr| is_bump(&self.settings.dependencies, pr))
        {
            let known = (self.state.bumps.iter()).find(|b| b.pull_request == pr.number);
            followed.push(match known {
                Some(known) if known.head == pr.commit => known.clone(),
                Some(known) => known.pushed(pr.commit.clone(), now),
                None => Bump::new(
                    pr.number,
                    pr.author.clone(),
                    pr.head.clone(),
                    pr.commit.clone(),
                    now,
                ),
            });
        }
        // A bump kelpie decided to close is owed its issue wherever it went: off
        // the board, a draft, or no longer a listed bot's.
        let owed: Vec<Bump> = (self.state.bumps.iter())
            .filter(|b| b.issue_owed() && followed.iter().all(|f| f.pull_request != b.pull_request))
            .cloned()
            .collect();
        followed.extend(owed);
        let place = |b: &Bump| {
            let held = (self.state.bumps.iter()).position(|k| k.pull_request == b.pull_request);
            (held.unwrap_or(usize::MAX), b.pull_request)
        };
        followed.sort_by_key(place);
        if followed == self.state.bumps {
            return Ok(());
        }
        let mut next = self.state.clone();
        next.follow_bumps(followed);
        self.save(next)
    }

    // Takes bump `number` as far as it can go without a write, and returns the write it needs
    fn work_bump(
        &mut self,
        number: u64,
        open: &[OpenPullRequest],
        now: Timestamp,
        reads: &mut Reads,
    ) -> Result<Option<Act>, StateError> {
        let Some(bump) = self.bump(number) else {
            return Ok(None);
        };
        if bump.red.is_some() {
            let on_board = open.iter().any(|pr| pr.number == number);
            if !on_board && bump.issue_owed() && self.settle_left_red(&bump, reads)? {
                return Ok(None);
            }
            return self.finish_red(number);
        }
        let Some(pr) = open.iter().find(|pr| pr.number == number) else {
            return Ok(None);
        };
        match self.prove(&bump, pr, now, reads) {
            Next::Waiting(why) => self.stand(number, why, false).map(|()| None),
            Next::Red(why) => self.go_red(number, &bump.head, why),
            Next::Rebase(why) => Ok(Some(Act::Rebase { bump, why })),
            Next::Merge { behind } => Ok(Some(Act::Merge { bump, behind })),
        }
    }

    // A red bump that left the board before kelpie closed it: closed by
    // someone else, its issue is still owed; merged, there is nothing to
    // file; unreadable, it waits. Says whether that is all for this poll.
    fn settle_left_red(&mut self, bump: &Bump, reads: &mut Reads) -> Result<bool, StateError> {
        let number = bump.pull_request;
        if bump
            .red
            .as_ref()
            .is_none_or(|red| red.step == RedStep::File)
        {
            return Ok(false);
        }
        let Some(read) = reads.left.remove(&number) else {
            return Ok(true);
        };
        match read {
            Ok(PullRequestState::Merged) => {
                self.notes.push(format!(
                    "bump #{number}: merged by someone else while red, so no issue"
                ));
                self.drop_bump(number)?;
                Ok(true)
            }
            Ok(PullRequestState::Closed) => {
                self.amend(number, |b| {
                    if let Some(red) = b.red.as_mut() {
                        red.step = RedStep::File;
                    }
                })?;
                Ok(false)
            }
            Ok(_) => Ok(false),
            Err(e) => {
                let why = format!("red, and cannot tell whether it was merged: {e}");
                self.stand(number, why, true)?;
                Ok(true)
            }
        }
    }

    fn bump(&self, number: u64) -> Option<Bump> {
        (self.state.bumps.iter())
            .find(|b| b.pull_request == number)
            .cloned()
    }

    // Changes bump `number`'s record with `change`, saving it when it changed.
    fn amend(&mut self, number: u64, change: impl FnOnce(&mut Bump)) -> Result<(), StateError> {
        let mut next = self.state.clone();
        if let Some(bump) = next.bumps.iter_mut().find(|b| b.pull_request == number) {
            change(bump);
        }
        match next == self.state {
            true => Ok(()),
            false => self.save(next),
        }
    }

    // Says why bump `number` is not merged yet, in the log too when `noted`.
    fn stand(&mut self, number: u64, why: String, noted: bool) -> Result<(), StateError> {
        if noted && self.bump(number).is_some_and(|b| b.standing != why) {
            self.notes.push(format!("bump #{number}: {why}"));
        }
        self.amend(number, |bump| bump.standing = why)
    }

    // Where `bump`, whose listing is `pr`, stands, and what to do about it.
    fn prove(
        &mut self,
        bump: &Bump,
        pr: &OpenPullRequest,
        now: Timestamp,
        reads: &mut Reads,
    ) -> Next {
        let Some(read) = reads.proofs.remove(&bump.pull_request) else {
            return Next::Waiting(UNREAD.into());
        };
        let behind = match read.base {
            Err(e) => return Next::Waiting(format!("cannot tell where it stands on main: {e}")),
            Ok(Base::Lagging) => {
                return Next::Waiting("waiting for its branch to match the forge's head".into());
            }
            Ok(Base::Current) => None,
            Ok(Base::Behind) => match behind_rule(&read, reads.up_to_date.clone()) {
                Behind::Ask(why) => {
                    return match rebase_due(bump, why) {
                        Ok(why) => Next::Rebase(why),
                        Err(standing) => Next::Waiting(standing),
                    };
                }
                Behind::Prove => Some(BEHIND_ALLOWED),
            },
        };
        if !settled(bump.seen, now) {
            return stood(
                behind,
                Next::Waiting("waiting for its checks to register".into()),
            );
        }
        let hours = self.settings.dependencies.wait_hours.get();
        let overdue = now.0.saturating_sub(bump.seen.0) >= u64::from(hours) * 3600;
        match &pr.checks {
            Checks::Failed(names) => {
                return Next::Red(
                    names
                        .iter()
                        .map(|n| format!("check `{n}` failed"))
                        .collect(),
                );
            }
            Checks::Pending(_) if overdue => {
                return Next::Red(vec![format!(
                    "its checks were still pending after {hours} hours"
                )]);
            }
            Checks::None if overdue => {
                return Next::Red(vec![format!("no check reported on it after {hours} hours")]);
            }
            Checks::None | Checks::Pending(_) => {
                return stood(
                    behind,
                    Next::Waiting("waiting for its checks to finish".into()),
                );
            }
            Checks::Passed | Checks::Skipped => {}
        }
        stood(behind, self.prove_web(bump, read.web, behind))
    }

    // Keeps the answer to a rebase ask, and says where the bump stands. The ask is made once for
    // each head, for the reason `why` gives.
    fn rebased(&mut self, bump: &Bump, why: &Why, asked: Result<(), String>) -> String {
        let standing = why.standing();
        if let Err(e) = asked {
            return format!("{standing}, and cannot ask for a rebase: {e}");
        }
        self.notes.push(format!(
            "bump #{}: asked {} to rebase it onto main, at {}: {}",
            bump.pull_request,
            bump.author,
            short(&bump.head),
            why.note(),
        ));
        let head = bump.head.clone();
        match self.amend(bump.pull_request, |b| b.rebase_asked = Some(head)) {
            Ok(()) => format!("{standing}: asked to rebase"),
            Err(e) => format!("asked to rebase, but cannot save that: {e}"),
        }
    }

    // A web bump also needs its proof on this head: its routes opened in the local
    // browser where the project has one, and a passing e2e run where it has a host.
    fn prove_web(
        &mut self,
        bump: &Bump,
        web: Option<Result<bool, String>>,
        behind: Option<&'static str>,
    ) -> Next {
        let number = bump.pull_request;
        if bump.e2e_failure().is_some() {
            return Next::Red(vec!["its e2e run failed".into()]);
        }
        if bump.e2e_passed() {
            return Next::Merge { behind };
        }
        if self.flights.proving(number, &bump.head) {
            return Next::Waiting("proving it: running the browser and e2e tests".into());
        }
        if let Some(why) = self.flights.declined(number, &bump.head) {
            return Next::Waiting(why.to_owned());
        }
        if self.settings.dependencies.web.is_empty() {
            return Next::Merge { behind };
        }
        let web = match web {
            Some(Ok(web)) => web,
            Some(Err(e)) => return Next::Waiting(format!("cannot list the files it changes: {e}")),
            None => return Next::Waiting(UNREAD.into()),
        };
        if !web {
            return Next::Merge { behind };
        }
        if self.draining() {
            return Next::Waiting("a web bump, waiting for the runner to stop draining".into());
        }
        if self.flights.proof_running() {
            return Next::Waiting("a web bump, waiting for the e2e host".into());
        }
        let browser = match self.browser_proof(number) {
            Ok(browser) => browser,
            Err(why) => return Next::Waiting(why),
        };
        if !self.launch_proof(number, &bump.head, browser) {
            self.release_proof_port(number);
            return Next::Waiting("a web bump, waiting for the e2e host".into());
        }
        Next::Waiting("proving it: running the browser and e2e tests".into())
    }

    // Records how the proof of `head` ended, if the bump is still at it.
    pub(super) fn record_proof(
        &mut self,
        number: u64,
        head: &str,
        verdict: Verdict,
    ) -> Result<(), StateError> {
        if self.bump(number).is_none_or(|b| b.head != head) {
            return Ok(());
        }
        let at = short(head);
        let (outcome, browsed) = match verdict {
            Verdict::E2e { outcome, browsed } => (outcome, browsed),
            Verdict::Browser(Browsed::Failed(why)) => {
                self.notes
                    .push(format!("bump #{number}: browser proof failed at {at}"));
                return self
                    .go_red(number, head, why)
                    .and_then(|act| self.settle_acts(act));
            }
            Verdict::Browser(Browsed::Unavailable(why)) => {
                self.flights.decline(number, head, &why);
                self.notes.push(format!(
                    "bump #{number}: cannot be proved in the browser: {why}"
                ));
                return self.stand(number, why, false);
            }
            Verdict::Browser(Browsed::NotRun(reason)) => {
                let why = format!("the browser proof did not happen: {reason}");
                return self.stand(number, why, true);
            }
            // A pass goes on to the e2e run, so the thread never ends on one.
            Verdict::Browser(Browsed::Passed) => return Ok(()),
        };
        if browsed {
            self.notes
                .push(format!("bump #{number}: browser passed at {at}"));
        }
        let state = match outcome {
            E2eOutcome::Passed => {
                self.notes
                    .push(format!("bump #{number}: e2e passed at {at}"));
                E2eState::Passed
            }
            E2eOutcome::Failed { output } => {
                let last = output.lines().last().unwrap_or_default();
                self.notes
                    .push(format!("bump #{number}: e2e failed at {at}: {last}"));
                E2eState::Failed(output)
            }
            // The browser proof stands where the dog has no host to add to it.
            E2eOutcome::NoHost if browsed => E2eState::Passed,
            E2eOutcome::NoHost => {
                self.flights.decline(number, head, NO_HOST);
                self.notes
                    .push(format!("bump #{number}: the project has no e2e host"));
                return self.stand(number, NO_HOST.into(), false);
            }
            E2eOutcome::NotRun { reason } => {
                let why = format!("the e2e run did not happen: {reason}");
                return self.stand(number, why, true);
            }
        };
        let head = head.to_owned();
        self.amend(number, |bump| bump.e2e = Some(E2eResult { head, state }))
    }

    // Keeps the answer to a merge, and returns the ask to rebase a refused merge of a bump that
    // was behind calls for
    fn bump_merged(
        &mut self,
        bump: &Bump,
        behind: Option<&'static str>,
        merged: Result<(), String>,
        reads: &mut Reads,
    ) -> Result<Option<Act>, StateError> {
        let number = bump.pull_request;
        if let Err(e) = merged {
            return match behind {
                // The forge refused a merge of a head that was behind as it
                // stands: that is the rule kelpie could not read saying
                // itself. Ask for the rebase, once for this head, as for any
                // rule kelpie cannot tell.
                Some(_) => {
                    let why = Why::Unknown(format!("the forge refused the merge: {e}"));
                    match rebase_due(bump, why) {
                        Ok(why) => Ok(Some(Act::Rebase {
                            bump: bump.clone(),
                            why,
                        })),
                        Err(standing) => self.stand(number, standing, false).map(|()| None),
                    }
                }
                None => self
                    .stand(number, format!("cannot merge it: {e}"), true)
                    .map(|()| None),
            };
        }
        let at = short(&bump.head);
        self.notes.push(match behind {
            None => format!("bump #{number}: merged at {at}"),
            Some(rule) => format!("bump #{number}: merged at {at}, {rule}"),
        });
        self.drop_bump(number)?;
        if self.bump(number).is_none() {
            reads.moved_on();
        }
        Ok(None)
    }

    fn drop_bump(&mut self, number: u64) -> Result<(), StateError> {
        let mut next = self.state.clone();
        next.bumps.retain(|b| b.pull_request != number);
        self.save(next)
    }

    // Marks bump `number` red at `head`, with what failed, and returns the write that comes
    // next: the comment, then the close, then the issue.
    fn go_red(
        &mut self,
        number: u64,
        head: &str,
        why: Vec<String>,
    ) -> Result<Option<Act>, StateError> {
        self.notes.push(format!(
            "bump #{number}: red: {}; closing it",
            why.join("; ")
        ));
        let red = Red {
            head: head.to_owned(),
            why,
            step: RedStep::Comment,
        };
        self.amend(number, |bump| bump.red = Some(red))?;
        self.finish_red(number)
    }

    // The write a red bump's next step needs, the steps each saved before the next. A step the
    // forge refuses is tried again at the next poll.
    fn finish_red(&mut self, number: u64) -> Result<Option<Act>, StateError> {
        let Some((bump, red)) = self.bump(number).and_then(|b| Some((b.clone(), b.red?))) else {
            return Ok(None);
        };
        Ok(match red.step {
            RedStep::Left => None,
            RedStep::Comment => Some(self.comment_red(bump, &red)),
            RedStep::Close => Some(Act::Close { bump }),
            RedStep::File => Some(self.file_red(bump, &red)),
        })
    }

    fn comment_red(&self, bump: Bump, red: &Red) -> Act {
        let follows = match self.local_store() {
            true => "A ticket follows.",
            false => "An issue follows.",
        };
        let body = format!(
            "Kelpie is closing this pull request because it broke at `{}`:\n\n{}\n\n{follows}",
            short(&red.head),
            lines_of(&red.why),
        );
        Act::Comment { bump, body }
    }

    // The issue for a closed bump, which ends its record
    fn file_red(&self, bump: Bump, red: &Red) -> Act {
        let number = bump.pull_request;
        let title = format!("dependencies: #{number}, {}, broke", bump.branch);
        let body = format!(
            "Kelpie closed pull request #{number} from `{}`, branch `{}`, because it broke \
             at `{}`:\n\n{}",
            bump.author,
            bump.branch,
            short(&red.head),
            lines_of(&red.why),
        );
        let local = self.local_store();
        Act::File {
            bump,
            title,
            body,
            local,
        }
    }

    // Keeps what came of a write, and returns the write after it, if there is one
    fn apply_act(&mut self, done: Done, reads: &mut Reads) -> Result<Option<Act>, StateError> {
        let Done { act, result } = done;
        let number = act.number();
        if let Ok(Made::Comment(id)) = &result {
            self.keep_comment_id(number, *id);
        }
        match act {
            Act::Rebase { bump, why } => {
                let standing = self.rebased(&bump, &why, result.map(drop));
                self.stand(number, standing, false).map(|()| None)
            }
            Act::Merge { bump, behind } => self.bump_merged(&bump, behind, result.map(drop), reads),
            Act::Comment { .. } => self.red_stepped(number, result.map(|_| RedStep::Close)),
            Act::Close { .. } => {
                if result.is_ok() {
                    self.notes.push(format!("bump #{number}: closed, red"));
                }
                self.red_stepped(number, result.map(|_| RedStep::File))
            }
            Act::File { .. } => {
                let issue = match result {
                    Ok(Made::Filed(issue)) => issue,
                    Ok(_) => String::new(),
                    Err(e) => {
                        let why = format!("closed, and cannot file its issue: {e}");
                        return self.stand(number, why, true).map(|()| None);
                    }
                };
                self.notes
                    .push(format!("bump #{number}: filed {issue} for it"));
                self.drop_bump(number).map(|()| None)
            }
        }
    }

    // Saves that a red bump's step went through, and returns the write after it
    fn red_stepped(
        &mut self,
        number: u64,
        step: Result<RedStep, String>,
    ) -> Result<Option<Act>, StateError> {
        match step {
            Ok(step) => {
                self.amend(number, |bump| {
                    if let Some(red) = bump.red.as_mut() {
                        red.step = step;
                    }
                })?;
                self.finish_red(number)
            }
            Err(e) => self
                .stand(number, format!("red, and the forge refused: {e}"), true)
                .map(|()| None),
        }
    }

    // Makes the writes `act` and the acts after it call for, with the lock held, for the
    // callers outside the poll.
    fn settle_acts(&mut self, mut act: Option<Act>) -> Result<(), StateError> {
        let mut reads = Reads::default();
        while let Some(next) = act {
            let done = next.run(&self.writer());
            act = self.apply_act(done, &mut reads)?;
        }
        Ok(())
    }
}

fn lines_of(why: &[String]) -> String {
    (why.iter())
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// What `status` says of a bump: the end of a red one, or why it is waiting.
fn standing_of(bump: &Bump) -> String {
    let Some(red) = &bump.red else {
        return match bump.standing.is_empty() {
            true => "not looked at yet".to_owned(),
            false => bump.standing.clone(),
        };
    };
    let then = match red.step {
        RedStep::Left => "left open",
        RedStep::Comment | RedStep::Close => "being closed",
        RedStep::File => "closed, its issue is next",
    };
    format!("red ({}), {then}", red.why.join("; "))
}

/// A bump as `status` lists it
// wire format: changing this is a breaking change to the status answer
#[derive(Debug, Clone, serde::Serialize)]
pub struct BumpStatus {
    /// The pull request
    pub pull_request: u64,
    /// Its author's login
    pub author: String,
    /// Its head commit's hash
    pub head: String,
    /// Where it stands, in a line
    pub standing: String,
}

pub(super) use read::follow_dependencies;
mod acts;
#[cfg(test)]
mod behind_tests;
#[cfg(test)]
mod browser_tests;
mod read;
#[cfg(test)]
pub(in crate::runner) mod tests;
