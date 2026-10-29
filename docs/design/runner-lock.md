# The runner's lock

One `Mutex<Runner>` per project. #1201. The queued verbs and the board read: runner-lock-passes.md.

## What it covers

- Everything the runner knows: the state file, work items, forge and model ports. A pass takes it (`runner/trigger/lock`) and holds it through its calls, which wait behind the login's pace. The reads before the rotation and a step's reads do not: see Off the lock and The rotation.
- `status`, `drain`, `undrain`, `timings` and `board` do not wait: `answer` tries the lock and, while a pass holds it, answers from the copy the loop last posted (status, drain state, board listing, finished history). `board move` and `board top` write, so wait.
- `add`, `rework`, `adopt`, `triage` and `rule` do not wait either. While a pass holds the lock, `answer` queues the verb on `Posted` and replies with the posted status. `rule` is checked against the posted rulings first: an id none waits on is refused in the reply. Every other trigger waits.

## What the metric shows

- `trigger::lock` and `answer` return a `Held` guard (`runner/lock_hold.rs`) that derefs to the `Runner`. On drop it records how long the lock was held against the `file:line` of the call that took it. `#[track_caller]` does the keying, so a function that calls `lock` must not carry it.
- `Holds` keeps the longest hold per line, which `status` shows as `lock` (seconds, by `file:line`).
- `send_metrics` sends what was recorded since its last send: `kelpie_runner_lock_held_seconds_total` and `kelpie_runner_lock_holds_total{le="1"}` and `{le="+Inf"}` (`crates/core/src/metrics/lock.rs`).
- A hold over 5s (`LONG`) is also told once it ends, in the log: `the runner's lock was held 18.2s by <file>:<line>`. The line is the site's own, so the next long hold is found without `status --json`.
- A long hold names the call to move off the lock. The sheep's own `lock()` calls (`src/sheep/`) are not timed.

## Off the lock

`runner/off_lock.rs`: `ItemKey` is a work item's `issue`, `session`, `turn`, `phase` and `pull_request` when the work began. `with_item` takes the lock, returns `None` if the item is gone or any of them changed, and otherwise sets `focus` to it again and applies the answer.

These run with the lock let go in `one_pass` before the rotation, each a job that copies what it needs, reads, and applies on the lock:

- Main's CI (`main_ci/read.rs`): `still_red` is worked out again at the apply.
- Parked items' pull requests and issues (`closed/read.rs`): one read per item.
- Dropped tickets (`dispatch/mod.rs`) and the bumps (`dependencies/read.rs`).
- Each stamps its `looks` or `parked_reads` entry at the apply, so a failed read is made again next pass.

Calls that left the lock in #1306, each made with it let go and applied on it after a recheck of the state:

- `read_rate_limit` (`timings/mod.rs`): the login's rate limits are read in `one_pass`, and the note and the dog's copy are applied after.
- Main's CI reruns and failed-log reads (`main_ci/read.rs`): the plan of which workflows are rerun or read is made on the lock, the calls off it (`Acts`), and `act_on_reds` applies the answers. A workflow with no answer waits for the next read.
- The held rulings' comments (`pm/mod.rs`, `ruling/post.rs`): `post_released` takes the posts as `PostJob`s, sends them off the lock and keeps each comment's id after. A ruling settled while its post is in flight has its comment edited once it is up. A failed post is told in the notes.
- The bumps' writes (`dependencies/acts.rs`): rebase, merge, comment, close and filing the red bump's issue are `Act`s that `work_bump` returns, `Act::run` makes off the lock and `apply_act` records. `record_proof` still settles its acts in place.
- A turn's end (`turn/end_reads.rs`): for a worker on `origin`, `hear_news` reads the branch's head, the worktree's head, its uncommitted files and the open pull requests as one `EndJob`, and `end_turn` takes each where it asked. A fork worker pushes before it reads, so its end reads in place.

