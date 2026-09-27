# Design log

Decisions from the design sessions starting 2026-09-24, and the facts behind them. The hard-to-reverse ones are also ADRs in `docs/adr/`. The vocabulary is in `CONTEXT.md`.

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
- Book leases (CodeRabbit first) live in the dog, and only runners and the maintainer ask for them. Workers and crew never touch the GPU or CodeRabbit. The GPU lease is the lock the maintainer's qwen scripts already take, and the dog stays out of it (decided 2026-09-26 on #5, option A): a qwen round takes the lock itself, and `kelpie lease run|take gpu` takes the same lock in the same format.
- A runner asks for a book lease by raising running totals, `lease.<kind>.want.<epoch>` and `lease.<kind>.return.<epoch>`, and the dog grants with `grant <kind> <epoch>`. The epoch is the runner's pid, which shep's process events also carry, so the dog reclaims an old run's leases on `restart` without racing the new run's first metric.
- MVP: a thin vertical slice. One project, one worker at a time, merge on `ask`, no GUI. shep-pm/shep-kelpie#5 is its spec.
- Project settings live in one TOML file per project, `~/.kelpie/projects/<project>/settings.toml`, read when the runner starts. Every setting is required, so a missing or malformed one stops the runner with a message naming it. The project's state file sits beside it and is written atomically.

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
- Workers start with `--setting-sources project,local` plus a settings file kelpie writes for each one. They get the project's CLAUDE.md and project skills, and none of the maintainer's hooks, plugins or skills. The guard hooks the project settings name go into kelpie's file.
- Permissions: `bypassPermissions` with three layers under it. The project's `main` ruleset (shep's already blocks direct pushes, force-pushes and deletion, and requires a PR and passing checks). Claude Code's sandbox, confining Bash writes to the worktree (`sandbox.filesystem.allowWrite`, `failIfUnavailable`, `allowUnsandboxedCommands: false`), with kelpie's `confine` hook holding the file tools to the same folders. A short denylist of what only the project manager does: `gh pr merge`, `gh pr ready`, adding the `review please` label, reading credential paths. Kelpie flags any label or ready change it did not make and parks that worker. No auto mode: classifier outages have blocked work before.
- Model defaults, settled 2026-09-26 and kept as project settings: workers on Sonnet 5 at medium effort, implementing inline. Claude review rounds on Sonnet 5 at medium effort. Judging findings on Opus 5.5 at low effort. The relay on Haiku 4.5 at low effort. A `worker:<model>-<effort>` label on an issue overrides the worker's defaults for that item. Planning stays on Opus.

## Review and merge

