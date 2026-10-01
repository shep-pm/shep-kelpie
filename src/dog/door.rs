//! The dog's side of the door: one task per command asking for `cargo-test`
//!
//! Each connection gets a [`Ticket`], asks once, and is answered at once
//! or when room opens. A command already running under a dog that went
//! away is seated at once, and for a [`REJOIN_GRACE`] after the door opens
//! only such commands are: the others wait, so the runs a restart left
//! going are counted before anyone new. Any line from a holder, or its
//! connection closing, gives its place back.

use std::collections::HashMap;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

use super::{Kept, lock_desk};
use crate::lease::counted::{CARGO_TEST, Taken, Taker, Ticket};
use crate::lease::door::{Answer, Ask};

// macOS refuses a socket path of 104 bytes or more.
const LONGEST_SOCKET: usize = 103;

/// How long a connection has to send its ask before the dog hangs up
const ASK_WITHIN: Duration = Duration::from_secs(10);

/// The longest ask the dog reads; a command line past this is cut short
const LONGEST_ASK: u64 = 64 * 1024;

/// How long the door rests after a failed accept, such as out of files
const ACCEPT_REST: Duration = Duration::from_millis(500);

/// How long after the door opens only running commands are seated. A run
/// whose dog went away knocks every second, so three seconds covers it.
pub const REJOIN_GRACE: Duration = Duration::from_secs(3);

// Waiters to wake when the lease grants them, by ticket.
type Waiters = Arc<Mutex<HashMap<Ticket, oneshot::Sender<()>>>>;

/// Opens the door at `socket`, only the maintainer's user may connect
///
/// A socket left by a dog that is gone is replaced. One another dog still
/// answers on is left alone. The socket is bound under a private name and
/// restricted before it takes its own, so it never accepts anyone else.
///
/// # Errors
///
/// A message when the path is too long, taken by a live dog or something
/// else, or cannot be bound.
pub fn open(socket: &Path) -> Result<UnixListener, String> {
    let private = socket.with_extension(format!("{}.new", std::process::id()));
    if private.as_os_str().len() > LONGEST_SOCKET {
        return Err(format!(
            "{} is too long a path for a socket",
            socket.display()
        ));
    }
    match std::fs::symlink_metadata(socket) {
        Ok(meta) if meta.file_type().is_socket() => {
            if std::os::unix::net::UnixStream::connect(socket).is_ok() {
                return Err(format!("another dog answers at {}", socket.display()));
            }
        }
        Ok(_) => return Err(format!("{} is not a socket", socket.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot read {}: {e}", socket.display())),
    }
    let _ = std::fs::remove_file(&private);
    let listener = UnixListener::bind(&private)
        .map_err(|e| format!("cannot open {}: {e}", private.display()))?;
    let placed = std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600))
        .and_then(|()| std::fs::rename(&private, socket));
    if let Err(e) = placed {
        let _ = std::fs::remove_file(&private);
        return Err(format!("cannot open {}: {e}", socket.display()));
    }
    Ok(listener)
}

/// Answers every command that knocks at `listener`, for as long as it runs,
/// granting nothing new for `grace` from now
pub async fn serve(listener: UnixListener, desk: Arc<Mutex<Kept>>, grace: Duration) {
    let fresh_from = tokio::time::Instant::now() + grace;
    let waiters = Waiters::default();
    let mut next = 0;
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
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
        };
        tokio::spawn(visit(stream, place, fresh_from));
    }
}

// One connection's hold on the lease, given back however the visit ends.
struct Place {
    ticket: Ticket,
    desk: Arc<Mutex<Kept>>,
    waiters: Waiters,
}

impl Drop for Place {
    fn drop(&mut self) {
        let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
        waiters.remove(&self.ticket);
        let granted = lock_desk(&self.desk).desk.leave_test(self.ticket);
        for ticket in granted {
            if let Some(wake) = waiters.remove(&ticket) {
                let _ = wake.send(());
            }
        }
    }
}

async fn visit(stream: UnixStream, place: Place, fresh_from: tokio::time::Instant) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut first = Vec::new();
    let mut limited = (&mut reader).take(LONGEST_ASK);
    let asked = limited.read_until(b'\n', &mut first);
    let Ok(Ok(1..)) = tokio::time::timeout(ASK_WITHIN, asked).await else {
        return;
    };
    let ask = match serde_json::from_slice::<Ask>(&first) {
        Ok(ask) if ask.take == CARGO_TEST => ask,
        Ok(ask) => {
            let why = format!(
                "{:?} is not a lease the door holds: only {CARGO_TEST}",
                ask.take
            );
            return say(&mut write, &Answer::Error(why)).await;
        }
        Err(e) => return say(&mut write, &Answer::Error(format!("not an ask: {e}"))).await,
    };
    let mut lines = reader.lines();
    if !ask.running {
        tokio::select! {
            () = tokio::time::sleep_until(fresh_from) => {}
            _ = lines.next_line() => return,
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
            _ = lines.next_line() => return,
        }
    }
    println!("{CARGO_TEST}: granted to pid {pid} running {what}");
    say(&mut write, &Answer::Granted).await;
    // Any line, or the connection's end, gives it back.
    let _ = lines.next_line().await;
    println!("{CARGO_TEST}: pid {pid} gave it back");
}

// A command that went away is heard as the connection's end.
async fn say(write: &mut OwnedWriteHalf, answer: &Answer) {
    let mut line = serde_json::to_vec(answer).expect("an answer serializes to JSON");
    line.push(b'\n');
    let _ = write.write_all(&line).await;
}

#[cfg(test)]
mod tests;
