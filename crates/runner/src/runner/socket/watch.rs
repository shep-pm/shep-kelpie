//! A connection that watches: the status now, then again after each change
//!
//! The connection's thread writes, and one more reads what the peer sends, only
//! to learn that it closed. Neither takes the runner's lock: the writer waits on
//! [`Posted`](super::Posted)'s snapshot and writes the newest, so a client that
//! reads slowly skips the snapshots it missed and none queues.

use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Handles, Stream};

/// Writes the status to `stream` until a write fails, the peer closes or `ending` is set
pub(super) fn serve(stream: &Stream, handles: &Handles, ending: &AtomicBool) {
    // A watcher says nothing after its request, so silence is no reason to close it.
    if stream.set_read_timeout(None).is_err() {
        return;
    }
    let gone = AtomicBool::new(false);
    let mut watcher = handles.postings.watch();
    std::thread::scope(|scope| {
        let reading = std::thread::Builder::new()
            .name("socket-watch-peer".into())
            .spawn_scoped(scope, || {
                until_closed(stream);
                gone.store(true, Ordering::SeqCst);
                handles.postings.wake_watchers();
            });
        if reading.is_err() {
            return;
        }
        let mut write = stream;
        let quit = || ending.load(Ordering::SeqCst) || gone.load(Ordering::SeqCst);
        while let Some(line) = watcher.next(quit) {
            if write.write_all(line.as_bytes()).is_err() {
                break;
            }
        }
        // Ends the read that waits on the peer, which the scope joins.
        let _ = stream.shutdown(Shutdown::Both);
    });
}

// Reads and drops whatever the peer sends, until it closes or the read breaks.
fn until_closed(mut stream: &Stream) {
    let mut ignored = [0_u8; 256];
    loop {
        match stream.read(&mut ignored) {
            Ok(0) => return,
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}
