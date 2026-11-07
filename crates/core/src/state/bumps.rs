//! The bumps bots open that kelpie follows

use serde::{Deserialize, Serialize};

use super::ProjectState;
use crate::ports::Timestamp;

/// A pull request from a listed bot, as the board last saw it, and how far
/// kelpie has brought it
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bump {
    /// The pull request
    pub pull_request: u64,
    /// Its author's login
    pub author: String,
    /// The branch it merges from, which names the dependency
    pub branch: String,
    /// Its head commit's hash
    pub head: String,
    /// When the board first saw that head: the checks' clock starts there
    pub seen: Timestamp,
    /// The head the bot was last asked to rebase, so one head is asked once
    ///
    /// Left out until it was asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebase_asked: Option<String>,
    /// How the proof of a web bump's head ended: the end-to-end run, or the browser
    /// proof alone where the project has no e2e host
    ///
    /// A failed browser proof is not kept here; the bump goes red at once. Left out
    /// until a proof ends. A new head clears it, so it names the
    /// current `head` or nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e2e: Option<E2eResult>,
    /// Why kelpie has not merged it yet, in a line for `status`
    ///
    /// Left out until the first poll that follows it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub standing: String,
    /// Set once the bump is red, with what to do about it and how far that
    /// has got
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub red: Option<Red>,
}

/// How the end-to-end run of one head ended
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct E2eResult {
    /// The head it ran on
    pub head: String,
    /// Whether it passed, and the end of its output when it did not
    pub state: E2eState,
}

/// Whether an end-to-end run passed
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum E2eState {
    /// The run's command exited 0
    Passed,
    /// It exited with anything else, or ran past its ceiling
    Failed(String),
}

/// A bump kelpie decided is red, and what it has done about it
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Red {
    /// The head that is red
    pub head: String,
    /// What failed, a line each
    pub why: Vec<String>,
    /// What is left to do
    pub step: RedStep,
}

/// How far kelpie has got with a red bump
///
/// Each step is saved before the next is taken, so a restart repeats none.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RedStep {
    /// The comment naming what failed is next
    Comment,
    /// The comment is posted and the close is next
    Close,
    /// The bump is closed and the issue for it is next
    File,
    /// Logged, and nothing more is done to it until the bot pushes a new head: only a
    /// record saved by a build that had `on_red = "leave"` holds it, and no new bump gets it
    Left,
}

impl Bump {
    /// A bump at `head`, first seen at `seen`, that nothing has been done to
    pub fn new(
        pull_request: u64,
        author: String,
        branch: String,
        head: String,
        seen: Timestamp,
    ) -> Self {
        Self {
            pull_request,
            author,
            branch,
            head,
            seen,
            rebase_asked: None,
            e2e: None,
            standing: String::new(),
            red: None,
        }
    }

    /// This bump after the bot pushed `head`, seen at `seen`
    ///
    /// The new head is proved afresh. A red bump that kelpie only left alone
    /// gets another chance, but one it has begun to close is closed all the
    /// same.
    #[must_use]
    pub fn pushed(&self, head: String, seen: Timestamp) -> Self {
        let red = (self.red.clone()).filter(|red| red.step != RedStep::Left);
        Self {
            head,
            seen,
            e2e: None,
            standing: String::new(),
            red,
            ..self.clone()
        }
    }

    /// Whether the end-to-end run of the current head passed
    pub fn e2e_passed(&self) -> bool {
        self.e2e
            .as_ref()
            .is_some_and(|run| run.head == self.head && run.state == E2eState::Passed)
    }

    /// How the end-to-end run of the current head failed, if it did
    pub fn e2e_failure(&self) -> Option<&str> {
        match self.e2e.as_ref() {
            Some(E2eResult {
                head,
                state: E2eState::Failed(output),
            }) if *head == self.head => Some(output),
            _ => None,
        }
    }

    /// Whether kelpie decided to close this bump, so its issue is still to be
    /// filed, which keeps the record after the pull request has left the board
    pub fn issue_owed(&self) -> bool {
        self.red
            .as_ref()
            .is_some_and(|red| red.step != RedStep::Left)
    }
}

impl ProjectState {
    /// Follows `followed` from now on
    pub fn follow_bumps(&mut self, followed: Vec<Bump>) {
        self.bumps = followed;
    }
}
