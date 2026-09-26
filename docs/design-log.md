# Design log

Decisions from the design sessions of 2026-09-24 and 2026-09-25, and the facts behind them. The hard-to-reverse ones are also ADRs in `docs/adr/`. The vocabulary is in `CONTEXT.md`.

## Why kelpie exists

Before kelpie, one long-running Claude session per project acted as a control room. It held the board, rationed reviews, relayed messages between the sessions that owned pull requests, and merged. Measured 2026-09-24 over shep's transcripts, in units weighted by cache pricing (cache read 0.1, one-hour cache write 2, output 5, uncached input 1):

- 2,352 transcripts, 6,020M units. The control room was 7% of that, and subagents were 54%.
- Transcripts that peaked above 500k context were 43% of all spend.
- The control room itself: 7,883 calls, median context 349k, 41 compactions, 80% of its cost in cache reads. 62% of its cost was calls that followed a Bash result (git 20%, gh 17%, the review driver 13%, the GPU queue 12%). Relaying messages was about 15%.

So kelpie moves the control room's rules (gates, locks, rate windows) into code, calls Claude only for the work and for judgement, and controls the other big cost: how large each agent's context grows.

## The shape

- Kelpie is a dog under its own pinned shepherd. Each project is a sheep, kelpie's project runner, and the project's workers are its lambs. See ADR 0001 and ADR 0002.
- Rust for all business code. A Tauri GUI comes after the MVP. Until then kelpie is run and watched from a Claude session.
- One kelpie serves every project, because the GPU lease, the CodeRabbit window and the usage pacer are machine-wide or account-wide. Realistically two projects at once. shep comes first.
- Kelpie and its runners talk through shep: triggers down, `channel.metric` up on the bus. Runners report running totals, because metrics drop under backpressure. A private socket only if the MVP shows the round trips do not hold up.
- Leases live in the dog, and only runners ask for them. Workers and crew never touch the GPU or CodeRabbit. The dog holds the real `gpu.lock` for the length of each round, so the maintainer's own interactive qwen use still queues fairly against it.
- MVP: a thin vertical slice. One project, one worker at a time, merge on `ask`, no GUI.

## Work

- GitHub Issues stay the tracker for the work kelpie does, with the `ready-for-agent` family of triage labels. The board of work items lives inside kelpie.
- A planning session forms work items: the maintainer with an Opus agent, interactive, fed mechanical footprints. Kelpie adds the results to the board and dispatches.
- Overlapping items are bundled when the combined PR stays small, and chained otherwise. The next worker in a chain starts from `--fork-session` of the previous worker's last session.
- One work item, one PR, and every PR boundary is a reset point. A worker that finds more than one PR's worth of work splits the rest into chained work items and divides its remaining budget among them. The motivating case: a shep refactor issue became four PRs in one session with a huge context, though the pieces barely overlapped.
- Conflicts nobody predicted: the item that reaches the gate later rebases after the first merges, and the footprint miss is logged. Serializing instead is a project setting for small codebases.
- What a worker finds along the way: fixed now if small and in a file it already touches, otherwise filed by the worker itself as a `needs-triage` issue.

## Workers

- A worker outlives its sessions. A reset, by compact or by clear with a handoff, keeps the same worker. Which reset is cheaper, and when, is for the work-split tests.
- The worker picks the work split: inline, phased, or a crew. The project manager never does, so it keeps a minimal context for merges, git and gates.
- The calculator runs as needed. Kelpie code reads every call's usage, which is free, and acts only at a phase boundary or a threshold. Inside a turn, a skill plus a hook that fires before an agent spawn or past a context threshold.
- Agents never see budget numbers. Budget talk makes models stop after every task. Budgets live in kelpie's code, and the in-turn hook hands a worker a decision ("delegate the next chunk"), never a cost figure.
- Permissions: `bypassPermissions` with three layers under it. The project's `main` ruleset (shep's already blocks direct pushes, force-pushes and deletion, and requires a PR and passing checks). Claude Code's sandbox, confining Bash writes to the worktree (`sandbox.filesystem.allowWrite`, `failIfUnavailable`). A short denylist of what only the project manager does: `gh pr merge`, `gh pr ready`, adding the `review please` label, reading credential paths. Kelpie flags any label or ready change it did not make and parks that worker. No auto mode: classifier outages have blocked work before.
- Model defaults are placeholders until the calibration series runs. Planning on Opus. Workers on Sonnet, with Opus at medium effort where being wrong is expensive (credentials, deletion, boot, the supervision engine). Review rounds on Sonnet at high effort. Project manager judgement calls on Sonnet, with Opus at medium effort for the read before a merge. Crew on Sonnet or Haiku.

## Review and merge

