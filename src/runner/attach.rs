//! `attach`: a work item held while the maintainer drives its session
//!
//! `attach <issue> <pid>` holds the work item for process `pid`, so no call
//! starts for it. A call in flight runs to its end, and the answer says to
//! wait. Once none is, the answer is the worker's session as a command,
//! with the worker's settings file and inside its sandbox, for the
//! maintainer's terminal. `attach <issue> <pid> <session pid>` then names
//! the session's process, and the hold stands while either runs, each
//! known by its pid and start time. `detach` lets the work item go, and so
//! does the first pass after both have ended. What the session pushed is
//! the worker's own, as a turn's push is.

use std::fmt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use super::Runner;
use super::trigger::WhichItem;
use crate::ports::{Role, Session, SessionId};
use crate::settings::Harness;
use crate::state::StateError;
use crate::terminal::Foreground;
use crate::work_item::{Attached, Holder, Phase, Review, Turn, WorkItem};

/// What `attach` answers
// wire format: `shep kelpie attach` reads it from the runner's answer
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "attach", rename_all = "lowercase", deny_unknown_fields)]
pub enum Attaching {
    /// The work item is held, and a call for it is still in flight
    Waiting {
        /// Its issue
        issue: u64,
    },
    /// The work item is held, and its worker's session is ready to run
    Ready {
        /// Its issue
        issue: u64,
        /// The worker's session
        session: SessionId,
        /// Its worktree
        worktree: PathBuf,
        /// The session, resumed in the worker's settings and sandbox
        command: Foreground,
    },
    /// The work item is held while the session's process runs too
    Running {
        /// Its issue
        issue: u64,
    },
}

/// Why `attach` or `detach` was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachError {
    /// No work item is open for the issue named
    Which(WhichItem),
    /// The worker has run no turn, so it has no session to resume
    NoSession(u64),
    /// The work item is parked on this ruling
    Ruling(u64, u64),
    /// The work item is merging or being closed, past its worker's turns
    Merging(u64),
    /// The worker runs on a harness whose sessions resume another way
    Harness(u64, Harness, SessionId),
    /// Another live process holds the work item
    Held(u64, u32),
    /// `detach`, or a session's pid, came from a process other than the
    /// one holding the work item
    NotHeld(u64, u32),
    /// No process with this pid is running
    NotRunning(u32),
    /// `ps` could not say whether this pid runs
    Unknown(u32),
    /// The worker's session could not be made ready, with why
    Setup(u64, String),
    /// The change could not be saved
    State(StateError),
}

