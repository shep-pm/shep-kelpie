//! The dog's side of the door: one task per command asking for `cargo-test`
//!
//! Each connection gets a [`Ticket`], asks once, and is answered at once
//! or when room opens. A command already running under a dog that went
//! away is seated at once, and for a [`REJOIN_GRACE`] after the door opens
//! only such commands are: the others wait, so the runs a restart left
//! going are counted before anyone new. A holder's connection closing gives
//! its place back; a line on it does not.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

use super::e2e::Remote;
use super::{Kept, lock_desk};
use crate::ipc::{Listener, ServerReadHalf, ServerStream, ServerWriteHalf, split};
use crate::lease::counted::{CARGO_TEST, Taken, Taker, Ticket};
use crate::lease::door::{Answer, Ask, E2E, FORGE, LONGEST_LINE};

/// How long a connection has to send its ask before the dog hangs up
const ASK_WITHIN: Duration = Duration::from_secs(10);

/// How long the door rests after a failed accept, such as out of files
const ACCEPT_REST: Duration = Duration::from_millis(500);

/// How long after the door opens only running commands are seated. A run
/// whose dog went away knocks every second, so three seconds covers it, with
/// room for a knock that lands late.
pub const REJOIN_GRACE: Duration = Duration::from_secs(3);

// Waiters to wake when the lease grants them, by ticket.
type Waiters = Arc<Mutex<HashMap<Ticket, oneshot::Sender<()>>>>;

/// Opens the door at `socket`, only the maintainer's user may connect
///
/// On Unix a socket left by a dog that is gone is replaced, and one a live
/// dog answers on is left alone. It is bound under a private name and
/// restricted before it takes its own. On Windows the door is a named pipe
/// named from `socket`, writable only by its creator and administrators,
/// which a second dog cannot create.
///
/// # Errors
///
/// A message when the path is too long, taken by a live dog or something
/// else, or cannot be bound.
pub fn open(socket: &Path) -> Result<Listener, String> {
    crate::ipc::open_owner_only(socket, "dog", Listener::bind)
}

/// Answers every command that knocks at `listener`, for as long as it runs,
/// granting nothing new for `grace` from now
///
/// An `e2e` ask is placed by the worktree it names under `kelpie_home`, and
/// run through `remote`.
pub(super) async fn serve(
    mut listener: Listener,
    desk: Arc<Mutex<Kept>>,
    grace: Duration,
    kelpie_home: PathBuf,
    remote: Remote,
) {
    let kelpie_home = Arc::new(kelpie_home);
    let remote = Arc::new(remote);
    let fresh_from = tokio::time::Instant::now() + grace;
    let waiters = Waiters::default();
    let mut next = 0;
    loop {
        let stream = match listener.accept().await {
            Ok(stream) => stream,
            Err(e) => {
                println!("the door could not take a connection: {e}");
                tokio::time::sleep(ACCEPT_REST).await;
                continue;
            }
        };
        next += 1;
        let place = Place {
            ticket: Ticket(next),
            desk: Arc::clone(&desk),
            waiters: Arc::clone(&waiters),
            kelpie_home: Arc::clone(&kelpie_home),
            remote: Arc::clone(&remote),
            e2e_host: None,
        };
        tokio::spawn(visit(stream, place, fresh_from));
    }
}

// One connection's hold on the lease, given back however the visit ends.
pub(super) struct Place {
    pub(super) ticket: Ticket,
    pub(super) desk: Arc<Mutex<Kept>>,
    waiters: Waiters,
    pub(super) kelpie_home: Arc<PathBuf>,
    pub(super) remote: Arc<Remote>,
    // The host whose e2e lease this connection holds or waits for, if it is
    // an e2e ask; otherwise it is for cargo-test.
    pub(super) e2e_host: Option<String>,
}

