# Transport test series

The first of three test series (transport, model calibration, work split) that run before any kelpie code. This one decides how kelpie drives a worker, how a worker gets woken, how the maintainer takes one over, and whether kelpie's lease traffic can go through shep. The design log's "Waiting on the tests" list is what it answers.

## Setup

- A dedicated shepherd: shep v0.10.1, the latest release, installed apart from any dev build. `SHEP_HOME` must be short, because macOS caps a socket path at 104 bytes and the control socket lives at `$SHEP_HOME/run/shep.sock`. `~/.kelpie/shep` fits.
- A worker repo: a detached worktree of shep at a pinned commit, so every floor includes a real `CLAUDE.md`. Workers in these tests never push.
- Workers run on Sonnet unless a test says otherwise. Prompts are trivial except where a test needs context to grow.
- A worker's stdin is either closed or carries its prompt. A background `claude -p` with an open stdin can wait for EOF before it sends anything.
- The runner prototype is a Python sheep with `channel = true`, speaking the shepherd channel on fd 3. The channel is newline JSON and language-agnostic, so no kelpie code is needed yet.
- The dog side of test L is a small Rust program on `shep-client`, because the shep CLI has no verb that watches the bus.
- Every worker call appends one JSON line to `experiments/transport/results/<test>.jsonl`: test id, run number, timestamp, `claude --version`, model, the call's `usage`, `total_cost_usd`, wall clock, and whatever the test adds. `total_cost_usd` is cumulative across a resumed session, so diff it rather than summing it.
- Weighted units: cache read 0.1, one-hour cache write 2, output 5, uncached input 1.
- Each measured test runs three times. Report the median and the spread, never one run.

## Tests

### T1: one `claude -p` process per turn

The measured baseline. Each turn is a lamb of the runner.

1. Start turn 1 with `claude -p --output-format json`, choosing the id up front with `--session-id` if that hidden flag does what its name says.
2. Run ten `--resume` turns, each reading a file of about 3k tokens so context grows.
3. Repeat with the system prompt passed through `--append-system-prompt-file` on the first call only, as Paperclip does.
4. Repeat with the runner killed and restarted between turns 5 and 6.

Measure: cache write and read per turn, time to first token and duration per turn, and what a restart loses. Find out why the first `--resume` onto a fresh session rewrote about 29k of cache in the earlier measurement: hooks, the git status block, or something else.

### T2: a long-lived stream-json worker

1. The runner spawns `claude --input-format stream-json --output-format stream-json --verbose` as its child and writes each user turn as one JSON line.
2. Run the same ten turns as T1.
3. Leave the worker idle for ten minutes, then send a turn.
4. Make a tool return more than 100 KB in one line, and check the runner receives it whole.
5. Send `/compact` as a user turn and check what comes back.
6. Kill the runner mid-turn, resume the session in a fresh child with `--resume`, and record what was lost.

Measure: cache write and read per turn against T1, latency on the turn after the idle gap, the large line, and the cost of the crash.

### T3: a background session steered over its socket

Needs a permission rule, added by the maintainer, that lets the test read `~/.claude/sessions/<pid>.*.key` for sessions the test itself started.

1. Start a session with `claude --bg` in the worker repo and let it go idle.
2. Send a user message over its socket, with the auth line first.
3. Check through `claude agents --json` and the transcript whether a turn started.
4. `claude attach` to it, type a turn, detach, then send another message over the socket.

Measure: whether a message wakes an idle session, how long it takes, cost per turn against T1, and whether attaching conflicts with injected turns.

### T4: tools kelpie serves to a worker

1. A stdio MCP server in Python, loaded with `--mcp-config`, exposing one tool (`lease_status`) that asks the runner over shep and returns the answer.
2. With T2's worker only: an in-process MCP server over the stream-json control protocol, declared by the runner and answered on the worker's own stdout and stdin, the way the desktop app serves its session tools through the Agent SDK. Check whether the plain CLI accepts this without the SDK.

Measure: what each adds to the floor, tool call latency, and whether the in-process route works at all.

### T5: MCP channel push

1. A stdio MCP server that declares the channel capability, loaded with `--channels` or `--dangerously-load-development-channels`.
2. Push a `notifications/claude/channel` message to T2's worker while it is idle, then to a `--bg` session.

Measure: whether channels are available on this account at all, whether a push wakes an idle session, and latency.

### L: a lease round trip through shep

1. Two Python runner sheep, standing in for two projects and named after them: `koji` and `reactmap`. The dog-side program subscribes to `channel.*`.
2. `koji` raises a `wants.gpu` running total. The dog sees it, runs the equivalent of `shep trigger koji status` (params, when there are any, go positionally after the action), reads the JSON in the reply body, then `shep trigger koji grant gpu`.
3. `reactmap` asks while `koji` holds the lease.
4. Kill `koji` while it holds the lease. The dog must see its exit on the bus and grant `reactmap`.
5. Flood metrics so some drop, and check the running totals still converge.
6. Adopt the dog-side program as a dog, and try `shep trigger` against it by exact name.

Measure: round-trip latency, reclaim time after a crash, behaviour under dropped metrics, and whether triggers reach a dog.

### R: rate-limit visibility

Look for the `five_hour` and `seven_day` utilization and reset times in T2's stream-json output, and in `claude -p --output-format stream-json`, which is Paperclip's mode.

Measure: which modes expose utilization, and how often it updates.

## How the results decide

Proposed thresholds, for the maintainer to confirm:

- T1 stays the worker transport unless T2 or T3 is at least 15% cheaper per turn over the ten-turn run, or gives the maintainer a better takeover at no extra cost per turn.
- T3 depends on an undocumented socket protocol, so it needs a clear win to beat that churn risk.
- Tools: the stdio relay, unless the in-process route works on the plain CLI with a smaller floor.
- Wake-up (T5) matters only if T2 or T3 wins. Under T1, kelpie starts every turn itself and never needs to wake anyone.
- Lease traffic stays on shep if a round trip takes under a second and reclaim works. Otherwise kelpie gets a private socket, as ADR 0002 allows.

## Open questions

1. The thresholds above.
2. The T3 permission rule.
3. Whether the dog-side program for test L is a throwaway under `experiments/`, or the seed of kelpie's dog.