The triage poll (`runner/triage/poll_read.rs`, #1204) has the same shape without a work item:

- `Runner::triage_poll_job` returns none unless `triage.pick` is not `off` and the look is due (`Looks::triage_due`). The run lists the open issues, reads each local wait's issue, and lists the open pulls only where used.
- The apply stamps the look (`triage_listed`) and does every write from the state it finds then. A failed listing stamps nothing.
- The listing and pulls stay as `triage_run.listing`, which `triage_job` starts the triager from. A `triage` verb sent during the read (`Looks::triage_asked`) makes the apply keep none and stamp nothing.
- Still on the lock: the reads in `ask.rs`, `comment.rs`, `limits.rs`, `addressed.rs`.

## The rotation

`runner/flight/rotation.rs`: `begin_rotation(&Mutex<Runner>)` takes the lock once to open the pass and plan it, then once for each slot's step and the call it starts. `snapshot.end_pass()` runs however it ends.

- A step that needs a read returns `Begin::Reads(job)` (`turn/reads.rs`), built from cloned `Arc`s, owned ids and an `ItemKey`. The rotation lets go, runs it, and takes the lock with `with_item`.
- A changed or dropped item gives an idle slot. Otherwise the answer goes into the pass's `Reads`, errors included, and the step is made again, taking the answer or reading in place where the store has none. State is read after the gap and drain is checked then, so a drain that lands in the gap starts no call.
- Reads that ship: the pull request at the top of `check_ci`, which lists the open pulls first when the pass has listed none; the review head (`review/reads.rs`); a bot's activity (`review_bot/bot_reads.rs`); `talk_turn`'s comments; the gate's and merge's pull request, failed logs and queue standing (`gate/reads.rs`); `origin_head`, `base_of` and `fence::changed` (`git_reads.rs`). The gate's `mark_ready` and `merge`, with the reads that settle a lost answer, are jobs too (`merge/call.rs`), so the merge no longer holds the lock. A step's reads are kept until it ends, so a step with several misses is made again over the earlier ones. A failed git read is kept as the inline call returned it, a base by its head. `talk_turn` tells a failed read once (`Reads::talked`).
- A turn's start (`Read::Start`, #1210) is one job: the worker token, the first prompt's issue, then the maintainer's comments, each only where the turn takes it, none after a failed one. `start_job` is asked once the drain, token wait and slot let the turn start, so a turn that does not start mints no token. Its worktree is a job after it.
- Usage (`Read::Usage`): a work item's `pace` reads the meter as a job. The board's read reads it for dispatch when a ready issue is not open. `pace_now` (project manager) reads in place.
- The project manager's wake (`pm/ci_logs.rs`, #1210) reads `failed_run_logs` per still-red item, `run_log_tail` per red workflow, and `tickets.issue` per standing line whose ticket end the board cannot answer, in one job, and writes the files and the standing notes under the lock. `pm_ended` and `pm` wakes read in place.
- Still on the lock: `labelled`, the notice a merge comments (`merged`), `visibility`, `stopped_short`'s `holds_work`, `origin_head` in `adopt`, `ruling`, `rework`, forge writes and their saves, `contributions/`. Worktree jobs: runner-lock-worktrees.md.
- `turn_ended` returns `Ended::StartOver(ticket)`, and `hear_news` runs `rotation::start_over` after it drops the lock.

## Still on the lock after #1306

These make forge or git calls with the lock held, and are the next to measure:

- A turn's end: `open_handed_over` and `edit_handed_over` (the pull request's create and edit), the `turn_end` and `handover` hooks, `push_to_fork` and the ruling comment of `fill_comment_failed`.
- The other arms of `hear_news`: `label_ended`, `security_heard`, `triage_heard`, `proof_heard`, `retro_heard`, `pm_ended`, `end_review`.
- Rulings raised and posted in place (`raise`, `ask_again`, `closed/read.rs`), `voice.post` in `pm/rulings.rs` and `review/disputes.rs`, `rerun_run` outside the polls (`gate/queued`, `ruling/act.rs`), `close_merged.rs`, `rework`.