impl fmt::Display for AttachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Which(e) => e.fmt(f),
            Self::NoSession(issue) => write!(
                f,
                "the worker on #{issue} has no session yet: its first turn starts one once \
                 the project runs (`shep kelpie start`), and `shep kelpie attach {issue}` \
                 then waits for that turn to end"
            ),
            Self::Ruling(issue, id) => write!(
                f,
                "the work item for #{issue} is parked on ruling {id}: answer it with \
                 `shep kelpie rule {id} ...`, and `shep kelpie attach {issue}` then waits \
                 for any turn the answer starts"
            ),
            Self::Merging(issue) => write!(
                f,
                "the work item for #{issue} is merging or closing, which no attach can \
                 hold: file a new issue for anything left to do"
            ),
            Self::Harness(issue, harness, session) => write!(
                f,
                "the worker on #{issue} runs on {}, and only a Claude Code session can be \
                 attached: pause the project (`shep kelpie pause`) and resume session {} \
                 with {} by hand",
                harness.as_str(),
                session.0,
                harness.command()
            ),
            Self::Held(issue, pid) => write!(
                f,
                "the work item for #{issue} is already attached, by process {pid}"
            ),
            Self::NotHeld(issue, pid) => write!(
                f,
                "the work item for #{issue} is attached by process {pid}, not this one"
            ),
            Self::NotRunning(pid) => write!(f, "no process {pid} is running"),
            Self::Unknown(pid) => write!(
                f,
                "`ps` could not say whether process {pid} runs, so nothing is held: try again"
            ),
            Self::Setup(issue, why) => write!(f, "cannot ready #{issue}'s session: {why}"),
            Self::State(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for AttachError {}

impl Runner {
    /// Holds the work item for `issue` for process `pid`, and hands back
    /// its worker's session once no call for it is in flight
    ///
    /// Asking again from the same process while it waits keeps the hold.
    /// With `session`, the pid of the session it started, the hold also
    /// stands while that process runs.
    ///
    /// # Errors
    ///
    /// [`AttachError`] naming why the work item cannot be attached, with
    /// what to do instead. It is not held then.
    pub fn attach(
        &mut self,
        issue: u64,
        pid: u32,
        session: Option<u32>,
    ) -> Result<Attaching, AttachError> {
        self.choose(Some(issue)).map_err(AttachError::Which)?;
        let me = running(pid)?;
        let attached = self.current().and_then(|item| item.attached.clone());
        if let Some(session) = session {
            return self.session_started(issue, &me, attached.as_ref(), session);
        }
        let item = self.current().expect("the work item chosen");
        if let Some(held) = attached.as_ref().filter(|a| a.by != me && live(a)) {
            return Err(AttachError::Held(issue, held.by.pid));
        }
        let mine = attached.as_ref().is_some_and(|a| a.by == me);
        let flying = self.flights.flying(issue);
        if let Some(refused) = self.refusal(item, flying) {
            if mine {
                self.update(|item| item.attached = None)
                    .map_err(AttachError::State)?;
            }
            return Err(refused);
        }
        if !mine {
            let by = me.clone();
            let since = self.ports.clock.now();
            self.update(|item| {
                item.attached = Some(Attached {
                    by,
                    session: None,
                    since,
                });
            })
            .map_err(AttachError::State)?;
        }
        if flying {
            return Ok(Attaching::Waiting { issue });
        }
        let ready = self.session_here();
        if ready.is_err() {
            self.update(|item| item.attached = None)
                .map_err(AttachError::State)?;
        }
        ready
    }

    /// Lets go of the work item for `issue` that process `pid` attached,
    /// with what its session pushed as the worker's own. One not attached
    /// is left as it is.
    ///
    /// # Errors
    ///
    /// [`AttachError`] when no work item is open for `issue`, another
    /// process holds it, or the change cannot be saved.
    pub fn detach(&mut self, issue: u64, pid: u32) -> Result<(), AttachError> {
        self.choose(Some(issue)).map_err(AttachError::Which)?;
        let held = self.current().and_then(|item| item.attached.as_ref());
        match held.map(|a| a.by.pid) {
            None => Ok(()),
            Some(by) if by != pid => Err(AttachError::NotHeld(issue, by)),
            Some(_) => self.end_attach().map_err(AttachError::State),
        }
    }

    // Lets go of each work item whose attach and session have both ended
    // without a `detach`, as a terminal closed under them would.
    pub(super) fn let_go_of_the_gone(&mut self) {
        let gone: Vec<u64> = (self.state.work_items.iter())
            .filter(|item| item.attached.as_ref().is_some_and(|a| !live(a)))
            .map(|item| item.issue)
            .collect();
        for issue in gone {
            self.focus = Some(issue);
            match self.end_attach() {
                Ok(()) => eprintln!("the attach on #{issue} ended, so its work item carries on"),
                Err(e) => eprintln!("cannot let go of #{issue}, whose attach ended: {e}"),
            }
        }
    }

    // The session the holding `attach` started runs as `pid`, which holds
    // the work item too.
    fn session_started(
        &mut self,
        issue: u64,
        me: &Holder,
        attached: Option<&Attached>,
        pid: u32,
    ) -> Result<Attaching, AttachError> {
        match attached.map(|a| &a.by) {
            Some(by) if by == me => {}
            Some(by) => return Err(AttachError::NotHeld(issue, by.pid)),
            None => return Err(AttachError::NotHeld(issue, me.pid)),
        }
        let session = running(pid)?;
        self.update(|item| {
            if let Some(attached) = &mut item.attached {
                attached.session = Some(session);
            }
        })
        .map_err(AttachError::State)?;
        Ok(Attaching::Running { issue })
    }

    // Why the work item cannot be attached now, if it cannot. Whether it
    // has a session waits for a call in flight to end.
    fn refusal(&self, item: &WorkItem, flying: bool) -> Option<AttachError> {
        let issue = item.issue;
        let harness = match self.worker_agent(item) {
            Ok(agent) => agent.harness.harness(),
            Err(why) => return Some(AttachError::Setup(issue, why)),
        };
        if harness != Harness::ClaudeCode {
            return Some(AttachError::Harness(issue, harness, item.session.clone()));
        }
        match item.phase {
            Phase::Ruling { id } => return Some(AttachError::Ruling(issue, id)),
            Phase::Merge { .. } | Phase::Done { .. } => return Some(AttachError::Merging(issue)),
            Phase::Implement | Phase::Review(_) | Phase::Ci { .. } => {}
        }
        let begun = item
            .calls
            .iter()
            .any(|c| c.role == Role::Worker && c.session == item.session);
        let running = matches!(item.turn, Turn::Running { .. });
        (!flying && !begun && !running).then_some(AttachError::NoSession(issue))
    }

    // The worker's session, resumed for the terminal as a turn would run it.
    fn session_here(&self) -> Result<Attaching, AttachError> {
        let item = self.current().expect("the work item held");
        let issue = item.issue;
        let setup = |why: String| AttachError::Setup(issue, why);
        let session = Session::Resume(item.session.clone());
        let call = self.prepare(item, session, Some(String::new()));
        let command = call
            .and_then(|call| {
                self.ports
                    .agents
                    .foreground(&call)
                    .map_err(|e| e.to_string())
            })
            .and_then(|command| Foreground::of(&command))
            .map_err(setup)?;
        Ok(Attaching::Ready {
            issue,
            session: item.session.clone(),
            worktree: item.worktree.clone(),
            command,
        })
    }

    // Lets go of the work item in focus. Its branch's head is the worker's
    // own, and a pull request the session opened is the one a turn would find.
    fn end_attach(&mut self) -> Result<(), StateError> {
        let pushed = self.own_push();
        let item = self.current().expect("the work item held");
        let found = (item.pull_request.is_none())
            .then(|| self.pull_request_from(&item.branch))
            .flatten();
        self.update(|item| {
            item.attached = None;
            if pushed.is_some() {
                item.known.head = pushed;
            }
            let Some(number) = found else { return };
            item.pull_request = Some(number);
            if item.phase != Phase::Implement {
                return;
            }
            // As a turn that opens it does: its review comes first.
            match item.turn {
                Turn::Ended { .. } => item.phase = Phase::Review(Review::first()),
                _ if item.resume.is_none() => item.resume = Some(Phase::Review(Review::first())),
                _ => {}
            }
        })
    }
}

/// Whether the hold stands: its `attach` or its session still runs, or
/// `ps` cannot say, and the next pass asks again
pub(super) fn live(attached: &Attached) -> bool {
    live_by(attached, PS)
}

/// The program that names a process and its start time
const PS: &str = "ps";

// `live`, asking `ps`.
fn live_by(attached: &Attached, ps: &str) -> bool {
    let runs = |h: &Holder| match seen(ps, h.pid) {
        Seen::Runs(now) => now == *h,
        Seen::Gone => false,
        Seen::Unknown => true,
    };
    runs(&attached.by) || attached.session.as_ref().is_some_and(runs)
}

// Process `pid` with its start time, as a hold records it.
fn running(pid: u32) -> Result<Holder, AttachError> {
    match seen(PS, pid) {
        Seen::Runs(holder) => Ok(holder),
        Seen::Gone => Err(AttachError::NotRunning(pid)),
        Seen::Unknown => Err(AttachError::Unknown(pid)),
    }
}

// What `ps` says of a pid
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    Runs(Holder),
    // `ps` ran and found no such process
    Gone,
    // `ps` could not run, or failed some other way
    Unknown,
}

// Process `pid` with its start time, while it runs. A pid given to a later
// process has another start time. The C locale and UTC keep the text the
// same across a restart under another locale, time zone or clock change.
fn seen(ps: &str, pid: u32) -> Seen {
    let output = Command::new(ps)
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return Seen::Unknown;
    };
    let started = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    match (output.status.code(), started.is_empty()) {
        (Some(0), false) => Seen::Runs(Holder { pid, started }),
        (Some(1), true) => Seen::Gone,
        _ => Seen::Unknown,
    }
}

#[cfg(test)]
mod tests;
