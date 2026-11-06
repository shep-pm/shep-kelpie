//! What the maintainer does with the project manager: tells it something,
//! opens its session in a terminal, or reads it in `status`

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::Wake;
use crate::ports::{Session, SessionId, Timestamp};
use crate::runner::attach::{live, running};
use crate::runner::{AttachError, Runner};
use crate::settings::{AgentName, PmAgent};
pub use crate::state::PmHeld;
use crate::state::StateError;
use crate::terminal::Foreground;
use crate::ticket::TicketId;
use crate::work_item::{Attached, new_session_id};

/// The project manager as `status` shows it
#[derive(Debug, Serialize)]
pub struct PmStatus<'a> {
    /// Its agent
    pub agent: &'a AgentName,
    /// Its session, once it has one
    pub session: Option<&'a SessionId>,
    /// Its folder: the board as its last wake read it, and its notes
    pub folder: &'a std::path::Path,
    /// Whether a call of its is in flight
    pub answering: bool,
    /// Whether the maintainer's terminal holds its session, pausing its wakes
    pub attached: bool,
    /// Until when it is passed over after a call failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub down_until: Option<Timestamp>,
    /// What the maintainer told it that its next wake carries
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub told: &'a [String],
    /// The ready issues it holds, each with the open work item it waits on
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub held: &'a [PmHeld],
    /// The last few worker questions it answered or passed on, newest last
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub settled: &'a [Settled],
    /// Its last message to the maintainer
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply: Option<&'a str>,
}

/// A worker's question the project manager answered or passed on
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Settled {
    /// The ruling the question raised
    pub ruling: TicketId,
    /// The work item's ticket
    pub issue: TicketId,
    /// What it did: `answered`, so the maintainer was not asked, `passed on`, or
    /// for a follow-up ruling `filed` or `dropped`
    pub how: &'static str,
    /// The choice it recommended, when it passed the question on with one
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommends: Option<String>,
    /// Why, in its words
    pub why: String,
}

/// What `pm attach` answers
// wire format: `shep kelpie pm` reads it from the runner's answer
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "attach", rename_all = "lowercase", deny_unknown_fields)]
pub enum PmAttaching {
    /// The project manager is held, and a wake of its is still in flight
    Waiting,
    /// The project manager is held, and its session is ready to run
    Ready {
        /// Its session, which a wake has started or this starts
        session: SessionId,
        /// Its folder
        folder: PathBuf,
        /// The session in its settings and sandbox
        command: Foreground,
    },
    /// It is held while the session's process runs too
    Running,
}

/// Why a `tell` or `pm` trigger was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PmError {
    /// The project's settings name no `agents.pm`
    NotSetUp,
    /// The note was blank
    Blank,
    /// Another live process, this one, holds the project manager
    Held(u32),
    /// `detach`, or a session's pid, came from a process other than this
    /// one, which holds the project manager
    NotHeld(u32),
    /// The pid given runs no process, or `ps` cannot say
    Process(AttachError),
    /// The project manager's files or session could not be made ready, with why
    Files(String),
    /// The change could not be saved
    State(StateError),
}

