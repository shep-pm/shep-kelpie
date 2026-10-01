//! The dog's side of the door: one task per command asking for `cargo-test`
//!
//! Each connection gets a [`Ticket`], asks once, and is answered at once
//! or when room opens. Whatever ends the connection, the command finishing
//! or dying or the dog stopping, its place is given back.

use std::collections::HashMap;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

use super::{Kept, lock_desk};
use crate::lease::counted::{CARGO_TEST, Taken, Taker, Ticket};
use crate::lease::door::{Answer, Ask};

// macOS refuses a socket path of 104 bytes or more.
const LONGEST_SOCKET: usize = 103;

// Waiters to wake when the lease grants them, by ticket.
type Waiters = Arc<Mutex<HashMap<Ticket, oneshot::Sender<()>>>>;

/// Opens the door at `socket`, only the maintainer's user may connect
///
/// A socket left by a dog that is gone is replaced. One another dog still
/// answers on is left alone.
///
/// # Errors
///
/// A message when the path is too long, taken by a live dog or something
/// else, or cannot be bound.
pub fn open(socket: &Path) -> Result<UnixListener, String> {
    if socket.as_os_str().len() > LONGEST_SOCKET {
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
            std::fs::remove_file(socket)
                .map_err(|e| format!("cannot remove {}: {e}", socket.display()))?;
        }
        Ok(_) => return Err(format!("{} is not a socket", socket.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot read {}: {e}", socket.display())),
    }
    let listener =
        UnixListener::bind(socket).map_err(|e| format!("cannot open {}: {e}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("cannot restrict {}: {e}", socket.display()))?;
    Ok(listener)
}

/// Answers every command that knocks at `listener`, for as long as it runs
pub async fn serve(listener: UnixListener, desk: Arc<Mutex<Kept>>) {
    let waiters = Waiters::default();
    let mut next = 0;
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                println!("the door could not take a connection: {e}");
                continue;
            }
        };
        next += 1;
        let place = Place {
            ticket: Ticket(next),
            desk: Arc::clone(&desk),
            waiters: Arc::clone(&waiters),
        };
        tokio::spawn(visit(stream, place));
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

async fn visit(stream: UnixStream, place: Place) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let Ok(Some(line)) = lines.next_line().await else {
        return;
    };
    let ask = match serde_json::from_str::<Ask>(&line) {
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
    let taken = lock_desk(&place.desk).desk.take_test(taker);
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
    while let Ok(Some(_)) = lines.next_line().await {}
    println!("{CARGO_TEST}: pid {pid} gave it back");
}

// A command that went away is heard as the connection's end.
async fn say(write: &mut OwnedWriteHalf, answer: &Answer) {
    let mut line = serde_json::to_vec(answer).expect("an answer serializes to JSON");
    line.push(b'\n');
    let _ = write.write_all(&line).await;
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::time::Duration;

    use super::*;
    use crate::dog::open as open_book;
    use crate::lease::cli::run_counted;
    use crate::lease::door::Visit;
    use crate::lease::gpu::GpuLock;
    use crate::lease::saved::BookFile;
    use crate::review_bot::Reviewers;
    use crate::test::FakeClock;

    const BOUND: Duration = Duration::from_secs(10);

    // A dog's door in its own folder, with `capacity` places.
    fn door(capacity: u32) -> (tempfile::TempDir, std::path::PathBuf, Arc<Mutex<Kept>>) {
        let dir = tempfile::tempdir().unwrap();
        let file = BookFile::new(dir.path().join("book.json"));
        let mut kept = open_book(
            file,
            Box::new(FakeClock::at(1_790_000_000)),
            GpuLock::under(dir.path()),
            Reviewers::default(),
        );
        kept.desk
            .set_test_capacity(NonZeroU32::new(capacity).unwrap());
        let desk = Arc::new(Mutex::new(kept));
        let socket = dir.path().join("lease.sock");
        let listener = open(&socket).unwrap();
        tokio::spawn(serve(listener, Arc::clone(&desk)));
        (dir, socket, desk)
    }

    fn ask(what: &str) -> Ask {
        Ask {
            take: CARGO_TEST.into(),
            pid: std::process::id(),
            what: what.into(),
        }
    }

    async fn answer(visit: &mut Visit) -> Option<Answer> {
        tokio::time::timeout(BOUND, visit.answer())
            .await
            .expect("the dog never answered")
            .unwrap()
    }

    fn holders(desk: &Mutex<Kept>) -> Vec<String> {
        let status = lock_desk(desk).desk.tests.status();
        status.holders.into_iter().map(|h| h.taker.what).collect()
    }

    #[tokio::test]
    async fn a_fourth_command_waits_until_one_of_three_gives_back() {
        let (_dir, socket, desk) = door(3);
        let mut held = Vec::new();
        for n in 1..=3 {
            let mut visit = Visit::knock(&socket, &ask(&format!("suite {n}")))
                .await
                .unwrap();
            assert_eq!(answer(&mut visit).await, Some(Answer::Granted), "suite {n}");
            held.push(visit);
        }
        let mut fourth = Visit::knock(&socket, &ask("suite 4")).await.unwrap();
        assert_eq!(answer(&mut fourth).await, Some(Answer::Queued(0)));
        drop(held.remove(1));
        assert_eq!(answer(&mut fourth).await, Some(Answer::Granted));
        assert_eq!(holders(&desk), ["suite 1", "suite 3", "suite 4"]);
    }

    // The holder is a real process killed outright, so nothing of it says goodbye.
    #[tokio::test]
    async fn a_holder_that_dies_gives_its_place_back() {
        let (_dir, socket, desk) = door(1);
        let exe = std::env::current_exe().unwrap();
        let mut holder = std::process::Command::new(&exe)
            .args([
                "--exact",
                "dog::door::tests::hold_the_door",
                "--ignored",
                "--nocapture",
            ])
            .env("KELPIE_TEST_DOOR", &socket)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let held = async {
            while holders(&desk).is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(BOUND, held)
            .await
            .expect("the holder never took the lease");
        let mut waiter = Visit::knock(&socket, &ask("waiter")).await.unwrap();
        assert_eq!(answer(&mut waiter).await, Some(Answer::Queued(0)));
        holder.kill().unwrap();
        holder.wait().unwrap();
        assert_eq!(answer(&mut waiter).await, Some(Answer::Granted));
        assert_eq!(holders(&desk), ["waiter"]);
    }

    // The process the test above kills: it takes the lease and holds it.
    #[tokio::test]
    #[ignore = "run by a_holder_that_dies_gives_its_place_back"]
    async fn hold_the_door() {
        let Some(socket) = std::env::var_os("KELPIE_TEST_DOOR") else {
            return;
        };
        let mut visit = Visit::knock(Path::new(&socket), &ask("holder"))
            .await
            .unwrap();
        assert_eq!(answer(&mut visit).await, Some(Answer::Granted));
        std::future::pending::<()>().await;
    }

    #[tokio::test]
    async fn a_waiter_that_hangs_up_leaves_the_queue() {
        let (_dir, socket, desk) = door(1);
        let mut first = Visit::knock(&socket, &ask("first")).await.unwrap();
        answer(&mut first).await;
        let mut gone = Visit::knock(&socket, &ask("gone")).await.unwrap();
        assert_eq!(answer(&mut gone).await, Some(Answer::Queued(0)));
        drop(gone);
        let mut next = Visit::knock(&socket, &ask("next")).await.unwrap();
        let queued = answer(&mut next).await;
        assert!(matches!(queued, Some(Answer::Queued(0 | 1))), "{queued:?}");
        drop(first);
        assert_eq!(answer(&mut next).await, Some(Answer::Granted));
        assert_eq!(holders(&desk), ["next"]);
    }

    #[tokio::test]
    async fn a_run_waits_for_room_and_keeps_its_commands_exit_code() {
        let (dir, socket, _desk) = door(1);
        let mut first = Visit::knock(&socket, &ask("first")).await.unwrap();
        answer(&mut first).await;
        let marker = dir.path().join("ran");
        let script = format!("touch {}; exit 7", marker.display());
        let run = tokio::spawn(async move { run_counted(&socket, &["sh", "-c", &script]).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!marker.exists(), "the command ran before it was granted");
        drop(first);
        let ran = tokio::time::timeout(BOUND, run)
            .await
            .expect("the run never ended")
            .unwrap();
        assert_eq!(ran, Ok(std::process::ExitCode::from(7)));
        assert!(marker.exists());
    }

    #[tokio::test]
    async fn any_other_lease_is_refused_at_the_door() {
        let (_dir, socket, _desk) = door(3);
        let other = Ask {
            take: "coderabbit".into(),
            ..ask("x")
        };
        let mut visit = Visit::knock(&socket, &other).await.unwrap();
        let Some(Answer::Error(why)) = answer(&mut visit).await else {
            panic!("the door let coderabbit in");
        };
        assert!(why.contains("only cargo-test"), "{why}");
        assert_eq!(answer(&mut visit).await, None);
    }

    #[tokio::test]
    async fn a_live_door_is_never_taken_over_and_a_dead_one_is() {
        let (dir, socket, _desk) = door(3);
        let err = open(&socket).unwrap_err();
        assert!(err.contains("another dog answers"), "{err}");

        let stale = dir.path().join("stale.sock");
        drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
        assert!(open(&stale).is_ok());
        let mode = std::fs::metadata(&stale).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