- Kelpie runs the qwen round itself and hands the findings file straight to the worker's next turn. No model reads the findings and rewrites them.
- The qwen queue: critical first, then closest to merge, then arrival. A running round is never preempted, since a kill takes 20 to 70 seconds and throws the partial round away.
- The Claude round is a fresh review session each time, never the worker's, on Sonnet 5 at medium effort, with findings delivered as a file like qwen's.
- Opus 5.5 at low effort judges every qwen, Claude and CodeRabbit finding before the worker sees it: whether it holds, and its severity, which it may regrade. The worker fixes only the findings that hold.
- CodeRabbit is in the first build's gates. Its free plan reviews public repos only, so the gate is a per-project setting, and the runner refuses to start with it on for a repo the forge reports as private. With it off, a work item goes from the qwen-review loop and CI straight to the merge ruling.
- The worker opens its own draft PR with its own title and body. Only the project manager summons CodeRabbit (adds `review please`) or marks a PR ready. shep's `.coderabbit.yaml` gates auto-review on that label, so opening a PR spends nothing.
- Merge authority is a project setting: `auto`, `ask`, or `ask-surface` (ask only when a PR touches operator-facing surface). The default is `ask` until kelpie has merged a handful of PRs cleanly, and the first build accepts only `ask`.
- The relay, a stopgap for the first build: a background Claude Code session that kelpie sends each ruling to over the session's messaging socket. It pushes the question to the maintainer's phone and passes the reply back verbatim through `shep trigger`. A merge yes through it needs the maintainer's tap on a permission prompt. It rides an undocumented protocol and costs tokens per ruling, so a webhook (Discord or ntfy) is the fallback now and becomes the primary path when the relay goes.
- The qwen-review loop (shep-pm/shep-kelpie#11). Once the worker's draft pull request is open, `Phase::Review` runs before CI: rounds alternate qwen and Claude, qwen first, each round's raw findings judged one at a time by Opus before the worker ever sees them. A round with no findings is clean at once; one with findings, once every finding is judged, sends only the held ones to the worker's next turn as a file, using the judge's own (possibly regraded) severity for the nit rule. The loop ends once two rounds in a row hold nothing above a nit, which a strict alternation makes one of each reviewer without a special case. Past the round guard (`review.loop_guard`, default 8), the worker parks for a ruling; a yes clears the guard for the rest of that work item, rather than asking again every following round. Kelpie never takes or holds the qwen GPU lock itself: the script does, waiting up to an hour, which is normal, not a hang. A round's findings and its completion marker both come from disk (`QWEN_REVIEW_OUT`, redirected per work item rather than the script's own path slug, so a test rig never touches the maintainer's real `~/.claude`), never from stdout. A file the script skips as too large is cut into a `-U25` hunk against `origin/main` and reviewed again on its own, with its findings folded back in against the original path. The Claude round and the judge run with no sandbox and no bypassed permissions, since neither writes anything; the Claude round may read the worktree with Read, Grep or Glob to check its work, the same as `pi` mode in the maintainer's own qwen-review skill.
- The merge gate (shep-pm/shep-kelpie#9). After the qwen-review loop settles, kelpie waits for CI. A red run is the worker's next turn, naming the failed checks; a second red run on a head the worker left alone parks it on a ruling. A green run on a branch that has the latest `main` raises the merge ruling, and a branch without it is rebased and pushed by the project manager first. A conflict parks the worker on a ruling.
- A ruling is a pull request comment, a log line and a status entry, all carrying the same question and the triggers that answer it. Ruling ids never repeat. A no with a note is the worker's next turn.
- A yes merges only the head it was asked about, while CI on it is green and it has the latest `main`. Otherwise kelpie withdraws the yes, reruns CI, and asks again. The merge is `gh pr merge --merge --match-head-commit`, never a squash, and kelpie then removes the worktree, both branches and the build folder.
- A pull request the maintainer merges by hand ends its work item with no merge by kelpie. One closed without merging parks the worker.
- Kelpie records each issue whose work item it finished, merged or dropped, and the board never takes one again: GitHub closes an issue from `Resolves` a moment after the merge, and the playground's board re-picked #22 in that moment. `shep trigger <project> drop` ends the work item in flight without merging, paused or not, keeping its pull request and branch on the forge.
- A work item saved before the gate existed stays with its worker until `shep trigger <project> gate` sends it in. The trigger takes only an item whose turn has ended with a known pull request.
- The project manager is kelpie code plus one-shot judgement calls (reading commits that landed after a review, auditing a docs PR's claims, checking new plans for overlapping intent) and a state file workers read. Coordination needs footprints, not a codebase map: planned files from the plan, actual files from the branch diff, `git merge-tree` for conflicts, and GitHub issue dependencies for order.

## Budget

- Kelpie paces against the account's weekly window. The daily allowance is what is left of the week divided by the days until reset (100/7 on a fresh week), spread over the hours set at kickoff (default 8, about 1.8% an hour). It covers all account usage, the maintainer's own sessions included.
- The 5-hour window is a local limiter: pause near 50% until it resets.
- Built in #15. A day is 24 hours counted from the weekly reset, so the days until reset are always whole. The allowance is fixed when the day begins (what the week had left, over the days left) and kept in the state file, so a restart keeps it. Today's spend is the week's utilization now minus its utilization at that read, the maintainer's own sessions included. Once spend reaches the allowance, no work item is dispatched and the one in flight continues. The kickoff hours only divide the allowance into the per-hour figure `status` shows.
- A maintainer's `add` is not held by the daily allowance, the way it already skips the board's screening. The 5-hour window still holds its turns.
- Decided 2026-09-27 on #15: the allowance is one figure for the day, not spread by the hour. The kickoff hours only produce the per-hour figure `status` shows, given the gates already in place: the 5-hour window and the board's own screening.
- The 50% mark stops a turn from starting, dispatch included, until the window resets. Usage is read only before a dispatch and before a turn that has not begun, never mid-turn, and a turn resumed after a restart is not held. A hold is trusted for at most 10 minutes, since reset times are printed to the minute and a window can reset mid-read.
- When usage cannot be read, the pacer holds both limits and says so in `status`. A pacer that guessed would spend the account it exists to protect.
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
- `total_cost_usd` is cumulative across a resumed session. `usage` is per call. Measured again 2026-09-26 on 2.1.283: `modelUsage` is cumulative too, and the top-level `usage` sums every request inside one call. The recorded pair is in `crates/kelpie/fixtures/`.
- A call killed within its first second leaves no transcript. `--resume` onto it prints "No conversation found with session ID" and exits 1 with no JSON, and `--session-id` can take the same id again. Once a transcript exists, `--session-id` refuses the id as already in use.
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
- Headless `/usage`, measured 2026-09-26 on Claude Code 2.1.283 (recorded in `crates/kelpie/fixtures/usage-result.json`): `total_cost_usd` 0, `num_turns` 0, `local_command: "usage"`, about 1.5 to 3 seconds. The `result` text has one line per window, `Current session: 2% used · resets Sep 27 at 3:40am (America/New_York)`, and a `Current week (all models)` line beside per-model ones (`Current week (Fable)`). Percentages are whole numbers.
- Reset times carry no year and are rounded to the minute. The same weekly reset printed as `Sep 25 at 10:59pm` on two reads and `Sep 25 at 11pm` on a third, so a weekly reset that moves by under an hour is the same week. A reset later today may print as time only.
- `--setting-sources ""` and `--no-session-persistence` both leave `/usage` working, so the read runs none of the maintainer's hooks and writes no transcript.

### Worker profile and sandbox

Measured 2026-09-26 on Claude Code 2.1.283.

- `--setting-sources ""` also drops the project's CLAUDE.md and project skills, so workers do not use it.
- `--setting-sources project,local` keeps both and drops the maintainer's hooks, plugins and skills. It brought the floor from 29.2k to 24.8k. It also loaded the maintainer's global CLAUDE.md in one probe.
- A hook passed through `--settings` still runs under `project,local`.
- The sandbox keys exist as named: `sandbox.enabled`, `sandbox.failIfUnavailable`, `sandbox.filesystem.allowWrite` and `sandbox.network.allowedDomains`. With only the worktree and the build folder writable, a warm-cache `cargo check` passed and a write to the home folder was refused.
- The build folder must exist before the worker starts.
- The sandbox confines Bash and its children only. Claude's Edit and Write tools go through permissions, and under `bypassPermissions` only deny rules and hooks stop them. So kelpie adds a PreToolUse hook, `kelpie confine`, that holds the file tools to the worktree and the build folder. Live, it refused a Write to the home folder under `bypassPermissions`.
- When the working folder is a git worktree, Claude Code opens the repo's whole common git dir to sandboxed writes, `config` and `hooks` included, whatever `allowWrite` says. A worker could plant a hook that kelpie later runs unsandboxed.
- `denyWrite` beats `allowWrite`, so the git dir cannot be narrowed by allowing less. Denying `config`, `hooks`, `info`, `HEAD`, `index`, `packed-refs`, `refs/remotes` and `refs/tags` still lets a commit from the worktree land, and refuses writes to all of them. A wildcard deny spans `/`: denying `refs/heads/*` also refused the work item's own `refs/heads/kelpie/7`.
- The worker's settings file still lists what a commit writes (`objects`, the worktree's own git dir, and its branch's ref, lock and reflog), in case a later Claude Code stops opening the git dir itself. On this version that list cannot be shown minimal, since the whole dir is open anyway.
- Live on the pinned worker repo's commit (#7's demo), two workers committed on their branches inside the sandbox. One ran cargo into a cold build folder with a warm registry, 2.4 GB of it. Writes outside were refused through Bash and through Write, and so were `.git/config`, `~/.ssh` and `gh pr merge`.
- The pinned worker repo, `~/.kelpie/repos/shep`, has no `origin` remote, so kelpie cannot cut branches in it. A runner now refuses to start on a repo without one. The demo ran on a clone whose origin held the pinned commit as `main`.

### A worker's push, `gh` and package installs

Measured 2026-09-26 on Claude Code 2.1.283, gh 2.96 and bun 1.4.0, from a sandboxed worker on a private bun, Vite and TypeScript project, the playground.

- `git push origin <branch>` reaches GitHub through the system's osxkeychain credential helper. With `refs/remotes` denied, the push lands but the tracking ref fails to write. So `refs/remotes` is no longer denied: kelpie fetches `origin/main` before cutting a branch, and a fetch forces tracking refs.
- `git push -u` cannot write `config`, which stays denied. Workers push with `git push origin HEAD`.
- `gh` will not start without `~/.config/gh/config.yml`, so that folder is no longer read-denied. It holds no token on the maintainer's machine; the token is in the keychain. Writes there are still refused. `gh` will not start without `hosts.yml` either, so it cannot be denied alone. On a machine where `gh` keeps its token in a file rather than the keychain, `hosts.yml` holds the token, and the folder must stay denied.
- `gh auth` is denied, since `gh auth token` prints the maintainer's token.
- Inside the sandbox, `gh` fails TLS verification with `x509: OSStatus -26276`. Claude Code's docs name this for Go tools on macOS. `sandbox.enableWeakerNetworkIsolation` fixes it and keeps `gh` inside the write fence and the domain allowlist. `excludedCommands` would have run `gh` unsandboxed.
- A cold `bun install` reaches only `registry.npmjs.org`. Inside the sandbox it fails with "unable to write files to tempdir: EPERM", because bun stages downloads in `~/.bun/install/cache`. With `BUN_INSTALL_CACHE_DIR` in the build folder it installed 733 packages in 9.7 seconds, and the project's tests and typecheck passed.
- The sandbox refused deleting `.idea` folders that a bun install outside it had left in `node_modules`. Installing them from inside worked.
- Final fence, for every worker. Writable: the worktree, the build folder, the common git dir's `objects`, the worktree's own git dir, and the branch's ref, lock and reflog under both `refs/heads` and `refs/remotes/origin`. Denied inside the git dir: `config`, `hooks`, `info`, `modules`, `HEAD`, `index`, `packed-refs` and `refs/tags`. Domains: `github.com` and `api.github.com`. Also denied: the clone's `refs/heads/main`. Denied commands, besides merge, ready and the `review please` label: `gh api`, `gh auth`, a push naming `main` as `origin main`, `HEAD:main` or `refs/heads/main`, and a push carrying `--mirror`, `--all`, `--delete`, `-d`, `--force`, `-f` or a `+` refspec. Those are pattern rules over the command text, so a script can get past them; a token scoped to a worker's needs would be a real fence.
- Per project, `worker.allowed_domains` adds domains (shep: `crates.io`, `index.crates.io`, `static.crates.io`; the playground: `registry.npmjs.org`), and `worker.build_env` points tool caches into the build folder (the playground: `BUN_INSTALL_CACHE_DIR`).

### The relay

Measured 2026-09-26 in the experiments repo.

- Two rulings sent over the messaging socket woke a background session in about 2 seconds each.
- Its pushes reached the maintainer's phone, and replies from the phone came back verbatim.
- The merge approval needed a tap on the permission prompt, even in auto mode.
- A question cost 12k to 26k units and an answer about 11k.

### CI and the merge

Read 2026-09-26 with gh 2.96.

- `gh pr view --json statusCheckRollup` lists two kinds of check: a `CheckRun` (GitHub Actions, with `status` and `conclusion`) and a `StatusContext` (a commit status such as CodeRabbit's, with `state`). shep's checks include `SKIPPED` runs, which count as passing.
- The playground's pull requests carry no checks at all: an empty rollup. GitHub cannot tell that from CI not registered yet, so a project says whether it runs CI with `ci` in its settings. With it on, an empty rollup is pending, never green; with it off, kelpie reads no checks. The playground's settings have `ci = false`, set 2026-09-27.
- A check set registers a check at a time, so kelpie reads a verdict from a head's rollup only two minutes after it first saw that head, and merges two minutes after marking a draft ready, which can start a fresh run on the same head.
- `gh pr view` lags a push. Seen live 2026-09-27 on the playground: two seconds after kelpie pushed a rebase, it still reported the old head. Kelpie now reads the branch on `origin` after each fetch and waits while the forge's head differs.
- `gh pr merge --delete-branch` also deletes and switches branches in the checkout gh runs from, so kelpie leaves it out and removes the branch with `git push origin --delete`.
- The runner's own `gh` and `git` calls are plain child processes. The worker's settings file, its sandbox and its denylist apply only to `claude` sessions started with it, so they never reach the project manager's merge, rebase or `--force-with-lease` push.

### CodeRabbit

- The free plan reviews public repos only.
- The included quota on shep was 1 review an hour when last read. It has changed without notice before, so kelpie reads it from each review's footer.

### The shep surface kelpie leans on

- `Request::Trigger` becomes an `action` message on a sheep's shepherd channel (fd 3), answered with an `action-reply`. `ready`, `metric` and `action-reply` are all republished on the bus as `channel.*`.
- `SendLine` (`shep whisper`) writes one line to a sheep's stdin.
- A lamb is a pid and an executable name, found by walking the process tree, never argv. The stop ladder kills the process group.
- A sheep with `shutdown_with_message` that exits on the message never reaches the stop ladder, so its lambs are orphaned. Seen 2026-09-26: `shep restart` left the worker running beside the resumed one. The runner now stops its `claude` children itself before it exits: SIGTERM, then SIGKILL after 3 seconds.
- A dog is an ordinary sheep with a marker. Wildcard selectors never touch a dog. A dog's `--schema` feeds lookout's settings pane.
- A sheep under the pinned shep 0.10.1 starts with only `HOME`, `LANG`, `PATH`, `USER` and its `SHEP_*` variables, read with `ps eww` on 2026-09-26. `TMPDIR` and `SHEP_HOME` are not among them, though the shepherd itself has both. So `${TMPDIR:-/tmp}` names `/tmp` under the shepherd and macOS's per-user temporary folder in the maintainer's shell: two different GPU locks. Kelpie falls back to `getconf DARWIN_USER_TEMP_DIR`, the folder macOS sets `TMPDIR` from at login, and whatever runs a qwen script for kelpie must pass it on as `TMPDIR`.
- shep emits `restart` after the new process is spawned, with the new pid in its info (read from shep-daemon 0.10.0's `actor_lifecycle.rs`). A crash that will restart emits `exit` first.
- Kelpie's lease round trip through the pinned shepherd, three runs of `crates/kelpie/tests/dog_smoke.rs` on 2026-09-26: want to grant 1, 6 and 11 ms, and a SIGKILLed holder to the next grant 8, 11 and 9 ms.
- `shep trigger <sheep> <action> [params]` takes params as one argument, passed through verbatim (read from shep 0.10.0's `cli/sheep.rs`). So an answer with more than one word is quoted: `shep trigger hazels-lab rule '3 no rename the flag'`. Unquoted, clap refuses the extra arguments.
- Open against shep: shep-pm/shep#623 (an opaque per-dog settings table on a sheep) and shep-pm/shep#624 (a sheep labels its own lambs).

### codebase-memory-mcp

DeusData/codebase-memory-mcp, MIT per its badge, written in C. Indexing is local CPU and costs no tokens ("average repo in milliseconds", the Linux kernel in 3 minutes), stored in SQLite under `~/.cache/codebase-memory-mcp/`, and a background watcher reindexes on git changes. It has 17 MCP tools, including `detect_changes` (git diff to affected symbols) and `trace_path`. Its installer writes to agent config files, so load it through `--mcp-config` instead. Claude Code lists its tools as deferred behind a tool search: in the implementation series no worker looked them up unprompted, in 12 runs out of 12.

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

- Claude Code's sandbox with cargo, which writes to `~/.cargo`. A warm cache passes (see Worker profile and sandbox). A cold cache waits for the hands-on run.
- Whether removing `review please` after a review stops a later push from spending the next window.

## Waiting on the tests

- Worker transport: settled on `claude -p --resume` by the thresholds. See `docs/specs/transport-results.md`: stream-json costs the same per turn and runs 2.2 times faster, which the thresholds did not price.
- How the maintainer takes over a worker.
- A worker settings profile: settled on `--setting-sources project,local` plus kelpie's settings file. See Workers, and Worker profile and sandbox under Facts.
- Compact or clear, and when.
- The handoff format.
- Model defaults: settled 2026-09-26 (see Workers). First data in the experiments repo's `calibration/` (commit d93f73b): as a reviewer and checker, Opus at low effort matches Opus medium (94% checker accuracy, no false alarms) and is the fastest config; Fable medium is the most accurate checker. The first implementer set did not separate the configs (all 36 runs passed). The harder set barely did either (23 of 24 passed; `docs/specs/implementation-results.md`), and its proposed rule picks Sonnet at medium effort, the cheapest config within one case of the best. Haiku cost more than Sonnet there. The maintainer took that rule for workers.
- Work split rules and the calculator's constants. First grid in the experiments repo's `worksplit/results/SUMMARY.md` (commit d93f73b), reading tasks only: no strategy wins on every module. Inline is cheapest per verified finding on two of three modules, a crew has the best precision on all three (0.67, 0.51, 0.62) at the highest cost, and phased has the weakest precision everywhere. On implementation tasks (`docs/specs/implementation-results.md`) every strategy passed 12 of 12 on Sonnet medium, so the proposed rule picks the cheapest per pass: inline at $0.36, against crew at $0.59 and phased at $0.65. Crew leads often skipped delegating. Whether codebase-memory-mcp helps an implementer is unmeasured, because no run called it.