- Kelpie runs the qwen round itself and hands the findings file straight to the worker's next turn. No model reads the findings and rewrites them.
- The qwen queue: critical first, then closest to merge, then arrival. A running round is never preempted, since a kill takes 20 to 70 seconds and throws the partial round away.
- The Claude round is a fresh review session each time, on Sonnet at high effort, with findings delivered as a file like qwen's.
- The worker opens its own draft PR with its own title and body. Only the project manager summons CodeRabbit (adds `review please`) or marks a PR ready. shep's `.coderabbit.yaml` gates auto-review on that label, so opening a PR spends nothing.
- Merge authority is a project setting: `auto`, `ask`, or `ask-surface` (ask only when a PR touches operator-facing surface). The default is `ask` until kelpie has merged a handful of PRs cleanly.
- The project manager is kelpie code plus one-shot judgement calls (reading commits that landed after a review, auditing a docs PR's claims, checking new plans for overlapping intent) and a state file workers read. Coordination needs footprints, not a codebase map: planned files from the plan, actual files from the branch diff, `git merge-tree` for conflicts, and GitHub issue dependencies for order.

## Budget

- Kelpie paces against the account's weekly window. The daily allowance is what is left of the week divided by the days until reset (100/7 on a fresh week), spread over the hours set at kickoff (default 8, about 1.8% an hour). It covers all account usage, the maintainer's own sessions included.
- The 5-hour window is a local limiter: pause near 50% until it resets.
- The pacer sets concurrency, and logs utilization per unit by hour of day. A peak-hour effect, if one returns, shows up in the data rather than being hard-coded.
- "Do n work items today" comes later, once per-item costs are measured.

## Lift list

Worth lifting, with attribution:

- Paperclip (MIT), `packages/adapters/claude-local/src/server/execute.ts`: the argument builder, which skips `--append-system-prompt-file` on resume (5 to 10k tokens per call, by their comment), the stream-json parser, and the fresh retry on a poisoned session.
- Paperclip, `evaluateSessionCompaction` in `server/src/services/heartbeat.ts`: rotate a session by run count, input tokens or age, with a generated handoff doc.
- Paperclip: decisions and decision queues, for parking a worker on a ruling. The execution-workspace lease, for worktree exclusivity.
- Vibe Kanban (Apache-2.0): `crates/executors/src/executors/claude.rs`, `crates/worktree-manager`, `crates/mcp`.

## Facts

### Headless Claude Code

Measured 2026-09-24 on Claude Code 2.1.282, `--model sonnet`, one sample each. Floor means input plus cache write plus cache read on a trivial prompt.

- Baseline floor 41,012. `--setting-sources ""` gives 22,927, and also drops hooks and plugins. `--strict-mcp-config` with an empty `--mcp-config` gives 36,935. `--disable-slash-commands` gives 36,482. All three together give 26,273. `--tools ""` grows the floor to 91,744.
- `--bare` refuses subscription login. It reads only an API key.
- Two identical fresh calls share the whole prefix: the second reads 41,010 from cache. A different working directory shares only about 10k.
- The first `--resume` onto a fresh session rewrote about 29k of cache in one early probe. Seven runs in the transport series wrote only the new turn (about 5.3k), so it did not reproduce.
- `total_cost_usd` is cumulative across a resumed session. `usage` is per call.
- `/compact` works under `-p`. The result carries `local_command: "compact"` and `num_turns: 0`. Context went from 125,122 to 54,484 on the next turn, which wrote a fresh 42.6k cache. `/compact <instructions>` is accepted.
- Two concurrent `--resume` calls on one session both succeed and run one after the other. `--fork-session` gives a new session id and a separate transcript.
- Flags missing from `--help` but present in the binary: `--max-budget-usd`, `--task-budget`, `--autocompact`, `--session-id`, `--resume-session-at`, `--channels`, `--dangerously-load-development-channels`.
- The CLI parses rate-limit data into `five_hour` and `seven_day` windows (plus `seven_day_opus` and `seven_day_sonnet`), each with a utilization and a reset time. `-p --output-format json` does not carry it, stream-json does as `rate_limit_event`, and headless `claude -p "/usage"` reads it for free.

### How messages reach sessions

Read from the desktop app's bundle and the CLI binary, 2026-09-25.

- Desktop app sessions run through the Agent SDK: the app spawns `claude --input-format stream-json --output-format stream-json` and holds its stdin. Its session tools are an in-process SDK MCP server with no socket or port. Sending a message writes a user turn into the target's stdin. An outside program cannot reach a desktop session.
- CLI sessions each register `~/.claude/sessions/<pid>.json` with a messaging socket (`/tmp/cc-socks/<pid>.sock`) and a 0600 peer token file. `SendMessage` is JSON lines over that socket, with an auth line first. The protocol is undocumented.
- `claude agents --json` lists live sessions, interactive and background, with their status. It is meant for scripting and needs no TTY.
- Background sessions: `claude --bg` prints an id that `claude attach`, `logs`, `stop` and `rm` take. `--bg --resume <id>` continues a session in the background.
- An MCP server can push `notifications/claude/channel` into a session, which would wake it. It is gated per organisation and off on Bedrock and Vertex.
- A socket message wakes an idle background session, about 5 seconds, 3 times out of 3. It arrives framed as a message from another Claude session.

### Usage limits

- Weekly limits were never tied to time of day. Anthropic announced in March 2026 that 5-hour limits drain faster on weekdays between 5 and 11am PT. Secondary sources report that was removed for Claude Code on Pro and Max on 2026-05-06.
- `/usage` reports the share of usage spent while four or more sessions ran in parallel, and says queueing uses the shared limit more evenly. Parallelism is about timing against the 5-hour window, not a surcharge.

### The shep surface kelpie leans on

- `Request::Trigger` becomes an `action` message on a sheep's shepherd channel (fd 3), answered with an `action-reply`. `ready`, `metric` and `action-reply` are all republished on the bus as `channel.*`.
- `SendLine` (`shep whisper`) writes one line to a sheep's stdin.
- A lamb is a pid and an executable name, found by walking the process tree, never argv. The stop ladder kills the process group.
- A dog is an ordinary sheep with a marker. Wildcard selectors never touch a dog. A dog's `--schema` feeds lookout's settings pane.
- Open against shep: shep-pm/shep#623 (an opaque per-dog settings table on a sheep) and shep-pm/shep#624 (a sheep labels its own lambs).

### codebase-memory-mcp

DeusData/codebase-memory-mcp, MIT per its badge, written in C. Indexing is local CPU and costs no tokens ("average repo in milliseconds", the Linux kernel in 3 minutes), stored in SQLite under `~/.cache/codebase-memory-mcp/`, and a background watcher reindexes on git changes. It has 17 MCP tools, including `detect_changes` (git diff to affected symbols) and `trace_path`. Its installer writes to agent config files, so load it through `--mcp-config` instead.

## Test series

In order: transport, model calibration, work split, then code. Each series gets its own spec when it starts.

### Transport

- T1: `claude -p --resume`, one process per turn. The measured baseline.
- T2: a long-lived `claude --input-format stream-json --output-format stream-json` worker, held by a project runner.
- T3: `claude --bg` plus socket messages, attached with `claude attach`. Needs a permission rule for reading the target's peer token.
- T4: kelpie-served tools. In-process MCP over the stream-json control protocol, against a small stdio MCP binary that relays to kelpie over shep.
- T5: MCP channel push, if the account has channels.
- Lease round trip, runner to dog: a "wants" metric, then `status` and `grant` triggers, against a direct `shep trigger` to the dog if that reaches one. Latency, a dropped metric, and reclaim when a runner dies holding a lease.
- Rate limits: whether stream-json carries the utilization windows.

Measure each: cache write and read per turn, wake latency on an idle worker, a 100 KB output line through the runner, recovery after a crash, subscription auth, and whether the maintainer can take a worker over.

### Model calibration

Extend the maintainer's local model-calibration harness, which has only scored local models, to Opus, Sonnet, Haiku and Fable at two or three effort levels each. Reviewer and checker roles reuse its frozen sets: planted bugs, real false findings, and clean diffs for false alarms. A new implementer set of small shep specs with hidden tests scores tests passing, review findings, units and wall clock.

### Work split

Run the same real work items as inline, inline with compaction, a phased chain with handoffs, a crew of subagents working from an implementation doc, and agent teams (only if a long-lived transport wins, since teams do not spawn under `-p`). Run each with and without codebase-memory-mcp.

The model to fit: one session's cache reads grow with the square of its length. Splitting it into k pieces cuts reads about k times and adds a fixed cost per piece, so the best k is about √(0.025 × calls × growth ÷ per-piece cost). Worked example with guessed inputs (150 calls, a 41k floor, 500 output tokens a call): inline to 650k is about 6.9M units, three quarters of it cache reads; five phases, each with a 10k handoff and 30k of re-reading, is about 4.6M. The tests measure the real inputs.

### Other checks

- Claude Code's sandbox with cargo, which writes to `~/.cargo`.
- Whether removing `review please` after a review stops a later push from spending the next window.

## Waiting on the tests

- Worker transport: settled on `claude -p --resume` by the thresholds. See `docs/specs/transport-results.md`: stream-json costs the same per turn and runs 2.2 times faster, which the thresholds did not price.
- How the maintainer takes over a worker.
- A worker settings profile: which of the maintainer's hooks, skills and plugins a worker loads. Workers inherit them today, and in the work-split run a delegation hook blocked a worker's crew.
- Compact or clear, and when.
- The handoff format.
- Model defaults. First data in the experiments repo's `calibration/` (commit d93f73b): as a reviewer and checker, Opus at low effort matches Opus medium (94% checker accuracy, no false alarms) and is the fastest config; Fable medium is the most accurate checker. The implementer set did not separate the configs (all 36 runs passed), so it needs harder cases.
- Work split rules and the calculator's constants. First grid in the experiments repo's `worksplit/results/SUMMARY.md` (commit d93f73b), reading tasks only: no strategy wins on every module. Inline is cheapest per verified finding on two of three modules, a crew has the best precision on two at the highest cost, and phased has the weakest precision everywhere. Implementation tasks are not measured yet.
