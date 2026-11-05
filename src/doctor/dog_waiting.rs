//! Which of the dog's settings wait for a restart
//!
//! The dog applies a change to its `[kelpie]` section where it can and holds
//! the rest until it restarts, listing those keys in `status`. A GUI badges
//! them; this is the same word for a terminal.

use serde::Deserialize;

use super::Line;
use crate::flock::control::{Answered, trigger};
use crate::shepherd::Connection;

#[derive(Deserialize)]
struct Status {
    waiting_for_restart: Vec<String>,
}

/// One unsure line naming the keys the dog waits to restart on, in the order its `status` gives them
///
/// No line when none wait, or when the dog is down, does not answer in time,
/// is a build from before `status` listed them, or answers what is not JSON.
pub(super) async fn check(client: &dyn Connection) -> Option<Line> {
    let Ok(Answered::Runner(body)) = trigger(client, crate::dog::NAME, "status", None).await else {
        return None;
    };
    let status: Status = serde_json::from_str(&body).ok()?;
    if status.waiting_for_restart.is_empty() {
        return None;
    }
    Some(Line::unsure(
        "dog settings",
        format!(
            "waiting for restart: {}",
            status.waiting_for_restart.join(", ")
        ),
        format!("`shep restart {}` applies them", crate::dog::NAME),
    ))
}
