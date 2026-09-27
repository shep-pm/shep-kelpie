//! The work item in flight, as the state file keeps it

use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::board::WorkerModel;
use crate::ports::{Cost, Role, SessionId, Timestamp, Usage};

/// The work item in flight
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkItem {
    /// The issue it resolves
    pub issue: u64,
    /// The issue's title when it was added
    pub title: String,
    /// Its branch, cut from `origin/main`
    pub branch: String,
    /// Its worktree
    pub worktree: PathBuf,
    /// Its worker's build folder
    pub build: PathBuf,
    /// The model and effort its worker runs on
    pub worker: WorkerModel,
    /// The worker's session, chosen before its first turn
    pub session: SessionId,
    /// Where the worker's turn stands
    pub turn: Turn,
    /// The draft pull request its worker opened, once kelpie has seen it
    pub pull_request: Option<u64>,
    /// Every Claude call made for it, oldest first
    pub calls: Vec<CallRecord>,
}

impl WorkItem {
    /// What its calls have cost so far
    pub fn cost(&self) -> Cost {
        Cost(self.calls.iter().map(|c| c.cost.0).sum())
    }

    /// What `session` had cost as of its last recorded call
    pub fn session_cost(&self, session: &SessionId) -> Cost {
        let last = self.calls.iter().rev().find(|c| &c.session == session);
        last.map_or(Cost(0), |c| c.session_cost)
    }
}

/// A random version 4 UUID, which `claude --session-id` takes
pub fn new_session_id() -> io::Result<SessionId> {
    let mut b = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let (a, rest) = hex.split_at(8);
    let (b, rest) = rest.split_at(4);
    let (c, rest) = rest.split_at(4);
    let (d, e) = rest.split_at(4);
    Ok(SessionId(format!("{a}-{b}-{c}-{d}-{e}")))
}

/// Where the worker's turn stands
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase", deny_unknown_fields)]
pub enum Turn {
    /// The first turn waits for the project to run
    Due,
    /// A turn started and has not ended; after a restart, it is resumed
    Running {
        /// When it started
        since: Timestamp,
    },
    /// The turn ended and the worker waits for kelpie
    Ended {
        /// When it ended
        at: Timestamp,
    },
    /// The turn could not run, and waits for the maintainer
    Failed {
        /// When it failed
        at: Timestamp,
        /// Why
        reason: String,
    },
}

/// One Claude call made for a work item
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallRecord {
    /// The role it was made for
    pub role: Role,
    /// When it ended
    pub at: Timestamp,
    /// The session it ran in
    pub session: SessionId,
    /// What it used
    pub usage: Usage,
    /// What it cost: the change in its session's cost
    pub cost: Cost,
    /// What its session had cost when it ended
    pub session_cost: Cost,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test::a_work_item;

    #[test]
    fn the_format_is_pinned() {
        assert_eq!(
            serde_json::to_value(a_work_item()).unwrap(),
            json!({
                "issue": 42,
                "title": "Add a thing",
                "branch": "kelpie/42",
                "worktree": "/k/wt/shep/42",
                "build": "/k/targets/shep/42",
                "worker": { "model": "claude-opus-5-5", "effort": "medium" },
                "session": "5e55",
                "turn": { "state": "running", "since": 9 },
                "pull_request": 51,
                "calls": [{
                    "role": "worker",
                    "at": 10,
                    "session": "5e55",
                    "usage": { "input": 1, "cache_write": 2, "cache_read": 3, "output": 4 },
                    "cost": 5,
                    "session_cost": 6,
                }],
            })
        );
    }

    #[test]
    fn every_turn_state_is_pinned() {
        let value = |t: Turn| serde_json::to_value(t).unwrap();
        assert_eq!(value(Turn::Due), json!({ "state": "due" }));
        assert_eq!(
            value(Turn::Ended { at: Timestamp(3) }),
            json!({ "state": "ended", "at": 3 })
        );
        assert_eq!(
            value(Turn::Failed {
                at: Timestamp(4),
                reason: "no worktree".into()
            }),
            json!({ "state": "failed", "at": 4, "reason": "no worktree" })
        );
    }

    #[test]
    fn session_ids_are_distinct_version_4_uuids() {
        let a = new_session_id().unwrap().0;
        let b = new_session_id().unwrap().0;
        assert_ne!(a, b);
        let groups: Vec<_> = a.split('-').map(str::len).collect();
        assert_eq!(groups, [8, 4, 4, 4, 12]);
        assert_eq!(&a[14..15], "4");
        assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"), "{a}");
    }

    #[test]
    fn a_calls_cost_is_measured_from_its_own_sessions_last_call() {
        let mut item = a_work_item();
        let other = SessionId("0th3r".into());
        let mut call = item.calls[0].clone();
        call.session = other.clone();
        call.session_cost = Cost(40);
        call.cost = Cost(40);
        item.calls.push(call);
        assert_eq!(item.session_cost(&item.session), Cost(6));
        assert_eq!(item.session_cost(&other), Cost(40));
        assert_eq!(item.session_cost(&SessionId("new".into())), Cost(0));
        assert_eq!(item.cost(), Cost(45));
    }
}
