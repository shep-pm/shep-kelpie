# The verbs' side of the socket

The client that dials a runner's socket, and the verbs that ask it there or fall back to `shep trigger`. The runner's own side, the watch and opening the socket: runner-socket.md. ADR 0017.

## The client

`crates/client`, package `shep-kelpie-client`, using `protocol` and `core` (for `ipc`).

- `connect(socket, deadline)` makes the hello. A socket that is missing or refused, a runner silent at the hello, one that answers no hello, and one below this protocol are one outcome, `Unreachable`.
- `Connection::ask(&Request, deadline)` gives a `Reply` (a refusal is `Reply::Refused`), or a `Failure`: `TimedOut`, `Unknown` (the runner has no such request) or `Broken`.
- `watch(socket, deadline)` sends `watch` and reads the first status before it returns, so a runner that predates it fails there (`WatchError::Failed(Failure::Unknown)`). `Watching::next()` gives the current status first, then each change, and `None` once the runner ends the stream.
- A timeout spends the connection. A later ask sends nothing, since an answer may still arrive and would be read as the next ask's.

## The verbs

`src/flock/control/socket.rs`. A `Reach` holds kelpie's home and a once-flag, and `Reach::trigger` has `control::trigger`'s signature and returns its `Answered`, with the reply written as the JSON text the verbs parse. Output is unchanged, plain and `--json`.

- `status`, `rule`, `tell`, `tell_all`, `send` and `change` take `&Reach`. That covers `board`, `timings`, `gate`, `drop`, `rework`, `adopt`, `triage`, `add <ticket>`, `drain`, `undrain`, `attach` and `pm`. `tell_all` holds the one `Reach` across the projects, so its fallback line is said once per command.
- `start`, `finish`, `finish_all`, `pause` and `src/upgrade/` keep `control::trigger`, since they must reach a stopped or older runner through the shepherd.
- `Unreachable` and `Unknown` take the one-release path (`Reach::fallback`, the one function to delete): one line to stderr per command naming the socket path, then `shep trigger`.
- `TimedOut` is `Answered::TimedOut` and never falls back, since the runner may have done the request. A connection that breaks after the request is sent is an error, for the same reason.
- Text `request_of` refuses is answered `{"error": ...}` without a connection, as `shep trigger` would.

## Tests

- `flock::control::tests::socket` drives the verbs against a real runner's socket, against a scripted socket and the shepherd giving the same bodies, and against a socket that never answers.
