# Queued verbs, the board read and trigger threads

The pieces around the runner's lock that wait for it least. #1201, and #1288 for the threads. The rest of the lock: runner-lock.md.

## Queued verbs

`runner/queued.rs`. `one_pass` takes the queue right after `take_posted`, so a verb waits for at most the pass that was running.

- A verb splits into a guard check, a read and an apply. `add`, `rework`, `adopt` and `triage` check under the lock, ask the forge with it let go (the issue, the pull request or the open issues), then take it again, check once more and apply. Refusals read as the verb's own (`add_room`, `add_allowed`, `add_found`), plus two: `add` refuses a closed issue, and one that carries no `ready-for-agent` where it is queued rather than opened. An `add` whose guard answers `Room::Queues` reads its issue the same way and clears `dropped` instead of opening a work item (board.md). #1367
- A refusal goes to `Notes::standing` under the verb's key (`add 7`): logged once, listed in `status` under `standing`, and ended when that verb next goes through.
- `rule` applies under the lock: its forge reads sit inside `Runner::rule`. `rework` and `adopt` read the pull request off the lock, but what `start_rework` and `adoptable` read after it (the viewer, the issue, the review bots, the branch's head) is still under it.
- A queued verb lives only in memory, so a handover waits for its pass. `Handing::attempt` closes the queue under the lock (`Posted::close_queue`, which refuses while a verb waits) and opens it when the exec is made or refused. A verb that finds it closed waits for the lock.
- A queued verb wakes the loop (`Posted::needs_wake`).

## The board read

`runner/board_read.rs`: `read_board` runs in `one_pass` after the reads above and before the rotation. `poll_job` takes the lock and, if the board may open a work item, its look is due and the forge is not held, copies what the read needs into a `BoardJob`. `BoardJob::run` reads with the lock let go. `store_board_read` takes the lock again and keeps the result for the pass.

- The job reads in the pass's old order: the ready issues, the open pull requests, each unlisted finished ticket's `issue`, then the parents when the board lists none to close. A failed read ends the rest. `forge.jsonl` is unchanged.
- The open-pull-request listing goes into the `Snapshot` as a preload, which `begin_pass` keeps, so the gate, merge and rework steps serve from it.
- `dispatch` takes the stored read and only then stamps the look (`reading_board`). A failed read stamps nothing and `stepped` gives the slot its retry: 15 seconds, doubling to 10 minutes, as `main_ci_read` does. `trim_finished`, `close_done_parent`, `close_issue` and every save stay on the lock.
- A read the pass did not use is dropped at `end_pass`.
- A read is also dropped, with its listing, when a write of kelpie's came after it (`forge_ledger.writes()` changed). A board that opens after the pre-pass found it not due reads in place.
- It follows `main`'s CI, the dropped tickets and the bumps, so they learn a used-up limit first, and `poll_job` reads nothing while the forge is held.

## Trigger threads

Each trigger runs on a handler thread of its own since shep 0.17.

### Handlers

- shep-channel runs each action handler on a thread, up to 32 at once. A trigger that waits for the runner's lock holds only its own thread, so a `status` sent after it is not held behind it.
- The posted copies stay. `status` would wait on the same lock, past shep's `action_timeout`.
- `status_sent_after_a_trigger_that_waits_for_the_pass_is_answered_at_once` in `crates/runner/src/runner/trigger/tests/mod.rs`: `finish` waits on a held lock on one thread, and a `status` on another is answered at once.
