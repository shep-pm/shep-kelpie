//! Draining a runner before its restart, so the restart cuts no call short
//!
//! The runner is sent `drain`, after which it starts no new call, and asked
//! again until it shows none running. The wait is bounded by the longest a
//! call of the runner's may run, which its answer says, plus
//! [`Patience::margin`], counted from when the wait began. A turn's ceiling
//! counts from when it got the lease it waited for, and a reviewer's call
//! has none, so either can outrun the bound. Past it the runner is named and
//! nothing is restarted for it. A runner on a build from before `drain`
//! answers it, twice, as an action nobody took while it answers `status`: it
//! is restarted as before, and the restart cuts its calls short.

use std::time::Duration;

use serde_json::Value;
use shep_client::Client;
use tokio::time::Instant;

use super::restart::Patience;
use crate::flock::control::{Answered, trigger};

/// What a draining runner's answer says
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drained {
    /// Each call still running, as the upgrade names it
    pub calls: Vec<String>,
    /// The longest a call of the runner's may run, in seconds
    pub ceiling: u64,
}

/// Reads a runner's answer to `drain`: `None` when it is not draining, as a
/// runner from before `drain` is not
pub fn drained(body: &str) -> Option<Drained> {
    let answer = serde_json::from_str::<Value>(body).ok()?;
    let draining = answer.get("draining")?;
    let calls = draining.get("calls")?.as_array()?;
    Some(Drained {
        calls: calls.iter().map(call).collect(),
        ceiling: draining.get("ceiling")?.as_u64()?,
    })
}

// How a call in a draining runner's answer reads.
fn call(call: &Value) -> String {
    let issue = call["issue"].as_u64();
    match (call["role"].as_str(), issue) {
        (Some("pm"), _) => "the project manager's wake".to_owned(),
        (Some("worker"), Some(issue)) => format!("#{issue}'s worker turn"),
        (Some("reviewer"), Some(issue)) => format!("#{issue}'s review"),
        (_, Some(issue)) => format!("a call for #{issue}"),
        (_, None) => "a call".to_owned(),
    }
}

/// Drains `name` and returns once it has no call running, saying what it
/// waits on through `say`
///
/// A runner that is not running, or runs a build from before `drain`, is
/// not waited on.
///
/// # Errors
///
/// A message naming the runner and what it still runs when the wait runs
/// out, or one from the shepherd. A runner that never says it drains is
/// waited on for `patience.merge`, as one that never answers `status` is.
/// The runner is left draining: the caller sends it [`undrain`].
pub async fn drain(
    client: &Client,
    name: &str,
    patience: Patience,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    let waited = Instant::now();
    let mut limit = patience.merge;
    let mut told = Vec::new();
    let mut began = false;
    loop {
        let mut answer = trigger(client, name, "drain", None).await?;
        // An action nobody took: a runner still opening, or one from before
        // `drain`. One that answers `status` and then the same again is old.
        if matches!(answer, Answered::Starting)
            && matches!(
                trigger(client, name, "status", None).await?,
                Answered::Runner(_)
            )
        {
            answer = trigger(client, name, "drain", None).await?;
            if matches!(answer, Answered::Starting) {
                return predates(name, say);
            }
        }
        let busy = match answer {
            Answered::Runner(body) => match drained(&body) {
                Some(drained) => {
                    if !began {
                        began = true;
                        say(format!(
                            "draining `{name}`, which starts no new call until its restart: if \
                             this upgrade stops first, `shep kelpie undrain -p {name}` lets it \
                             start them again"
                        ));
                    }
                    limit = Duration::from_secs(drained.ceiling).saturating_add(patience.margin);
                    let running = |call| format!("`{name}` is running {call}");
                    drained.calls.into_iter().map(running).collect()
                }
                None => return predates(name, say),
            },
            Answered::Starting => vec![format!("`{name}` is starting")],
            Answered::TimedOut => vec![format!("`{name}` did not answer `drain`")],
            Answered::Down => return Ok(()),
        };
        if busy.is_empty() {
            return Ok(());
        }
        if waited.elapsed() >= limit {
            return Err(format!(
                "{} after {}s, so nothing was restarted for it",
                busy.join(" and "),
                limit.as_secs()
            ));
        }
        if told != busy {
            say(format!("waiting: {}", busy.join(" and ")));
            told = busy;
        }
        tokio::time::sleep(patience.poll).await;
    }
}

fn predates(name: &str, say: &mut dyn FnMut(String)) -> Result<(), String> {
    say(format!(
        "`{name}` runs a kelpie from before `drain`, so its restart cuts short any call it has \
         running"
    ));
    Ok(())
}

/// Sends `name` `undrain`, and says so for a message when it was answered
///
/// A failure to send it is let go: only a shepherd that cannot be reached
/// leaves the runner draining, and its restart ends that.
pub async fn undrain(client: &Client, name: &str) -> String {
    match trigger(client, name, "undrain", None).await {
        Ok(Answered::Runner(_)) => {
            ", and it was sent `undrain`, so it starts calls again on the old build".to_owned()
        }
        _ => String::new(),
    }
}