impl fmt::Display for PmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotSetUp => f.write_str(
                "the project has no project manager: name one in `agents.pm`, such as `pm`",
            ),
            Self::Blank => f.write_str("`tell` takes a note"),
            Self::Held(pid) => write!(
                f,
                "the project manager's session is already held, by process {pid}"
            ),
            Self::NotHeld(pid) => write!(
                f,
                "the project manager's session is held by process {pid}, not this one"
            ),
            Self::Process(e) => e.fmt(f),
            Self::Files(why) => write!(f, "cannot ready the project manager's session: {why}"),
            Self::State(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for PmError {}

/// What a held comment's entry in `pm_told` begins with
const HELD: &str = "The maintainer's comment ";

/// The `pm_told` entry for `text`, which the maintainer wrote as comment
/// `comment` on `thread`, under `ruling` when one waited there
///
/// Its first line names where the words were written and the rest is the
/// words, so the project manager reads them with their place.
pub(in crate::runner) fn held_line(
    thread: u64,
    comment: u64,
    ruling: Option<TicketId>,
    text: &str,
) -> String {
    let under = ruling.map_or_else(String::new, |id| format!(", under ruling {id}"));
    format!("{HELD}{comment} on #{thread}{under}:\n{}", text.trim())
}

/// The thread and the comment a [`held_line`] names, when `line` is one
pub(in crate::runner) fn held_on(line: &str) -> Option<(u64, u64)> {
    let head = line.lines().next()?.strip_suffix(':')?;
    let (comment, rest) = head.strip_prefix(HELD)?.split_once(" on #")?;
    let thread = rest.split_once(", under ruling ").map_or(rest, |(t, _)| t);
    Some((thread.parse().ok()?, comment.parse().ok()?))
}

impl Runner {
    /// Queues a note from the maintainer for the next wake, and wakes it
    ///
    /// # Errors
    ///
    /// [`PmError`] when the project has no project manager, the note is
    /// blank, or it cannot be saved.
    pub fn tell(&mut self, note: &str) -> Result<(), PmError> {
        self.looks.poll_reset();
        if self.agents.pm.is_none() {
            return Err(PmError::NotSetUp);
        }
        let note = note.trim();
        if note.is_empty() {
            return Err(PmError::Blank);
        }
        let mut next = self.state.clone();
        next.pm_told.push(note.to_owned());
        self.save(next).map_err(PmError::State)?;
        self.pm_due(Wake::Told);
        Ok(())
    }

    /// Holds the project manager for process `pid`, so no wake starts, and
    /// hands back its session once no wake is in flight
    ///
    /// Asking again from the same process while it waits keeps the hold.
    /// With `session`, the pid of the session it started, the hold also
    /// stands while that process runs. One process holds it at a time.
    ///
    /// # Errors
    ///
    /// [`PmError`] when the project has no project manager, another live
    /// process holds it, a pid runs no process, or its session cannot be
    /// made ready or the hold saved. It is not held then.
    pub fn attach_pm(&mut self, pid: u32, session: Option<u32>) -> Result<PmAttaching, PmError> {
        let Some(agent) = self.agents.pm.clone() else {
            return Err(PmError::NotSetUp);
        };
        let me = running(pid).map_err(PmError::Process)?;
        let attached = self.state.pm_attached.clone();
        if let Some(session) = session {
            match attached.as_ref().map(|a| &a.by) {
                Some(by) if *by == me => {}
                Some(by) => return Err(PmError::NotHeld(by.pid)),
                None => return Err(PmError::NotHeld(me.pid)),
            }
            let session = running(session).map_err(PmError::Process)?;
            let mut next = self.state.clone();
            if let Some(attached) = &mut next.pm_attached {
                attached.session = Some(session);
            }
            self.save(next).map_err(PmError::State)?;
            return Ok(PmAttaching::Running);
        }
        if let Some(held) = attached.as_ref().filter(|a| a.by != me && live(a)) {
            return Err(PmError::Held(held.by.pid));
        }
        if attached.as_ref().is_none_or(|a| a.by != me) {
            let mut next = self.state.clone();
            next.pm_attached = Some(Attached {
                by: me,
                session: None,
                since: self.ports.clock.now(),
            });
            self.save(next).map_err(PmError::State)?;
        }
        if self.pm.flying.is_some() {
            return Ok(PmAttaching::Waiting);
        }
        let ready = self.pm_here(&agent);
        if ready.is_err() {
            let mut next = self.state.clone();
            next.pm_attached = None;
            self.save(next).map_err(PmError::State)?;
        }
        ready
    }

    // Its session for the terminal, in its settings and sandbox: the one
    // its wakes resume, or a new one it starts and keeps.
    fn pm_here(&mut self, agent: &PmAgent) -> Result<PmAttaching, PmError> {
        let (id, session) = match &self.state.pm_session {
            Some(id) => (id.clone(), Session::Resume(id.clone())),
            None => {
                let id = new_session_id()
                    .map_err(|e| PmError::Files(format!("cannot draw a session id: {e}")))?;
                (id.clone(), Session::New(id))
            }
        };
        let board = self.pm_board().map_err(PmError::Files)?;
        let call = self.pm_call(agent, session, String::new(), Some(board));
        let command = call
            .and_then(|call| {
                self.ports
                    .agents
                    .foreground(&call)
                    .map_err(|e| e.to_string())
            })
            .and_then(|command| Foreground::of(&command))
            .map_err(PmError::Files)?;
        if self.state.pm_session.is_none() {
            let mut next = self.state.clone();
            next.pm_session = Some(id.clone());
            self.save(next).map_err(PmError::State)?;
        }
        Ok(PmAttaching::Ready {
            session: id,
            folder: self.paths.pm.clone(),
            command,
        })
    }

    /// Lets go of the project manager that process `pid` holds, so its
    /// wakes go on. One not held is left as it is.
    ///
    /// # Errors
    ///
    /// [`PmError`] when the project has no project manager, another
    /// process holds it, or the change cannot be saved.
    pub fn detach_pm(&mut self, pid: u32) -> Result<(), PmError> {
        if self.agents.pm.is_none() {
            return Err(PmError::NotSetUp);
        }
        match self.state.pm_attached.as_ref().map(|a| a.by.pid) {
            None => Ok(()),
            Some(by) if by != pid => Err(PmError::NotHeld(by)),
            Some(_) => {
                let mut next = self.state.clone();
                next.pm_attached = None;
                self.save(next).map_err(PmError::State)?;
                self.pm_terminal_ended();
                Ok(())
            }
        }
    }

    /// Whether the maintainer's terminal holds the project manager now,
    /// letting go of a hold whose processes have both ended
    pub(super) fn pm_held_by_terminal(&mut self) -> bool {
        let Some(attached) = &self.state.pm_attached else {
            return false;
        };
        if live(attached) {
            return true;
        }
        let mut next = self.state.clone();
        next.pm_attached = None;
        if let Err(e) = self.save(next) {
            eprintln!("cannot let go of the project manager, whose attach ended: {e}");
            return true;
        }
        eprintln!("the attach on the project manager ended, so its wakes go on");
        self.pm_terminal_ended();
        false
    }

    // The terminal session of the project manager is over, and so is what its
    // harness kept for it.
    fn pm_terminal_ended(&self) {
        if let Some(session) = &self.state.pm_session {
            self.ports.agents.foreground_ended(session);
        }
    }

    /// The project manager as `status` shows it, when the project has one
    pub(in crate::runner) fn pm_status(&self) -> Option<PmStatus<'_>> {
        let agent = self.agents.pm.as_ref()?;
        Some(PmStatus {
            agent: &agent.name,
            session: self.state.pm_session.as_ref(),
            folder: &self.paths.pm,
            answering: self.pm.flying.is_some(),
            attached: self.state.pm_attached.is_some(),
            down_until: self.pm.down_until,
            told: &self.state.pm_told,
            held: &self.state.pm_holds,
            settled: &self.pm.settled,
            reply: self.pm.reply.as_deref(),
        })
    }
}
