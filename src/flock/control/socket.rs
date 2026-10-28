//! The verbs' way to a runner: its own socket first, `shep trigger` when it has none
//!
//! [`Reach::trigger`] has the signature of [`trigger`](super::trigger) and gives
//! the same [`Answered`], with the reply written as the JSON text the verbs
//! already read. A socket that cannot be reached (missing, refused, silent at
//! the hello, or on an older protocol) falls back to the shepherd, and says so
//! once per command. A timeout never falls back: the runner may still take the
//! request, and asking again would do it twice.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use shep_client::TRIGGER_DEADLINE;
use shep_kelpie_client::{Failure, connect};
use shep_kelpie_protocol::Reply;

use super::{Answered, trigger};
use crate::home::socket_of;
use crate::runner::request_of;
use crate::shepherd::Connection;

/// How long the connect and the hello may take before the runner counts as unreachable
const CONNECT_WITHIN: Duration = Duration::from_secs(5);

/// Where the verbs of one command reach a runner from, and whether the
/// fallback to `shep trigger` has been said yet
pub struct Reach {
    kelpie_home: PathBuf,
    deadline: Duration,
    said: AtomicBool,
    say: Box<dyn Fn(&str) + Send + Sync>,
}

impl std::fmt::Debug for Reach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reach")
            .field("kelpie_home", &self.kelpie_home)
            .field("deadline", &self.deadline)
            .field("said", &self.said)
            .finish_non_exhaustive()
    }
}

impl Reach {
    /// Reaches the runners whose sockets sit under `kelpie_home`, saying on stderr
    #[must_use]
    pub fn new(kelpie_home: &Path) -> Self {
        Self::saying(kelpie_home, |line| eprintln!("{line}"))
    }

    /// [`Reach::new`], with the fallback's line going to `say`
    #[must_use]
    pub fn saying(kelpie_home: &Path, say: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self {
            kelpie_home: kelpie_home.to_owned(),
            deadline: TRIGGER_DEADLINE,
            said: AtomicBool::new(false),
            say: Box::new(say),
        }
    }

    /// [`Reach`] that waits `deadline` for a runner's answer, in place of
    /// `shep trigger`'s own
    #[cfg(test)]
    #[must_use]
    pub(crate) fn waiting(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// What `sheep` made of `action`, asked on its socket, and of `shep trigger`
    /// when it has none
    ///
    /// # Errors
    ///
    /// A message when the connection to the runner broke after the request was
    /// sent, or when the fallback's trigger cannot be sent.
    pub(crate) async fn trigger(
        &self,
        client: &dyn Connection,
        sheep: &str,
        action: &str,
        params: Option<&str>,
    ) -> Result<Answered, String> {
        // The runner refuses text it cannot read in these words over either path.
        let request = match request_of(action, params) {
            Ok(request) => request,
            Err(why) => return Ok(Answered::Runner(body(&Reply::Refused(why)))),
        };
        let socket = socket_of(&self.kelpie_home, sheep);
        let mut connection = match connect(&socket, CONNECT_WITHIN).await {
            Ok(connection) => connection,
            Err(unreachable) => {
                let why = unreachable.to_string();
                return self
                    .fallback(client, &socket, &why, sheep, action, params)
                    .await;
            }
        };
        match connection.ask(&request, self.deadline).await {
            Ok(reply) => Ok(Answered::Runner(body(&reply))),
            Err(Failure::TimedOut) => Ok(Answered::TimedOut),
            Err(Failure::Unknown(unknown)) => {
                let why = unknown.to_string();
                self.fallback(client, &socket, &why, sheep, action, params)
                    .await
            }
            Err(too_long @ Failure::TooLong { .. }) => {
                Err(format!("cannot ask {sheep} `{action}`: {too_long}"))
            }
            Err(Failure::Broken(why)) => Err(format!(
                "cannot ask {sheep} `{action}` on {}: {why}",
                socket.display()
            )),
        }
    }

    // THE ONE-RELEASE PATH (ADR 0017): `shep trigger` for a runner with no socket
    // to reach, which is a runner from before this one or one that is stopped.
    // Delete this function and its two calls to remove it.
    async fn fallback(
        &self,
        client: &dyn Connection,
        socket: &Path,
        why: &str,
        sheep: &str,
        action: &str,
        params: Option<&str>,
    ) -> Result<Answered, String> {
        if !self.said.swap(true, Ordering::SeqCst) {
            (self.say)(&format!(
                "no runner answers on {} ({why}), so this command uses `shep trigger`",
                socket.display()
            ));
        }
        trigger(client, sheep, action, params).await
    }
}

// A reply as the JSON text `shep trigger` returns.
fn body(reply: &Reply) -> String {
    serde_json::to_string(reply).expect("a reply serializes to JSON")
}