impl Place {
    // Lists the waiter before it asks, so a grant cannot miss it.
    pub(super) fn list_waiter(&self, wake: oneshot::Sender<()>) {
        self.waiters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(self.ticket, wake);
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
        waiters.remove(&self.ticket);
        let mut kept = lock_desk(&self.desk);
        let granted = match &self.e2e_host {
            Some(host) => kept.desk.leave_e2e(host, self.ticket),
            None => kept.desk.leave_test(self.ticket),
        };
        drop(kept);
        for ticket in granted {
            if let Some(wake) = waiters.remove(&ticket) {
                let _ = wake.send(());
            }
        }
    }
}

async fn visit(stream: ServerStream, mut place: Place, fresh_from: tokio::time::Instant) {
    let (read, mut write) = split(stream);
    let mut reader = BufReader::new(read);
    let mut first = Vec::new();
    let mut limited = (&mut reader).take(LONGEST_LINE);
    let asked = limited.read_until(b'\n', &mut first);
    let Ok(Ok(1..)) = tokio::time::timeout(ASK_WITHIN, asked).await else {
        return;
    };
    let ask = match serde_json::from_slice::<Ask>(&first) {
        Ok(ask) if ask.take == CARGO_TEST => ask,
        Ok(ask) if ask.take == E2E => {
            return super::e2e::visit(&mut place, ask, &mut reader, &mut write).await;
        }
        Ok(ask) if ask.take == FORGE => {
            return super::forge::visit(&place, ask, &mut write).await;
        }
        Ok(ask) => {
            let why = format!(
                "{:?} is not a lease the door holds: only {CARGO_TEST} and {E2E}",
                ask.take
            );
            return say(&mut write, &Answer::Error(why)).await;
        }
        Err(e) => return say(&mut write, &Answer::Error(format!("not an ask: {e}"))).await,
    };
    if !ask.running {
        tokio::select! {
            () = tokio::time::sleep_until(fresh_from) => {}
            _ = heard(&mut reader) => return,
        }
    }
    let (wake, woken) = oneshot::channel();
    let taker = Taker {
        ticket: place.ticket,
        pid: ask.pid,
        what: ask.what,
    };
    let (pid, what) = (taker.pid, taker.what.clone());
    // The waiter is listed before it asks, so a grant cannot miss it.
    place
        .waiters
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(place.ticket, wake);
    let taken = {
        let mut kept = lock_desk(&place.desk);
        if ask.running {
            kept.desk.seat_test(taker)
        } else {
            kept.desk.take_test(taker)
        }
    };
    if let Taken::Queued { ahead } = taken {
        say(&mut write, &Answer::Queued(ahead)).await;
        tokio::select! {
            _ = woken => {}
            // Anything but a grant first, even a line, ends the wait.
            _ = heard(&mut reader) => return,
        }
    }
    let waited = matches!(taken, Taken::Queued { .. });
    if let Some(line) = granted_after_wait(
        taken,
        format!("{CARGO_TEST}: granted to pid {pid} running {what}"),
    ) {
        println!("{line}");
    }
    say(&mut write, &Answer::Granted).await;
    // Only the connection's end gives it back: a descendant of the command
    // holds the connection too, and a line from it is not the command ending.
    // What it writes is dropped as it arrives, never kept.
    while heard(&mut reader).await {}
    if waited {
        println!("{CARGO_TEST}: pid {pid} gave it back");
    }
}

// The line for a lease granted, when the ask had to wait for it: one granted
// at once says nothing new, and the book holds the rest.
pub(super) fn granted_after_wait(taken: Taken, line: String) -> Option<String> {
    match taken {
        Taken::Queued { ahead } => Some(format!("{line}, after {ahead} ahead of it")),
        Taken::Granted => None,
    }
}

// Whether the connection sent anything, which it discards: a fixed buffer
// bounds what the dog holds however much a holder writes.
pub(super) async fn heard(reader: &mut BufReader<ServerReadHalf>) -> bool {
    let mut sink = [0; 1024];
    matches!(reader.read(&mut sink).await, Ok(1..))
}

// A command that went away is heard as the connection's end.
pub(super) async fn say(write: &mut ServerWriteHalf, answer: &Answer) {
    let mut line = serde_json::to_vec(answer).expect("an answer serializes to JSON");
    line.push(b'\n');
    let _ = write.write_all(&line).await;
}

#[cfg(test)]
mod tests;
