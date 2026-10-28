//! The login's forge hold and pace, reached through the dog's door
//!
//! Every runner on a login asks the one dog, so a hold any of them reports
//! stops all of them, and the minute's calls are counted once. A runner that
//! cannot reach the dog, or cannot read its answer, holds and spaces its own
//! calls alone with a [`LocalPace`] until the dog answers again. The door
//! takes one line and answers one, so each question is a short connection.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ipc::blocking::Stream;
use crate::lease::door::{Answer, Ask, FORGE, ForgeAsk, LONGEST_LINE};
use crate::ports::{LocalPace, Pace, Pass, Timestamp};

/// How long the dog gets to take a question and answer it
const ANSWER_WITHIN: Duration = Duration::from_secs(2);

/// The login's pace as the dog at a door keeps it
#[derive(Debug)]
pub struct DoorPace {
    socket: PathBuf,
    // Kept for the time the dog cannot be reached.
    local: LocalPace,
}

impl DoorPace {
    /// The pace the dog answers for at `socket`
    pub fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            local: LocalPace::new(),
        }
    }

    fn ask(&self, forge: ForgeAsk) -> Option<Answer> {
        let ask = Ask {
            forge: Some(forge),
            ..Ask::new(FORGE, std::process::id(), "forge")
        };
        answered(&self.socket, &ask)
    }
}

// The dog's one-line answer to `ask` at `socket`, or none when it cannot be asked or its answer read.
pub(super) fn answered(socket: &Path, ask: &Ask) -> Option<Answer> {
    let mut stream = Stream::connect(socket).ok()?;
    stream.set_read_timeout(Some(ANSWER_WITHIN)).ok()?;
    stream.set_write_timeout(Some(ANSWER_WITHIN)).ok()?;
    let mut line = serde_json::to_vec(ask).ok()?;
    line.push(b'\n');
    stream.write_all(&line).ok()?;
    let mut answer = String::new();
    BufReader::new(stream.take(LONGEST_LINE))
        .read_line(&mut answer)
        .ok()?;
    serde_json::from_str(&answer).ok()
}

impl Pace for DoorPace {
    fn pass(&self, project: &str, now: Timestamp) -> Pass {
        // A hold set while the dog was out of reach stands until it ends.
        if let Some(until) = self.local.held(now) {
            return Pass::Held(until);
        }
        let project_ask = ForgeAsk::Pass {
            project: project.to_owned(),
        };
        match self.ask(project_ask) {
            Some(Answer::Go) => Pass::Go,
            Some(Answer::Wait(seconds)) => Pass::Wait(seconds),
            Some(Answer::Held(until)) => Pass::Held(Timestamp(until)),
            _ => self.local.pass(project, now),
        }
    }

    fn held(&self, now: Timestamp) -> Option<Timestamp> {
        let local = self.local.held(now);
        match self.ask(ForgeAsk::Held) {
            Some(Answer::Held(until)) => local.max(Some(Timestamp(until))),
            _ => local,
        }
    }

    fn hold(&self, until: Timestamp) {
        // Kept here too, so the hold stands if the dog is out of reach or restarts.
        self.local.hold(until);
        let _ = self.ask(ForgeAsk::Hold(until.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Timestamp = Timestamp(1_790_000_000);

    // A door nobody listens at.
    fn nobody() -> DoorPace {
        DoorPace::new(PathBuf::from("/nonexistent/kelpie/lease.sock"))
    }

    #[test]
    fn a_runner_that_cannot_reach_the_dog_paces_and_holds_alone() {
        let pace = nobody();

        assert_eq!(pace.pass("koji", T), Pass::Go);
        assert_eq!(pace.held(T), None);
        pace.hold(Timestamp(T.0 + 60));
        assert_eq!(pace.held(T), Some(Timestamp(T.0 + 60)));
        assert_eq!(pace.pass("koji", T), Pass::Held(Timestamp(T.0 + 60)));
        assert_eq!(pace.pass("koji", Timestamp(T.0 + 60)), Pass::Go);
    }
}
