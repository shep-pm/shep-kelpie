# The runner's socket

A runner answers the typed protocol on `run/<project>.sock` under kelpie's home, and the verbs ask it there first. `shep trigger` stays and answers the same JSON. The decision: ADR 0017. The types: `crates/protocol/`.

## The listener

`runner/socket/mod.rs`, opened by `src/sheep/mod.rs` once the runner is built and ended, with its file removed, after the stop message.

- The runner has no tokio. `ipc::blocking` accepts on one thread, and each connection is a thread of its own, up to `MOST_CONNECTIONS` (32); the next is closed.
- A connection sends a hello (`{"protocol":1}`) and is answered with the runner's, then asks one request a line and gets one reply a line. A first line that is no hello closes it.
- Everything is bounded: a hello within 10 s, 5 minutes of silence between requests, a line of at most `LONGEST_FRAME` (64 KiB), a write that takes over 10 s. Each closes the connection. The bounds guard the serving side: the client reads a reply of up to 64 MiB, and refuses to send a request over `LONGEST_FRAME` (`Failure::TooLong`, no fallback).
- An unknown request gets `{"request": ..., "version": ...}` and the connection stays open. Params that do not read are a refusal (`{"error": ...}`), not unknown.
- A reply is `trigger::answer_request`, the function `shep trigger` runs through `answer`, so the two cannot drift. It answers `status`, `timings` and the board listing from `Posted` while a pass holds the runner's lock. `request_of` is the one reader of `<action> [params]` text, exported for the client's callers.
- A request wakes the loop as `on_trigger` does: `wakes(action)` or `Posted::needs_wake`, through `Handles::wake`.

## The watch

`runner/socket/watch.rs`. A `watch` request turns the connection into a stream: the full status now, then again after each change, each a line `{"event":"status","status":{...}}` (`Event::Status`). It is not in `ACTIONS`, wakes no pass, and `shep trigger` cannot send it. An older runner answers the unknown-request error, and the client's `watch` names it as too old; the protocol stays at 1.

- `Posted` keeps the latest snapshot, a sequence number and a condvar. `Posted::post` and `Runner::post` move it. The snapshot is the protocol `Status` that `trigger::wire` builds, and it is built only while a client watches, so a runner with none pays nothing per post. The first snapshot of a watch, made before any post since the watch began, is the posted body read back.
- `draining` is filled in as `status_of` does. A `drain` or `undrain` that lands while a pass holds the lock changes `status` with no post, so `ask_drain`, `ask_undrain` and `set_draining` publish too.
- Merge rule: a watcher writes an event only when its line differs from the last it wrote. Whole snapshots are compared and nothing is stripped, so `timings` and `lock`, which move with the clock, make fewer merges on a real clock than on the rig's stand-in. A field the wire leaves out (a skill's plugin path) cannot tell two snapshots apart.
- Each watcher is two threads. One waits on the condvar and writes the newest snapshot, so a slow client skips the ones it missed and nothing queues. The other reads the peer's half, so a client that closes ends both. A failed write and `Serving::end` end them too. Neither holds the runner's lock while it writes.
- The watch has no idle bound: a connection that watches is silent by design.

## Opening it

`core/ipc` `open_owner_only` is the one routine for the dog's door and the runner's socket. The runner makes `run/` 0700 first.

- A private name (`<socket>.<pid>.new`), mode 0600, then a rename, so no client reaches a half-made socket.
- A socket nothing answers at is replaced. One that answers is never replaced: the message names `another runner answers at <path>`, and the door's says `dog`.
- A path over 103 bytes is refused before the bind by `home::socket_fits`, not by `sockets_fit`, which refuses a runner's start. The runner logs one line to stderr and goes on without a socket, and its verbs use `shep trigger`.
- A std socket is close-on-exec, so after a handover's exec the new image binds again and replaces the stale file.
- On Windows the same call binds a named pipe whose name `ipc::address` makes from the path. Only the non-blocking `windows` job compiles it, as `cargo check`; no test runs it.

## The client

`crates/client`, package `shep-kelpie-client`, using `protocol` and `core` (for `ipc`).

- `connect(socket, deadline)` makes the hello. A socket that is missing or refused, a runner silent at the hello, one that answers no hello, and one below this protocol are one outcome, `Unreachable`.
- `Connection::ask(&Request, deadline)` gives a `Reply` (a refusal is `Reply::Refused`), or a `Failure`: `TimedOut`, `Unknown` (the runner has no such request) or `Broken`.
- `watch(socket, deadline)` sends `watch` and reads the first status before it returns, so a runner that predates it fails there (`WatchError::Failed(Failure::Unknown)`). `Watching::next()` gives the current status first, then each change, and `None` once the runner ends the stream.
- A timeout spends the connection. A later ask sends nothing, since an answer may still arrive and would be read as the next ask's.

## The verbs

`src/flock/control/socket.rs`. A `Reach` holds kelpie's home and a once-flag, and `Reach::trigger` has `control::trigger`'s signature and returns its `Answered`, with the reply written as the JSON text the verbs parse. Output is unchanged, plain and `--json`.

- `status`, `rule`, `tell`, `send` and `change` take `&Reach`. That covers `board`, `timings`, `gate`, `drop`, `rework`, `adopt`, `triage`, `add <ticket>`, `drain`, `undrain`, `attach` and `pm`.
- `start`, `finish`, `finish_all`, `pause` and `src/upgrade/` keep `control::trigger`, since they must reach a stopped or older runner through the shepherd.
- `Unreachable` and `Unknown` take the one-release path (`Reach::fallback`, the one function to delete): one line to stderr per command naming the socket path, then `shep trigger`.
- `TimedOut` is `Answered::TimedOut` and never falls back, since the runner may have done the request. A connection that breaks after the request is sent is an error, for the same reason.
- Text `request_of` refuses is answered `{"error": ...}` without a connection, as `shep trigger` would.

## Tests

- `runner::socket::tests` serve a rig's runner under `short_tempdir()`, never the rig's own home, whose path can pass 103 bytes under a worker's `TMPDIR`. A dead socket is made by a child process that binds and exits, so no fork of the test holds the listener.
- `flock::control::tests::socket` drives the verbs against a real runner's socket, against a scripted socket and the shepherd giving the same bodies, and against a socket that never answers.
