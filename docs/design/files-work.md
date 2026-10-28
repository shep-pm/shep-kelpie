# Design: Work and the runner

Part of the design index, `README.md`. Read the rows for the code you touch, then those files.

| file | covers | touch (`core/`: `crates/core/src/`, `dog/`: `crates/dog/src/dog/`, `adapters/`: `crates/adapters/src/`, `runner/`: `crates/runner/src/runner/`) |
|---|---|---|
| work.md | projects, slots, issues | `src/{issues/check,flock/verbs}.rs`, `runner/slots.rs`, `core/board/check.rs` |
| board.md | board, items, adoption | `core/board/`, `runner/{dispatch,adopt}/` |
| pm.md | the project manager | `runner/pm/` |
| pm-board.md | its board | `core/board/briefing/`, `runner/briefing/` |
| workers.md | runner loop, turns | `runner/{turn,flight}/` |
| runner-lock.md | lock, hold metric | `runner/{lock_hold,off_lock}.rs` |
| runner-lock-passes.md | queued verbs, threads | `runner/{queued,board_read,trigger}` |
| runner-socket.md | socket, client, fallback | `runner/socket/`, `crates/client/`, `src/flock/control/socket.rs` |
| runner-lock-worktrees.md | worktree jobs | `runner/worktree_jobs.rs` |
| compaction.md | when a session compacts, `COMPACT` | `runner/compact/`, `adapters/{pi/compact,goose/compact,codex/context}.rs` |
| handover.md | in-flight turns handed on | `src/{sheep/handover/,upgrade/handover.rs}`, `runner/flight/carry/`, `adapters/process/adopt/` |
| local-tickets.md | local ticket store, `ticket` verbs | `core/local_tickets/`, `src/flock/ticket.rs`, `runner/close_merged.rs` |
